import { describe, expect, it } from 'vitest';
// Reads the checked-out sources, as `i18n/catalogCoverage.test.ts` does.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { readFileSync, readdirSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { extname, join, relative } from 'node:path';

declare const process: { cwd(): string };

// card_4c186d530f59: an irreversible action is confirmed in the page's own
// `ConfirmModal`, never in `window.confirm()` / `alert()` / `prompt()`. The
// native dialog can only show a string, and a browser that suppresses dialogs
// answers `false` without showing anything, so the button looks dead. The
// runtime half of this guard is in `test/setup.ts`: any `window.confirm()`
// a test reaches throws.

// Pending removal in card_340703b4564b: the route redirects to `/boards` and
// this component is never rendered. Drop the entry together with the file.
const EXEMPT = new Set(['routes/[owner]/[repo]/issues/board/+page.svelte']);

function sources(root: string, files: Record<string, string> = {}): Record<string, string> {
	for (const entry of readdirSync(root, { withFileTypes: true })) {
		const path = join(root, entry.name);
		if (entry.isDirectory()) {
			// Test harness, not the app: `setup.ts` names the dialog it forbids.
			if (relative(join(process.cwd(), 'src'), path) !== join('lib', 'test')) sources(path, files);
		} else if (['.svelte', '.ts'].includes(extname(entry.name)) && !/\.test\.ts$/.test(entry.name)) {
			files[path] = readFileSync(path, 'utf8');
		}
	}
	return files;
}

/** Comments name the native dialog when they explain why it is not used. */
function withoutComments(source: string): string {
	return source
		.replace(/<!--[\s\S]*?-->/g, '')
		.replace(/\/\*[\s\S]*?\*\//g, '')
		.replace(/(^|\s)\/\/.*$/gm, '$1');
}

function nativeDialogCalls(source: string): string[] {
	const code = withoutComments(source);
	// A page may name its own handler `confirm` (verify-email does); calling
	// that is not the browser's dialog, but `window.confirm(` still is.
	const ownConfirm = /\bfunction\s+confirm\s*\(/.test(code);
	const calls: string[] = [];
	for (const match of code.matchAll(/(?<![\w$.])(window\.)?(confirm|alert|prompt)\s*\(/g)) {
		const before = code.slice(Math.max(0, match.index - 9), match.index);
		if (/function\s*$/.test(before)) continue;
		if (match[2] === 'confirm' && !match[1] && ownConfirm) continue;
		const line = code.slice(0, match.index).split('\n').length;
		calls.push(`${line}: ${match[0]}`);
	}
	return calls;
}

describe('native browser dialogs', () => {
	it('recognises the calls it is meant to forbid', () => {
		expect(nativeDialogCalls("if (!confirm(t('x'))) return;")).toHaveLength(1);
		expect(nativeDialogCalls('window.confirm("x"); alert(1); prompt("y")')).toHaveLength(3);
		expect(nativeDialogCalls('// rather than `window.confirm()`\nauth.confirmEmail(token)')).toEqual([]);
		expect(nativeDialogCalls('async function confirm() {}\nonclick={confirm}\nconfirm();')).toEqual([]);
		expect(nativeDialogCalls('async function confirm() {}\nwindow.confirm("x");')).toHaveLength(1);
	});

	it('are never called by the app', () => {
		const root = join(process.cwd(), 'src');
		const files = sources(root);
		expect(Object.keys(files).length).toBeGreaterThan(150);
		const offenders = Object.entries(files)
			.map(([path, source]) => [relative(root, path).replaceAll('\\', '/'), nativeDialogCalls(source)] as const)
			.filter(([file, calls]) => calls.length > 0 && !EXEMPT.has(file))
			.map(([file, calls]) => `${file} ${calls.join(', ')}`);
		expect(offenders).toEqual([]);
	});
});
