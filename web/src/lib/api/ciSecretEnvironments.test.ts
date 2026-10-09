import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import CiSecretsPage from '../../routes/[owner]/[repo]/settings/ci-secrets/+page.svelte';
import { setTestPage } from '../test/app';
import { ciEnvironments, ciSecrets, resetTestClient } from '../test/client';
import {
	change,
	element,
	input,
	renderComponent,
	settle,
	submit,
	type RenderedComponent,
} from '../test/render';

const timestamp = '2026-08-26T00:00:00Z';
const secret = (name: string, environment: string | null) => ({
	name,
	environment,
	created_at: timestamp,
	updated_at: timestamp,
});
const environment = (id: number, name: string) => ({
	id,
	name,
	protected: true,
	required_approvals: 1,
	allowed_approver_ids: [],
	allowed_approvers: [],
	created_at: timestamp,
	updated_at: timestamp,
});

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings', { owner: 'alice', repo: 'demo' });
	vi.stubGlobal('confirm', vi.fn(() => true));
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

describe('CI-secret environment scoping', () => {
	it('offers the repository environments and stores the selected scope', async () => {
		ciSecrets.list
			.mockResolvedValueOnce([])
			.mockResolvedValueOnce([secret('DEPLOY_TOKEN', 'production')]);
		ciEnvironments.list.mockResolvedValue([environment(10, 'production')]);
		ciSecrets.put.mockResolvedValue(secret('DEPLOY_TOKEN', 'production'));
		rendered = await renderComponent(CiSecretsPage);
		await settle();

		const scope = element<HTMLSelectElement>(rendered.container, '#secret-environment');
		expect(scope.textContent).toContain('Repository-wide');
		expect(scope.textContent).toContain('production');

		await input(element(rendered.container, '#secret-name'), 'deploy_token');
		await input(element(rendered.container, '#secret-value'), 'correct horse battery staple');
		await change(scope, 'production');
		await submit(element<HTMLFormElement>(rendered.container, 'form'));
		await settle();

		expect(ciSecrets.put).toHaveBeenCalledWith(
			'alice',
			'demo',
			'DEPLOY_TOKEN',
			'correct horse battery staple',
			'production',
		);
		expect(rendered.container.textContent).toContain('Environment: production');
	});

	it('stores no environment when the repository-wide scope stays selected', async () => {
		ciSecrets.list.mockResolvedValue([]);
		ciEnvironments.list.mockResolvedValue([environment(10, 'production')]);
		ciSecrets.put.mockResolvedValue(secret('REPO_TOKEN', null));
		rendered = await renderComponent(CiSecretsPage);
		await settle();

		await input(element(rendered.container, '#secret-name'), 'repo_token');
		await input(element(rendered.container, '#secret-value'), 'correct horse battery staple');
		await submit(element<HTMLFormElement>(rendered.container, 'form'));
		await settle();

		expect(ciSecrets.put).toHaveBeenCalledWith(
			'alice',
			'demo',
			'REPO_TOKEN',
			'correct horse battery staple',
			null,
		);
	});

	it('deletes the row of the scope it belongs to', async () => {
		ciSecrets.list.mockResolvedValue([
			secret('DEPLOY_TOKEN', 'production'),
			secret('DEPLOY_TOKEN', null),
		]);
		ciEnvironments.list.mockResolvedValue([environment(10, 'production')]);
		rendered = await renderComponent(CiSecretsPage);
		await settle();

		const scopedRow = element(rendered.container, '.list article');
		const scopedDelete = element<HTMLButtonElement>(scopedRow, 'button');
		scopedDelete.click();
		await settle();

		expect(ciSecrets.delete).toHaveBeenCalledWith(
			'alice',
			'demo',
			'DEPLOY_TOKEN',
			'production',
		);
		expect(vi.mocked(globalThis.confirm)).toHaveBeenCalledWith(
			'Delete DEPLOY_TOKEN from the production environment?',
		);
	});
});
