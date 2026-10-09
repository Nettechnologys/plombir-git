import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
// This test reads a static asset.
// @ts-expect-error Node declarations are intentionally absent from the app.
import { readFileSync } from 'node:fs';
// @ts-expect-error Node declarations are intentionally absent from the app.
import { join } from 'node:path';

declare const process: { cwd(): string };

import ErrorPage from '../../routes/+error.svelte';
import { load as issueBoardLoad } from '../../routes/[owner]/[repo]/issues/board/+page';
import Navbar from '../components/Navbar.svelte';
import { copyToClipboard } from '../clipboard';
import { registerKeyboardShortcuts } from '../stores/instance.svelte';
import { pageState } from '../test/app';
import { resetTestClient } from '../test/client';
import { click, renderComponent, settle, type RenderedComponent } from '../test/render';

// card_c30077df5603: the small holes a visitor falls into first.

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
	document.body.innerHTML = '';
});

describe('an unknown address', () => {
	it('gets the app’s own error page with a way home', async () => {
		pageState.status = 404;
		rendered = await renderComponent(ErrorPage);
		expect(rendered.container.textContent).toContain('Page not found');
		expect(rendered.container.querySelector('a[href="/"]')).not.toBeNull();
		pageState.status = 200;
	});
});

describe('the "?" shortcut', () => {
	it('focuses the search when "?" is typed with Shift, as it is on most layouts', () => {
		const search = document.createElement('input');
		search.type = 'search';
		search.setAttribute('data-global-search', '');
		document.body.append(search);
		const unregister = registerKeyboardShortcuts();

		document.body.dispatchEvent(new KeyboardEvent('keydown', { key: '?', shiftKey: true, bubbles: true }));
		expect(document.activeElement).toBe(search);

		search.blur();
		search.dispatchEvent(new KeyboardEvent('keydown', { key: '?', shiftKey: true, bubbles: true }));
		unregister?.();
	});
});

describe('the navigation bar', () => {
	it('folds behind a menu button that says whether it is open', async () => {
		rendered = await renderComponent(Navbar);
		await settle();
		const toggle = rendered.container.querySelector<HTMLButtonElement>('.menu-toggle')!;
		expect(toggle.getAttribute('aria-expanded')).toBe('false');
		await click(toggle);
		expect(toggle.getAttribute('aria-expanded')).toBe('true');
		expect(rendered.container.querySelector('nav.navbar.menu-open')).not.toBeNull();
	});
});

describe('copying', () => {
	it('reports a failure instead of announcing a copy that did not happen', async () => {
		vi.stubGlobal('navigator', { ...navigator, clipboard: undefined });
		document.execCommand = vi.fn(() => false);
		expect(await copyToClipboard('secret')).toBe(false);
	});

	it('falls back to the selection copy where the Clipboard API is missing', async () => {
		vi.stubGlobal('navigator', { ...navigator, clipboard: undefined });
		document.execCommand = vi.fn(() => true);
		expect(await copyToClipboard('secret')).toBe(true);
		expect(document.execCommand).toHaveBeenCalledWith('copy');
	});
});

describe('the dead issue-board route', () => {
	it('sends an old bookmark to the live boards page', () => {
		let thrown: any;
		try {
			(issueBoardLoad as any)({ params: { owner: 'alice', repo: 'demo' } });
		} catch (error) {
			thrown = error;
		}
		expect(thrown?.status).toBe(308);
		expect(thrown?.location).toBe('/alice/demo/boards');
	});
});

describe('robots.txt', () => {
	it('keeps crawlers out of search results and the API', () => {
		const robots = readFileSync(join(process.cwd(), 'static/robots.txt'), 'utf8');
		expect(robots).toMatch(/^Disallow: \/search$/m);
		expect(robots).toMatch(/^Disallow: \/api\/$/m);
	});
});
