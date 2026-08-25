import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import IssuePage from '../../routes/[owner]/[repo]/issues/[number]/+page.svelte';
import { buildIssueLinksPayload } from './issueForm';
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
	repos.get.mockResolvedValue({ owner_id: 1, default_branch: 'main' });
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
