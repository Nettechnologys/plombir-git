import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// card_87f9b1c97489: a pull request from a fork could not be opened from the UI.
// The page's client namespace is the shared mock, but its `pulls.create` /
// `pulls.compare` are pointed at the REAL client below, whose transport is this
// mock — so each assertion reads the request as it would leave the browser,
// not the arguments the page happened to pass.
const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn((params: Record<string, unknown>) => {
		const parts = Object.entries(params)
			.filter(([, value]) => value !== undefined && value !== null && value !== '')
			.map(([key, value]) => `${encodeURIComponent(key)}=${encodeURIComponent(String(value))}`);
		return parts.length ? `?${parts.join('&')}` : '';
	}),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

const auth = vi.hoisted(() => ({ username: 'bob' as string | null }));

vi.mock('$lib/stores/auth.svelte', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/stores/auth.svelte')>();
	return {
		...actual,
		getUser: () =>
			auth.username
				? { id: 2, username: auth.username, email: '', is_admin: false, display_name: null }
				: null,
	};
});

import ComparePage from '../../routes/[owner]/[repo]/compare/[...spec]/+page.svelte';
import PullListPage from '../../routes/[owner]/[repo]/pulls/+page.svelte';
import { compareHref, formatHeadRef, newPullHref, parseCompareSpec, parseHeadRef } from '../pullHeadRef';
import { setTestPage } from '../test/app';
import { pulls as routePulls, repos as routeRepos, resetTestClient } from '../test/client';
import { button, click, element, input, renderComponent, settle, submit, type RenderedComponent } from '../test/render';
import { pulls } from './pulls';

const timestamp = '2026-10-01T00:00:00Z';

function fork(ownerName: string, overrides: Record<string, unknown> = {}) {
	return {
		id: ownerName.length,
		owner_id: ownerName.length,
		owner_name: ownerName,
		name: 'demo',
		description: null,
		is_private: false,
		default_branch: 'main',
		fork_id: null,
		stars_count: 0,
		forks_count: 0,
		org_id: null,
		created_at: timestamp,
		updated_at: timestamp,
		deleted_at: null,
		origin_repo_id: 1,
		...overrides,
	};
}

const compareResult = {
	base_branch: 'main',
	head_branch: 'fork-feature',
	commits: [
		{ sha: 'abc1234def5678', message: 'Teach the parser forks\n\nLong body', author: 'bob', date: timestamp },
		{ sha: 'fed9876cba5432', message: 'Cover the fork head', author: 'bob', date: timestamp },
	],
	files_changed: [
		{
			path: 'src/parser.rs',
			status: 'modified',
			additions: 1,
			deletions: 1,
			patch: null,
			lines: [
				{ kind: 'meta', content: '@@ -1 +1 @@', old_line: null, new_line: null },
				{ kind: 'deletion', content: '-old line', old_line: 1, new_line: null },
				{ kind: 'addition', content: '+new line', old_line: null, new_line: 1 },
			],
		},
	],
	stats: { total_additions: 1, total_deletions: 1, files_changed: 1 },
};

function sentBody(): Record<string, unknown> {
	const call = base.request.mock.calls.find(([path, init]) => path === '/repos/alice/demo/pulls' && init?.method === 'POST');
	if (!call) throw new Error('no POST /repos/alice/demo/pulls left the client');
	return JSON.parse(call[1].body);
}

async function choose(select: HTMLSelectElement, value: string) {
	if (!Array.from(select.options).some((option) => option.value === value)) {
		throw new Error(`select offers no "${value}": ${Array.from(select.options).map((o) => o.value).join(', ')}`);
	}
	select.value = value;
	select.dispatchEvent(new Event('change', { bubbles: true }));
	await settle();
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	base.request.mockReset();
	auth.username = 'bob';
	base.request.mockImplementation(async (path: string) =>
		path.includes('/compare') ? compareResult : { id: 9, number: 9, title: 'Fork PR' },
	);
	routePulls.create.mockImplementation(pulls.create);
	routePulls.compare.mockImplementation(pulls.compare);
	routePulls.list.mockResolvedValue({ data: [], pagination: { page: 1, per_page: 20, total: 0, total_pages: 1 } });
	routePulls.template.mockResolvedValue(null);
	routeRepos.get.mockResolvedValue({ owner_id: 1, default_branch: 'main' });
	routeRepos.branches.mockImplementation(async (owner: string) =>
		owner === 'alice'
			? [
					{ name: 'main', is_default: true },
					{ name: 'feature', is_default: false },
				]
			: [
					{ name: 'main', is_default: true },
					{ name: 'fork-feature', is_default: false },
				],
	);
	routeRepos.forks.mockResolvedValue({
		data: [fork('carol'), fork('bob'), fork('dave', { name: 'renamed' }), fork('erin', { deleted_at: timestamp })],
		pagination: { page: 1, per_page: 100, total: 4, total_pages: 1 },
	});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('head references', () => {
	it('sends a fork head as owner:branch and a same-repo head bare', () => {
		expect(formatHeadRef({ owner: 'bob', branch: 'fork-feature' }, 'alice')).toBe('bob:fork-feature');
		expect(formatHeadRef({ owner: 'alice', branch: 'feature' }, 'alice')).toBe('feature');
		expect(formatHeadRef({ owner: null, branch: 'feature' }, 'alice')).toBe('feature');
		expect(parseHeadRef('bob:feat/x')).toEqual({ owner: 'bob', branch: 'feat/x' });
		expect(parseHeadRef('feat/x')).toEqual({ owner: null, branch: 'feat/x' });
	});

	it('reads base...head compare specs, slashes and all', () => {
		expect(parseCompareSpec('main...bob:feat/x')).toEqual({ base: 'main', head: { owner: 'bob', branch: 'feat/x' } });
		expect(parseCompareSpec('release/1.0...hotfix')).toEqual({
			base: 'release/1.0',
			head: { owner: null, branch: 'hotfix' },
		});
		expect(parseCompareSpec('bob:feature')).toEqual({ base: null, head: { owner: 'bob', branch: 'feature' } });
		expect(parseCompareSpec('main...')).toBeNull();
		expect(parseCompareSpec('')).toBeNull();
		expect(compareHref('alice', 'demo', 'main', { owner: 'bob', branch: 'feat/x' })).toBe(
			'/alice/demo/compare/main...bob:feat/x',
		);
		expect(newPullHref('alice', 'demo', 'main', { owner: 'bob', branch: 'feat/x' })).toBe(
			'/alice/demo/pulls?new=1&base=main&head=bob%3Afeat%2Fx',
		);
	});
});

describe('opening a pull request from a fork', () => {
	it('offers the caller’s fork as head repository and sends head "<fork owner>:<branch>"', async () => {
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);
		await click(button(rendered.container, 'New Pull Request'));

		const headRepo = element<HTMLSelectElement>(rendered.container, '.head-repo-select');
		const offered = Array.from(headRepo.options).map((option) => option.value);
		// This repository, then the caller's own fork, then the rest. A fork that
		// was renamed or deleted cannot be a head: the server resolves the prefix
		// as `<owner>/<this repo's name>`.
		expect(offered).toEqual(['', 'bob', 'carol']);
		expect(headRepo.options[1].textContent).toContain('bob/demo');

		await choose(headRepo, 'bob');
		expect(routeRepos.branches).toHaveBeenCalledWith('bob', 'demo');
		const [headBranch, baseBranch] = Array.from(
			rendered.container.querySelectorAll<HTMLSelectElement>('.branch-row select'),
		);
		expect(Array.from(headBranch.options).map((option) => option.value)).toContain('fork-feature');
		await choose(headBranch, 'fork-feature');
		await choose(baseBranch, 'main');
		await input(element(rendered.container, '.create-form input[type="text"]'), 'Fork PR');
		await submit(element<HTMLFormElement>(rendered.container, '.create-form form'));

		expect(routePulls.create).toHaveBeenCalledOnce();
		expect(sentBody()).toMatchObject({ title: 'Fork PR', head: 'bob:fork-feature', base: 'main', draft: false });
	});

	it('still sends a branch of this repository bare', async () => {
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);
		await click(button(rendered.container, 'New Pull Request'));

		const [headBranch] = Array.from(rendered.container.querySelectorAll<HTMLSelectElement>('.branch-row select'));
		await choose(headBranch, 'feature');
		await input(element(rendered.container, '.create-form input[type="text"]'), 'Same repo');
		await submit(element<HTMLFormElement>(rendered.container, '.create-form form'));

		expect(sentBody()).toMatchObject({ head: 'feature', base: 'main' });
		expect(routeRepos.branches).not.toHaveBeenCalledWith('bob', 'demo');
	});

	it('does not offer a stale fork branch list after switching back to this repository', async () => {
		let releaseFork!: (value: unknown) => void;
		routeRepos.branches.mockImplementation((owner: string) =>
			owner === 'alice'
				? Promise.resolve([{ name: 'main', is_default: true }, { name: 'feature', is_default: false }])
				: new Promise((resolve) => (releaseFork = resolve)),
		);
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);
		await click(button(rendered.container, 'New Pull Request'));

		const headRepo = element<HTMLSelectElement>(rendered.container, '.head-repo-select');
		await choose(headRepo, 'bob');
		await choose(headRepo, '');
		releaseFork([{ name: 'fork-only', is_default: false }]);
		await settle();

		const [headBranch] = Array.from(rendered.container.querySelectorAll<HTMLSelectElement>('.branch-row select'));
		const offered = Array.from(headBranch.options).map((option) => option.value);
		expect(offered).toContain('feature');
		expect(offered).not.toContain('fork-only');
	});

	it('keeps same-repo pull requests possible when the fork list cannot be read', async () => {
		routeRepos.forks.mockRejectedValue(new Error('forks down'));
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);
		await click(button(rendered.container, 'New Pull Request'));

		const headRepo = element<HTMLSelectElement>(rendered.container, '.head-repo-select');
		expect(Array.from(headRepo.options).map((option) => option.value)).toEqual(['']);
		expect(element(rendered.container, '.forks-note').textContent).toContain('Forks could not be loaded');
		const [headBranch] = Array.from(rendered.container.querySelectorAll<HTMLSelectElement>('.branch-row select'));
		expect(headBranch.disabled).toBe(false);
	});
});

describe('compare page', () => {
	it('shows the commits and diff of a fork head and leads to a prefilled pull request', async () => {
		setTestPage('/alice/demo/compare/main...bob:fork-feature', {
			owner: 'alice',
			repo: 'demo',
			spec: 'main...bob:fork-feature',
		});
		rendered = await renderComponent(ComparePage);

		expect(base.request).toHaveBeenCalledWith('/repos/alice/demo/compare?base=main&head=bob%3Afork-feature');
		const text = rendered.container.textContent ?? '';
		expect(text).toContain('Teach the parser forks');
		expect(text).not.toContain('Long body');
		expect(text).toContain('Cover the fork head');
		expect(text).toContain('src/parser.rs');
		expect(text).toContain('+new line');
		expect(element(rendered.container, '.head-ref').textContent).toBe('bob:fork-feature');
		expect(element<HTMLAnchorElement>(rendered.container, '.commit-message').getAttribute('href')).toBe(
			'/bob/demo/commits/abc1234def5678',
		);

		const create = element<HTMLAnchorElement>(rendered.container, '.create-pr-link');
		const href = create.getAttribute('href')!;
		expect(href).toBe('/alice/demo/pulls?new=1&base=main&head=bob%3Afork-feature');

		// Follow the link: the list page opens its form with the fork head chosen.
		await rendered.destroy();
		setTestPage(href, { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);

		expect(element<HTMLSelectElement>(rendered.container, '.head-repo-select').value).toBe('bob');
		const [headBranch, baseBranch] = Array.from(
			rendered.container.querySelectorAll<HTMLSelectElement>('.branch-row select'),
		);
		expect(headBranch.value).toBe('fork-feature');
		expect(baseBranch.value).toBe('main');
		await input(element(rendered.container, '.create-form input[type="text"]'), 'From compare');
		await submit(element<HTMLFormElement>(rendered.container, '.create-form form'));

		expect(sentBody()).toMatchObject({ title: 'From compare', head: 'bob:fork-feature', base: 'main' });
	});

	it('compares a bare head against the default branch', async () => {
		routeRepos.get.mockResolvedValue({ owner_id: 1, default_branch: 'trunk' });
		setTestPage('/alice/demo/compare/feature', { owner: 'alice', repo: 'demo', spec: 'feature' });
		rendered = await renderComponent(ComparePage);

		expect(base.request).toHaveBeenCalledWith('/repos/alice/demo/compare?base=trunk&head=feature');
		expect(element<HTMLAnchorElement>(rendered.container, '.create-pr-link').getAttribute('href')).toBe(
			'/alice/demo/pulls?new=1&base=trunk&head=feature',
		);
	});

	it('offers no pull request when head has nothing base lacks', async () => {
		base.request.mockResolvedValue({
			...compareResult,
			commits: [],
			files_changed: [],
			stats: { total_additions: 0, total_deletions: 0, files_changed: 0 },
		});
		setTestPage('/alice/demo/compare/main...feature', { owner: 'alice', repo: 'demo', spec: 'main...feature' });
		rendered = await renderComponent(ComparePage);

		expect(rendered.container.querySelector('.create-pr-link')).toBeNull();
		expect(element(rendered.container, '.compare-empty').textContent).toContain('feature');
	});

	it('names an unreadable spec instead of asking the server', async () => {
		setTestPage('/alice/demo/compare/main...', { owner: 'alice', repo: 'demo', spec: 'main...' });
		rendered = await renderComponent(ComparePage);

		expect(base.request).not.toHaveBeenCalled();
		expect(element(rendered.container, '.error-banner').textContent).toBeTruthy();
	});
});
