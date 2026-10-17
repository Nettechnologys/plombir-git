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
import { setTestPage } from '../test/app';
import {
	attachments as routeAttachments,
	pulls as routePulls,
	reviews as routeReviews,
	resetTestClient,
	repos,
} from '../test/client';
import { element, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const DESCRIPTION = [
	'Closes #2.',
	'',
	'**What changed**',
	'- `slugify(..., transliterate=True)` maps Cyrillic to Latin',
	'- Opt-in: the default output is unchanged',
	'',
	'<img src="x" onerror="alert(1)">',
].join('\n');

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	repos.get.mockResolvedValue({ default_branch: 'main', viewer_permission: 'read' });
	setTestPage('/alice/demo/pulls/7', { owner: 'alice', repo: 'demo', number: '7' });
	routePulls.get.mockResolvedValue({
		id: 17,
		number: 7,
		title: 'Transliterate before slugifying',
		body: DESCRIPTION,
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
	});
	routePulls.diff.mockResolvedValue(null);
	routePulls.mergeQueue.mockResolvedValue([]);
	routeAttachments.list.mockResolvedValue([]);
	routeReviews.list.mockResolvedValue([]);
	routeReviews.comments.mockResolvedValue([]);
	routeReviews.timeline.mockResolvedValue([
		{
			id: 101,
			kind: 'review_comment',
			actor: { username: 'reviewer' },
			metadata: {},
			body: 'One nit: map `ё` to **yo**.',
			created_at: '2026-08-24T10:01:00Z',
		},
	]);
	routeReviews.requestedReviewers.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('pull request Markdown', () => {
	it('renders the description as Markdown, like an issue body', async () => {
		rendered = await renderComponent(PullRequestPage);
		const body = element(rendered.container, '.pr-body .comment-body');

		expect(body.querySelector('strong')?.textContent).toBe('What changed');
		expect(body.querySelectorAll('li')).toHaveLength(2);
		expect(body.querySelector('li code')?.textContent).toBe('slugify(..., transliterate=True)');
		expect(body.textContent).not.toContain('**');
	});

	it('sanitizes the rendered description', async () => {
		rendered = await renderComponent(PullRequestPage);
		const body = element(rendered.container, '.pr-body .comment-body');

		expect(body.querySelector('[onerror]')).toBeNull();
		expect(body.querySelector('script')).toBeNull();
	});

	it('renders a review event body as Markdown', async () => {
		rendered = await renderComponent(PullRequestPage);
		const event = element(rendered.container, '.timeline-body');

		expect(event.querySelector('code')?.textContent).toBe('ё');
		expect(event.querySelector('strong')?.textContent).toBe('yo');
	});
});
