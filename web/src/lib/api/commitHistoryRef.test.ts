import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import RepositoryPage from '../../routes/[owner]/[repo]/+page.svelte';
import CommitsPage from '../../routes/[owner]/[repo]/commits/+page.svelte';
import { logout } from '../stores/auth.svelte';
import { navigation, setTestPage } from '../test/app';
import { repos, resetTestClient } from '../test/client';
import { button, click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

// card_2e320f5287d7: the commits page asked for `ref=main` whatever the
// repository's default branch was, never asked for more than the first page,
// and the repository home fetched 50 commits to show 5.

const timestamp = '2026-08-30T12:00:00Z';

function commit(index: number) {
	const sha = index.toString(16).padStart(40, '0');
	return { sha, message: `commit ${index}`, author: 'alice', date: timestamp };
}

function history(from: number, count: number) {
	return Array.from({ length: count }, (_, offset) => commit(from + offset));
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	repos.get.mockResolvedValue({ default_branch: 'master' });
	repos.starred.mockResolvedValue({ starred: false });
	repos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
	repos.branches.mockResolvedValue([
		{ name: 'master', is_default: true },
		{ name: 'dev', is_default: false },
	]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
	vi.unstubAllGlobals();
});

describe('commit history ref and paging', () => {
	it('asks for the server HEAD, not a hard-coded main, when the URL names no ref', async () => {
		repos.log.mockResolvedValue({ commits: history(1, 3) });
		setTestPage('/alice/demo/commits', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(CommitsPage);

		expect(repos.log).toHaveBeenCalledTimes(1);
		expect(repos.log.mock.calls[0]).toEqual(['alice', 'demo', undefined, undefined, 50]);
		expect(rendered.container.textContent).toContain('commit 1');
		expect(rendered.container.textContent).toContain('commit 3');
		// The picker names the repository's actual default branch.
		expect(element(rendered.container, '.dropdown-trigger').textContent).toContain('master');
		expect(rendered.container.textContent).not.toContain('Load more');
	});

	it('honours ?ref= from the URL', async () => {
		repos.log.mockResolvedValue({ commits: history(1, 2) });
		setTestPage('/alice/demo/commits?ref=dev', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(CommitsPage);

		expect(repos.log.mock.calls[0]).toEqual(['alice', 'demo', 'dev', undefined, 50]);
		expect(element(rendered.container, '.dropdown-trigger').textContent).toContain('dev');
	});

	it('keeps the selected branch in the URL', async () => {
		repos.log.mockResolvedValue({ commits: history(1, 2) });
		setTestPage('/alice/demo/commits', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(CommitsPage);

		await click(element(rendered.container, '.dropdown-trigger'));
		await click(button(rendered.container, 'dev'));
		expect(navigation.goto).toHaveBeenCalledWith(
			'/alice/demo/commits?ref=dev',
			expect.objectContaining({ replaceState: true }),
		);

		setTestPage('/alice/demo/commits?ref=dev', { owner: 'alice', repo: 'demo' });
		await settle();
		expect(repos.log).toHaveBeenLastCalledWith('alice', 'demo', 'dev', undefined, 50);
	});

	it('shows the empty state for a repository without commits', async () => {
		repos.log.mockResolvedValue({ commits: [] });
		repos.branches.mockResolvedValue([]);
		setTestPage('/alice/empty/commits', { owner: 'alice', repo: 'empty' });
		rendered = await renderComponent(CommitsPage);

		expect(rendered.container.textContent).toContain('No commits yet.');
		expect(rendered.container.querySelector('.error-banner')).toBeNull();
	});

	it('pages the same walk with skip and stops at the end', async () => {
		const firstPage = history(1, 50);
		repos.log
			.mockResolvedValueOnce({ commits: firstPage })
			.mockResolvedValueOnce({ commits: history(51, 50) })
			.mockResolvedValueOnce({ commits: history(101, 7) });
		setTestPage('/alice/demo/commits?ref=dev', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(CommitsPage);

		expect(rendered.container.querySelectorAll('.commit-item')).toHaveLength(50);
		await click(button(rendered.container, 'Load more'));
		// The ref stays the ref; the server skips what is already shown, so a
		// merged side branch is not lost the way a "history of the last
		// commit" cursor loses it (card_2e320f5287d7).
		expect(repos.log.mock.calls[1]).toEqual(['alice', 'demo', 'dev', undefined, 50, 50]);
		const items = rendered.container.querySelectorAll('.commit-item');
		expect(items).toHaveLength(100);
		expect(items[50].textContent).toContain('commit 51');

		await click(button(rendered.container, 'Load more'));
		expect(repos.log.mock.calls[2]).toEqual(['alice', 'demo', 'dev', undefined, 50, 100]);
		expect(rendered.container.querySelectorAll('.commit-item')).toHaveLength(107);
		expect(rendered.container.textContent).not.toContain('Load more');
	});

	it('drops a load-more answer that belongs to the previous ref', async () => {
		let resolveMore!: (value: unknown) => void;
		repos.log
			.mockResolvedValueOnce({ commits: history(1, 50) })
			.mockReturnValueOnce(new Promise((resolve) => { resolveMore = resolve; }))
			.mockResolvedValueOnce({ commits: [commit(900)] });
		setTestPage('/alice/demo/commits', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(CommitsPage);

		await click(button(rendered.container, 'Load more'));
		setTestPage('/alice/demo/commits?ref=dev', { owner: 'alice', repo: 'demo' });
		await settle();
		resolveMore({ commits: history(51, 50) });
		await settle();

		const items = rendered.container.querySelectorAll('.commit-item');
		expect(items).toHaveLength(1);
		expect(items[0].textContent).toContain('commit 900');
	});

	it('asks the repository home for the five commits it shows, not fifty', async () => {
		repos.tree.mockResolvedValue({ entries: [] });
		repos.log.mockResolvedValue({ commits: history(1, 5) });
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(RepositoryPage);

		expect(repos.log).toHaveBeenCalledWith('alice', 'demo', undefined, undefined, 5);
	});
});
