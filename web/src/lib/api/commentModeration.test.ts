import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// card_60961272e1ba: a comment could not be corrected or withdrawn, and an
// issue could not be deleted, except with `curl`. The pages' client namespace
// is the shared mock, but the calls under test are pointed at the REAL client,
// whose transport is this mock — so each assertion reads the request as it
// leaves the browser.
const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

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

import IssuePage from '../../routes/[owner]/[repo]/issues/[number]/+page.svelte';
import PullRequestPage from '../../routes/[owner]/[repo]/pulls/[number]/+page.svelte';
import { isEdited } from '../commentState';
import { ApiError } from './error';
import { issues } from './issues';
import { reviews } from './pulls';
import { navigation, setTestPage } from '../test/app';
import { openModalFrom } from '../test/modalContract';
import {
	attachments as routeAttachments,
	collaborators as routeCollaborators,
	issues as routeIssues,
	milestones as routeMilestones,
	notifications as routeNotifications,
	pulls as routePulls,
	repos as routeRepos,
	resetTestClient,
	reviews as routeReviews,
} from '../test/client';
import { click, element, input, renderComponent, type RenderedComponent } from '../test/render';

const ALICE = { id: 1, username: 'alice' };
const BOB = { id: 2, username: 'bob' };
const CAROL = { id: 3, username: 'carol' };

let rendered: RenderedComponent | undefined;

function requestTo(path: string, method: string) {
	return base.request.mock.calls.find(([url, init]) => url === path && init?.method === method);
}

function commentBox(container: ParentNode, attribute: 'comment' | 'reviewComment', id: number): HTMLElement {
	const found = Array.from(container.querySelectorAll<HTMLElement>(`[data-${attribute === 'comment' ? 'comment' : 'review-comment'}]`)).find(
		(candidate) => candidate.dataset[attribute] === String(id),
	);
	if (!found) throw new Error(`no ${attribute} ${id}`);
	return found;
}

function issueComment(overrides: Record<string, unknown> = {}) {
	return {
		id: 11,
		issue_id: 5,
		author_id: ALICE.id,
		author: 'alice',
		body: 'First words',
		created_at: '2026-10-01T10:00:00Z',
		updated_at: '2026-10-01T10:00:00Z',
		...overrides,
	};
}

function permission(level: 'admin' | 'write' | 'read') {
	routeRepos.get.mockResolvedValue({ owner_id: 1, name: 'demo', default_branch: 'main', viewer_permission: level });
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	base.request.mockReset();
	viewer.user = ALICE;
	routeAttachments.list.mockResolvedValue([]);
	// RepoHeader reads these for a signed-in viewer.
	routeRepos.starred.mockResolvedValue({ starred: false });
	routeRepos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
	routeIssues.editComment.mockImplementation(issues.editComment);
	routeIssues.deleteComment.mockImplementation(issues.deleteComment);
	routeIssues.delete.mockImplementation(issues.delete);
	routeReviews.editComment.mockImplementation(reviews.editComment);
	routeReviews.deleteComment.mockImplementation(reviews.deleteComment);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	document.body.innerHTML = '';
});

describe('the edited marker', () => {
	it('is set only when updated_at is later than created_at', () => {
		expect(isEdited(issueComment())).toBe(false);
		expect(isEdited(issueComment({ updated_at: '2026-10-01T10:05:00Z' }))).toBe(true);
		expect(isEdited(issueComment({ updated_at: '2026-10-01T09:00:00Z' }))).toBe(false);
		expect(isEdited({ created_at: null, updated_at: '2026-10-01T10:05:00Z' })).toBe(false);
	});
});

describe('issue page', () => {
	beforeEach(() => {
		setTestPage('/alice/demo/issues/5', { owner: 'alice', repo: 'demo', number: '5' });
		routeIssues.get.mockResolvedValue({
			id: 50,
			repo_id: 2,
			number: 5,
			title: 'Broken thing',
			body: null,
			state: 'open',
			author_id: BOB.id,
			author: 'bob',
			assignee_id: null,
			milestone_id: null,
			labels: [],
			created_at: '2026-10-01T09:00:00Z',
			updated_at: '2026-10-01T09:00:00Z',
			closed_at: null,
		});
		routeIssues.comments.mockResolvedValue([
			issueComment(),
			issueComment({ id: 12, author_id: BOB.id, author: 'bob', body: 'Second words', updated_at: '2026-10-01T11:00:00Z' }),
		]);
		routeMilestones.list.mockResolvedValue([]);
		routeCollaborators.list.mockResolvedValue([]);
		permission('read');
	});

	it('carries the subscription control, asking about this issue (card_349c2b6a0d7c)', async () => {
		routeNotifications.subscription.mockResolvedValue({ subscribed: true, reason: 'author' });
		rendered = await renderComponent(IssuePage);

		expect(routeNotifications.subscription).toHaveBeenCalledWith('alice', 'demo', 'issues', 5);
		expect(element(rendered.container, '.thread-subscription .btn-subscription').textContent?.trim()).toBe('Unsubscribe');
	});

	it('marks an edited comment and only that one', async () => {
		rendered = await renderComponent(IssuePage);

		expect(commentBox(rendered.container, 'comment', 11).querySelector('.edited-marker')).toBeNull();
		expect(commentBox(rendered.container, 'comment', 12).querySelector('.edited-marker')?.textContent).toContain('edited');
	});

	it('offers the actions to the author only, for a reader who is not an admin', async () => {
		rendered = await renderComponent(IssuePage);

		expect(commentBox(rendered.container, 'comment', 11).querySelector('.edit-comment')).not.toBeNull();
		expect(commentBox(rendered.container, 'comment', 11).querySelector('.delete-comment')).not.toBeNull();
		expect(commentBox(rendered.container, 'comment', 12).querySelector('.edit-comment')).toBeNull();
		expect(commentBox(rendered.container, 'comment', 12).querySelector('.delete-comment')).toBeNull();
		expect(rendered.container.querySelector('.delete-issue')).toBeNull();
	});

	it('offers nothing to a reader who wrote neither comment', async () => {
		viewer.user = CAROL;
		permission('write');
		rendered = await renderComponent(IssuePage);

		expect(rendered.container.querySelector('.edit-comment')).toBeNull();
		expect(rendered.container.querySelector('.delete-comment')).toBeNull();
		expect(rendered.container.querySelector('.delete-issue')).toBeNull();
	});

	it('offers every comment and the issue itself to an admin', async () => {
		viewer.user = CAROL;
		permission('admin');
		rendered = await renderComponent(IssuePage);

		expect(rendered.container.querySelectorAll('.edit-comment')).toHaveLength(2);
		expect(rendered.container.querySelectorAll('.delete-comment')).toHaveLength(2);
		expect(rendered.container.querySelector('.delete-issue')).not.toBeNull();
	});

	it('edits a comment inline and keeps the author it was listed with', async () => {
		base.request.mockResolvedValue({
			id: 11,
			issue_id: 5,
			author_id: ALICE.id,
			body: 'Corrected words',
			created_at: '2026-10-01T10:00:00Z',
			updated_at: '2026-10-09T08:00:00Z',
		});
		rendered = await renderComponent(IssuePage);
		const box = commentBox(rendered.container, 'comment', 11);

		await click(element(box, '.edit-comment'));
		const editor = element<HTMLTextAreaElement>(box, '.comment-edit-input');
		expect(editor.value).toBe('First words');
		await input(editor, 'Corrected words');
		await click(element(box, '.save-comment-edit'));

		const call = requestTo('/repos/alice/demo/issues/comments/11', 'PATCH');
		expect(call).toBeTruthy();
		expect(JSON.parse(call![1].body)).toEqual({ body: 'Corrected words' });
		const after = commentBox(rendered.container, 'comment', 11);
		expect(after.querySelector('.comment-edit-input')).toBeNull();
		expect(after.textContent).toContain('Corrected words');
		expect(after.textContent).toContain('alice');
		expect(after.querySelector('.edited-marker')).not.toBeNull();
	});

	it('cancels an edit without a request', async () => {
		rendered = await renderComponent(IssuePage);
		const box = commentBox(rendered.container, 'comment', 11);

		await click(element(box, '.edit-comment'));
		await input(element<HTMLTextAreaElement>(box, '.comment-edit-input'), 'Thrown away');
		await click(element(box, '.cancel-comment-edit'));

		expect(base.request).not.toHaveBeenCalled();
		expect(commentBox(rendered.container, 'comment', 11).textContent).toContain('First words');
	});

	it('shows the server refusal of an edit', async () => {
		routeIssues.editComment.mockRejectedValue(new ApiError('only the author or an administrator may edit this comment', 403));
		rendered = await renderComponent(IssuePage);
		const box = commentBox(rendered.container, 'comment', 11);

		await click(element(box, '.edit-comment'));
		await input(element<HTMLTextAreaElement>(box, '.comment-edit-input'), 'Corrected words');
		await click(element(box, '.save-comment-edit'));

		expect(element(box, '.comment-edit-error').textContent).toContain(
			'only the author or an administrator may edit this comment',
		);
		expect(element<HTMLTextAreaElement>(box, '.comment-edit-input').value).toBe('Corrected words');
	});

	it('deletes a comment through a confirm dialog', async () => {
		rendered = await renderComponent(IssuePage);

		const opener = element<HTMLButtonElement>(commentBox(rendered.container, 'comment', 11), '.delete-comment');
		const dialog = await openModalFrom(document.body, opener);
		expect(base.request).not.toHaveBeenCalled();
		await click(element(dialog, '.confirm-delete-comment'));

		expect(requestTo('/repos/alice/demo/issues/comments/11', 'DELETE')).toBeTruthy();
		expect(document.body.querySelector('[role="dialog"]')).toBeNull();
		expect(rendered.container.querySelector('[data-comment="11"]')).toBeNull();
		expect(rendered.container.querySelector('[data-comment="12"]')).not.toBeNull();
	});

	it('deletes the issue through a confirm dialog and returns to the list', async () => {
		viewer.user = CAROL;
		permission('admin');
		rendered = await renderComponent(IssuePage);

		const dialog = await openModalFrom(document.body, element(rendered.container, '.delete-issue'));
		expect(dialog.textContent).toContain('#5');
		expect(base.request).not.toHaveBeenCalled();
		await click(element(dialog, '.confirm-delete-issue'));

		expect(requestTo('/repos/alice/demo/issues/5', 'DELETE')).toBeTruthy();
		expect(navigation.goto).toHaveBeenCalledWith('/alice/demo/issues');
	});

	it('keeps the issue dialog open with the server refusal', async () => {
		viewer.user = CAROL;
		permission('admin');
		routeIssues.delete.mockRejectedValue(new ApiError('repository administrators only', 403));
		rendered = await renderComponent(IssuePage);

		await click(element(rendered.container, '.delete-issue'));
		await click(element(document.body, '[role="dialog"] .confirm-delete-issue'));

		expect(element(document.body, '[role="dialog"] .issue-delete-error').textContent).toContain('repository administrators only');
		expect(navigation.goto).not.toHaveBeenCalled();
	});
});

describe('pull request review comments', () => {
	function reviewComment(overrides: Record<string, unknown> = {}) {
		return {
			id: 31,
			review_id: 1,
			pr_id: 17,
			author_id: ALICE.id,
			path: 'src/lib.rs',
			line: 3,
			side: 'RIGHT',
			body: 'Rename this',
			suggestion: null,
			reply_to_id: null,
			resolved_at: null,
			created_at: '2026-10-01T10:00:00Z',
			updated_at: '2026-10-01T10:00:00Z',
			...overrides,
		};
	}

	beforeEach(() => {
		setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
		routePulls.get.mockResolvedValue({
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
		routePulls.diff.mockResolvedValue(null);
		routePulls.mergeQueue.mockResolvedValue([]);
		routeReviews.list.mockResolvedValue([]);
		routeReviews.timeline.mockResolvedValue([]);
		routeReviews.requestedReviewers.mockResolvedValue([]);
		routeReviews.comments.mockResolvedValue([
			reviewComment(),
			reviewComment({ id: 32, author_id: BOB.id, body: 'Agreed', reply_to_id: 31, updated_at: '2026-10-02T10:00:00Z' }),
		]);
		permission('read');
	});

	it('carries the subscription control, asking about this pull request (card_349c2b6a0d7c)', async () => {
		routeNotifications.subscription.mockResolvedValue({ subscribed: false, reason: null });
		rendered = await renderComponent(PullRequestPage);

		expect(routeNotifications.subscription).toHaveBeenCalledWith('alice', 'demo', 'pulls', 7);
		expect(element(rendered.container, '.thread-subscription .btn-subscription').textContent?.trim()).toBe('Subscribe');
	});

	it('offers the actions on the author’s own comment and marks the edited reply', async () => {
		rendered = await renderComponent(PullRequestPage);

		expect(routeRepos.get).toHaveBeenCalledWith('alice', 'demo');
		const own = commentBox(rendered.container, 'reviewComment', 31);
		const reply = commentBox(rendered.container, 'reviewComment', 32);
		expect(own.querySelector('.edit-comment')).not.toBeNull();
		expect(own.querySelector('.edited-marker')).toBeNull();
		expect(reply.querySelector('.edit-comment')).toBeNull();
		expect(reply.querySelector('.delete-comment')).toBeNull();
		expect(reply.querySelector('.edited-marker')?.textContent).toContain('edited');
	});

	it('offers nothing to a non-author who is not an admin, and everything to an admin', async () => {
		viewer.user = CAROL;
		rendered = await renderComponent(PullRequestPage);
		expect(rendered.container.querySelector('.edit-comment')).toBeNull();
		expect(rendered.container.querySelector('.delete-comment')).toBeNull();
		await rendered.destroy();

		permission('admin');
		rendered = await renderComponent(PullRequestPage);
		expect(rendered.container.querySelectorAll('.edit-comment')).toHaveLength(2);
		expect(rendered.container.querySelectorAll('.delete-comment')).toHaveLength(2);
	});

	it('edits a review comment inline', async () => {
		base.request.mockResolvedValue(reviewComment({ body: 'Rename this, please', updated_at: '2026-10-09T08:00:00Z' }));
		rendered = await renderComponent(PullRequestPage);
		const box = commentBox(rendered.container, 'reviewComment', 31);

		await click(element(box, '.edit-comment'));
		await input(element<HTMLTextAreaElement>(box, '.comment-edit-input'), 'Rename this, please');
		await click(element(box, '.save-comment-edit'));

		const call = requestTo('/repos/alice/demo/pulls/7/comments/31', 'PATCH');
		expect(call).toBeTruthy();
		expect(JSON.parse(call![1].body)).toEqual({ body: 'Rename this, please' });
		const after = commentBox(rendered.container, 'reviewComment', 31);
		expect(after.textContent).toContain('Rename this, please');
		expect(after.querySelector('.edited-marker')).not.toBeNull();
	});

	it('deletes a review comment through a confirm dialog', async () => {
		viewer.user = BOB;
		rendered = await renderComponent(PullRequestPage);

		await openModalFrom(document.body, element(commentBox(rendered.container, 'reviewComment', 32), '.delete-comment'));
		await click(element(document.body, '[role="dialog"] .confirm-delete-comment'));

		expect(requestTo('/repos/alice/demo/pulls/7/comments/32', 'DELETE')).toBeTruthy();
		expect(rendered.container.querySelector('[data-review-comment="32"]')).toBeNull();
	});

	it('keeps the dialog open with the server message when others replied', async () => {
		routeReviews.deleteComment.mockRejectedValue(new ApiError('others replied to this comment; it cannot be deleted', 409));
		rendered = await renderComponent(PullRequestPage);

		await click(element(commentBox(rendered.container, 'reviewComment', 31), '.delete-comment'));
		await click(element(document.body, '[role="dialog"] .confirm-delete-comment'));

		expect(element(document.body, '[role="dialog"] .comment-delete-error').textContent).toContain(
			'others replied to this comment; it cannot be deleted',
		);
		expect(rendered.container.querySelector('[data-review-comment="31"]')).not.toBeNull();
	});
});
