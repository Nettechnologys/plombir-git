import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// card_e1baa94866ed, card_270a0a77fd79: the rest of the repository pages
// offered their write controls to every visitor, so a reader's click could
// only end in a 403. Each now follows `viewer_permission` from
// `GET /repos/{owner}/{name}` the way the server's routes do: `RepoWrite`
// controls to `write` and `admin`, `RepoAdmin` sections to `admin`,
// `RepoAuthRead` (reviews, comments) to anyone signed in.

const viewer = vi.hoisted(() => ({ user: null as null | { id: number; username: string } }));

vi.mock('$lib/stores/auth.svelte', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/stores/auth.svelte')>();
	return {
		...actual,
		isLoggedIn: () => viewer.user !== null,
		getUser: () =>
			viewer.user ? { ...viewer.user, email: '', is_admin: false, display_name: null } : null,
	};
});

import BlobPage from '../../routes/[owner]/[repo]/blob/[...path]/+page.svelte';
import BoardsPage from '../../routes/[owner]/[repo]/boards/+page.svelte';
import IssuePage from '../../routes/[owner]/[repo]/issues/[number]/+page.svelte';
import PackagesPage from '../../routes/[owner]/[repo]/packages/+page.svelte';
import PipelinesPage from '../../routes/[owner]/[repo]/pipelines/+page.svelte';
import PullRequestPage from '../../routes/[owner]/[repo]/pulls/[number]/+page.svelte';
import RepoPage from '../../routes/[owner]/[repo]/+page.svelte';
import TimeTrackingPage from '../../routes/[owner]/[repo]/time_tracking/+page.svelte';
import RepoHeader from '../components/RepoHeader.svelte';
import SettingsLayout from '../../routes/[owner]/[repo]/settings/+layout.svelte';
import { createRawSnippet } from 'svelte';
import { setTestPage } from '../test/app';
import {
	artifacts,
	attachments,
	boards,
	collaborators,
	issues,
	milestones,
	packages,
	pipelines,
	pulls,
	repos,
	resetTestClient,
	reviews,
	timeTracking,
} from '../test/client';
import { click, renderComponent, settle, type RenderedComponent } from '../test/render';

type Level = 'admin' | 'write' | 'read' | null;

let rendered: RenderedComponent | undefined;

function viewerIs(level: Level, signedIn = level !== null) {
	viewer.user = signedIn ? { id: 3, username: 'carol' } : null;
	repos.get.mockResolvedValue({
		id: 1,
		name: 'demo',
		default_branch: 'main',
		stars_count: 0,
		is_private: false,
		created_at: '2026-10-07T00:00:00Z',
		...(level === null ? {} : { viewer_permission: level }),
	});
}

function text(container: ParentNode): string {
	return container.textContent ?? '';
}

function hasButton(container: ParentNode, label: string): boolean {
	return Array.from(container.querySelectorAll('button')).some(
		(candidate) => candidate.textContent?.trim() === label,
	);
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	attachments.list.mockResolvedValue([]);
	repos.starred.mockResolvedValue({ starred: false });
	repos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
	repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	document.body.innerHTML = '';
});

const LEVELS = [
	['admin', true],
	['write', true],
	['read', false],
	[null, false],
] as const;

describe.each(LEVELS)('viewer_permission %s', (level, writes) => {
	it(`${writes ? 'offers' : 'withholds'} merging, reviewers and resolving on a pull request`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
		pulls.get.mockResolvedValue({
			id: 17,
			number: 7,
			title: 'Ship it',
			body: '',
			state: 'open',
			is_draft: false,
			author: 'bob',
			created_at: '2026-10-01T00:00:00Z',
			head_branch: 'feature',
			base_branch: 'main',
			head_repo_id: null,
			head_sha: 'head',
			ci_approved_sha: null,
			auto_merge_enabled: false,
			auto_merge_strategy: 'merge',
		});
		pulls.diff.mockResolvedValue(null);
		pulls.mergeQueue.mockResolvedValue([]);
		reviews.list.mockResolvedValue([]);
		reviews.timeline.mockResolvedValue([]);
		reviews.requestedReviewers.mockResolvedValue([]);
		reviews.comments.mockResolvedValue([
			{
				id: 31,
				review_id: 1,
				pr_id: 17,
				author_id: 2,
				path: 'src/lib.rs',
				line: 3,
				side: 'RIGHT',
				body: 'Rename this',
				suggestion: 'renamed()',
				commit_id: 'head',
				reply_to_id: null,
				resolved_at: null,
				created_at: '2026-10-01T10:00:00Z',
				updated_at: '2026-10-01T10:00:00Z',
			},
		]);

		rendered = await renderComponent(PullRequestPage);
		await settle();
		const container = rendered.container;

		expect(container.querySelector('.btn-merge') !== null).toBe(writes);
		expect(container.querySelector('.reviewer-form') !== null).toBe(writes);
		expect(hasButton(container, 'Resolve conversation')).toBe(writes);
		expect(hasButton(container, 'Apply suggestion')).toBe(writes);
		expect(hasButton(container, 'Convert to draft')).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} closing and linking an issue`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/issues/5', { owner: 'alice', repo: 'demo', number: '5' });
		issues.get.mockResolvedValue({
			id: 50,
			repo_id: 2,
			number: 5,
			title: 'Broken thing',
			body: null,
			state: 'open',
			author_id: 2,
			author: 'bob',
			assignee_id: null,
			milestone_id: null,
			labels: [],
			created_at: '2026-10-01T09:00:00Z',
			updated_at: '2026-10-01T09:00:00Z',
			closed_at: null,
		});
		issues.comments.mockResolvedValue([]);
		milestones.list.mockResolvedValue([]);
		collaborators.list.mockResolvedValue([]);

		rendered = await renderComponent(IssuePage);
		await settle();
		const container = rendered.container;

		expect(container.querySelector('.save-links') !== null).toBe(writes);
		expect(hasButton(container, 'Close Issue')).toBe(writes);
		expect(container.querySelector('.upload-button') !== null).toBe(writes);
		// Commenting is open to every signed-in reader.
		expect(container.querySelector('.comment-form') !== null).toBe(level !== null);
	});

	it(`${writes ? 'offers' : 'withholds'} changing a board`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/boards', { owner: 'alice', repo: 'demo' });
		const board = { id: 7, name: 'Roadmap', description: 'Q4' };
		boards.list.mockResolvedValue([board]);
		boards.get.mockResolvedValue({
			board,
			columns: [
				{
					column: { id: 3, board_id: 7, name: 'Todo', position: 0 },
					cards: [{ id: 9, column_id: 3, note: 'a note', issue_id: null, position: 0, issue: null }],
				},
			],
		});
		issues.list.mockResolvedValue({ data: [] });

		rendered = await renderComponent(BoardsPage);
		await settle();
		const container = rendered.container;

		expect(text(container)).toContain('Todo');
		expect(container.querySelector('.board-header-actions') !== null).toBe(writes);
		expect(container.querySelector('.card-actions') !== null).toBe(writes);
		expect(container.querySelector('.card-move') !== null).toBe(writes);
		expect(container.querySelector('.add-card-btn') !== null).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} running a pipeline`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
		pipelines.list.mockResolvedValue({ data: [], pagination: { total_pages: 1 } });
		artifacts.list.mockResolvedValue([]);

		rendered = await renderComponent(PipelinesPage);
		await settle();

		expect(rendered.container.querySelector('.pipeline-trigger') !== null).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} logging time`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/time_tracking', { owner: 'alice', repo: 'demo' });
		issues.list.mockResolvedValue({ data: [{ id: 50, number: 5, title: 'Broken thing' }] });
		timeTracking.list.mockResolvedValue({
			data: [{ id: 1, duration_minutes: 30, description: 'triage', created_at: '2026-10-01T00:00:00Z' }],
			pagination: { total_pages: 1 },
		});
		timeTracking.total.mockResolvedValue({ total_minutes: 30, total_formatted: '30m' });

		rendered = await renderComponent(TimeTrackingPage);
		await settle();
		await click(rendered.container.querySelector<HTMLButtonElement>('.issue-item')!);
		await settle();

		expect(text(rendered.container)).toContain('triage');
		expect(rendered.container.querySelector('.form-card') !== null).toBe(writes);
		expect(hasButton(rendered.container, 'Delete')).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} publishing a package`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/packages', { owner: 'alice', repo: 'demo' });
		packages.list.mockResolvedValue({ data: [], pagination: { total_pages: 1, total: 0 }, failedRegistryTypes: [] });

		rendered = await renderComponent(PackagesPage);
		await settle();

		expect(rendered.container.querySelector('a[href="/alice/demo/packages/upload"]') !== null).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} editing and deleting a file`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo/blob/README.md?ref=main', { owner: 'alice', repo: 'demo', path: 'README.md' });
		repos.blob.mockResolvedValue({
			path: 'notes.txt',
			sha: 'b'.repeat(40),
			size: 5,
			content: 'hello',
			encoding: 'utf-8',
			is_binary: false,
			name: 'notes.txt',
		});

		rendered = await renderComponent(BlobPage);
		await settle();

		expect(text(rendered.container)).toContain('hello');
		expect(rendered.container.querySelector('button.danger') !== null).toBe(writes);
		expect(text(rendered.container).includes('Edit File')).toBe(writes);
	});

	it(`${writes ? 'offers' : 'withholds'} a new file, and links directories either way`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });
		repos.tree.mockResolvedValue({
			entries: [
				{ name: 'src', kind: 'tree', size: null },
				{ name: 'README.md', kind: 'blob', size: 5 },
			],
		});
		repos.log.mockResolvedValue({ commits: [] });
		repos.blob.mockRejectedValue(new Error('no readme'));

		rendered = await renderComponent(RepoPage);
		await settle();
		const container = rendered.container;

		expect(container.querySelector('a[href="/alice/demo/new"]') !== null).toBe(writes);
		// card_61e77c8abec1: a directory is a link, so Enter, a middle click and
		// the back button work.
		expect(container.querySelector('a.entry[href="/alice/demo?path=src"]')).not.toBeNull();
		expect(container.querySelector('[role="button"].entry')).toBeNull();
	});

	it(`${writes ? 'shows' : 'hides'} the Settings tab`, async () => {
		viewerIs(level);
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });

		rendered = await renderComponent(RepoHeader, { owner: 'alice', repo: 'demo' });
		await settle();

		expect(rendered.container.querySelector('a.tab[href="/alice/demo/settings"]') !== null).toBe(writes);
	});
});

describe('pull request reviews', () => {
	it('are not offered to an anonymous reader', async () => {
		viewerIs(null, false);
		setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
		pulls.get.mockResolvedValue({
			id: 17,
			number: 7,
			title: 'Ship it',
			body: '',
			state: 'open',
			is_draft: false,
			author: 'bob',
			created_at: '2026-10-01T00:00:00Z',
			head_branch: 'feature',
			base_branch: 'main',
			head_repo_id: null,
			head_sha: 'head',
			ci_approved_sha: null,
			auto_merge_enabled: false,
			auto_merge_strategy: 'merge',
		});
		pulls.diff.mockResolvedValue(null);
		pulls.mergeQueue.mockResolvedValue([]);
		reviews.list.mockResolvedValue([]);
		reviews.timeline.mockResolvedValue([]);
		reviews.requestedReviewers.mockResolvedValue([]);
		reviews.comments.mockResolvedValue([]);

		rendered = await renderComponent(PullRequestPage);
		await settle();
		const reviewTab = Array.from(rendered.container.querySelectorAll<HTMLButtonElement>('.pr-tabs .tab')).at(-1)!;
		await click(reviewTab);

		expect(rendered.container.querySelector('.review-form')).toBeNull();
	});
});

describe('repository settings sections', () => {
	const child = createRawSnippet(() => ({ render: () => '<p class="section-body">section</p>' }));

	async function openSettings(level: Level, path: string) {
		viewerIs(level);
		setTestPage(path, { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(SettingsLayout, { children: child });
		await settle();
		return rendered.container;
	}

	it('lists to a writer only the sections a writer can use', async () => {
		const container = await openSettings('write', '/alice/demo/settings/labels');

		expect(container.querySelector('a[href="/alice/demo/settings/labels"]')).not.toBeNull();
		expect(container.querySelector('a[href="/alice/demo/settings/mirror"]')).not.toBeNull();
		expect(container.querySelector('a[href="/alice/demo/settings/webhooks"]')).toBeNull();
		expect(container.querySelector('a[href="/alice/demo/settings/ci-secrets"]')).toBeNull();
		expect(container.querySelector('.section-body')).not.toBeNull();
	});

	it('renders an administrators’ section to a writer as a notice, not as a page that 403s', async () => {
		const container = await openSettings('write', '/alice/demo/settings/webhooks');

		expect(container.querySelector('.section-body')).toBeNull();
		expect(text(container)).toContain('This page is for the repository’s administrators.');
	});

	it('lists every section to an administrator', async () => {
		const container = await openSettings('admin', '/alice/demo/settings/webhooks');

		expect(container.querySelectorAll('.sidebar a.nav-item')).toHaveLength(14);
		expect(container.querySelector('.section-body')).not.toBeNull();
	});
});
