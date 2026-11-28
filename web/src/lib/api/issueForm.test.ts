import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import IssuePage from '../../routes/[owner]/[repo]/issues/[number]/+page.svelte';
import { agentReadsUntrustedAuthor, buildIssueLinksPayload } from './issueForm';
import { setTestPage } from '../test/app';
import {
	attachments,
	collaborators,
	issues,
	milestones,
	repos,
	resetTestClient,
} from '../test/client';
import { change, click, element, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const issue = {
	id: 1,
	repo_id: 2,
	number: 42,
	title: 'Linked state',
	body: null,
	state: 'open',
	author_id: 1,
	author: 'alice',
	assignee_id: 7,
	assignee: 'bob',
	milestone_id: 3,
	labels: [],
	created_at: '2026-08-15T12:00:00Z',
	updated_at: '2026-08-15T12:00:00Z',
	closed_at: null,
};

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/issues/42', { owner: 'alice', repo: 'demo', number: '42' });
	issues.get.mockResolvedValue(issue);
	issues.comments.mockResolvedValue([]);
	issues.update.mockResolvedValue({ ...issue, assignee_id: null, milestone_id: null });
	milestones.list.mockResolvedValue([
		{ id: 3, title: 'v1', state: 'open' },
	]);
	collaborators.list.mockResolvedValue([
		{ user_id: 7, username: 'bob' },
	]);
	repos.get.mockResolvedValue({ owner_id: 1, default_branch: 'main', viewer_permission: 'write' });
	attachments.list.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('buildIssueLinksPayload', () => {
  it('uses explicit nulls for both clear operations', () => {
    expect(buildIssueLinksPayload({ assigneeId: '', milestoneId: '' })).toEqual({
      assignee_id: null,
      milestone_id: null,
    });
  });

  it('keeps selected ids numeric on the wire', () => {
    expect(buildIssueLinksPayload({ assigneeId: ' 12 ', milestoneId: '34' })).toEqual({
      assignee_id: 12,
      milestone_id: 34,
    });
  });

	it('submits both rendered clear choices as explicit nulls', async () => {
		rendered = await renderComponent(IssuePage);
		const selects = rendered.container.querySelectorAll<HTMLSelectElement>('.issue-links select');
		await change(selects[0], '');
		await change(selects[1], '');
		await click(element(rendered.container, '.save-links'));

		expect(issues.update).toHaveBeenCalledWith('alice', 'demo', 42, {
			assignee_id: null,
			milestone_id: null,
		});
	});
});

describe('the warning before an agent reads a stranger\'s issue', () => {
	const bots = new Set([9]);

	it('warns only for a new assignment of a bot to an issue whose author cannot write', () => {
		const form = (assigneeId: string) => ({ assigneeId, milestoneId: '' });
		expect(agentReadsUntrustedAuthor(form('9'), 7, bots, false)).toBe(true);
		expect(agentReadsUntrustedAuthor(form('9'), 7, bots, true)).toBe(false);
		expect(agentReadsUntrustedAuthor(form('9'), 7, bots, undefined)).toBe(false);
		expect(agentReadsUntrustedAuthor(form('7'), null, bots, false)).toBe(false);
		expect(agentReadsUntrustedAuthor(form('9'), 9, bots, false)).toBe(false);
		expect(agentReadsUntrustedAuthor(form(''), 7, bots, false)).toBe(false);
	});

	it('shows on the page when a bot is chosen for a stranger\'s issue, and not before', async () => {
		issues.get.mockResolvedValue({ ...issue, author_can_write: false });
		collaborators.list.mockResolvedValue([
			{ user_id: 7, username: 'bob', is_bot: false },
			{ user_id: 9, username: 'alice-agent', is_bot: true },
		]);
		rendered = await renderComponent(IssuePage);
		expect(rendered.container.querySelector('.agent-untrusted-author')).toBeNull();
		const assignee = rendered.container.querySelector<HTMLSelectElement>('.issue-links select')!;
		await change(assignee, '9');
		expect(element(rendered.container, '.agent-untrusted-author').textContent).toContain(
			'cannot write to this repository',
		);
		await change(assignee, '7');
		expect(rendered.container.querySelector('.agent-untrusted-author')).toBeNull();
	});

	it('stays quiet when the author can write', async () => {
		issues.get.mockResolvedValue({ ...issue, author_can_write: true });
		collaborators.list.mockResolvedValue([{ user_id: 9, username: 'alice-agent', is_bot: true }]);
		rendered = await renderComponent(IssuePage);
		await change(rendered.container.querySelector<HTMLSelectElement>('.issue-links select')!, '9');
		expect(rendered.container.querySelector('.agent-untrusted-author')).toBeNull();
	});
});
