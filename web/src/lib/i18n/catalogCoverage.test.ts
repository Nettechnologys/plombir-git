import { parse } from 'svelte/compiler';
import ts from 'typescript';
import { describe, expect, it, vi } from 'vitest';
// This test alone reads the checked-out sources. Keep the app's deliberate
// no-Node-globals type boundary intact instead of adding `@types/node` to all
// of `src/` for two test-only modules.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { readFileSync, readdirSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { extname, join, relative } from 'node:path';

declare const process: { cwd(): string };

import { formatTranslationFallback, t } from '.';
import en from './translations/en.json';
import zhCN from './translations/zh-CN.json';

type TranslationCall = {
	file: string;
	key: string | null;
	line: number;
	hasStringFallback: boolean;
	hasDynamicFallback: boolean;
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

function sourceFiles(
	root: string,
	extensions: ReadonlySet<string>,
	prefix = '',
): Record<string, string> {
	const sources: Record<string, string> = {};
	for (const entry of readdirSync(root, { withFileTypes: true })) {
		const path = join(root, entry.name);
		if (entry.isDirectory()) {
			Object.assign(sources, sourceFiles(path, extensions, prefix));
		} else if (extensions.has(extname(entry.name))) {
			const file = relative(prefix || root, path).replaceAll('\\', '/');
			sources[file] = readFileSync(path, 'utf8');
		}
	}
	return sources;
}

const sourceRoot = join(process.cwd(), 'src');
const productionSources = sourceFiles(sourceRoot, new Set(['.js', '.svelte', '.ts']), sourceRoot);

// The catalogs as text, not as parsed objects. A key written twice in the same
// object is already gone by the time `import ... from '*.json'` hands the
// catalog over — the parser keeps one of the two values and says nothing — so
// the only place that question can still be asked is the source.
const catalogRoot = join(sourceRoot, 'lib', 'i18n', 'translations');
const catalogSources = sourceFiles(catalogRoot, new Set(['.json']), catalogRoot);

type DuplicateKey = { path: string; key: string };

/**
 * Every key that appears more than once inside the same object.
 *
 * A JSON scanner rather than `JSON.parse`, for the reason above: this has to
 * see the members before one of them is dropped. It only tracks where objects
 * begin and end — numbers, strings and literals are skipped whole, since no key
 * can live inside one.
 */
function duplicateKeys(source: string): DuplicateKey[] {
	const found: DuplicateKey[] = [];
	let at = 0;

	function fail(message: string): never {
		throw new Error(`${message} at offset ${at}`);
	}

	function skipWhitespace(): void {
		while (at < source.length && /\s/.test(source[at])) at++;
	}

	function readString(): string {
		if (source[at] !== '"') fail('expected a string');
		at++;
		let out = '';
		while (at < source.length) {
			const char = source[at++];
			if (char === '"') return out;
			if (char !== '\\') {
				out += char;
				continue;
			}
			const escape = source[at++];
			if (escape === 'u') {
				out += String.fromCharCode(Number.parseInt(source.slice(at, at + 4), 16));
				at += 4;
			} else {
				out += ({ b: '\b', f: '\f', n: '\n', r: '\r', t: '\t' } as Record<string, string>)[escape] ?? escape;
			}
		}
		fail('unterminated string');
	}

	function readValue(path: string): void {
		skipWhitespace();
		const char = source[at];
		if (char === '{') return readObject(path);
		if (char === '[') return readArray(path);
		if (char === '"') {
			readString();
			return;
		}
		// A number or a literal: advance to whatever ends it.
		while (at < source.length && !',}] \t\n\r'.includes(source[at])) at++;
	}

	function readArray(path: string): void {
		at++;
		skipWhitespace();
		if (source[at] === ']') {
			at++;
			return;
		}
		for (;;) {
			readValue(`${path}[]`);
			skipWhitespace();
			if (source[at] === ',') {
				at++;
				continue;
			}
			if (source[at] === ']') {
				at++;
				return;
			}
			fail('expected , or ] inside an array');
		}
	}

	function readObject(path: string): void {
		at++;
		const seen = new Set<string>();
		skipWhitespace();
		if (source[at] === '}') {
			at++;
			return;
		}
		for (;;) {
			skipWhitespace();
			const key = readString();
			if (seen.has(key)) found.push({ path: path || '(root)', key });
			seen.add(key);
			skipWhitespace();
			if (source[at] !== ':') fail('expected : after a key');
			at++;
			readValue(path ? `${path}.${key}` : key);
			skipWhitespace();
			if (source[at] === ',') {
				at++;
				continue;
			}
			if (source[at] === '}') {
				at++;
				return;
			}
			fail('expected , or } inside an object');
		}
	}

	readValue('');
	return found;
}

/** Every key path a catalog resolves to a string, in `a.b.c` form. */
function stringKeyPaths(catalog: unknown, prefix = ''): string[] {
	if (typeof catalog !== 'object' || catalog === null) return [];
	return Object.entries(catalog).flatMap(([key, value]) => {
		const path = prefix ? `${prefix}.${key}` : key;
		return typeof value === 'string' ? [path] : stringKeyPaths(value, path);
	});
}

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

function svelteTranslationCalls(source: string, file: string): TranslationCall[] {
	const calls: TranslationCall[] = [];
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
			calls.push({
				file,
				key,
				line: node.loc?.start.line ?? 0,
				hasStringFallback: estreeString(node.arguments?.[1]) !== null,
				hasDynamicFallback: node.arguments?.[2] !== undefined,
				usesLogicalOrFallback:
					parent?.type === 'LogicalExpression' &&
					parent.operator === '||' &&
					parent.left === node
			});
		}

		for (const child of Object.values(node)) visit(child, node);
	}

	visit(parse(source, { modern: true }), null);
	return calls;
}

function typescriptTranslationCalls(source: string, file: string): TranslationCall[] {
	const calls: TranslationCall[] = [];
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
			node.arguments.length > 0
		) {
			const parent = node.parent;
			const position = sourceFile.getLineAndCharacterOfPosition(node.getStart(sourceFile));
			calls.push({
				file,
				key: staticString(node.arguments[0]),
				line: position.line + 1,
				hasStringFallback: staticString(node.arguments[1]) !== null,
				hasDynamicFallback: node.arguments[2] !== undefined,
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
	const file = path;
	if (/\.(?:spec|test)\.[^.]+$/.test(file)) return [];
	return file.endsWith('.svelte')
		? svelteTranslationCalls(source, file)
		: typescriptTranslationCalls(source, file);
});

describe('translation catalogs', () => {
	// Without this the duplicate check could pass by scanning nothing at all,
	// which is the failure mode of every gate that only ever reports an empty
	// list — and it is what this file's own coverage checks looked like from
	// the outside while `repo.private` sat in `en.json` twice.
	it('can see a duplicate key at all', () => {
		expect(duplicateKeys('{"a": {"b": 1, "b": 2}, "c": [{"d": 1, "d": 2}]}')).toEqual([
			{ path: 'a', key: 'b' },
			{ path: 'c[]', key: 'd' }
		]);
		expect(duplicateKeys('{"a": {"b": 1}, "b": 2}')).toEqual([]);
		expect(Object.keys(catalogSources).length).toBeGreaterThan(1);
	});

	// One key, one value. Two members with the same name are not a duplicate in
	// any useful sense: the parser keeps one of them and drops the other, so
	// which translation a page shows is decided by parser order rather than by
	// whoever edited the file — and editing the losing line changes nothing,
	// with no error anywhere to say why (card_394be96f6058).
	it('give every key exactly one value', () => {
		const duplicates = Object.entries(catalogSources)
			.flatMap(([file, source]) =>
				duplicateKeys(source).map(({ path, key }) => `${file}: ${path}.${key}`)
			)
			.sort();

		expect(duplicates).toEqual([]);
	});

	// The coverage checks below only ask whether a key a source file *uses* is
	// present. A key that exists in one catalog and not the other passes them
	// untouched — which is how the two files drift apart, one untranslated
	// string at a time.
	it('carry the same set of keys as each other', () => {
		const english = new Set(stringKeyPaths(en));
		const chinese = new Set(stringKeyPaths(zhCN));

		expect({
			missingFromChinese: [...english].filter((key) => !chinese.has(key)).sort(),
			missingFromEnglish: [...chinese].filter((key) => !english.has(key)).sort()
		}).toEqual({ missingFromChinese: [], missingFromEnglish: [] });
	});
});

describe('translation catalog coverage', () => {
	it('keeps every static translation without an explicit fallback in both catalogs', () => {
		const catalogs = [
			['en', en],
			['zh-CN', zhCN]
		] as const;
		const missing = calls
			.filter((call): call is TranslationCall & { key: string } =>
				call.key !== null && !call.hasStringFallback
			)
			.flatMap((call) =>
				catalogs
					.filter(([, catalog]) => !catalogHasString(catalog, call.key))
					.map(([locale]) => `${call.file}:${call.line} ${locale}: ${call.key}`)
			)
			.sort();

		expect(missing).toEqual([]);
	});

	it('keeps every dynamic translation behind an explicit fallback', () => {
		const unguarded = calls
			.filter((call) => call.key === null && !call.hasDynamicFallback)
			.map((call) => `${call.file}:${call.line}`)
			.sort();

		expect(unguarded).toEqual([]);
	});

	it('uses a readable fallback for an unknown dynamic key with interpolation params', () => {
		const warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
		try {
			expect(
				t('pulls.timeline.future.event', { actor: 'alice' }, formatTranslationFallback('future.event'))
			).toBe('Future event');
			expect(formatTranslationFallback('waiting_approval')).toBe('Waiting approval');
			expect(formatTranslationFallback(null)).toBe('Unknown');
		} finally {
			warn.mockRestore();
		}
	});

	it('does not pretend logical OR can provide a fallback after t()', () => {
		const deadFallbacks = calls
			.filter((call) => call.usesLogicalOrFallback)
			.map((call) => `${call.file}:${call.line} ${call.key}`)
			.sort();

		expect(deadFallbacks).toEqual([]);
	});
});
