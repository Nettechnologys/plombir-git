import { describe, expect, it } from 'vitest';
// This test reads the checked-out stylesheets and components.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { readFileSync, readdirSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { join, relative } from 'node:path';

declare const process: { cwd(): string };

// card_770a114fdafe: about twenty custom properties were used and never
// defined. Ten had no fallback and resolved to nothing — a starred button
// lost its background — and the rest fell back to light-theme colours: an
// error banner on #fee with the app's near-white text, unreadable. Every
// `var(--name)` a page uses has to be defined somewhere in the app, fallback
// or not.

const src = join(process.cwd(), 'src');

function styleSources(dir: string): string[] {
	return readdirSync(dir, { withFileTypes: true }).flatMap((entry: any) =>
		entry.isDirectory()
			? styleSources(join(dir, entry.name))
			: /\.(svelte|css|html)$/.test(entry.name)
				? [join(dir, entry.name)]
				: [],
	);
}

function scan() {
	const defined = new Set<string>();
	const used: Array<{ name: string; at: string }> = [];
	for (const file of styleSources(src)) {
		const text = readFileSync(file, 'utf8');
		for (const match of text.matchAll(/(--[\w-]+)\s*:/g)) defined.add(match[1]);
		// `style:--name={…}` and `style="--name: …"` define one inline.
		for (const match of text.matchAll(/style:(--[\w-]+)/g)) defined.add(match[1]);
		for (const match of text.matchAll(/var\(\s*(--[\w-]+)/g)) {
			const line = text.slice(0, match.index).split('\n').length;
			used.push({ name: match[1], at: `${relative(src, file)}:${line}` });
		}
	}
	return { defined, used };
}

describe('CSS custom properties', () => {
	it('are scanned at all', () => {
		const { defined, used } = scan();
		expect(defined.has('--bg-primary')).toBe(true);
		expect(used.length).toBeGreaterThan(500);
	});

	it('are all defined wherever a page uses one', () => {
		const { defined, used } = scan();
		const undefinedUses = used.filter((use) => !defined.has(use.name)).map((use) => `${use.at} ${use.name}`);
		expect(undefinedUses).toEqual([]);
	});

	it('include color-scheme: dark, so native controls are not light', () => {
		const css = readFileSync(join(src, 'lib/app.css'), 'utf8');
		const root = css.slice(css.indexOf(':root {'), css.indexOf('}', css.indexOf(':root {')));
		expect(root).toContain('color-scheme: dark;');
	});
});
