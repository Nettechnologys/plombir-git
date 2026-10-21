import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import PipelinesPage from '../../routes/[owner]/[repo]/pipelines/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import {
	artifacts,
	auth,
	connectJobLogWebSocket,
	pipelines,
	repos,
	resetTestClient,
} from '../test/client';
import { openModalFrom } from '../test/modalContract';
import { click, element, renderComponent, type RenderedComponent } from '../test/render';

// card_c8b3fa9f0ebc: a job translated from an Actions workflow runs its steps
// one by one, and the job log lists them — each with its own status and its
// own log, the whole job's log one click away.

const timestamp = '2026-10-20T00:00:00Z';

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	resetTestClient();
	auth.me.mockResolvedValue({ id: 1, username: 'alice', email: 'alice@example.com', is_admin: false });
	repos.get.mockResolvedValue({ default_branch: 'main', viewer_permission: 'read' });
	repos.starred.mockResolvedValue({ starred: false });
	repos.watchStatus.mockResolvedValue({ watch_state: 'not_watching' });
	repos.branches.mockResolvedValue([]);
	artifacts.list.mockResolvedValue([]);
	connectJobLogWebSocket.mockReturnValue({ close: vi.fn() });
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

describe('pipeline job steps', () => {
	it('lists a job’s steps and shows one step’s own log', async () => {
		const pipeline = {
			id: 1, status: 'success', commit_sha: 'abcdef1', commit_message: 'Pipeline 1',
			ref_name: 'main', started_at: timestamp, finished_at: timestamp,
		};
		const step = (number: number, name: string, status: string, conclusion: string | null) => ({
			number, name, status, conclusion, exit_code: null, has_log: true,
			started_at: timestamp, finished_at: timestamp,
		});
		const job = {
			id: 5, name: 'build/build [os=linux]', display_name: 'Build on linux', status: 'success',
			started_at: timestamp, finished_at: timestamp,
			steps: [
				step(1, 'Compile', 'success', 'success'),
				step(2, 'Flaky', 'failure', 'success'),
				step(3, 'Deploy', 'skipped', 'skipped'),
			],
		};
		pipelines.list.mockResolvedValue({ data: [pipeline], pagination: { total_pages: 1 } });
		pipelines.get.mockResolvedValue({
			pipeline,
			stages: [{ id: 2, name: 'build', status: 'success', started_at: timestamp, finished_at: timestamp, jobs: [job] }],
		});
		pipelines.jobLog.mockResolvedValue({ content: 'the whole job', offset: 0, next_offset: 13, total_length: 13 });
		pipelines.jobStepLog.mockResolvedValue({ number: 2, status: 'failure', content: 'flaky output' });
		setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PipelinesPage);
		await click(element(rendered.container, '.pipeline-item'));
		// card_1535b3af81ac: `jobs.<id>.name` is shown, the job's id beside it.
		expect(element(rendered.container, '.job-card .job-name').textContent).toBe('Build on linux');
		expect(element(rendered.container, '.job-card .job-id').textContent).toBe('build/build [os=linux]');

		const dialog = await openModalFrom(rendered.container, element<HTMLElement>(rendered.container, '.job-card'));
		expect(element(dialog, '#pipeline-log-title').textContent).toBe('Build on linux');
		const names = [...dialog.querySelectorAll('.log-step-name')].map((name) => name.textContent);
		expect(names).toEqual(['1. Compile', '2. Flaky', '3. Deploy']);
		expect(dialog.textContent).toContain('failed, continued on error');
		expect(element(dialog, '.log-content').textContent).toContain('the whole job');

		const steps = [...dialog.querySelectorAll<HTMLButtonElement>('.log-step')];
		await click(steps[2]);
		await vi.waitFor(() => expect(element(dialog, '.log-content').textContent).toContain('flaky output'));
		expect(pipelines.jobStepLog).toHaveBeenCalledWith('alice', 'demo', 1, 5, 2);
		expect(element(dialog, '.log-content').textContent).not.toContain('the whole job');
		expect(steps[2].getAttribute('aria-pressed')).toBe('true');

		await click(steps[0]);
		await vi.waitFor(() => expect(element(dialog, '.log-content').textContent).toContain('the whole job'));
	});

	it('shows no step list for a job that runs one script', async () => {
		const pipeline = {
			id: 1, status: 'success', commit_sha: 'abcdef1', commit_message: 'Pipeline 1',
			ref_name: 'main', started_at: timestamp, finished_at: timestamp,
		};
		const job = { id: 5, name: 'native', status: 'success', started_at: timestamp, finished_at: timestamp, steps: [] };
		pipelines.list.mockResolvedValue({ data: [pipeline], pagination: { total_pages: 1 } });
		pipelines.get.mockResolvedValue({
			pipeline,
			stages: [{ id: 2, name: 'build', status: 'success', started_at: timestamp, finished_at: timestamp, jobs: [job] }],
		});
		pipelines.jobLog.mockResolvedValue({ content: 'script output', offset: 0, next_offset: 13, total_length: 13 });
		setTestPage('/alice/demo/pipelines', { owner: 'alice', repo: 'demo' });
		rendered = await renderComponent(PipelinesPage);
		await click(element(rendered.container, '.pipeline-item'));
		expect(element(rendered.container, '.job-card .job-name').textContent).toBe('native');
		expect(rendered.container.querySelector('.job-card .job-id')).toBeNull();

		const dialog = await openModalFrom(rendered.container, element<HTMLElement>(rendered.container, '.job-card'));
		expect(dialog.querySelector('.log-steps')).toBeNull();
		expect(element(dialog, '.log-content').textContent).toContain('script output');
	});
});
