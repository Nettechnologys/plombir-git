import { parse } from 'svelte/compiler';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';

import en from './translations/en.json';
import zhCN from './translations/zh-CN.json';

type StaticTranslationCall = {
	file: string;
	key: string;
	line: number;
	hasStringFallback: boolean;
	usesLogicalOrFallback: boolean;
};

type EstreeNode = {
	arguments?: EstreeNode[];
	callee?: EstreeNode;
	expressions?: EstreeNode[];
	left?: EstreeNode;
	loc?: { start: { line: number } };
	name?: string;
	operator?: string;
	quasis?: Array<{ value?: { cooked?: string } }>;
	raw?: string;
	type?: string;
	value?: unknown;
	[key: string]: unknown;
};

const productionSources = import.meta.glob(
	['../../**/*.js', '../../**/*.svelte', '../../**/*.ts'],
	{ eager: true, import: 'default', query: '?raw' }
) as Record<string, string>;

function catalogHasString(catalog: unknown, key: string): boolean {
	let value = catalog;
	for (const segment of key.split('.')) {
		if (typeof value !== 'object' || value === null || !(segment in value)) return false;
		value = (value as Record<string, unknown>)[segment];
	}
	return typeof value === 'string';
}

function estreeString(node: EstreeNode | undefined): string | null {
	if (node?.type === 'Literal' && typeof node.value === 'string') return node.value;
	if (node?.type === 'TemplateLiteral' && node.expressions?.length === 0) {
		return node.quasis?.[0]?.value?.cooked ?? null;
	}
	return null;
}

function svelteTranslationCalls(source: string, file: string): StaticTranslationCall[] {
	const calls: StaticTranslationCall[] = [];
	const visited = new WeakSet<object>();

	function visit(value: unknown, parent: EstreeNode | null): void {
		if (Array.isArray(value)) {
			for (const item of value) visit(item, parent);
			return;
		}
		if (typeof value !== 'object' || value === null || visited.has(value)) return;
		visited.add(value);

		const node = value as EstreeNode;
		if (node.type === 'CallExpression' && node.callee?.type === 'Identifier' && node.callee.name === 't') {
			const key = estreeString(node.arguments?.[0]);
			if (key !== null) {
				calls.push({
					file,
					key,
					line: node.loc?.start.line ?? 0,
					hasStringFallback: estreeString(node.arguments?.[1]) !== null,
					usesLogicalOrFallback:
						parent?.type === 'LogicalExpression' &&
						parent.operator === '||' &&
						parent.left === node
				});
			}
		}

		for (const child of Object.values(node)) visit(child, node);
	}

	visit(parse(source, { modern: true }), null);
	return calls;
}

function typescriptTranslationCalls(source: string, file: string): StaticTranslationCall[] {
	const calls: StaticTranslationCall[] = [];
	const scriptKind = file.endsWith('.js') ? ts.ScriptKind.JS : ts.ScriptKind.TS;
	const sourceFile = ts.createSourceFile(file, source, ts.ScriptTarget.Latest, true, scriptKind);
	const staticString = (node: ts.Expression | undefined): string | null =>
		node && (ts.isStringLiteral(node) || ts.isNoSubstitutionTemplateLiteral(node))
			? node.text
			: null;

	function visit(node: ts.Node): void {
		if (
			ts.isCallExpression(node) &&
			ts.isIdentifier(node.expression) &&
			node.expression.text === 't' &&
			node.arguments.length > 0 &&
			staticString(node.arguments[0]) !== null
		) {
			const parent = node.parent;
			const position = sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile));
			calls.push({
				file,
				key: staticString(node.arguments[0])!,
				line: position.line + 1,
				hasStringFallback: staticString(node.arguments[1]) !== null,
				usesLogicalOrFallback:
					ts.isBinaryExpression(parent) &&
					parent.left === node &&
					parent.operatorToken.kind === ts.SyntaxKind.BarBarToken
			});
		}
		ts.forEachChild(node, visit);
	}

	visit(sourceFile);
	return calls;
}

const calls = Object.entries(productionSources).flatMap(([path, source]) => {
	const file = path.replace(/^\.\.\/\.\.\//, '');
	if (/\.(?:spec|test)\.[^.]+$/.test(file)) return [];
	return file.endsWith('.svelte')
		? svelteTranslationCalls(source, file)
		: typescriptTranslationCalls(source, file);
});

describe('translation catalog coverage', () => {
	it('keeps every static translation without an explicit fallback in both catalogs', () => {
		const catalogs = [
			['en', en],
			['zh-CN', zhCN]
		] as const;
		const missing = calls
			.filter((call) => !call.hasStringFallback)
			.flatMap((call) =>
				catalogs
					.filter(([, catalog]) => !catalogHasString(catalog, call.key))
					.map(([locale]) => `${call.file}:${call.line} ${locale}: ${call.key}`)
			)
			.sort();

		expect(missing).toEqual([]);
	});

	it('does not pretend logical OR can provide a fallback after t()', () => {
		const deadFallbacks = calls
			.filter((call) => call.usesLogicalOrFallback)
			.map((call) => `${call.file}:${call.line} ${call.key}`)
			.sort();

		expect(deadFallbacks).toEqual([]);
	});
});
