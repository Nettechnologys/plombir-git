import { afterEach, beforeEach, describe, expect, it } from 'vitest';
// This test reads the checked-out sources for the bundle guard.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { readFileSync, readdirSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { join } from 'node:path';

declare const process: { cwd(): string };

import BlobPage from '../../routes/[owner]/[repo]/blob/[...path]/+page.svelte';
import { highlightLines, languageForPath, splitHighlightedLines } from '../highlight';
import { formatLineHash, parseLineHash, selectLine } from '../lineAnchors';
import { navigation, setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { renderComponent, settle, type RenderedComponent } from '../test/render';

// card_61e77c8abec1: the code view highlighted each line on its own (a block
// comment lost its colour after the first line), linked to no line, and
// shipped every grammar highlight.js has.

const RUST = ['/* one', '   two', '   three */', 'fn main() {}'].join('\n');

describe('highlighting a whole file', () => {
	it('colours every line of a block comment as a comment', () => {
		const lines = highlightLines(RUST, languageForPath('src/main.rs'));
		expect(lines).toHaveLength(4);
		for (const line of lines.slice(0, 3)) {
			expect(line).toMatch(/^<span class="hljs-comment">/);
		}
		expect(lines[3]).toContain('hljs-keyword');
	});

	it('cuts the HTML into lines that each stand alone', () => {
		const lines = splitHighlightedLines('<span class="a">x\n<span class="b">y\nz</span></span>w');
		expect(lines).toEqual([
			'<span class="a">x</span>',
			'<span class="a"><span class="b">y</span></span>',
			'<span class="a"><span class="b">z</span></span>w',
		]);
	});

	it('escapes, and does not guess, without a known grammar', () => {
		expect(languageForPath('notes.unknownext')).toBe('');
		expect(highlightLines('<b>\nx', '')).toEqual(['&lt;b&gt;', 'x']);
	});

	it('knows files by name', () => {
		expect(languageForPath('deploy/Dockerfile')).toBe('dockerfile');
		expect(languageForPath('Makefile')).toBe('makefile');
		expect(languageForPath('web/src/App.svelte')).toBe('xml');
	});

	it('never imports highlight.js whole', () => {
		const src = join(process.cwd(), 'src');
		const offenders: string[] = [];
		const walk = (dir: string) => {
			for (const entry of readdirSync(dir, { withFileTypes: true }) as any[]) {
				const path = join(dir, entry.name);
				if (entry.isDirectory()) walk(path);
				else if (/\.(svelte|ts)$/.test(entry.name) && !entry.name.endsWith('.test.ts')) {
					if (/(from|import\()\s*['"]highlight\.js['"]/.test(readFileSync(path, 'utf8'))) offenders.push(path);
				}
			}
		};
		walk(src);
		expect(offenders).toEqual([]);
	});
});

describe('line anchors', () => {
	it('read and write #L10 and #L10-L20', () => {
		expect(parseLineHash('#L10')).toEqual({ start: 10, end: 10 });
		expect(parseLineHash('#L20-L10')).toEqual({ start: 10, end: 20 });
		expect(parseLineHash('#readme')).toBeNull();
		expect(formatLineHash({ start: 3, end: 3 })).toBe('#L3');
		expect(formatLineHash({ start: 3, end: 9 })).toBe('#L3-L9');
		expect(selectLine({ start: 5, end: 5 }, 9, true)).toEqual({ start: 5, end: 9 });
		expect(selectLine({ start: 5, end: 9 }, 2, false)).toEqual({ start: 2, end: 2 });
	});
});

describe('the blob page', () => {
	let rendered: RenderedComponent | undefined;

	beforeEach(() => {
		resetTestClient();
		repos.get.mockResolvedValue({ id: 1, name: 'demo', default_branch: 'main' });
		repos.blob.mockResolvedValue({
			path: 'src/main.rs',
			sha: 'b'.repeat(40),
			size: RUST.length,
			content: RUST,
			encoding: 'utf-8',
			is_binary: false,
			name: 'main.rs',
		});
	});

	afterEach(async () => {
		await rendered?.destroy();
		rendered = undefined;
	});

	async function open(hash = '') {
		setTestPage(`/alice/demo/blob/src/main.rs?ref=main${hash}`, {
			owner: 'alice',
			repo: 'demo',
			path: 'src/main.rs',
		});
		rendered = await renderComponent(BlobPage);
		await settle();
		await new Promise((resolve) => setTimeout(resolve, 0));
		await settle();
		return rendered.container;
	}

	it('renders the file highlighted as a whole', async () => {
		const container = await open();
		const lines = container.querySelectorAll('.line-content code');
		expect(lines).toHaveLength(4);
		expect(lines[1].querySelector('.hljs-comment')?.textContent).toBe('   two');
	});

	it('gives every line an anchor, and selects the lines a link names', async () => {
		const container = await open('#L2-L3');
		const anchor = container.querySelector<HTMLAnchorElement>('a#L4');
		expect(anchor?.getAttribute('href')).toBe('#L4');
		const selected = Array.from(container.querySelectorAll('tr.selected a'), (a) => a.textContent);
		expect(selected).toEqual(['2', '3']);
	});

	it('selects a line on click and a range on shift-click, in the URL', async () => {
		const container = await open();
		container.querySelector<HTMLAnchorElement>('a#L1')!.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
		await settle();
		expect(navigation.replaceState).toHaveBeenLastCalledWith('/alice/demo/blob/src/main.rs?ref=main#L1', {});
		container
			.querySelector<HTMLAnchorElement>('a#L3')!
			.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, shiftKey: true }));
		await settle();
		expect(navigation.replaceState).toHaveBeenLastCalledWith('/alice/demo/blob/src/main.rs?ref=main#L1-L3', {});
		expect(Array.from(container.querySelectorAll('tr.selected a'), (a) => a.textContent)).toEqual(['1', '2', '3']);
	});
});
