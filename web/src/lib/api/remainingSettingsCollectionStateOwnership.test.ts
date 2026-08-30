import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import BranchesPage from '../../routes/[owner]/[repo]/settings/branches/+page.svelte';
import CollaboratorsPage from '../../routes/[owner]/[repo]/settings/collaborators/+page.svelte';
import DeployKeysPage from '../../routes/[owner]/[repo]/settings/deploy-keys/+page.svelte';
import LabelsPage from '../../routes/[owner]/[repo]/settings/labels/+page.svelte';
import WebhooksPage from '../../routes/[owner]/[repo]/settings/webhooks/+page.svelte';
import { setTestPage } from '../test/app';
import {
	branchProtections,
	collaborators,
	deployKeys,
	labels,
	resetTestClient,
	webhooks,
} from '../test/client';
import {
	change,
	check,
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

const timestamp = '2026-08-30T00:00:00Z';
const webhook = (id: number, url: string, active = true) => ({
	id,
	repo_id: 1,
	url,
	content_type: 'json',
	has_secret: false,
	active,
	events: 'push',
	created_at: timestamp,
	updated_at: timestamp,
});
const label = (id: number, name: string) => ({
	id,
	name,
	color: '#ff0000',
	description: `${name} description`,
});
const deployKey = (id: number, title: string) => ({
	id,
	title,
	public_key: `ssh-ed25519 ${title}`,
	fingerprint: `SHA256:${title}`,
	read_only: true,
	created_by_id: 1,
	created_at: timestamp,
	last_used_at: null,
});
const collaborator = (id: number, username: string, permission = 'read') => ({
	id,
	repo_id: 1,
	user_id: id + 100,
	username,
	display_name: null,
	permission,
	created_at: timestamp,
});
const branchRule = (id: number, branchName: string) => ({
	id,
	repo_id: 1,
	branch_name: branchName,
	require_pr: true,
	require_status_check: false,
	required_status_checks: null,
	require_approval: true,
	required_approvals: 1,
	allow_force_push: false,
	require_signed_commits: false,
	allowed_push_user_ids: null,
	allowed_push_users: [],
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

describe('remaining repository settings collection state ownership', () => {
	it('keeps the post-create webhook list when the initial list finishes last', async () => {
		const initial = deferred<ReturnType<typeof webhook>[]>();
		const mutation = deferred<ReturnType<typeof webhook>>();
		webhooks.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([webhook(2, 'https://current.example/hook')]);
		webhooks.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(WebhooksPage);

		await input(element(rendered.container, '#webhook-url'), 'https://current.example/hook');
		const form = element<HTMLFormElement>(rendered.container, 'form.webhook-form');
		await submit(form);
		await submit(form);
		expect(webhooks.create).toHaveBeenCalledOnce();

		mutation.resolve(webhook(2, 'https://current.example/hook'));
		await settle();
		expect(rendered.container.textContent).toContain('https://current.example/hook');

		initial.resolve([webhook(1, 'https://stale.example/hook')]);
		await settle();
		expect(rendered.container.textContent).toContain('https://current.example/hook');
		expect(rendered.container.textContent).not.toContain('https://stale.example/hook');
	});

	it('keeps the post-create label list when the initial list finishes last', async () => {
		const initial = deferred<ReturnType<typeof label>[]>();
		const mutation = deferred<ReturnType<typeof label>>();
		labels.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([label(2, 'current-label')]);
		labels.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(LabelsPage);

		await click(element(rendered.container, '.page-header .btn-primary'));
		await input(element(rendered.container, '#label-name'), 'current-label');
		const save = element<HTMLButtonElement>(rendered.container, '.form-modal .btn-primary');
		await click(save);
		await click(save);
		expect(labels.create).toHaveBeenCalledOnce();

		mutation.resolve(label(2, 'current-label'));
		await settle();
		expect(rendered.container.textContent).toContain('current-label');

		initial.resolve([label(1, 'stale-label')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-label');
		expect(rendered.container.textContent).not.toContain('stale-label');
	});

	it('keeps the post-create deploy-key list when the initial list finishes last', async () => {
		const initial = deferred<ReturnType<typeof deployKey>[]>();
		const mutation = deferred<ReturnType<typeof deployKey>>();
		deployKeys.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([deployKey(2, 'current-key')]);
		deployKeys.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(DeployKeysPage);

		await input(element(rendered.container, '#deploy-key-title'), 'current-key');
		await input(element(rendered.container, '#deploy-public-key'), 'ssh-ed25519 current-key');
		const form = element<HTMLFormElement>(rendered.container, 'form');
		await submit(form);
		await submit(form);
		expect(deployKeys.create).toHaveBeenCalledOnce();

		mutation.resolve(deployKey(2, 'current-key'));
		await settle();
		expect(rendered.container.textContent).toContain('current-key');

		initial.resolve([deployKey(1, 'stale-key')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-key');
		expect(rendered.container.textContent).not.toContain('stale-key');
	});

	it('keeps the post-add collaborator list when the initial list finishes last', async () => {
		const initial = deferred<ReturnType<typeof collaborator>[]>();
		const mutation = deferred<ReturnType<typeof collaborator>>();
		collaborators.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([collaborator(2, 'current-user')]);
		collaborators.add.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(CollaboratorsPage);

		await input(element(rendered.container, '#collaborator-user'), 'current-user');
		const form = element<HTMLFormElement>(rendered.container, 'form.add-form');
		await submit(form);
		await submit(form);
		expect(collaborators.add).toHaveBeenCalledOnce();

		mutation.resolve(collaborator(2, 'current-user'));
		await settle();
		expect(rendered.container.textContent).toContain('current-user');

		initial.resolve([collaborator(1, 'stale-user')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-user');
		expect(rendered.container.textContent).not.toContain('stale-user');
	});

	it('keeps the post-create branch-rule list when the initial list finishes last', async () => {
		const initial = deferred<ReturnType<typeof branchRule>[]>();
		const mutation = deferred<ReturnType<typeof branchRule>>();
		branchProtections.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([branchRule(2, 'current-branch')]);
		branchProtections.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(BranchesPage);

		await input(element(rendered.container, '#protected-branch'), 'current-branch');
		const form = element<HTMLFormElement>(rendered.container, 'form.rule-form');
		await submit(form);
		await submit(form);
		expect(branchProtections.create).toHaveBeenCalledOnce();

		mutation.resolve(branchRule(2, 'current-branch'));
		await settle();
		expect(rendered.container.textContent).toContain('current-branch');

		initial.resolve([branchRule(1, 'stale-branch')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-branch');
		expect(rendered.container.textContent).not.toContain('stale-branch');
	});

	it('shares a webhook row claim between active update and delete', async () => {
		const existing = webhook(7, 'https://row.example/hook');
		const mutation = deferred<ReturnType<typeof webhook>>();
		webhooks.list.mockResolvedValueOnce([existing]);
		webhooks.update.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(WebhooksPage);

		await check(element(rendered.container, '.hook-actions input[type="checkbox"]'), false);
		await click(element(rendered.container, '.hook-actions .btn-danger'));

		expect(webhooks.update).toHaveBeenCalledOnce();
		expect(webhooks.remove).not.toHaveBeenCalled();
	});

	it('shares a label row claim between update and delete', async () => {
		const existing = label(7, 'row-label');
		const mutation = deferred<ReturnType<typeof label>>();
		labels.list.mockResolvedValueOnce([existing]);
		labels.update.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(LabelsPage);

		await click(element(rendered.container, 'button[title="Edit"]'));
		await click(element(rendered.container, 'button[title="Delete"]'));
		await click(element(rendered.container, '.form-modal .btn-primary'));
		await click(element(rendered.container, '.form-modal .btn-danger'));

		expect(labels.update).toHaveBeenCalledOnce();
		expect(labels.delete).not.toHaveBeenCalled();
	});

	it('shares a collaborator row claim between permission update and remove', async () => {
		const existing = collaborator(7, 'row-user');
		const mutation = deferred<ReturnType<typeof collaborator>>();
		collaborators.list.mockResolvedValueOnce([existing]);
		collaborators.updatePermission.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(CollaboratorsPage);

		await change(element(rendered.container, 'tbody select'), 'write');
		await click(element(rendered.container, 'tbody .btn-outline'));
		await click(element(rendered.container, 'tbody .btn-danger'));

		expect(collaborators.updatePermission).toHaveBeenCalledOnce();
		expect(collaborators.remove).not.toHaveBeenCalled();
	});

	it('shares a branch-rule row claim between update and delete', async () => {
		const existing = branchRule(7, 'row-branch');
		const mutation = deferred<ReturnType<typeof branchRule>>();
		branchProtections.list.mockResolvedValueOnce([existing]);
		branchProtections.update.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(BranchesPage);

		await click(element(rendered.container, 'tbody .btn-outline'));
		await submit(element(rendered.container, 'form.rule-form'));
		await click(element(rendered.container, 'tbody .btn-danger'));

		expect(branchProtections.update).toHaveBeenCalledOnce();
		expect(branchProtections.remove).not.toHaveBeenCalled();
	});

	const routeCases = [
		{
			name: 'webhooks',
			component: WebhooksPage,
			list: webhooks.list,
			oldItems: [webhook(1, 'https://old-visit.example/hook')],
			middleItems: [webhook(2, 'https://middle-repo.example/hook')],
			currentItems: [webhook(3, 'https://current-visit.example/hook')],
			oldText: 'https://old-visit.example/hook',
			currentText: 'https://current-visit.example/hook',
		},
		{
			name: 'labels',
			component: LabelsPage,
			list: labels.list,
			oldItems: [label(1, 'old-visit-label')],
			middleItems: [label(2, 'middle-repo-label')],
			currentItems: [label(3, 'current-visit-label')],
			oldText: 'old-visit-label',
			currentText: 'current-visit-label',
		},
		{
			name: 'deploy keys',
			component: DeployKeysPage,
			list: deployKeys.list,
			oldItems: [deployKey(1, 'old-visit-key')],
			middleItems: [deployKey(2, 'middle-repo-key')],
			currentItems: [deployKey(3, 'current-visit-key')],
			oldText: 'old-visit-key',
			currentText: 'current-visit-key',
		},
		{
			name: 'collaborators',
			component: CollaboratorsPage,
			list: collaborators.list,
			oldItems: [collaborator(1, 'old-visit-user')],
			middleItems: [collaborator(2, 'middle-repo-user')],
			currentItems: [collaborator(3, 'current-visit-user')],
			oldText: 'old-visit-user',
			currentText: 'current-visit-user',
		},
		{
			name: 'branch rules',
			component: BranchesPage,
			list: branchProtections.list,
			oldItems: [branchRule(1, 'old-visit-branch')],
			middleItems: [branchRule(2, 'middle-repo-branch')],
			currentItems: [branchRule(3, 'current-visit-branch')],
			oldText: 'old-visit-branch',
			currentText: 'current-visit-branch',
		},
	];

	for (const routeCase of routeCases) {
		it(`${routeCase.name} rejects a response from the previous visit after A -> B -> A`, async () => {
			const oldVisit = deferred<any[]>();
			routeCase.list
				.mockReturnValueOnce(oldVisit.promise)
				.mockResolvedValueOnce(routeCase.middleItems)
				.mockResolvedValueOnce(routeCase.currentItems);
			rendered = await renderComponent(routeCase.component);

			setTestPage('/bob/other/settings', { owner: 'bob', repo: 'other' });
			await settle();
			setTestPage('/alice/demo/settings', { owner: 'alice', repo: 'demo' });
			await settle();
			expect(routeCase.list).toHaveBeenLastCalledWith('alice', 'demo');
			expect(rendered.container.textContent).toContain(routeCase.currentText);

			oldVisit.resolve(routeCase.oldItems);
			await settle();
			expect(rendered.container.textContent).toContain(routeCase.currentText);
			expect(rendered.container.textContent).not.toContain(routeCase.oldText);
		});
	}

	it('does not let a webhook mutation from an old visit release the current row claim', async () => {
		const existing = webhook(7, 'https://revisited.example/hook');
		const firstMutation = deferred<ReturnType<typeof webhook>>();
		const currentMutation = deferred<ReturnType<typeof webhook>>();
		webhooks.list.mockResolvedValue([existing]);
		webhooks.update
			.mockReturnValueOnce(firstMutation.promise)
			.mockReturnValueOnce(currentMutation.promise);
		rendered = await renderComponent(WebhooksPage);

		await check(element(rendered.container, '.hook-actions input[type="checkbox"]'), false);
		setTestPage('/bob/other/settings', { owner: 'bob', repo: 'other' });
		await settle();
		setTestPage('/alice/demo/settings', { owner: 'alice', repo: 'demo' });
		await settle();

		let currentToggle = element<HTMLInputElement>(
			rendered.container,
			'.hook-actions input[type="checkbox"]',
		);
		await check(currentToggle, false);
		expect(webhooks.update).toHaveBeenCalledTimes(2);

		firstMutation.resolve(webhook(7, 'https://revisited.example/hook', false));
		await settle();
		currentToggle = element(rendered.container, '.hook-actions input[type="checkbox"]');
		expect(currentToggle.disabled).toBe(true);
		await check(currentToggle, false);
		expect(webhooks.update).toHaveBeenCalledTimes(2);

		currentMutation.resolve(webhook(7, 'https://revisited.example/hook', false));
		await settle();
		currentToggle = element(rendered.container, '.hook-actions input[type="checkbox"]');
		expect(currentToggle.disabled).toBe(false);
	});
});
