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
	repos,
} from '../test/client';
import { click, element, renderComponent, type RenderedComponent } from '../test/render';
import { pulls } from './pulls';

let rendered: RenderedComponent | undefined;

function pullRequest(overrides: Record<string, unknown> = {}) {
	return {
		id: 17,
		number: 7,
		title: 'Run fork changes',
		body: '',
		state: 'open',
		is_draft: false,
		author: 'contributor',
		created_at: '2026-08-24T10:00:00Z',
		head_branch: 'feature',
		base_branch: 'main',
		head_repo_id: 91,
		head_sha: 'new-head',
		ci_approved_sha: null,
		auto_merge_enabled: false,
		auto_merge_strategy: 'merge',
		...overrides,
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	// The write controls under test are offered to writers only (card_270a0a77fd79).
	repos.get.mockResolvedValue({ default_branch: 'main', viewer_permission: 'admin' });
	setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
	routePulls.get.mockResolvedValue(pullRequest());
	routePulls.diff.mockResolvedValue(null);
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
});

describe('fork CI approval transport', () => {
	it('posts to the maintainer endpoint, with owner and repo escaped', () => {
		pulls.approveCi('alice/bob', 'de mo', 7);

		expect(base.request).toHaveBeenCalledWith('/repos/alice%2Fbob/de%20mo/pulls/7/ci-approval', {
			method: 'POST',
		});
	});
});

describe('fork CI approval production wiring', () => {
	it('explains the secret boundary, approves the current fork head, and removes the stale banner', async () => {
		routePulls.get
			.mockResolvedValueOnce(pullRequest())
			.mockResolvedValueOnce(pullRequest({ ci_approved_sha: 'new-head' }));
		rendered = await renderComponent(PullRequestPage);

		const banner = element(rendered.container, '.ci-held');
		expect(banner.textContent).toContain(en.pulls.fork_ci.held);
		expect(banner.textContent).toContain(en.pulls.fork_ci.explanation);
		expect(en.pulls.fork_ci.explanation).toMatch(/secret/i);
		await click(element(banner, '.ci-approve'));

		expect(routePulls.approveCi).toHaveBeenCalledWith('alice', 'demo', 7);
		expect(routePulls.get).toHaveBeenCalledTimes(2);
		expect(rendered.container.querySelector('.ci-held')).toBeNull();
	});

	it('holds CI again when the fork head moves past the approved commit', async () => {
		routePulls.get.mockResolvedValue(
			pullRequest({ head_sha: 'second-head', ci_approved_sha: 'first-head' }),
		);
		rendered = await renderComponent(PullRequestPage);

		expect(rendered.container.querySelector('.ci-held')).not.toBeNull();
	});

	it.each([
		['the current fork head is approved', { ci_approved_sha: 'new-head' }],
		['the pull request is closed', { state: 'closed', ci_approved_sha: null }],
		['the pull request uses the base repository', { head_repo_id: null, ci_approved_sha: null }],
	])('does not offer approval when %s', async (_case, overrides) => {
		routePulls.get.mockResolvedValue(pullRequest(overrides));
		rendered = await renderComponent(PullRequestPage);

		expect(rendered.container.querySelector('.ci-held')).toBeNull();
	});

	it.each(['held', 'explanation', 'approve', 'approving'])(
		'has a real label in both catalogs: pulls.fork_ci.%s',
		(key) => {
			expect(en.pulls.fork_ci).toHaveProperty(key);
			expect(zhCN.pulls.fork_ci).toHaveProperty(key);
		},
	);
});
