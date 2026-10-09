import { parse } from 'svelte/compiler';
import { describe, expect, it } from 'vitest';
// Same deliberate boundary as `catalogCoverage.test.ts`: Node declarations are
// absent from the app, and only these source-reading tests need them.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { readFileSync, readdirSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { join, relative } from 'node:path';

declare const process: { cwd(): string };

// card_0d18cf31d13b: switching language left whole pages in English, because
// their text was never routed through `t()` in the first place — seven pages
// had no `t()` call at all, and fifteen had no `<title>`, so the browser tab
// kept naming whatever page came before. This gate reads every route and
// shared component and fails on user-visible English written straight into
// the markup.

type SvelteNode = {
	type?: string;
	name?: string;
	data?: string;
	start?: number;
	attributes?: SvelteNode[];
	value?: unknown;
	[key: string]: unknown;
};

/** Attributes whose static text a user reads or a screen reader speaks. */
const USER_VISIBLE_ATTRIBUTES = new Set([
	'placeholder',
	'title',
	'aria-label',
	'aria-description',
	'aria-placeholder',
	'alt',
	'label',
	'ariaLabel',
]);

/**
 * Elements whose text is code, not prose: a clone command, a config sample, a
 * keyboard key. Translating `git push` would break it.
 */
const CODE_ELEMENTS = new Set(['code', 'pre', 'kbd', 'samp', 'script', 'style']);

/**
 * Names that are the same in every language. Removed from a string before it
 * is judged, so `{t('explore.title')} · Plombir Git` passes but
 * `Explore · Plombir Git` does not.
 */
const PROPER_NAMES = [/Plombir Git/g, /ForgeKeep/g];

/**
 * Whole strings that are not prose: protocol names, technology names, example
 * values in a placeholder (a branch called `main` is called `main` in any
 * language), units. Keep this list to tokens a translator would leave alone —
 * a word that merely happens to look technical belongs in the catalogs.
 */
const NON_TRANSLATABLE = new Set([
	// Protocols, products and technologies, named the same in every language.
	'HTTPS',
	'SSH',
	'HTTP · SSH',
	'LFS',
	'CI/CD',
	'Rust',
	'Git',
	'GitHub',
	'GitLab',
	'Gitea',
	'Google Workspace',
	'application/json',
	'application/x-www-form-urlencoded',
	// Units, prefixes and keywords rendered next to a value.
	'v',
	'if',
	'📄 README.md',
	// Example values in placeholders: identifiers, patterns, URLs and names a
	// user types verbatim, so translating them would teach the wrong input.
	'main',
	'v*',
	'repo',
	'DEPLOY_TOKEN',
	'alice, bob',
	'alice-agent',
	'alice/app',
	'claude-code',
	'get_issue, create_issue, create_pr',
	'linux-runner-01',
	'gpu,a100',
	'example.com, partner.org',
	'test [os=linux, version=stable]',
	'https://example.com/webhook',
	'https://github.com/org/repo.git',
	'https://idp.example.com/.well-known/openid-configuration',
]);

type Finding = { file: string; line: number; text: string };

function svelteFiles(root: string, prefix: string): Record<string, string> {
	const sources: Record<string, string> = {};
	for (const entry of readdirSync(root, { withFileTypes: true })) {
		const path = join(root, entry.name);
		if (entry.isDirectory()) {
			Object.assign(sources, svelteFiles(path, prefix));
		} else if (entry.name.endsWith('.svelte')) {
			sources[relative(prefix, path).replaceAll('\\', '/')] = readFileSync(path, 'utf8');
		}
	}
	return sources;
}

function isProse(text: string): boolean {
	const trimmed = text.replace(/\s+/g, ' ').trim();
	if (NON_TRANSLATABLE.has(trimmed)) return false;
	const withoutNames = PROPER_NAMES.reduce((rest, name) => rest.replace(name, ''), trimmed);
	return /[A-Za-z]/.test(withoutNames);
}

/** Every hard-coded, user-visible English string in one component's markup. */
function hardcodedText(source: string, file: string): Finding[] {
	const ast = parse(source, { modern: true }) as unknown as { fragment: SvelteNode };
	const findings: Finding[] = [];
	const lineOf = (offset: number | undefined) => source.slice(0, offset ?? 0).split('\n').length;

	function visit(value: unknown, inCode: boolean): void {
		if (Array.isArray(value)) {
			for (const item of value) visit(item, inCode);
			return;
		}
		if (typeof value !== 'object' || value === null) return;
		const node = value as SvelteNode;

		if (node.type === 'Text' && !inCode && isProse(node.data ?? '')) {
			findings.push({ file, line: lineOf(node.start), text: (node.data ?? '').replace(/\s+/g, ' ').trim() });
		}

		for (const attribute of node.attributes ?? []) {
			if (attribute.type !== 'Attribute' || !USER_VISIBLE_ATTRIBUTES.has(attribute.name ?? '')) continue;
			if (!Array.isArray(attribute.value)) continue;
			for (const part of attribute.value as SvelteNode[]) {
				if (part.type === 'Text' && isProse(part.data ?? '')) {
					findings.push({ file, line: lineOf(part.start), text: `${attribute.name}="${(part.data ?? '').trim()}"` });
				}
			}
		}

		const codeHere = inCode || (node.type === 'RegularElement' && CODE_ELEMENTS.has(node.name ?? ''));
		for (const [key, child] of Object.entries(node)) {
			// Attribute values were judged above; scripts and styles are not markup.
			if (key === 'attributes' || key === 'instance' || key === 'module' || key === 'css') continue;
			visit(child, codeHere);
		}
	}

	visit(ast.fragment, false);
	return findings;
}

/** A page with no `<title>` leaves the previous page's title in the tab. */
function hasTitle(source: string): boolean {
	return /<svelte:head>[\s\S]*?<title>[\s\S]*?<\/title>[\s\S]*?<\/svelte:head>/.test(source);
}

const sourceRoot = join(process.cwd(), 'src');
const scanned = {
	...svelteFiles(join(sourceRoot, 'routes'), sourceRoot),
	...svelteFiles(join(sourceRoot, 'lib', 'components'), sourceRoot),
};

describe('hard-coded user-visible text', () => {
	// A gate that only ever reports an empty list proves nothing until it is
	// seen to report a non-empty one.
	it('can see a literal at all', () => {
		const fixture = [
			'<script>let t = (k) => k;</script>',
			'<svelte:head><title>Settings · Plombir Git</title></svelte:head>',
			'<h1>Access tokens</h1>',
			'<input placeholder="Token name" aria-label={t(\'x\')} />',
			'<p>{t(\'ok\')} · Plombir Git</p>',
			'<pre>git push origin main</pre>',
			'<code>cargo build</code>',
			'<span>HTTPS</span> <span>—</span> <span>42</span>',
			'<Dropdown ariaLabel="User menu" />',
		].join('\n');

		expect(hardcodedText(fixture, 'fixture.svelte').map(({ text }) => text)).toEqual([
			'Settings · Plombir Git',
			'Access tokens',
			'placeholder="Token name"',
			'ariaLabel="User menu"',
		]);
		expect(hasTitle(fixture)).toBe(true);
		expect(hasTitle('<h1>{t(\'x\')}</h1>')).toBe(false);
		expect(Object.keys(scanned).length).toBeGreaterThan(60);
	});

	it('routes every route and shared component through t()', () => {
		const findings = Object.entries(scanned)
			.flatMap(([file, source]) => hardcodedText(source, file))
			.map(({ file, line, text }) => `${file}:${line} ${text}`);

		expect(findings).toEqual([]);
	});

	it('gives every page a title', () => {
		const untitled = Object.entries(scanned)
			.filter(([file]) => file.endsWith('/+page.svelte'))
			.filter(([, source]) => !hasTitle(source))
			.map(([file]) => file)
			.sort();

		expect(untitled).toEqual([]);
	});
});
