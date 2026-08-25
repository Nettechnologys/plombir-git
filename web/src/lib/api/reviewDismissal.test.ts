import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import PullRequestPage from '../../routes/[owner]/[repo]/pulls/[number]/+page.svelte';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { setTestPage } from '../test/app';
import {
	attachments as routeAttachments,
	pulls as routePulls,
	reviews as routeReviews,
	resetTestClient,
} from '../test/client';
import { click, element, input, renderComponent, type RenderedComponent } from '../test/render';
import { reviews } from './pulls';

let rendered: RenderedComponent | undefined;

function pullRequest(overrides: Record<string, unknown> = {}) {
	return {
		id: 17,
		number: 7,
		title: 'Review protected change',
		body: '',
		state: 'open',
		is_draft: false,
		author: 'alice',
		created_at: '2026-08-24T10:00:00Z',
		head_branch: 'feature',
		base_branch: 'main',
		head_repo_id: null,
		head_sha: 'current-head',
		ci_approved_sha: null,
		auto_merge_enabled: false,
		auto_merge_strategy: 'merge',
		...overrides,
	};
}

function review(overrides: Record<string, unknown> = {}) {
	return {
		id: 42,
		state: 'approve',
		body: 'Looks good',
		dismissed_at: null,
		...overrides,
	};
}

function timelineEvent(overrides: Record<string, unknown> = {}) {
	return {
		id: 101,
		kind: 'review_approve',
		actor: { username: 'reviewer' },
		metadata: { review_id: 42 },
		body: 'Looks good',
		created_at: '2026-08-24T10:01:00Z',
		...overrides,
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
	routePulls.get.mockResolvedValue(pullRequest());
	routePulls.diff.mockResolvedValue(null);
	routePulls.mergeQueue.mockResolvedValue([]);
	routeAttachments.list.mockResolvedValue([]);
	routeReviews.list.mockResolvedValue([review()]);
	routeReviews.comments.mockResolvedValue([]);
	routeReviews.timeline.mockResolvedValue([timelineEvent()]);
	routeReviews.requestedReviewers.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('review dismissal transport', () => {
	it('posts to the dismissal endpoint of one review, with owner and repo escaped', () => {
		reviews.dismiss('alice/bob', 'de mo', 7, 42, 'stale — the branch moved on');

		expect(base.request).toHaveBeenCalledWith('/repos/alice%2Fbob/de%20mo/pulls/7/reviews/42/dismiss', {
			method: 'POST',
			body: JSON.stringify({ message: 'stale — the branch moved on' }),
		});
	});
});

describe('review dismissal production wiring', () => {
	it('requires a reason, dismisses the exact verdict, and refreshes the pull request', async () => {
		rendered = await renderComponent(PullRequestPage);
		await click(element(rendered.container, '.dismiss-review'));

		const form = element(rendered.container, '.dismiss-form');
		const confirm = element<HTMLButtonElement>(form, '.btn-secondary');
		expect(confirm.disabled).toBe(true);
		await input(element(form, 'input'), 'stale — the branch moved on');
		expect(confirm.disabled).toBe(false);
		await click(confirm);

		expect(routeReviews.dismiss).toHaveBeenCalledWith(
			'alice',
			'demo',
			7,
			42,
			'stale — the branch moved on',
		);
		expect(routePulls.get).toHaveBeenCalledTimes(2);
	});

	it('does not offer dismissal for a comment-only review event', async () => {
		routeReviews.list.mockResolvedValue([review({ id: 43, state: 'comment' })]);
		routeReviews.timeline.mockResolvedValue([
			timelineEvent({ kind: 'review_comment', metadata: { review_id: 43 } }),
		]);
		rendered = await renderComponent(PullRequestPage);

		expect(rendered.container.querySelector('.dismiss-review')).toBeNull();
	});

	it('marks a withdrawn verdict and does not offer dismissal again', async () => {
		routeReviews.list.mockResolvedValue([
			review({ dismissed_at: '2026-08-24T10:02:00Z' }),
		]);
		rendered = await renderComponent(PullRequestPage);

		expect(element(rendered.container, '.withdrawn-badge').textContent).toBe(
			en.pulls.review.withdrawn,
		);
		expect(rendered.container.querySelector('.dismiss-review')).toBeNull();
	});

	it('does not offer dismissal after the pull request is closed', async () => {
		routePulls.get.mockResolvedValue(pullRequest({ state: 'closed' }));
		rendered = await renderComponent(PullRequestPage);

		expect(rendered.container.querySelector('.dismiss-review')).toBeNull();
	});

	it.each([
		'dismiss',
		'dismiss_placeholder',
		'dismiss_confirm',
		'dismiss_cancel',
		'dismissing',
		'withdrawn',
	])('has a real label in both catalogs: pulls.review.%s', (key) => {
		expect(en.pulls.review).toHaveProperty(key);
		expect(zhCN.pulls.review).toHaveProperty(key);
	});
});
