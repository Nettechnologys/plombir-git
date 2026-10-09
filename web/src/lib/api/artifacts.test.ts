import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import PipelinesPage from '../../routes/[owner]/[repo]/pipelines/+page.svelte';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import { setTestPage } from '../test/app';
import {
	artifacts as routeArtifacts,
	pipelines as routePipelines,
	repos as routeRepos,
	resetTestClient,
} from '../test/client';
import { click, element, renderComponent, type RenderedComponent } from '../test/render';
import { artifacts } from './artifacts';

let rendered: RenderedComponent | undefined;

function pipelineDetail() {
	return {
		pipeline: {
			id: 7,
			status: 'success',
			commit_sha: 'abcdef0123456789',
			commit_message: 'Publish artifact',
			ref_name: 'main',
			started_at: '2026-08-24T10:00:00Z',
			finished_at: '2026-08-24T10:01:00Z',
		},
		stages: [],
	};
}

function artifact() {
	return {
		id: 11,
		pipeline_id: 7,
		job_id: 3,
		name: 'build-report',
		size: 1024,
		content_type: 'application/zip',
		created_at: '2026-08-24T10:01:00Z',
		expires_at: null,
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	// The write controls under test are offered to writers only (card_270a0a77fd79).
	routeRepos.get.mockResolvedValue({ default_branch: 'main', viewer_permission: 'admin' });
	setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
	routePipelines.list.mockResolvedValue({
		data: [pipelineDetail().pipeline],
		pagination: { total_pages: 1 },
	});
	routePipelines.get.mockResolvedValue(pipelineDetail());
	routePipelines.workflowDispatchSchema.mockResolvedValue({ inputs: [] });
	routeRepos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
	routeArtifacts.list.mockResolvedValue([artifact()]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

describe('CI artifact client transport', () => {
	it('lists the artifacts of one pipeline, with owner and repo escaped', () => {
		artifacts.list('alice/bob', 'de mo', 7);

		expect(base.request).toHaveBeenCalledWith('/repos/alice%2Fbob/de%20mo/pipelines/7/artifacts');
	});

	it('downloads through the authenticated file path, not a bare link', () => {
		artifacts.download(11, 'build-report');

		expect(base.downloadApiFile).toHaveBeenCalledWith('/artifacts/11/download', 'build-report');
	});

	it('falls back to a filename rather than saving an unnamed blob', () => {
		artifacts.download(12, '');

		expect(base.downloadApiFile).toHaveBeenCalledWith('/artifacts/12/download', 'artifact');
	});

	it('deletes one artifact by id', () => {
		artifacts.remove(13);

		expect(base.request).toHaveBeenCalledWith('/artifacts/13', { method: 'DELETE' });
	});
});

describe('CI artifact production wiring', () => {
	it('renders authenticated download and confirmed destructive deletion', async () => {
		vi.stubGlobal('confirm', vi.fn(() => true));
		rendered = await renderComponent(PipelinesPage);

		expect(routeArtifacts.list).toHaveBeenCalledWith('alice', 'demo', 7);
		expect(rendered.container.textContent).toContain('build-report');
		await click(element(rendered.container, '.artifact-download'));
		expect(routeArtifacts.download).toHaveBeenCalledWith(11, 'build-report');

		await click(element(rendered.container, '.artifact-delete'));
		expect(globalThis.confirm).toHaveBeenCalledWith(
			expect.stringContaining('build-report'),
		);
		expect(routeArtifacts.remove).toHaveBeenCalledWith(11);
		expect(rendered.container.querySelector('.artifact-row')).toBeNull();
	});

	it('keeps an empty list a normal state and a failure a visible one', async () => {
		routeArtifacts.list.mockResolvedValueOnce([]);
		rendered = await renderComponent(PipelinesPage);
		expect(rendered.container.querySelector('.artifacts-error')).toBeNull();
		expect(rendered.container.textContent).toContain(en.pipeline.artifacts_empty);
		await rendered.destroy();

		rendered = undefined;
		resetTestClient();
		routePipelines.list.mockResolvedValue({
			data: [pipelineDetail().pipeline],
			pagination: { total_pages: 1 },
		});
		routePipelines.get.mockResolvedValue(pipelineDetail());
		routePipelines.workflowDispatchSchema.mockResolvedValue({ inputs: [] });
		routeRepos.branches.mockResolvedValue([{ name: 'main', is_default: true }]);
		routeArtifacts.list.mockRejectedValue(new Error('storage unavailable'));
		rendered = await renderComponent(PipelinesPage);
		expect(element(rendered.container, '.artifacts-error').textContent).toContain(
			'storage unavailable',
		);
	});

	it.each([
		'artifacts',
		'artifacts_empty',
		'artifacts_hint',
		'artifacts_field',
		'artifacts_load_failed',
		'artifact_download',
		'artifact_downloading',
		'artifact_download_failed',
		'artifact_expires',
		'artifact_expires_never',
		'artifact_delete',
		'artifact_deleting',
		'artifact_delete_confirm',
		'artifact_delete_failed',
	])('has a real label in both catalogs: pipeline.%s', (key) => {
		expect(en.pipeline).toHaveProperty(key);
		expect(zhCN.pipeline).toHaveProperty(key);
	});
});
