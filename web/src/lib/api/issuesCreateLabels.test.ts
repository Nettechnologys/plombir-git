import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import IssueListPage from '../../routes/[owner]/[repo]/issues/+page.svelte';
import { setTestPage } from '../test/app';
import { issues, labels, resetTestClient } from '../test/client';
import {
	button,
	check,
	click,
	element,
	input,
	renderComponent,
	submit,
	type RenderedComponent,
} from '../test/render';

let rendered: RenderedComponent | undefined;

const pagination = {
	page: 1,
	per_page: 20,
	total: 0,
	total_pages: 0,
};

const createdIssue = {
	id: 1,
	repo_id: 2,
	number: 1,
	title: 'Shipping regression',
	body: null,
	state: 'open',
	author_id: 1,
	author: 'alice',
	assignee_id: null,
	assignee: null,
	milestone_id: null,
	labels: [],
	created_at: '2026-09-06T00:00:00Z',
	updated_at: '2026-09-06T00:00:00Z',
	closed_at: null,
};

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/issues', { owner: 'alice', repo: 'demo' });
	issues.list.mockResolvedValue({ data: [], pagination });
	issues.templates.mockResolvedValue([]);
	issues.templateConfig.mockResolvedValue({ blank_issues_enabled: true, contact_links: [] });
	issues.create.mockResolvedValue(createdIssue);
	labels.list.mockResolvedValue([
		{ id: 1, name: 'release, urgent', color: '#d73a4a', description: null },
		{ id: 2, name: 'bug', color: '#0075ca', description: null },
	]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

async function openCreateForm(): Promise<HTMLFormElement> {
	rendered = await renderComponent(IssueListPage);
	await click(button(rendered.container, 'New Issue'));
	await input(element(rendered.container, '.create-form input[type="text"]'), 'Shipping regression');
	return element(rendered.container, '.create-form form');
}

describe('issue create label selection', () => {
	it('submits a comma-bearing label as one element next to a separate label', async () => {
		const form = await openCreateForm();
		await check(element(rendered!.container, 'input[value="release, urgent"]'), true);
		await check(element(rendered!.container, 'input[value="bug"]'), true);
		await submit(form);

		expect(issues.create).toHaveBeenCalledWith(
			'alice',
			'demo',
			'Shipping regression',
			undefined,
			['release, urgent', 'bug'],
		);
	});

	it('omits labels when none are selected', async () => {
		const form = await openCreateForm();
		await submit(form);

		expect(issues.create).toHaveBeenCalledWith(
			'alice',
			'demo',
			'Shipping regression',
			undefined,
			undefined,
		);
	});
});

// card_c899d7177f65: every chip used to be the same purple outline.
describe('issue list label chips', () => {
	it('paint each label in its own colour with readable text', async () => {
		issues.list.mockResolvedValue({
			data: [{ ...createdIssue, labels: ['bug', 'release, urgent', 'gone'] }],
			pagination: { ...pagination, total: 1, total_pages: 1 },
		});
		labels.list.mockResolvedValue([
			{ id: 1, name: 'release, urgent', color: '#FBCA04', description: null },
			{ id: 2, name: 'bug', color: '#0075ca', description: null },
		]);
		rendered = await renderComponent(IssueListPage);

		const chip = (name: string) => element<HTMLElement>(rendered!.container, `.issue-meta [data-label="${name}"]`);
		expect(chip('bug').style.backgroundColor).toBe('rgb(0, 117, 202)');
		expect(chip('bug').style.color).toBe('rgb(255, 255, 255)');
		expect(chip('release, urgent').style.backgroundColor).toBe('rgb(251, 202, 4)');
		expect(chip('release, urgent').style.color).toBe('rgb(0, 0, 0)');
		// A name the label list does not know keeps the neutral outline.
		expect(chip('gone').classList.contains('outline')).toBe(true);
		expect(chip('gone').getAttribute('style')).toBeNull();
	});
});
