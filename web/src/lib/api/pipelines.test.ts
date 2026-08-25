import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	request: vi.fn(),
	qs: vi.fn(() => ''),
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
import {
	change,
	check,
	element,
	input,
	renderComponent,
	submit,
	type RenderedComponent,
} from '../test/render';
import { pipelines } from './pipelines';

let rendered: RenderedComponent | undefined;

function pipelineDetail(id: number) {
	return {
		pipeline: {
			id,
			status: 'success',
			commit_sha: 'abcdef0123456789',
			commit_message: `Pipeline ${id}`,
			ref_name: 'main',
			started_at: '2026-08-24T10:00:00Z',
			finished_at: '2026-08-24T10:01:00Z',
		},
		stages: [],
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	resetTestClient();
	setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
	routePipelines.list.mockResolvedValue({
		data: [pipelineDetail(7).pipeline],
		pagination: { total_pages: 1 },
	});
	routePipelines.get.mockImplementation(
		(_owner: string, _repo: string, id: number) => Promise.resolve(pipelineDetail(id)),
	);
	routePipelines.trigger.mockResolvedValue({ id: 8 });
	routeRepos.branches.mockResolvedValue([
		{ name: 'release', is_default: false },
		{ name: 'main', is_default: true },
	]);
	routePipelines.workflowDispatchSchema.mockResolvedValue({
		inputs: [
			{
				name: 'deploy',
				type: 'boolean',
				required: true,
				default: null,
				options: [],
				description: 'Deploy after the build',
			},
			{
				name: 'target',
				type: 'choice',
				required: true,
				default: null,
				options: ['staging', 'production'],
				description: '',
			},
			{
				name: 'replicas',
				type: 'number',
				required: false,
				default: '2',
				options: [],
				description: '',
			},
			{
				name: 'environment',
				type: 'string',
				required: false,
				default: null,
				options: [],
				description: '',
			},
		],
	});
	routeArtifacts.list.mockResolvedValue([]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
});

describe('manual pipeline trigger transport', () => {
	it('loads the selected refs dispatch schema', () => {
		base.qs.mockReturnValue('?ref=refs%2Ftags%2Fv1.2.3');

		pipelines.workflowDispatchSchema('alice', 'demo', 'refs/tags/v1.2.3');

		expect(base.qs).toHaveBeenCalledWith({ ref: 'refs/tags/v1.2.3' });
		expect(base.request).toHaveBeenCalledWith(
			'/repos/alice/demo/pipelines/workflow-dispatch?ref=refs%2Ftags%2Fv1.2.3',
		);
	});

	it('sends the selected ref and user inputs on the canonical wire fields', () => {
		pipelines.trigger('alice', 'demo', 'refs/tags/v1.2.3', {
			deploy: 'true',
			target: 'production',
		});

		expect(base.request).toHaveBeenCalledWith('/repos/alice/demo/pipelines', {
			method: 'POST',
			body: JSON.stringify({
				ref: 'refs/tags/v1.2.3',
				inputs: { deploy: 'true', target: 'production' },
			}),
		});
	});
});

describe('manual pipeline trigger production wiring', () => {
	it('renders branch and schema controls with the committed defaults', async () => {
		rendered = await renderComponent(PipelinesPage);

		expect(routeRepos.branches).toHaveBeenCalledWith('alice', 'demo');
		expect(routePipelines.workflowDispatchSchema).toHaveBeenCalledWith('alice', 'demo', 'main');
		expect(element<HTMLInputElement>(rendered.container, '#pipeline-trigger-ref').value).toBe(
			'main',
		);
		expect(element<HTMLInputElement>(rendered.container, '#pipeline-trigger-input-deploy')).toMatchObject(
			{ type: 'checkbox', checked: false },
		);
		expect(element<HTMLSelectElement>(rendered.container, '#pipeline-trigger-input-target').value).toBe(
			'staging',
		);
		expect(element<HTMLInputElement>(rendered.container, '#pipeline-trigger-input-replicas')).toMatchObject(
			{ type: 'number', value: '2' },
		);
		expect(element<HTMLInputElement>(rendered.container, '#pipeline-trigger-input-environment')).toMatchObject(
			{ type: 'text', value: '' },
		);
		expect(rendered.container.textContent).toContain('deploy *');
		expect(rendered.container.textContent).toContain('Deploy after the build');
	});

	it('submits the exact ref and rendered input values, then selects the created run', async () => {
		rendered = await renderComponent(PipelinesPage);
		await check(element(rendered.container, '#pipeline-trigger-input-deploy'), true);
		await change(
			element(rendered.container, '#pipeline-trigger-input-target'),
			'production',
		);
		await input(element(rendered.container, '#pipeline-trigger-input-replicas'), '3');
		await input(element(rendered.container, '#pipeline-trigger-input-environment'), 'prod');
		await submit(element(rendered.container, '.pipeline-trigger'));

		expect(routePipelines.trigger).toHaveBeenCalledWith('alice', 'demo', 'main', {
			deploy: 'true',
			target: 'production',
			replicas: '3',
			environment: 'prod',
		});
		expect(routePipelines.get).toHaveBeenCalledWith('alice', 'demo', 8);
		expect(rendered.container.textContent).toContain('#8');
	});

	it.each(['run_pipeline', 'starting', 'run_ref', 'run_ref_placeholder'])(
		'has a real label in both catalogs: pipeline.%s',
		(key) => {
			expect(en.pipeline).toHaveProperty(key);
			expect(zhCN.pipeline).toHaveProperty(key);
		},
	);
});
