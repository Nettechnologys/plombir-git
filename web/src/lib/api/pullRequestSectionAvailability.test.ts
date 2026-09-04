import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import PullRequestPage from '../../routes/[owner]/[repo]/pulls/[number]/+page.svelte';
import { setTestPage } from '../test/app';
import {
	attachments as routeAttachments,
	pulls as routePulls,
	reviews as routeReviews,
	resetTestClient,
} from '../test/client';
import { button, click, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;
let warn: ReturnType<typeof vi.spyOn>;

function pullRequest() {
	return {
		id: 17,
		number: 7,
		title: 'Teach the reader what did not load',
		body: '',
		state: 'open',
		is_draft: false,
		author: 'contributor',
		created_at: '2026-09-01T10:00:00Z',
		head_branch: 'feature',
		base_branch: 'main',
		head_repo_id: null,
		head_sha: 'head-sha',
		ci_approved_sha: null,
		auto_merge_enabled: false,
		auto_merge_strategy: 'merge',
	};
}

function emptyDiff() {
	return {
		base_branch: 'main',
		head_branch: 'feature',
		files_changed: [],
		stats: { total_additions: 0, total_deletions: 0, files_changed: 0 },
	};
}

async function openDiffTab(): Promise<void> {
	await click(button(rendered!.container, 'Changes'));
}

beforeEach(() => {
	resetTestClient();
	warn = vi.spyOn(console, 'warn').mockImplementation(() => undefined);
	setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
	routePulls.get.mockResolvedValue(pullRequest());
	routePulls.diff.mockResolvedValue(emptyDiff());
	routePulls.mergeQueue.mockResolvedValue([]);
	routeAttachments.list.mockResolvedValue([]);
	routeReviews.list.mockResolvedValue([]);
	routeReviews.comments.mockResolvedValue([]);
	routeReviews.timeline.mockResolvedValue([]);
	routeReviews.requestedReviewers.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	warn.mockRestore();
});

// The defect this file exists for (card_c84bb28a36e1). Every side request of
// this page used to end in `.catch(() => null)` / `.catch(() => [])`, so a
// git layer that refused with a 5xx reached the reader as a statement about
// the pull request itself: "No diff available", no reviews, an empty timeline.
describe('a pull request section that did not load', () => {
	it('says the diff could not be loaded instead of that there is none', async () => {
		routePulls.diff.mockRejectedValue(new Error('HTTP 500'));

		rendered = await renderComponent(PullRequestPage);
		await openDiffTab();

		expect(rendered.container.textContent).toContain(
			'The diff for this pull request could not be loaded.',
		);
		expect(rendered.container.textContent).not.toContain('No diff available.');
		expect(warn).toHaveBeenCalled();
	});

	it('offers a retry that asks for the diff again', async () => {
		routePulls.diff.mockRejectedValueOnce(new Error('HTTP 500'));

		rendered = await renderComponent(PullRequestPage);
		await openDiffTab();
		expect(rendered.container.textContent).toContain(
			'The diff for this pull request could not be loaded.',
		);

		await click(button(rendered.container, 'Retry'));

		expect(routePulls.diff).toHaveBeenCalledTimes(2);
	});

	// The regression half: a diff the server really did compute as empty must
	// keep saying so. Collapsing the two back together in the other direction
	// would be the mirror-image defect.
	it('still says there is no diff when the server answered with an empty one', async () => {
		rendered = await renderComponent(PullRequestPage);
		await openDiffTab();

		expect(rendered.container.textContent).toContain('No diff available.');
		expect(rendered.container.textContent).not.toContain(
			'The diff for this pull request could not be loaded.',
		);
	});

	it('names every list section that failed, not only the diff', async () => {
		routeReviews.list.mockRejectedValue(new Error('HTTP 503'));
		routeReviews.timeline.mockRejectedValue(new Error('HTTP 503'));

		rendered = await renderComponent(PullRequestPage);

		const banner = rendered.container.querySelector('.partial-banner');
		expect(banner?.textContent).toContain('Some parts of this page could not be loaded:');
		expect(banner?.textContent).toContain('the reviews');
		expect(banner?.textContent).toContain('the timeline');
		expect(banner?.textContent).not.toContain('the merge queue');
	});

	it('says nothing about missing sections when every request answered', async () => {
		rendered = await renderComponent(PullRequestPage);

		expect(rendered.container.querySelector('.partial-banner')).toBeNull();
		expect(rendered.container.textContent).not.toContain(
			'Some parts of this page could not be loaded:',
		);
	});
});
