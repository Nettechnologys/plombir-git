import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import ExplorePage from '../../routes/explore/+page.svelte';
import IssueListPage from '../../routes/[owner]/[repo]/issues/+page.svelte';
import PullListPage from '../../routes/[owner]/[repo]/pulls/+page.svelte';
import Navbar from '../components/Navbar.svelte';
import { setTestPage } from '../test/app';
import { issues, pulls, repos, resetTestClient } from '../test/client';
import { button, click, element, renderComponent, settle, type RenderedComponent } from '../test/render';
import { activeLocale, formatDate, formatDateTime, locale, t } from '.';
import en from './translations/en.json';
import zhCN from './translations/zh-CN.json';
import ru from './translations/ru.json';

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
	vi.restoreAllMocks();
	locale.set('en');
	try {
		localStorage.removeItem('locale');
	} catch {
		// jsdom always has storage; nothing to clean up otherwise.
	}
});

describe('switching the locale', () => {
	it.each([
		['zh-CN', zhCN],
		['ru', ru],
	] as const)('re-renders an open page in %s and back to English', async (language, catalog) => {
		setTestPage('/explore', {});
		rendered = await renderComponent(ExplorePage);
		const heading = () => rendered!.container.querySelector('h1')?.textContent;
		const empty = () => rendered!.container.querySelector('.empty')?.textContent?.trim();

		expect(heading()).toBe(en.explore.title);
		expect(empty()).toBe(en.explore.empty);

		locale.set(language);
		await settle();

		expect(heading()).toBe(catalog.explore.title);
		expect(empty()).toBe(catalog.explore.empty);
		expect(document.title).toContain(catalog.explore.title);
		expect(document.documentElement.lang).toBe(language);
		expect(catalog.explore.title).not.toBe(en.explore.title);

		locale.set('en');
		await settle();

		expect(heading()).toBe(en.explore.title);
		expect(empty()).toBe(en.explore.empty);
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

	it('selects Russian from the navigation menu and keeps the choice on reload', async () => {
		rendered = await renderComponent(Navbar);
		const trigger = element<HTMLButtonElement>(rendered.container, '.lang-btn');
		await click(trigger);
		await click(button(rendered.container, 'RU'));

		expect(activeLocale()).toBe('ru');
		expect(localStorage.getItem('locale')).toBe('ru');
		expect(trigger.textContent?.trim()).toBe('RU');
		expect(trigger.getAttribute('aria-label')).toBe(ru.nav.change_language);
		expect(trigger.getAttribute('aria-expanded')).toBe('false');
		expect(rendered.container.querySelector('a[href="/explore"]')?.textContent).toBe(ru.explore.title);

		locale.init();
		expect(activeLocale()).toBe('ru');
		expect(document.documentElement.lang).toBe('ru');
		await click(trigger);
		expect(element(rendered.container, '[role="menuitem"].active').textContent?.trim()).toBe('RU');
		await click(button(rendered.container, 'EN'));
		expect(trigger.textContent?.trim()).toBe('EN');
		expect(activeLocale()).toBe('en');
	});

	it.each(['ru', 'ru-RU', 'ru-BY', 'RU-ru'])('detects Russian from browser language %s', (language) => {
		localStorage.removeItem('locale');
		vi.spyOn(navigator, 'language', 'get').mockReturnValue(language);
		locale.init();
		expect(activeLocale()).toBe('ru');
		expect(t('nav.sign_in')).toBe('Войти');
	});

	it('prefers a saved language over the browser language', () => {
		vi.spyOn(navigator, 'language', 'get').mockReturnValue('ru-RU');
		localStorage.setItem('locale', 'en');
		locale.init();
		expect(activeLocale()).toBe('en');
		localStorage.setItem('locale', 'ru');
		vi.spyOn(navigator, 'language', 'get').mockReturnValue('en-US');
		locale.init();
		expect(t('nav.sign_in')).toBe('Войти');
	});

	it.each(['fr', '__proto__'])('ignores unsupported saved locale %s', (stored) => {
		localStorage.setItem('locale', stored);
		vi.spyOn(navigator, 'language', 'get').mockReturnValue('ru-RU');
		locale.init();
		expect(activeLocale()).toBe('ru');
	});

	it('uses English for an unsupported browser language', () => {
		localStorage.removeItem('locale');
		vi.spyOn(navigator, 'language', 'get').mockReturnValue('fr-FR');
		locale.init();
		expect(activeLocale()).toBe('en');
	});

	it('detects and switches language when browser storage is blocked', () => {
		vi.spyOn(navigator, 'language', 'get').mockReturnValue('ru-RU');
		vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => { throw new Error('blocked'); });
		vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('blocked'); });
		locale.init();
		expect(t('nav.sign_in')).toBe('Войти');
		locale.set('en');
		expect(t('nav.sign_in')).toBe('Sign in');
		expect(document.documentElement.lang).toBe('en');
	});

	it('formats Russian dates and timestamps, including custom date options', () => {
		locale.set('ru');
		const timestamp = '2026-10-01T12:34:00Z';
		const date = new Date(timestamp);
		const options: Intl.DateTimeFormatOptions = { year: 'numeric', month: 'short', day: 'numeric' };
		expect(formatDate(timestamp)).toBe(date.toLocaleDateString('ru-RU', options));
		expect(formatDate(timestamp)).toContain('окт.');
		expect(formatDateTime(timestamp)).toBe(date.toLocaleString('ru-RU', {
			...options, hour: '2-digit', minute: '2-digit',
		}));
		expect(formatDate(timestamp, { month: 'long', timeZone: 'UTC' })).toBe('октябрь');
	});

	it('translates pull request filters in empty results', async () => {
		locale.set('ru');
		repos.get.mockResolvedValue({ id: 1, name: 'demo', default_branch: 'main', stars_count: 0 });
		pulls.list.mockResolvedValue({ data: [], pagination: { total_pages: 1 } });
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);
		const empty = () => element(rendered!.container, '.empty p').textContent;
		expect(empty()).toBe('Пул-реквестов со статусом «открыто» нет.');
		await click(button(rendered.container, ru.pulls.tabs.closed));
		expect(empty()).toBe('Пул-реквестов со статусом «закрыто» нет.');
		await click(button(rendered.container, ru.pulls.tabs.merged));
		expect(empty()).toBe('Пул-реквестов со статусом «слито» нет.');
	});

	it('handles the all-issues filter without inserting a nonexistent status', async () => {
		locale.set('ru');
		repos.get.mockResolvedValue({ id: 1, name: 'demo', default_branch: 'main', stars_count: 0 });
		issues.list.mockResolvedValue({ data: [], pagination: { total_pages: 1 } });
		setTestPage('/alice/demo/issues', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(IssueListPage);
		expect(element(rendered.container, '.empty p').textContent).toBe('Задач со статусом «открыто» нет.');
		await click(button(rendered.container, ru.issues.tabs.all));
		expect(element(rendered.container, '.empty p').textContent).toBe('Задач пока нет.');
	});
});
