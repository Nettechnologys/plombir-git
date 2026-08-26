import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import CiSecretsPage from '../../routes/[owner]/[repo]/settings/ci-secrets/+page.svelte';
import EnvironmentsPage from '../../routes/[owner]/[repo]/settings/environments/+page.svelte';
import TagsPage from '../../routes/[owner]/[repo]/settings/tags/+page.svelte';
import { setTestPage } from '../test/app';
import { ciEnvironments, ciSecrets, resetTestClient, tagProtections } from '../test/client';
import {
	button,
	click,
	element,
	input,
	renderComponent,
	settle,
	submit,
	type RenderedComponent,
} from '../test/render';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	const promise = new Promise<T>((resolvePromise) => {
		resolve = resolvePromise;
	});
	return { promise, resolve };
}

const timestamp = '2026-08-26T00:00:00Z';
const secret = (name: string) => ({ name, created_at: timestamp, updated_at: timestamp });
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
const tag = (id: number, pattern: string) => ({
	id,
	pattern,
	allowed_user_ids: [],
	allowed_users: [],
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

describe('repository settings collection state ownership', () => {
	it('keeps a refreshed CI-secret list and rejects a duplicate mutation when the initial list is late', async () => {
		const initial = deferred<ReturnType<typeof secret>[]>();
		const mutation = deferred<ReturnType<typeof secret>>();
		ciSecrets.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([secret('CURRENT_SECRET')]);
		ciSecrets.put.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(CiSecretsPage);

		await input(element(rendered.container, '#secret-name'), 'current_secret');
		await input(element(rendered.container, '#secret-value'), 'correct horse battery staple');
		const form = element<HTMLFormElement>(rendered.container, 'form');
		await submit(form);
		expect(button(rendered.container, 'Save secret').disabled).toBe(true);
		await submit(form);
		expect(ciSecrets.put).toHaveBeenCalledOnce();

		mutation.resolve(secret('CURRENT_SECRET'));
		await settle();
		expect(rendered.container.textContent).toContain('CURRENT_SECRET');

		initial.resolve([secret('STALE_SECRET')]);
		await settle();
		expect(rendered.container.textContent).toContain('CURRENT_SECRET');
		expect(rendered.container.textContent).not.toContain('STALE_SECRET');
	});

	it('keeps a refreshed environment list and rejects a duplicate mutation when the initial list is late', async () => {
		const initial = deferred<ReturnType<typeof environment>[]>();
		const mutation = deferred<ReturnType<typeof environment>>();
		ciEnvironments.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([environment(2, 'current-environment')]);
		ciEnvironments.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(EnvironmentsPage);

		await input(element(rendered.container, '#environment-name'), 'current-environment');
		const form = element<HTMLFormElement>(rendered.container, 'form');
		await submit(form);
		expect(button(rendered.container, 'Create environment').disabled).toBe(true);
		await submit(form);
		expect(ciEnvironments.create).toHaveBeenCalledOnce();

		mutation.resolve(environment(2, 'current-environment'));
		await settle();
		expect(rendered.container.textContent).toContain('current-environment');

		initial.resolve([environment(1, 'stale-environment')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-environment');
		expect(rendered.container.textContent).not.toContain('stale-environment');
	});

	it('keeps a refreshed tag list and rejects a duplicate mutation when the initial list is late', async () => {
		const initial = deferred<ReturnType<typeof tag>[]>();
		const mutation = deferred<ReturnType<typeof tag>>();
		tagProtections.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([tag(2, 'current-*')]);
		tagProtections.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(TagsPage);

		await input(element(rendered.container, '#tag-pattern'), 'current-*');
		const form = element<HTMLFormElement>(rendered.container, 'form');
		await submit(form);
		expect(button(rendered.container, 'Add protection').disabled).toBe(true);
		await submit(form);
		expect(tagProtections.create).toHaveBeenCalledOnce();

		mutation.resolve(tag(2, 'current-*'));
		await settle();
		expect(rendered.container.textContent).toContain('current-*');

		initial.resolve([tag(1, 'stale-*')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-*');
		expect(rendered.container.textContent).not.toContain('stale-*');
	});

	it('shares the CI-secret row claim between replace and delete', async () => {
		const existing = secret('DEPLOY_TOKEN');
		const mutation = deferred<ReturnType<typeof secret>>();
		ciSecrets.list.mockResolvedValueOnce([existing]);
		ciSecrets.put.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(CiSecretsPage);

		await input(element(rendered.container, '#secret-name'), existing.name);
		await input(element(rendered.container, '#secret-value'), 'replacement value');
		await submit(element(rendered.container, 'form'));
		await click(button(rendered.container, 'Delete'));

		expect(ciSecrets.put).toHaveBeenCalledOnce();
		expect(ciSecrets.delete).not.toHaveBeenCalled();
	});

	it('does not let a mutation from an earlier visit release the current row claim', async () => {
		const firstMutation = deferred<ReturnType<typeof secret>>();
		const currentMutation = deferred<ReturnType<typeof secret>>();
		ciSecrets.list.mockResolvedValue([]);
		ciSecrets.put
			.mockReturnValueOnce(firstMutation.promise)
			.mockReturnValueOnce(currentMutation.promise);
		rendered = await renderComponent(CiSecretsPage);

		await input(element(rendered.container, '#secret-name'), 'DEPLOY_TOKEN');
		await input(element(rendered.container, '#secret-value'), 'first visit');
		await submit(element(rendered.container, 'form'));

		setTestPage('/bob/other/settings', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/settings', { owner: 'alice', repo: 'demo' });
		await settle();
		await input(element(rendered.container, '#secret-name'), 'DEPLOY_TOKEN');
		await input(element(rendered.container, '#secret-value'), 'current visit');
		const currentForm = element<HTMLFormElement>(rendered.container, 'form');
		await submit(currentForm);
		expect(ciSecrets.put).toHaveBeenCalledTimes(2);

		firstMutation.resolve(secret('DEPLOY_TOKEN'));
		await settle();
		expect(button(rendered.container, 'Save secret').disabled).toBe(true);
		await submit(currentForm);
		expect(ciSecrets.put).toHaveBeenCalledTimes(2);

		currentMutation.resolve(secret('DEPLOY_TOKEN'));
		await settle();
		expect(button(rendered.container, 'Save secret').disabled).toBe(false);
	});

	it('shares the environment row claim between update and delete', async () => {
		const existing = environment(7, 'production');
		const mutation = deferred<ReturnType<typeof environment>>();
		ciEnvironments.list.mockResolvedValueOnce([existing]);
		ciEnvironments.update.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(EnvironmentsPage);

		await click(button(rendered.container, 'Edit'));
		await submit(element(rendered.container, 'form'));
		await click(button(rendered.container, 'Delete'));

		expect(ciEnvironments.update).toHaveBeenCalledOnce();
		expect(ciEnvironments.delete).not.toHaveBeenCalled();
	});

	it('shares the tag-protection row claim between update and delete', async () => {
		const existing = tag(7, 'release-*');
		const mutation = deferred<ReturnType<typeof tag>>();
		tagProtections.list.mockResolvedValueOnce([existing]);
		tagProtections.update.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(TagsPage);

		await click(button(rendered.container, 'Edit'));
		await submit(element(rendered.container, 'form'));
		await click(button(rendered.container, 'Delete'));

		expect(tagProtections.update).toHaveBeenCalledOnce();
		expect(tagProtections.delete).not.toHaveBeenCalled();
	});

	const routeCases = [
		{
			name: 'CI secrets',
			component: CiSecretsPage,
			list: ciSecrets.list,
			oldItems: [secret('OLD_REPO_SECRET')],
			newItems: [secret('NEW_REPO_SECRET')],
			oldText: 'OLD_REPO_SECRET',
			newText: 'NEW_REPO_SECRET',
		},
		{
			name: 'environments',
			component: EnvironmentsPage,
			list: ciEnvironments.list,
			oldItems: [environment(1, 'old-repo-environment')],
			newItems: [environment(2, 'new-repo-environment')],
			oldText: 'old-repo-environment',
			newText: 'new-repo-environment',
		},
		{
			name: 'tag protections',
			component: TagsPage,
			list: tagProtections.list,
			oldItems: [tag(1, 'old-repo-*')],
			newItems: [tag(2, 'new-repo-*')],
			oldText: 'old-repo-*',
			newText: 'new-repo-*',
		},
	];

	for (const routeCase of routeCases) {
		it(`${routeCase.name} rejects the previous repository's response`, async () => {
			const oldRoute = deferred<any[]>();
			routeCase.list
				.mockReturnValueOnce(oldRoute.promise)
				.mockResolvedValueOnce(routeCase.newItems);
			rendered = await renderComponent(routeCase.component);

			setTestPage('/bob/current/settings', { owner: 'bob', repo: 'current' });
			await settle();
			expect(routeCase.list).toHaveBeenLastCalledWith('bob', 'current');
			expect(rendered.container.textContent).toContain(routeCase.newText);

			oldRoute.resolve(routeCase.oldItems);
			await settle();
			expect(rendered.container.textContent).toContain(routeCase.newText);
			expect(rendered.container.textContent).not.toContain(routeCase.oldText);
		});
	}
});
