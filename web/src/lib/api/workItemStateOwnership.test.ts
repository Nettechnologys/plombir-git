import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import IssueListPage from '../../routes/[owner]/[repo]/issues/+page.svelte';
import IssuePage from '../../routes/[owner]/[repo]/issues/[number]/+page.svelte';
import MilestonesPage from '../../routes/[owner]/[repo]/milestones/+page.svelte';
import PullListPage from '../../routes/[owner]/[repo]/pulls/+page.svelte';
import PullPage from '../../routes/[owner]/[repo]/pulls/[number]/+page.svelte';
import TimeTrackingPage from '../../routes/[owner]/[repo]/time_tracking/+page.svelte';
import { setTestPage } from '../test/app';
import {
	attachments,
	collaborators,
	issues,
	milestones,
	pulls,
	repos,
	resetTestClient,
	reviews,
	timeTracking,
} from '../test/client';
import {
	button,
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
	reject: (reason?: unknown) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (reason?: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

const timestamp = '2026-08-30T00:00:00Z';
const pagination = (page = 1, totalPages = 1) => ({
	page,
	per_page: 20,
	total: totalPages,
	total_pages: totalPages,
});

function issue(number: number, title: string) {
	return {
		id: number,
		repo_id: 1,
		number,
		title,
		body: null,
		state: 'open',
		author_id: 1,
		author: 'alice',
		assignee_id: null,
		assignee: null,
		milestone_id: null,
		labels: [],
		created_at: timestamp,
		updated_at: timestamp,
		closed_at: null,
	};
}

function pullRequest(number: number, title: string, overrides: Record<string, unknown> = {}) {
	return {
		id: number,
		number,
		title,
		body: '',
		state: 'open',
		is_draft: false,
		author: 'alice',
		created_at: timestamp,
		head_branch: 'feature',
		base_branch: 'main',
		head_repo_id: null,
		head_sha: `head-${number}`,
		ci_approved_sha: null,
		auto_merge_enabled: false,
		auto_merge_strategy: 'merge',
		...overrides,
	};
}

function milestone(id: number, title: string) {
	return {
		id,
		title,
		description: '',
		due_date: null,
		state: 'open',
	};
}

function entry(id: number, description: string) {
	return {
		id,
		duration_minutes: 60,
		description,
		created_at: timestamp,
	};
}

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/issues', { owner: 'alice', repo: 'demo' });
	vi.stubGlobal('confirm', vi.fn(() => true));

	issues.list.mockResolvedValue({ data: [], pagination: pagination() });
	issues.templates.mockResolvedValue([]);
	issues.templateConfig.mockResolvedValue({ blank_issues_enabled: true, contact_links: [] });
	issues.get.mockResolvedValue(issue(7, 'Current issue'));
	issues.comments.mockResolvedValue([]);
	milestones.list.mockResolvedValue([]);
	milestones.get.mockResolvedValue(milestone(1, 'Current milestone'));
	collaborators.list.mockResolvedValue([]);
	repos.get.mockResolvedValue({ owner_id: 1, default_branch: 'main' });
	repos.branches.mockResolvedValue([
		{ name: 'main', is_default: true },
		{ name: 'feature', is_default: false },
	]);
	pulls.list.mockResolvedValue({ data: [], pagination: pagination() });
	pulls.template.mockResolvedValue(null);
	pulls.get.mockResolvedValue(pullRequest(7, 'Current pull request'));
	pulls.diff.mockResolvedValue(null);
	pulls.mergeQueue.mockResolvedValue([]);
	reviews.list.mockResolvedValue([]);
	reviews.comments.mockResolvedValue([]);
	reviews.timeline.mockResolvedValue([]);
	reviews.requestedReviewers.mockResolvedValue([]);
	attachments.list.mockResolvedValue([]);
	timeTracking.list.mockResolvedValue({ data: [], pagination: pagination() });
	timeTracking.total.mockResolvedValue({ total_minutes: 0, total_formatted: '0m' });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

async function revisit(path: string, middleParams: Record<string, string>, currentParams: Record<string, string>) {
	setTestPage(`/bob/other/${path}`, middleParams);
	await settle();
	setTestPage(`/alice/demo/${path}`, currentParams);
	await settle();
}

describe('repository work-item state ownership', () => {
	it('keeps the newest issue filter when list responses finish in reverse order', async () => {
		const staleClosed = deferred<{ data: ReturnType<typeof issue>[]; pagination: ReturnType<typeof pagination> }>();
		issues.list
			.mockResolvedValueOnce({ data: [issue(1, 'Open issue')], pagination: pagination() })
			.mockReturnValueOnce(staleClosed.promise)
			.mockResolvedValueOnce({ data: [issue(3, 'Current all issue')], pagination: pagination() });
		rendered = await renderComponent(IssueListPage);

		const filters = rendered.container.querySelectorAll<HTMLButtonElement>('.filter-btn');
		await click(filters[1]);
		await click(filters[2]);
		expect(rendered.container.textContent).toContain('Current all issue');

		staleClosed.resolve({ data: [issue(2, 'Stale closed issue')], pagination: pagination() });
		await settle();
		expect(rendered.container.textContent).toContain('Current all issue');
		expect(rendered.container.textContent).not.toContain('Stale closed issue');
	});

	it('keeps loading and error owned by the newest issue filter request', async () => {
		const staleClosed = deferred<{ data: ReturnType<typeof issue>[]; pagination: ReturnType<typeof pagination> }>();
		const currentAll = deferred<{ data: ReturnType<typeof issue>[]; pagination: ReturnType<typeof pagination> }>();
		issues.list
			.mockResolvedValueOnce({ data: [issue(1, 'Open issue')], pagination: pagination() })
			.mockReturnValueOnce(staleClosed.promise)
			.mockReturnValueOnce(currentAll.promise);
		rendered = await renderComponent(IssueListPage);

		const filters = rendered.container.querySelectorAll<HTMLButtonElement>('.filter-btn');
		await click(filters[1]);
		await click(filters[2]);
		staleClosed.reject(new Error('stale closed error'));
		await settle();
		expect(rendered.container.textContent).not.toContain('stale closed error');
		expect(rendered.container.querySelector('.text-secondary')).not.toBeNull();

		currentAll.resolve({ data: [issue(3, 'Current all issue')], pagination: pagination() });
		await settle();
		expect(rendered.container.textContent).toContain('Current all issue');
		expect(rendered.container.querySelector('.text-secondary')).toBeNull();
	});

	it('keeps a post-create issue list and serializes duplicate creates', async () => {
		const initial = deferred<{ data: ReturnType<typeof issue>[]; pagination: ReturnType<typeof pagination> }>();
		const create = deferred<ReturnType<typeof issue>>();
		issues.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce({ data: [issue(2, 'Created issue')], pagination: pagination() });
		issues.create.mockReturnValueOnce(create.promise);
		rendered = await renderComponent(IssueListPage);

		await click(button(rendered.container, 'New Issue'));
		await input(element(rendered.container, '.create-form input[type="text"]'), 'Created issue');
		const form = element<HTMLFormElement>(rendered.container, '.create-form form');
		await submit(form);
		await submit(form);
		expect(issues.create).toHaveBeenCalledOnce();

		create.resolve(issue(2, 'Created issue'));
		await settle();
		initial.resolve({ data: [issue(1, 'Stale initial issue')], pagination: pagination() });
		await settle();
		expect(rendered.container.textContent).toContain('Created issue');
		expect(rendered.container.textContent).not.toContain('Stale initial issue');
	});

	it('rejects the first issue-detail visit after A -> B -> A', async () => {
		const firstVisit = deferred<ReturnType<typeof issue>>();
		issues.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(issue(8, 'Middle issue'))
			.mockResolvedValueOnce(issue(7, 'Current issue'));
		setTestPage('/alice/demo/issues/7', { owner: 'alice', repo: 'demo', number: '7' });
		rendered = await renderComponent(IssuePage);

		await revisit(
			'issues/7',
			{ owner: 'bob', repo: 'other', number: '8' },
			{ owner: 'alice', repo: 'demo', number: '7' },
		);
		expect(rendered.container.textContent).toContain('Current issue');

		firstVisit.resolve(issue(7, 'Stale first visit'));
		await settle();
		expect(rendered.container.textContent).toContain('Current issue');
		expect(rendered.container.textContent).not.toContain('Stale first visit');
	});

	it('shares one issue-detail mutation claim across state and link controls', async () => {
		const update = deferred<ReturnType<typeof issue>>();
		issues.update.mockReturnValueOnce(update.promise);
		setTestPage('/alice/demo/issues/7', { owner: 'alice', repo: 'demo', number: '7' });
		rendered = await renderComponent(IssuePage);

		await click(element(rendered.container, '.btn-close'));
		const saveLinks = element<HTMLButtonElement>(rendered.container, '.save-links');
		expect(saveLinks.disabled).toBe(true);
		await click(saveLinks);
		expect(issues.update).toHaveBeenCalledOnce();

		update.resolve(issue(7, 'Current issue'));
		await settle();
	});

	it('keeps the newest pull-request filter when list responses reverse', async () => {
		const staleClosed = deferred<{ data: ReturnType<typeof pullRequest>[]; pagination: ReturnType<typeof pagination> }>();
		pulls.list
			.mockResolvedValueOnce({ data: [pullRequest(1, 'Open pull')], pagination: pagination() })
			.mockReturnValueOnce(staleClosed.promise)
			.mockResolvedValueOnce({ data: [pullRequest(3, 'Current merged pull')], pagination: pagination() });
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);

		const filters = rendered.container.querySelectorAll<HTMLButtonElement>('.filter-btn');
		await click(filters[1]);
		await click(filters[2]);
		expect(rendered.container.textContent).toContain('Current merged pull');

		staleClosed.resolve({ data: [pullRequest(2, 'Stale closed pull')], pagination: pagination() });
		await settle();
		expect(rendered.container.textContent).toContain('Current merged pull');
		expect(rendered.container.textContent).not.toContain('Stale closed pull');
	});

	it('keeps a post-create pull-request list and serializes duplicate creates', async () => {
		const initial = deferred<{ data: ReturnType<typeof pullRequest>[]; pagination: ReturnType<typeof pagination> }>();
		const create = deferred<ReturnType<typeof pullRequest>>();
		pulls.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce({ data: [pullRequest(2, 'Created pull')], pagination: pagination() });
		pulls.create.mockReturnValueOnce(create.promise);
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PullListPage);

		await click(button(rendered.container, 'New Pull Request'));
		await input(element(rendered.container, '.create-form input[type="text"]'), 'Created pull');
		const selects = rendered.container.querySelectorAll<HTMLSelectElement>('.branch-row select');
		selects[0].value = 'feature';
		selects[0].dispatchEvent(new Event('change', { bubbles: true }));
		await settle();
		const form = element<HTMLFormElement>(rendered.container, '.create-form form');
		await submit(form);
		await submit(form);
		expect(pulls.create).toHaveBeenCalledOnce();

		create.resolve(pullRequest(2, 'Created pull'));
		await settle();
		initial.resolve({ data: [pullRequest(1, 'Stale initial pull')], pagination: pagination() });
		await settle();
		expect(rendered.container.textContent).toContain('Created pull');
		expect(rendered.container.textContent).not.toContain('Stale initial pull');
	});

	it('rejects the first pull-detail visit after A -> B -> A', async () => {
		const firstVisit = deferred<ReturnType<typeof pullRequest>>();
		pulls.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(pullRequest(8, 'Middle pull'))
			.mockResolvedValueOnce(pullRequest(7, 'Current pull'));
		setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
		rendered = await renderComponent(PullPage);

		await revisit(
			'pulls/7',
			{ owner: 'bob', repo: 'other', number: '8' },
			{ owner: 'alice', repo: 'demo', number: '7' },
		);
		expect(rendered.container.textContent).toContain('Current pull');

		firstVisit.resolve(pullRequest(7, 'Stale first visit'));
		await settle();
		expect(rendered.container.textContent).toContain('Current pull');
		expect(rendered.container.textContent).not.toContain('Stale first visit');
	});

	it('shares one pull-detail mutation claim across conflicting controls', async () => {
		const update = deferred<ReturnType<typeof pullRequest>>();
		pulls.update.mockReturnValueOnce(update.promise);
		setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
		rendered = await renderComponent(PullPage);

		await click(element(rendered.container, '.pr-meta .btn-link'));
		const merge = element<HTMLButtonElement>(rendered.container, '.btn-merge');
		expect(merge.disabled).toBe(true);
		await click(merge);
		expect(pulls.update).toHaveBeenCalledOnce();
		expect(pulls.merge).not.toHaveBeenCalled();

		update.resolve(pullRequest(7, 'Current pull', { is_draft: true }));
		await settle();
	});

	it('keeps a post-create milestone list and shares one mutation claim', async () => {
		const initial = deferred<ReturnType<typeof milestone>[]>();
		const create = deferred<ReturnType<typeof milestone>>();
		milestones.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([milestone(2, 'Created milestone')]);
		milestones.create.mockReturnValueOnce(create.promise);
		setTestPage('/alice/demo/milestones', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(MilestonesPage);

		await input(element(rendered.container, '.editor input'), 'Created milestone');
		const form = element<HTMLFormElement>(rendered.container, '.editor form');
		await submit(form);
		await submit(form);
		expect(milestones.create).toHaveBeenCalledOnce();

		create.resolve(milestone(2, 'Created milestone'));
		await settle();
		initial.resolve([milestone(1, 'Stale initial milestone')]);
		await settle();
		expect(rendered.container.textContent).toContain('Created milestone');
		expect(rendered.container.textContent).not.toContain('Stale initial milestone');
	});

	it('does not publish an edit response from a previous milestone visit', async () => {
		const firstEdit = deferred<ReturnType<typeof milestone>>();
		milestones.list.mockResolvedValue([milestone(1, 'Visible milestone')]);
		milestones.get.mockReturnValueOnce(firstEdit.promise);
		setTestPage('/alice/demo/milestones', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(MilestonesPage);

		await click(button(rendered.container, 'Edit'));
		await revisit(
			'milestones',
			{ owner: 'bob', repo: 'other' },
			{ owner: 'alice', repo: 'demo' },
		);
		firstEdit.resolve(milestone(1, 'Stale edit form'));
		await settle();

		expect(element<HTMLInputElement>(rendered.container, '.editor input').value).toBe('');
		expect(rendered.container.textContent).not.toContain('Stale edit form');
	});

	it('keeps the newest time-tracking selection when detail responses reverse', async () => {
		const staleEntries = deferred<{ data: ReturnType<typeof entry>[]; pagination: ReturnType<typeof pagination> }>();
		const staleTotal = deferred<{ total_minutes: number; total_formatted: string }>();
		issues.list.mockResolvedValue({
			data: [issue(1, 'First issue'), issue(2, 'Current issue')],
			pagination: pagination(),
		});
		timeTracking.list
			.mockReturnValueOnce(staleEntries.promise)
			.mockResolvedValueOnce({ data: [entry(2, 'Current entry')], pagination: pagination() });
		timeTracking.total
			.mockReturnValueOnce(staleTotal.promise)
			.mockResolvedValueOnce({ total_minutes: 120, total_formatted: '2h' });
		setTestPage('/alice/demo/time_tracking', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(TimeTrackingPage);

		const issueButtons = rendered.container.querySelectorAll<HTMLButtonElement>('.issue-item');
		await click(issueButtons[0]);
		await click(issueButtons[1]);
		expect(rendered.container.textContent).toContain('Current entry');
		expect(rendered.container.textContent).toContain('Total: 2h');

		staleEntries.resolve({ data: [entry(1, 'Stale entry')], pagination: pagination() });
		staleTotal.resolve({ total_minutes: 60, total_formatted: '1h' });
		await settle();
		expect(rendered.container.textContent).toContain('Current entry');
		expect(rendered.container.textContent).toContain('Total: 2h');
		expect(rendered.container.textContent).not.toContain('Stale entry');
	});

	it('shares one time-entry mutation claim between add and delete', async () => {
		const add = deferred<unknown>();
		issues.list.mockResolvedValue({ data: [issue(1, 'Tracked issue')], pagination: pagination() });
		timeTracking.list
			.mockResolvedValueOnce({ data: [entry(1, 'Existing entry')], pagination: pagination() })
			.mockResolvedValueOnce({ data: [entry(2, 'Created entry')], pagination: pagination() });
		timeTracking.add.mockReturnValueOnce(add.promise);
		setTestPage('/alice/demo/time_tracking', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(TimeTrackingPage);

		await click(element(rendered.container, '.issue-item'));
		const addButton = element<HTMLButtonElement>(rendered.container, '.form-action .btn-primary');
		await click(addButton);
		await click(addButton);
		expect(element<HTMLButtonElement>(rendered.container, '.issue-item').disabled).toBe(true);
		const deleteButton = element<HTMLButtonElement>(rendered.container, '.entries-table .btn-danger');
		expect(deleteButton.disabled).toBe(true);
		await click(deleteButton);
		expect(timeTracking.add).toHaveBeenCalledOnce();
		expect(timeTracking.delete).not.toHaveBeenCalled();

		add.resolve({});
		await settle();
		expect(rendered.container.textContent).toContain('Created entry');
	});
});
