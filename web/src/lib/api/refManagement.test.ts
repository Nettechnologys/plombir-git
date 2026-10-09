import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// card_2060696224ff: branches and tags could be created and deleted only with
// `git push`. The pages' client namespace is the shared mock, but the calls
// under test are pointed at the REAL client, whose transport is this mock — so
// each assertion reads the request as it leaves the browser.
const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

const auth = vi.hoisted(() => ({ loggedIn: true }));

vi.mock('$lib/stores/auth.svelte', async (importOriginal) => {
	const actual = await importOriginal<typeof import('$lib/stores/auth.svelte')>();
	return {
		...actual,
		isLoggedIn: () => auth.loggedIn,
		getUser: () => (auth.loggedIn ? { id: 1, username: 'alice', email: '', is_admin: false, display_name: null } : null),
	};
});

import BranchesPage from '../../routes/[owner]/[repo]/branches/+page.svelte';
import TagsPage from '../../routes/[owner]/[repo]/tags/+page.svelte';
import PullRequestPage from '../../routes/[owner]/[repo]/pulls/[number]/+page.svelte';
import { ApiError } from './error';
import { refPatternMatches } from '../refPattern';
import { setTestPage } from '../test/app';
import {
	attachments as routeAttachments,
	branchProtections as routeBranchProtections,
	pulls as routePulls,
	repos as routeRepos,
	resetTestClient,
	reviews as routeReviews,
	tagProtections as routeTagProtections,
} from '../test/client';
import { click, element, input, renderComponent, settle, submit, type RenderedComponent } from '../test/render';
import { pulls } from './pulls';
import { repos } from './repos';

let rendered: RenderedComponent | undefined;

function requestTo(path: string, method: string) {
	return base.request.mock.calls.find(([url, init]) => url === path && init?.method === method);
}

function row(container: HTMLElement, attribute: 'branch' | 'tag', name: string): HTMLElement {
	const found = Array.from(container.querySelectorAll<HTMLElement>(`[data-${attribute}]`)).find(
		(candidate) => candidate.dataset[attribute] === name,
	);
	if (!found) throw new Error(`no ${attribute} row "${name}"`);
	return found;
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	base.request.mockReset();
	base.request.mockResolvedValue({ ref: 'refs/heads/x', sha: 'abc' });
	auth.loggedIn = true;
	// RepoHeader reads these for a signed-in viewer.
	routeRepos.starred.mockResolvedValue({ starred: false });
	routeRepos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
	// What the viewer may do here (card_3625a7b89abb); the write controls follow it.
	routeRepos.get.mockResolvedValue({ name: 'demo', default_branch: 'main', viewer_permission: 'write' });
	routeRepos.createBranch.mockImplementation(repos.createBranch);
	routeRepos.deleteBranch.mockImplementation(repos.deleteBranch);
	routeRepos.deleteTag.mockImplementation(repos.deleteTag);
	routeRepos.branches.mockResolvedValue([
		{ name: 'main', is_default: true },
		{ name: 'feature/x', is_default: false },
		{ name: 'release/1.0', is_default: false },
	]);
	routeBranchProtections.list.mockResolvedValue([{ branch_name: 'release/*' }]);
	routeRepos.tags.mockResolvedValue([{ name: 'v1.0/rc' }, { name: 'v2.0' }]);
	routeTagProtections.list.mockResolvedValue([{ pattern: 'v2*' }]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	document.body.innerHTML = '';
});

describe('protection patterns', () => {
	it('match like the receive-pack matcher: exact without *, * spans slashes', () => {
		expect(refPatternMatches('main', 'main')).toBe(true);
		expect(refPatternMatches('main2', 'main')).toBe(false);
		expect(refPatternMatches('release/1.0/hotfix', 'release/*')).toBe(true);
		expect(refPatternMatches('release', 'release/*')).toBe(false);
		expect(refPatternMatches('v2.0', 'v2*')).toBe(true);
		expect(refPatternMatches('v2x0', 'v2.0')).toBe(false);
	});
});

describe('branches page', () => {
	it('creates a branch with the name and start point it was given', async () => {
		setTestPage('/alice/demo/branches', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BranchesPage);

		await click(element(rendered.container, '.new-branch-btn'));
		const [name, from] = Array.from(rendered.container.querySelectorAll<HTMLInputElement>('.create-branch input'));
		expect(from.value).toBe('main');
		await input(name, 'topic/new');
		await input(from, 'v1.0/rc');
		await submit(element<HTMLFormElement>(rendered.container, '.create-branch'));

		const call = requestTo('/repos/alice/demo/branches', 'POST');
		expect(call).toBeTruthy();
		expect(JSON.parse(call![1].body)).toEqual({ name: 'topic/new', from: 'v1.0/rc' });
		expect(rendered.container.querySelector('[role="status"]')?.textContent).toContain('topic/new');
	});

	it('shows the server refusal of a create inline', async () => {
		routeRepos.createBranch.mockRejectedValue(new ApiError('branch feature/x already exists', 409));
		setTestPage('/alice/demo/branches', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BranchesPage);

		await click(element(rendered.container, '.new-branch-btn'));
		await input(element(rendered.container, '.create-branch input'), 'feature/x');
		await submit(element<HTMLFormElement>(rendered.container, '.create-branch'));

		expect(element(rendered.container, '.create-error').textContent).toContain('branch feature/x already exists');
	});

	it('offers no delete for the default branch and disables it for a protected one', async () => {
		setTestPage('/alice/demo/branches', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BranchesPage);

		expect(row(rendered.container, 'branch', 'main').querySelector('.delete-branch')).toBeNull();
		expect(row(rendered.container, 'branch', 'release/1.0').querySelector<HTMLButtonElement>('.delete-branch')!.disabled).toBe(true);
		expect(row(rendered.container, 'branch', 'feature/x').querySelector<HTMLButtonElement>('.delete-branch')!.disabled).toBe(false);
	});

	it('deletes a branch through a confirm dialog, the name sent as one encoded segment', async () => {
		setTestPage('/alice/demo/branches', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BranchesPage);

		await click(row(rendered.container, 'branch', 'feature/x').querySelector('.delete-branch')!);
		expect(base.request).not.toHaveBeenCalled();
		const dialog = element(document.body, '[role="dialog"]');
		expect(dialog.textContent).toContain('feature/x');
		await click(element(dialog, '.confirm-delete'));

		expect(requestTo('/repos/alice/demo/branches/feature%2Fx', 'DELETE')).toBeTruthy();
		expect(document.body.querySelector('[role="dialog"]')).toBeNull();
		expect(routeRepos.branches).toHaveBeenCalledTimes(2);
	});

	it('keeps the dialog open with the server message when a delete is refused', async () => {
		routeRepos.deleteBranch.mockRejectedValue(new ApiError('feature/x moved meanwhile', 409));
		setTestPage('/alice/demo/branches', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BranchesPage);

		await click(row(rendered.container, 'branch', 'feature/x').querySelector('.delete-branch')!);
		await click(element(document.body, '[role="dialog"] .confirm-delete'));

		expect(element(document.body, '[role="dialog"] .delete-error').textContent).toContain('feature/x moved meanwhile');
	});

	it('offers no write action to a signed-out reader', async () => {
		auth.loggedIn = false;
		// The server leaves `viewer_permission` out for an anonymous reader.
		routeRepos.get.mockResolvedValue({ name: 'demo', default_branch: 'main' });
		setTestPage('/alice/demo/branches', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BranchesPage);

		expect(rendered.container.querySelector('.new-branch-btn')).toBeNull();
		expect(rendered.container.querySelector('.delete-branch')).toBeNull();
		expect(rendered.container.textContent).toContain('feature/x');
	});

	it('offers no write action to a signed-in reader with read permission', async () => {
		// Signed in is not enough: these controls used to follow `isLoggedIn()`
		// and handed a read-only collaborator a button that could only end in 403.
		routeRepos.get.mockResolvedValue({ name: 'demo', default_branch: 'main', viewer_permission: 'read' });
		setTestPage('/alice/demo/branches', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BranchesPage);

		expect(routeRepos.get).toHaveBeenCalledWith('alice', 'demo');
		expect(rendered.container.textContent).toContain('feature/x');
		expect(rendered.container.querySelector('.new-branch-btn')).toBeNull();
		expect(rendered.container.querySelector('.delete-branch')).toBeNull();
	});

	it('offers the write actions to an admin as well as a writer', async () => {
		routeRepos.get.mockResolvedValue({ name: 'demo', default_branch: 'main', viewer_permission: 'admin' });
		setTestPage('/alice/demo/branches', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(BranchesPage);

		expect(rendered.container.querySelector('.new-branch-btn')).not.toBeNull();
		expect(row(rendered.container, 'branch', 'feature/x').querySelector('.delete-branch')).not.toBeNull();
	});
});

describe('tags page', () => {
	it('deletes a tag through a confirm dialog, encoded as one segment', async () => {
		setTestPage('/alice/demo/tags', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(TagsPage);

		expect(row(rendered.container, 'tag', 'v2.0').querySelector<HTMLButtonElement>('.delete-tag')!.disabled).toBe(true);
		await click(row(rendered.container, 'tag', 'v1.0/rc').querySelector('.delete-tag')!);
		await click(element(document.body, '[role="dialog"] .confirm-delete'));

		expect(requestTo('/repos/alice/demo/tags/v1.0%2Frc', 'DELETE')).toBeTruthy();
		expect(rendered.container.querySelector('[role="status"]')?.textContent).toContain('v1.0/rc');
	});

	it('offers no delete to a signed-in reader with read permission', async () => {
		routeRepos.get.mockResolvedValue({ name: 'demo', default_branch: 'main', viewer_permission: 'read' });
		setTestPage('/alice/demo/tags', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(TagsPage);

		expect(rendered.container.textContent).toContain('v1.0/rc');
		expect(rendered.container.querySelector('.delete-tag')).toBeNull();
	});
});

describe('merge with head-branch deletion', () => {
	function pullRequest(overrides: Record<string, unknown> = {}) {
		return {
			id: 17,
			number: 7,
			title: 'Ship it',
			body: '',
			state: 'open',
			is_draft: false,
			author: 'bob',
			created_at: '2026-10-01T00:00:00Z',
			head_branch: 'feature/x',
			base_branch: 'main',
			head_repo_id: null,
			head_sha: 'head',
			ci_approved_sha: null,
			auto_merge_enabled: false,
			auto_merge_strategy: 'merge',
			...overrides,
		};
	}

	beforeEach(() => {
		setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
		routePulls.get.mockResolvedValueOnce(pullRequest()).mockResolvedValue(pullRequest({ state: 'merged' }));
		routePulls.diff.mockResolvedValue(null);
		routePulls.mergeQueue.mockResolvedValue([]);
		routePulls.merge.mockImplementation(pulls.merge);
		routeAttachments.list.mockResolvedValue([]);
		routeReviews.list.mockResolvedValue([]);
		routeReviews.comments.mockResolvedValue([]);
		routeReviews.timeline.mockResolvedValue([]);
		routeReviews.requestedReviewers.mockResolvedValue([]);
	});

	it('leaves the head branch alone unless asked', async () => {
		base.request.mockResolvedValue({ merged: true });
		rendered = await renderComponent(PullRequestPage);

		const box = element<HTMLInputElement>(rendered.container, '.delete-head-branch');
		expect(box.checked).toBe(false);
		await click(element(rendered.container, '.btn-merge'));

		const call = requestTo('/repos/alice/demo/pulls/7/merge', 'POST');
		expect(JSON.parse(call![1].body)).toEqual({ strategy: 'merge', delete_head_branch: false });
		expect(rendered.container.querySelector('.head-branch-outcome')).toBeNull();
	});

	it('sends delete_head_branch and shows why the branch was kept', async () => {
		base.request.mockResolvedValue({
			merged: true,
			head_branch_deleted: false,
			head_branch_kept: 'pull request #9 still uses it',
		});
		rendered = await renderComponent(PullRequestPage);

		const box = element<HTMLInputElement>(rendered.container, '.delete-head-branch');
		box.checked = true;
		box.dispatchEvent(new Event('change', { bubbles: true }));
		await settle();
		await click(element(rendered.container, '.btn-merge'));

		const call = requestTo('/repos/alice/demo/pulls/7/merge', 'POST');
		expect(JSON.parse(call![1].body)).toEqual({ strategy: 'merge', delete_head_branch: true });
		const outcome = element(rendered.container, '.head-branch-outcome');
		expect(outcome.textContent).toContain('feature/x');
		expect(outcome.textContent).toContain('pull request #9 still uses it');
	});

	it('says the branch is gone when the server deleted it', async () => {
		base.request.mockResolvedValue({ merged: true, head_branch_deleted: true });
		rendered = await renderComponent(PullRequestPage);

		const box = element<HTMLInputElement>(rendered.container, '.delete-head-branch');
		box.checked = true;
		box.dispatchEvent(new Event('change', { bubbles: true }));
		await settle();
		await click(element(rendered.container, '.btn-merge'));

		expect(element(rendered.container, '.head-branch-outcome').textContent).toContain('Branch feature/x deleted.');
	});
});

