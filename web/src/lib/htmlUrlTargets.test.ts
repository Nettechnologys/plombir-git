import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./api/_base.svelte', () => base);

import PipelinesPage from '../routes/[owner]/[repo]/pipelines/+page.svelte';
import ReleasesPage from '../routes/[owner]/[repo]/releases/+page.svelte';
import { linkedId } from './htmlUrlTargets';
import { setTestPage } from './test/app';
import {
	artifacts as routeArtifacts,
	instance,
	pipelines as routePipelines,
	releases as routeReleases,
	repos as routeRepos,
	resetTestClient,
} from './test/client';
import { renderComponent, type RenderedComponent } from './test/render';

// card_dca6f5ecbc87: the API hands out `…/pipelines?pipeline=<id>` and
// `…/releases?release=<id>` as `html_url`. These pages have to open the
// resource the link names, not the newest one.

let rendered: RenderedComponent | undefined;

function pipeline(id: number) {
	return {
		id,
		status: 'success',
		commit_sha: 'abcdef0123456789',
		ref_name: 'main',
		started_at: '2026-08-24T10:00:00Z',
		finished_at: '2026-08-24T10:01:00Z',
	};
}

function release(id: number, tag: string) {
	return {
		id,
		tag_name: tag,
		title: `Release ${tag}`,
		body: '',
		is_prerelease: false,
		is_draft: false,
		created_at: '2026-08-15T12:00:00Z',
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	routeRepos.get.mockResolvedValue({ default_branch: 'main', viewer_permission: 'read' });
	routeRepos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
	routePipelines.list.mockResolvedValue({ data: [pipeline(7)], pagination: { total_pages: 1 } });
	routePipelines.get.mockImplementation((_owner: string, _repo: string, id: number) =>
		Promise.resolve({ pipeline: pipeline(id), stages: [] }),
	);
	routePipelines.workflowDispatchSchema.mockResolvedValue({ inputs: [] });
	routeArtifacts.list.mockResolvedValue([]);
	instance.get.mockResolvedValue({ attestation_enabled: false });
	routeReleases.list.mockResolvedValue({ data: [release(9, 'v2.0.0')], pagination: { total_pages: 3 } });
	routeReleases.get.mockImplementation((_owner: string, _repo: string, id: number) =>
		Promise.resolve(release(id, 'v0.1.0')),
	);
	routeReleases.listAssets.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('linkedId', () => {
	it('accepts a positive integer and nothing else', () => {
		expect(linkedId('42')).toBe(42);
		for (const raw of [null, '', '0', '-1', '1.5', '1e3', ' 4', '4 ', '0x10', 'abc', '99999999999999999999']) {
			expect(linkedId(raw), String(raw)).toBeNull();
		}
	});
});

describe('pipeline links', () => {
	it('opens the pipeline `?pipeline=` names, even when it is not on the first page', async () => {
		setTestPage('/alice/demo/pipelines?pipeline=42', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PipelinesPage);
		expect(routePipelines.get).toHaveBeenCalledWith('alice', 'demo', 42);
		expect(routePipelines.get).not.toHaveBeenCalledWith('alice', 'demo', 7);
	});

	it('opens the newest pipeline without one', async () => {
		setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PipelinesPage);
		expect(routePipelines.get).toHaveBeenCalledWith('alice', 'demo', 7);
	});
});

describe('release links', () => {
	it('shows the release `?release=` names on its own', async () => {
		setTestPage('/alice/demo/releases?release=5', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(ReleasesPage);
		expect(routeReleases.get).toHaveBeenCalledWith('alice', 'demo', 5);
		expect(routeReleases.list).not.toHaveBeenCalled();
		expect(rendered.container.textContent).toContain('v0.1.0');
		expect(rendered.container.textContent).not.toContain('v2.0.0');
	});

	it('lists the releases without one', async () => {
		setTestPage('/alice/demo/releases', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(ReleasesPage);
		expect(routeReleases.list).toHaveBeenCalledWith('alice', 'demo', 1, 20);
		expect(routeReleases.get).not.toHaveBeenCalled();
		expect(rendered.container.textContent).toContain('v2.0.0');
	});
});
