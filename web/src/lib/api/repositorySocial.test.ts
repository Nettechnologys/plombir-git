import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
  request: vi.fn(),
  qs: vi.fn(() => '?page=2&per_page=20'),
}));

vi.mock('./_base.svelte', () => base);

import NetworkPage from '../../routes/[owner]/[repo]/network/+page.svelte';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { repos } from './repos';
import { setTestPage } from '../test/app';
import { repos as routeRepos, resetTestClient } from '../test/client';
import { click, element, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

beforeEach(() => {
		vi.clearAllMocks();
		resetTestClient();
		setTestPage('/alice/demo/network', { owner: 'alice', repo: 'demo' });
		base.qs.mockReturnValue('?page=2&per_page=20');
		routeRepos.get.mockResolvedValue({ default_branch: 'main' });
		routeRepos.stargazers.mockResolvedValue({
			data: [
				{
					user_id: 7,
					username: 'bob',
					display_name: 'Bob',
					avatar_url: null,
					starred_at: '2026-08-15T12:00:00Z',
				},
			],
			pagination: { total_pages: 2 },
		});
		routeRepos.forks.mockResolvedValue({
			data: [
				{
					id: 8,
					owner_name: 'carol',
					name: 'demo-fork',
					description: 'A fork',
					is_private: false,
					stars_count: 1,
					forks_count: 0,
					updated_at: '2026-08-15T12:00:00Z',
				},
			],
			pagination: { total_pages: 2 },
		});
	});

afterEach(async () => {
		await rendered?.destroy();
		rendered = undefined;
});

describe('repository social-list transport', () => {

  it('loads a typed stargazer page through the canonical pagination helper', () => {
    repos.stargazers('alice', 'demo', 2, 20);

    expect(base.qs).toHaveBeenCalledWith({ page: 2, per_page: 20 });
    expect(base.request).toHaveBeenCalledWith(
      '/repos/alice/demo/stargazers?page=2&per_page=20',
    );
  });

  it('loads a typed fork page through the same pagination contract', () => {
    repos.forks('alice', 'demo', 2, 20);

    expect(base.qs).toHaveBeenCalledWith({ page: 2, per_page: 20 });
    expect(base.request).toHaveBeenCalledWith(
      '/repos/alice/demo/forks?page=2&per_page=20',
    );
  });
});

describe('repository social-list production wiring', () => {
	it('renders both lists, public links, and the active repository tab', async () => {
		rendered = await renderComponent(NetworkPage);

		expect(element<HTMLAnchorElement>(rendered.container, '.repo-tabs .active').getAttribute('href')).toBe(
			'/alice/demo/network',
		);
		expect(element<HTMLAnchorElement>(rendered.container, '.identity').getAttribute('href')).toBe('/bob');
		expect(element<HTMLAnchorElement>(rendered.container, '.repo-link').getAttribute('href')).toBe(
			'/carol/demo-fork',
		);
		for (const panel of rendered.container.querySelectorAll('.social-panel')) {
			expect(panel.getAttribute('aria-busy')).toBe('false');
		}
	});

	it('keeps independent rendered pagination for stargazers and forks', async () => {
		rendered = await renderComponent(NetworkPage);
		const panels = rendered.container.querySelectorAll('.social-panel');
		await click(element(panels[0], '.pagination button:last-child'));
		expect(routeRepos.stargazers).toHaveBeenLastCalledWith('alice', 'demo', 2, 20);
		expect(routeRepos.forks).toHaveBeenLastCalledWith('alice', 'demo', 1, 20);

		await click(element(panels[1], '.pagination button:last-child'));
		expect(routeRepos.forks).toHaveBeenLastCalledWith('alice', 'demo', 2, 20);
	});

  it.each(['network'])(
    'has a real repository tab label in both catalogs: repo.tabs.%s',
    (key) => {
      expect(en.repo.tabs).toHaveProperty(key);
      expect(zhCN.repo.tabs).toHaveProperty(key);
    },
  );

  it.each([
    'title',
    'stargazers_empty',
    'stargazers_load_failed',
    'starred_on',
    'forks_empty',
    'forks_load_failed',
    'page',
  ])('has a real social-list label in both catalogs: repo.social.%s', (key) => {
    expect(en.repo.social).toHaveProperty(key);
    expect(zhCN.repo.social).toHaveProperty(key);
  });
});
