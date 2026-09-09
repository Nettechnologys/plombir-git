import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import RepositoryPage from '../../routes/[owner]/[repo]/+page.svelte';
import CommitPage from '../../routes/[owner]/[repo]/commits/[sha]/+page.svelte';
import CommitsPage from '../../routes/[owner]/[repo]/commits/+page.svelte';
import PackageFormatPage from '../../routes/[owner]/[repo]/packages/[format]/+page.svelte';
import NewReleasePage from '../../routes/[owner]/[repo]/releases/new/+page.svelte';
import WikiHistoryPage from '../../routes/[owner]/[repo]/wiki/[title]/history/+page.svelte';
import { logout } from '../stores/auth.svelte';
import { navigation, setTestPage } from '../test/app';
import { packages, releases, repos, resetTestClient, wiki } from '../test/client';
import {
	click,
	element,
	input,
	renderComponent,
	settle,
	submit,
	type RenderedComponent,
} from '../test/render';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

const timestamp = '2026-08-31T00:00:00Z';

function repository(name: string) {
	return {
		id: 1,
		name,
		default_branch: 'main',
		stars_count: 0,
		is_private: false,
		created_at: timestamp,
	};
}

function commit(message: string, sha = 'aaaaaaaaaaaaaaaa') {
	return { sha, message, author: 'Alice', date: timestamp };
}

function packageInfo(name: string) {
	return {
		name,
		description: `${name} description`,
		latest_version: '1.0.0',
		created_at: timestamp,
	};
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	navigation.goto.mockReset();
	repos.get.mockResolvedValue(repository('demo'));
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

describe('repository read and create state ownership', () => {
	it('renders a submodule as a non-blob entry with no broken file link', async () => {
		repos.tree.mockResolvedValue({
			entries: [
				{ name: 'vendor', kind: 'commit', size: null },
				{ name: 'README.md', kind: 'blob', size: 9 },
			],
		});
		repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
		repos.log.mockResolvedValue({ commits: [] });
		repos.blob.mockResolvedValue({ content: '# parent' });
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });

		rendered = await renderComponent(RepositoryPage);
		await settle();

		const submodule = element(rendered.container, '.submodule-entry');
		expect(submodule.textContent).toContain('vendor');
		expect(submodule.textContent).toContain('Submodule');
		expect(submodule.querySelector('a')).toBeNull();
		const file = element(rendered.container, 'a.file-entry');
		expect(file.textContent).toContain('README.md');
	});

	it('rejects repository-home data from the first A -> B -> A visit', async () => {
		const firstVisit = deferred<{ entries: Array<{ name: string; kind: string }> }>();
		repos.tree
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce({ entries: [{ name: 'middle.txt', kind: 'blob' }] })
			.mockResolvedValueOnce({ entries: [{ name: 'current.txt', kind: 'blob' }] });
		repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
		repos.log.mockResolvedValue({ commits: [] });
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(RepositoryPage);

		setTestPage('/bob/other', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
		await settle();
		expect(rendered.container.textContent).toContain('current.txt');

		firstVisit.resolve({ entries: [{ name: 'stale.txt', kind: 'blob' }] });
		await settle();
		expect(rendered.container.textContent).toContain('current.txt');
		expect(rendered.container.textContent).not.toContain('stale.txt');
	});

	it('does not let an old README child load survive its repository parent', async () => {
		const firstReadme = deferred<{ content: string }>();
		repos.tree.mockResolvedValue({ entries: [{ name: 'README.md', kind: 'blob' }] });
		repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
		repos.log.mockResolvedValue({ commits: [] });
		repos.blob
			.mockReturnValueOnce(firstReadme.promise)
			.mockResolvedValueOnce({ content: '# Current README' });
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(RepositoryPage);

		setTestPage('/bob/other', { owner: 'bob', repo: 'other' });
		await settle();
		expect(rendered.container.textContent).toContain('Current README');

		firstReadme.resolve({ content: '# Stale README' });
		await settle();
		expect(rendered.container.textContent).toContain('Current README');
		expect(rendered.container.textContent).not.toContain('Stale README');
	});

	it('keeps the newest commit list after an A -> B -> A route reuse', async () => {
		const firstVisit = deferred<{ commits: ReturnType<typeof commit>[] }>();
		repos.log
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce({ commits: [commit('middle commit', 'bbbbbbbbbbbbbbbb')] })
			.mockResolvedValueOnce({ commits: [commit('current commit')] });
		setTestPage('/alice/demo/commits', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(CommitsPage);

		setTestPage('/bob/other/commits', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/commits', { owner: 'alice', repo: 'demo' });
		await settle();
		expect(rendered.container.textContent).toContain('current commit');

		firstVisit.resolve({ commits: [commit('stale commit')] });
		await settle();
		expect(rendered.container.textContent).toContain('current commit');
		expect(rendered.container.textContent).not.toContain('stale commit');
	});

	it('publishes one atomic commit-detail snapshot only to its owning visit', async () => {
		const firstLog = deferred<{ commits: ReturnType<typeof commit>[] }>();
		repos.getCombinedStatus
			.mockResolvedValueOnce({ state: 'success', total_count: 1 })
			.mockResolvedValueOnce({ state: 'pending', total_count: 1 })
			.mockResolvedValueOnce({ state: 'failure', total_count: 1 });
		repos.listCommitStatuses.mockResolvedValue([]);
		repos.log
			.mockReturnValueOnce(firstLog.promise)
			.mockResolvedValueOnce({ commits: [commit('middle detail', 'bbbbbbbbbbbbbbbb')] })
			.mockResolvedValueOnce({ commits: [commit('current detail')] });
		repos.commitSignature.mockResolvedValue(null);
		setTestPage('/alice/demo/commits/aaaaaaaa', {
			owner: 'alice',
			repo: 'demo',
			sha: 'aaaaaaaa',
		});
		rendered = await renderComponent(CommitPage);
		expect(rendered.container.textContent).toContain('Loading commit status');

		setTestPage('/bob/other/commits/bbbbbbbb', {
			owner: 'bob',
			repo: 'other',
			sha: 'bbbbbbbb',
		});
		await settle();
		setTestPage('/alice/demo/commits/aaaaaaaa', {
			owner: 'alice',
			repo: 'demo',
			sha: 'aaaaaaaa',
		});
		await settle();
		expect(rendered.container.textContent).toContain('current detail');
		expect(rendered.container.textContent).toContain('Some checks failed');

		firstLog.resolve({ commits: [commit('stale detail')] });
		await settle();
		expect(rendered.container.textContent).toContain('current detail');
		expect(rendered.container.textContent).not.toContain('stale detail');
		expect(repos.log.mock.calls[0]).toEqual(['alice', 'demo', 'aaaaaaaa']);
	});

	it('rejects release metadata from the first A -> B -> A visit', async () => {
		const firstBranches = deferred<Array<{ name: string }>>();
		repos.branches
			.mockReturnValueOnce(firstBranches.promise)
			.mockResolvedValueOnce([{ name: 'middle-branch' }])
			.mockResolvedValueOnce([{ name: 'current-branch' }]);
		repos.tags
			.mockResolvedValueOnce([{ name: 'stale-tag' }])
			.mockResolvedValueOnce([{ name: 'middle-tag' }])
			.mockResolvedValueOnce([{ name: 'current-tag' }]);
		setTestPage('/alice/demo/releases/new', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(NewReleasePage);

		setTestPage('/bob/other/releases/new', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/releases/new', { owner: 'alice', repo: 'demo' });
		await settle();
		expect(rendered.container.textContent).toContain('current-tag');

		firstBranches.resolve([{ name: 'stale-branch' }]);
		await settle();
		expect(rendered.container.textContent).toContain('current-tag');
		expect(rendered.container.textContent).not.toContain('stale-tag');
	});

	it('does not navigate or release current busy state from an old release create', async () => {
		const firstCreate = deferred<void>();
		const currentCreate = deferred<void>();
		repos.branches.mockResolvedValue([{ name: 'main' }]);
		repos.tags.mockResolvedValue([]);
		releases.create
			.mockReturnValueOnce(firstCreate.promise)
			.mockReturnValueOnce(currentCreate.promise);
		setTestPage('/alice/demo/releases/new', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(NewReleasePage);

		await input(element(rendered.container, '#tag-name'), 'v-old');
		await input(element(rendered.container, '#release-title'), 'Old release');
		await submit(element(rendered.container, 'form.release-form'));
		await submit(element(rendered.container, 'form.release-form'));
		expect(releases.create).toHaveBeenCalledOnce();

		setTestPage('/bob/other/releases/new', { owner: 'bob', repo: 'other' });
		await settle();
		await input(element(rendered.container, '#tag-name'), 'v-current');
		await input(element(rendered.container, '#release-title'), 'Current release');
		await submit(element(rendered.container, 'form.release-form'));
		expect(element<HTMLButtonElement>(rendered.container, 'button[type="submit"]').disabled).toBe(true);

		firstCreate.resolve();
		await settle();
		expect(navigation.goto).not.toHaveBeenCalled();
		expect(element<HTMLButtonElement>(rendered.container, 'button[type="submit"]').disabled).toBe(true);

		currentCreate.resolve();
		await settle();
		expect(navigation.goto).toHaveBeenCalledWith('/bob/other/releases');
	});

	it('binds a package-format collection to owner, repository, format and visit', async () => {
		const firstVisit = deferred<{ packages: ReturnType<typeof packageInfo>[] }>();
		packages.getFormat
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce({ packages: [packageInfo('middle-package')] })
			.mockResolvedValueOnce({ packages: [packageInfo('current-package')] });
		setTestPage('/alice/demo/packages/npm', { owner: 'alice', repo: 'demo', format: 'npm' });
		rendered = await renderComponent(PackageFormatPage);

		setTestPage('/bob/other/packages/cargo', { owner: 'bob', repo: 'other', format: 'cargo' });
		await settle();
		setTestPage('/alice/demo/packages/npm', { owner: 'alice', repo: 'demo', format: 'npm' });
		await settle();
		expect(rendered.container.textContent).toContain('current-package');

		firstVisit.resolve({ packages: [packageInfo('stale-package')] });
		await settle();
		expect(rendered.container.textContent).toContain('current-package');
		expect(rendered.container.textContent).not.toContain('stale-package');
	});

	it('reloads wiki history by route and rejects the first A -> B -> A result', async () => {
		const firstVisit = deferred<any[]>();
		wiki.listRevisions
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce([{ id: 2, message: 'middle history', created_at: timestamp }])
			.mockResolvedValueOnce([{ id: 3, message: 'current history', created_at: timestamp }]);
		setTestPage('/alice/demo/wiki/Home/history', { owner: 'alice', repo: 'demo', title: 'Home' });
		rendered = await renderComponent(WikiHistoryPage);

		setTestPage('/bob/other/wiki/Other/history', { owner: 'bob', repo: 'other', title: 'Other' });
		await settle();
		setTestPage('/alice/demo/wiki/Home/history', { owner: 'alice', repo: 'demo', title: 'Home' });
		await settle();
		expect(rendered.container.textContent).toContain('current history');

		firstVisit.resolve([{ id: 1, message: 'stale history', created_at: timestamp }]);
		await settle();
		expect(rendered.container.textContent).toContain('current history');
		expect(rendered.container.textContent).not.toContain('stale history');
	});

	it('does not let a revision detail survive its parent route generation', async () => {
		const firstDetail = deferred<{ id: number; content: string; created_at: string }>();
		wiki.listRevisions.mockResolvedValue([
			{ id: 1, message: 'revision', created_at: timestamp },
		]);
		wiki.getRevision
			.mockReturnValueOnce(firstDetail.promise)
			.mockResolvedValueOnce({ id: 1, content: 'current revision', created_at: timestamp });
		setTestPage('/alice/demo/wiki/Home/history', { owner: 'alice', repo: 'demo', title: 'Home' });
		rendered = await renderComponent(WikiHistoryPage);
		await click(element(rendered.container, '.rev-action button'));

		setTestPage('/bob/other/wiki/Other/history', { owner: 'bob', repo: 'other', title: 'Other' });
		await settle();
		setTestPage('/alice/demo/wiki/Home/history', { owner: 'alice', repo: 'demo', title: 'Home' });
		await settle();
		await click(element(rendered.container, '.rev-action button'));
		expect(rendered.container.textContent).toContain('current revision');

		firstDetail.resolve({ id: 1, content: 'stale revision', created_at: timestamp });
		await settle();
		expect(rendered.container.textContent).toContain('current revision');
		expect(rendered.container.textContent).not.toContain('stale revision');
	});
});
