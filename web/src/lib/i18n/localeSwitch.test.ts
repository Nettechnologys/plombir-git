import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import ExplorePage from '../../routes/explore/+page.svelte';
import { setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { renderComponent, settle, type RenderedComponent } from '../test/render';
import { activeLocale, formatDate, locale, t } from '.';
import en from './translations/en.json';
import zhCN from './translations/zh-CN.json';

// card_0d18cf31d13b: switching language used to change only the switcher's own
// label. `t()` read the catalog through `get()`, which registers no dependency,
// so a page that was already open kept every other string in the old language.

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	locale.set('en');
	repos.explore.mockResolvedValue({
		data: [],
		pagination: { total: 0, total_pages: 1, page: 1, per_page: 24 },
	});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	locale.set('en');
	try {
		localStorage.removeItem('locale');
	} catch {
		// jsdom always has storage; nothing to clean up otherwise.
	}
});

describe('switching the locale', () => {
	it('re-renders the text of a page that is already open', async () => {
		setTestPage('/explore', {});
		rendered = await renderComponent(ExplorePage);
		const heading = () => rendered!.container.querySelector('h1')?.textContent;
		const empty = () => rendered!.container.querySelector('.empty')?.textContent?.trim();

		expect(heading()).toBe(en.explore.title);
		expect(empty()).toBe(en.explore.empty);

		locale.set('zh-CN');
		await settle();

		expect(heading()).toBe(zhCN.explore.title);
		expect(empty()).toBe(zhCN.explore.empty);
		expect(zhCN.explore.title).not.toBe(en.explore.title);

		locale.set('en');
		await settle();

		expect(heading()).toBe(en.explore.title);
	});

	it('re-titles the browser tab of the open page', async () => {
		setTestPage('/explore', {});
		rendered = await renderComponent(ExplorePage);
		expect(document.title).toContain(en.explore.title);

		locale.set('zh-CN');
		await settle();

		expect(document.title).toContain(zhCN.explore.title);
	});

	it('tells the document which language it is in', () => {
		locale.set('zh-CN');
		expect(document.documentElement.lang).toBe('zh-CN');
		expect(activeLocale()).toBe('zh-CN');

		locale.set('en');
		expect(document.documentElement.lang).toBe('en');
	});

	it('sets the document language from the persisted choice on start-up', () => {
		localStorage.setItem('locale', 'zh-CN');
		document.documentElement.lang = 'en';

		locale.init();

		expect(document.documentElement.lang).toBe('zh-CN');
		expect(t('explore.title')).toBe(zhCN.explore.title);
	});

	it('formats dates in the active locale', () => {
		const english = formatDate('2026-10-01T00:00:00Z');
		locale.set('zh-CN');
		expect(formatDate('2026-10-01T00:00:00Z')).not.toBe(english);
	});
});
