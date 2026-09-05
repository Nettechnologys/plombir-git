import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import RepositoryPage from '../../routes/[owner]/[repo]/+page.svelte';
import PullListPage from '../../routes/[owner]/[repo]/pulls/+page.svelte';
import TimeTrackingPage from '../../routes/[owner]/[repo]/time_tracking/+page.svelte';
import { ApiError } from './_base.svelte';
import { setTestPage } from '../test/app';
import { issues, pulls, repos, resetTestClient, timeTracking } from '../test/client';
import { button, click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

const timestamp = '2026-09-06T00:00:00Z';

function issue() {
	return {
		id: 1,
		repo_id: 1,
		number: 7,
		title: 'Tracked issue',
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

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	repos.get.mockResolvedValue({ id: 1, name: 'demo', default_branch: 'main', stars_count: 0 });
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('repository route read availability', () => {
	it('shows and retries a failed branch read instead of presenting empty pull selectors', async () => {
		pulls.list.mockResolvedValue({ data: [], pagination: { total_pages: 1 } });
		pulls.template.mockResolvedValue(null);
		repos.branches
			.mockRejectedValueOnce(new Error('branch service unavailable'))
			.mockResolvedValueOnce([
				{ name: 'main', is_default: true },
				{ name: 'feature', is_default: false },
			]);
		setTestPage('/alice/demo/pulls', { owner: 'alice', repo: 'demo' });

		rendered = await renderComponent(PullListPage);
		await click(button(rendered.container, 'New Pull Request'));

		expect(rendered.container.textContent).toContain('Repository branches could not be loaded.');
		expect(element<HTMLButtonElement>(rendered.container, '.create-form button[type="submit"]').disabled).toBe(true);

		await click(button(rendered.container, 'Retry'));

		expect(repos.branches).toHaveBeenCalledTimes(2);
		expect(rendered.container.textContent).toContain('feature');
		expect(rendered.container.textContent).not.toContain('Repository branches could not be loaded.');
	});

	it('shows and retries a failed time-total read instead of hiding the total', async () => {
		issues.list.mockResolvedValue({ data: [issue()], pagination: { total_pages: 1 } });
		timeTracking.list.mockResolvedValue({ data: [], pagination: { total_pages: 1 } });
		timeTracking.total
			.mockRejectedValueOnce(new Error('time store unavailable'))
			.mockResolvedValueOnce({ total_minutes: 120, total_formatted: '2h' });
		setTestPage('/alice/demo/time_tracking', { owner: 'alice', repo: 'demo' });

		rendered = await renderComponent(TimeTrackingPage);
		await click(element(rendered.container, '.issue-item'));

		expect(rendered.container.textContent).toContain('The time total could not be loaded.');
		expect(rendered.container.textContent).not.toContain('Total: 0m');

		await click(button(rendered.container, 'Retry'));

		expect(timeTracking.total).toHaveBeenCalledTimes(2);
		expect(rendered.container.textContent).toContain('Total: 2h');
		expect(rendered.container.textContent).not.toContain('The time total could not be loaded.');
	});

	it('shows and retries a failed README read instead of claiming there is no README', async () => {
		repos.tree.mockResolvedValue({ entries: [{ name: 'README.md', kind: 'blob' }] });
		repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
		repos.log.mockResolvedValue({ commits: [] });
		repos.blob
			.mockRejectedValueOnce(new ApiError('blob store unavailable', 503))
			.mockResolvedValueOnce({ content: '# Current README' });
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });

		rendered = await renderComponent(RepositoryPage);
		await settle();

		expect(rendered.container.textContent).toContain('The README could not be loaded.');
		expect(rendered.container.textContent).not.toContain('Current README');

		await click(button(rendered.container, 'Retry'));
		await settle();

		expect(repos.blob).toHaveBeenCalledTimes(2);
		expect(rendered.container.textContent).toContain('Current README');
		expect(rendered.container.textContent).not.toContain('The README could not be loaded.');
	});

	it('keeps a missing README distinct from a failed README read', async () => {
		repos.tree.mockResolvedValue({ entries: [{ name: 'README.md', kind: 'blob' }] });
		repos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
		repos.log.mockResolvedValue({ commits: [] });
		repos.blob.mockRejectedValueOnce(new ApiError('not found', 404));
		setTestPage('/alice/demo', { owner: 'alice', repo: 'demo' });

		rendered = await renderComponent(RepositoryPage);
		await settle();

		expect(rendered.container.textContent).not.toContain('The README could not be loaded.');
		expect(rendered.container.querySelector('.readme-section')).toBeNull();
	});
});
