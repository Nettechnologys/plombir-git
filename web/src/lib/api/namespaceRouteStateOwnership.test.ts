import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import OwnerPage from '../../routes/[owner]/+page.svelte';
import OrganizationPage from '../../routes/orgs/[name]/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { setTestPage } from '../test/app';
import { auth, orgs, repos, resetTestClient } from '../test/client';
import {
	answerConfirm,
	button,
	click,
	element,
	renderComponent,
	settle,
	type RenderedComponent,
} from '../test/render';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (reason: unknown) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (reason: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

const timestamp = '2026-08-31T12:00:00Z';

function organization(name: string, id: number, displayName: string) {
	return {
		id,
		name,
		display_name: displayName,
		description: `${displayName} description`,
		visibility: 'public',
		owner_id: 1,
		created_at: timestamp,
	};
}

function repository(id: number, name: string) {
	return {
		id,
		name,
		description: `${name} description`,
		is_private: false,
		stars_count: 0,
		updated_at: timestamp,
	};
}

const alice = { id: 1, user_id: 1, username: 'alice', role: 'owner' };
const bob = { id: 2, user_id: 2, username: 'bob', role: 'member' };
const coreTeam = { id: 5, name: 'core', description: null, permission: 'write' };

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	resetTestClient();
	auth.me.mockResolvedValue({
		id: 1,
		username: 'alice',
		email: 'alice@example.com',
		is_admin: false,
		display_name: 'Alice',
	});
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
	vi.unstubAllGlobals();
});

function memberDeleteButton(container: ParentNode, username: string): HTMLButtonElement {
	const row = Array.from(container.querySelectorAll('.grid > .section:last-child .item')).find((item) =>
		item.textContent?.includes(username),
	);
	if (!row) throw new Error(`Rendered organization page is missing member ${username}`);
	return element<HTMLButtonElement>(row, '.btn-danger');
}

describe('namespace route state ownership', () => {
	it('clears owner controls while A -> B is loading', async () => {
		const nextRepositories = deferred<{ data: ReturnType<typeof repository>[] }>();
		repos.list
			.mockResolvedValueOnce({ data: [repository(1, 'legacy-repository')] })
			.mockReturnValueOnce(nextRepositories.promise);
		orgs.get
			.mockResolvedValueOnce(organization('acme', 1, 'Acme'))
			.mockRejectedValueOnce(new Error('user namespace'));
		orgs.list.mockResolvedValue([organization('acme', 1, 'Acme')]);
		setTestPage('/acme', { owner: 'acme' });
		rendered = await renderComponent(OwnerPage);
		expect(rendered.container.textContent).toContain('legacy-repository');
		expect(rendered.container.textContent).toContain('Acme');

		setTestPage('/bob', { owner: 'bob' });
		await settle();
		expect(rendered.container.textContent).not.toContain('legacy-repository');
		expect(rendered.container.textContent).not.toContain('Acme description');
		expect(rendered.container.querySelector('.header-actions a')).toBeNull();

		nextRepositories.resolve({ data: [repository(2, 'current-repository')] });
		await settle();
		expect(rendered.container.textContent).toContain('current-repository');
	});

	it('rejects an owner response from the first A -> B -> A visit', async () => {
		const firstVisit = deferred<{ data: ReturnType<typeof repository>[] }>();
		repos.list
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce({ data: [repository(2, 'middle-repository')] })
			.mockResolvedValueOnce({ data: [repository(3, 'current-repository')] });
		orgs.get.mockRejectedValue(new Error('user namespace'));
		setTestPage('/alice', { owner: 'alice' });
		rendered = await renderComponent(OwnerPage);

		setTestPage('/bob', { owner: 'bob' });
		await settle();
		setTestPage('/alice', { owner: 'alice' });
		await settle();
		expect(rendered.container.textContent).toContain('current-repository');

		firstVisit.resolve({ data: [repository(1, 'stale-repository')] });
		await settle();
		expect(rendered.container.textContent).toContain('current-repository');
		expect(rendered.container.textContent).not.toContain('stale-repository');
	});

	it('does not let an old owner failure clear the current route loading claim', async () => {
		const firstVisit = deferred<{ data: ReturnType<typeof repository>[] }>();
		const currentVisit = deferred<{ data: ReturnType<typeof repository>[] }>();
		repos.list
			.mockReturnValueOnce(firstVisit.promise)
			.mockReturnValueOnce(currentVisit.promise);
		orgs.get.mockRejectedValue(new Error('user namespace'));
		setTestPage('/alice', { owner: 'alice' });
		rendered = await renderComponent(OwnerPage);

		setTestPage('/bob', { owner: 'bob' });
		await settle();
		firstVisit.reject(new Error('stale owner failure'));
		await settle();
		expect(rendered.container.querySelector('.error-banner')).toBeNull();
		expect(rendered.container.querySelector('.text-secondary')).not.toBeNull();

		currentVisit.resolve({ data: [repository(2, 'current-repository')] });
		await settle();
		expect(rendered.container.textContent).toContain('current-repository');
	});

	it('rejects an organization load from the first A -> B -> A visit', async () => {
		const firstVisit = deferred<ReturnType<typeof organization>>();
		orgs.get
			.mockReturnValueOnce(firstVisit.promise)
			.mockResolvedValueOnce(organization('beta', 2, 'Middle Organization'))
			.mockResolvedValueOnce(organization('acme', 3, 'Current Organization'));
		orgs.listMembers.mockResolvedValue([alice, bob]);
		orgs.listTeams.mockResolvedValue([coreTeam]);
		repos.list.mockResolvedValue({ data: [] });
		setTestPage('/orgs/acme', { name: 'acme' });
		rendered = await renderComponent(OrganizationPage);

		setTestPage('/orgs/beta', { name: 'beta' });
		await settle();
		setTestPage('/orgs/acme', { name: 'acme' });
		await settle();
		expect(rendered.container.textContent).toContain('Current Organization');

		firstVisit.resolve(organization('acme', 1, 'Stale Organization'));
		await settle();
		expect(rendered.container.textContent).toContain('Current Organization');
		expect(rendered.container.textContent).not.toContain('Stale Organization');
	});

	it('clears organization controls when A -> B reuses the component', async () => {
		orgs.get.mockImplementation(async (namespace: string) =>
			organization(namespace, namespace === 'acme' ? 1 : 2, namespace === 'acme' ? 'Acme' : 'Beta'),
		);
		orgs.listMembers.mockResolvedValue([alice, bob]);
		orgs.listTeams.mockResolvedValue([coreTeam]);
		orgs.listTeamMembers.mockResolvedValue([
			{ id: 8, user_id: 8, username: 'acme-team-member', role: 'member' },
		]);
		repos.list.mockResolvedValue({ data: [] });
		setTestPage('/orgs/acme', { name: 'acme' });
		rendered = await renderComponent(OrganizationPage);

		await click(element(rendered.container, '.header-actions .btn-secondary'));
		await click(button(rendered.container, 'View members'));
		expect(rendered.container.querySelector('.edit-organization')).not.toBeNull();
		expect(rendered.container.textContent).toContain('acme-team-member');

		setTestPage('/orgs/beta', { name: 'beta' });
		await settle();
		expect(rendered.container.textContent).toContain('Beta');
		expect(rendered.container.querySelector('.edit-organization')).toBeNull();
		expect(rendered.container.querySelector('.team-members')).toBeNull();
		expect(rendered.container.textContent).not.toContain('acme-team-member');
	});

	it('does not publish a team-member request after its organization route is gone', async () => {
		const firstTeamMembers = deferred<Array<{ id: number; user_id: number; username: string; role: string }>>();
		orgs.get.mockImplementation(async (namespace: string) =>
			organization(namespace, namespace === 'acme' ? 1 : 2, namespace === 'acme' ? 'Acme' : 'Beta'),
		);
		orgs.listMembers.mockResolvedValue([alice, bob]);
		orgs.listTeams.mockImplementation(async (namespace: string) =>
			namespace === 'acme' ? [coreTeam] : [],
		);
		orgs.listTeamMembers.mockReturnValueOnce(firstTeamMembers.promise);
		repos.list.mockResolvedValue({ data: [] });
		setTestPage('/orgs/acme', { name: 'acme' });
		rendered = await renderComponent(OrganizationPage);

		await click(button(rendered.container, 'View members'));
		setTestPage('/orgs/beta', { name: 'beta' });
		await settle();
		firstTeamMembers.resolve([
			{ id: 8, user_id: 8, username: 'stale-team-member', role: 'member' },
		]);
		await settle();

		expect(rendered.container.textContent).toContain('Beta');
		expect(rendered.container.textContent).not.toContain('stale-team-member');
		expect(rendered.container.querySelector('.team-members')).toBeNull();
	});

	it('does not let an old organization mutation release the current visit busy claim', async () => {
		const firstRemoval = deferred<void>();
		const currentRemoval = deferred<void>();
		let memberRequest = 0;
		orgs.get.mockImplementation(async (namespace: string) =>
			organization(namespace, namespace === 'acme' ? 1 : 2, namespace === 'acme' ? 'Acme' : 'Beta'),
		);
		orgs.listMembers.mockImplementation(async () => {
			memberRequest += 1;
			return memberRequest >= 4 ? [alice] : [alice, bob];
		});
		orgs.listTeams.mockResolvedValue([coreTeam]);
		orgs.removeMember
			.mockReturnValueOnce(firstRemoval.promise)
			.mockReturnValueOnce(currentRemoval.promise);
		repos.list.mockResolvedValue({ data: [] });
		setTestPage('/orgs/acme', { name: 'acme' });
		rendered = await renderComponent(OrganizationPage);

		await click(memberDeleteButton(rendered.container, 'bob'));
		await answerConfirm();
		setTestPage('/orgs/beta', { name: 'beta' });
		await settle();
		setTestPage('/orgs/acme', { name: 'acme' });
		await settle();
		await click(memberDeleteButton(rendered.container, 'bob'));
		await answerConfirm();
		const currentButton = memberDeleteButton(rendered.container, 'bob');
		expect(currentButton.disabled).toBe(true);

		firstRemoval.reject(new Error('stale removal failure'));
		await settle();
		expect(rendered.container.textContent).toContain('bob');
		expect(rendered.container.textContent).not.toContain('stale removal failure');
		expect(memberDeleteButton(rendered.container, 'bob').disabled).toBe(true);
		expect(orgs.removeMember).toHaveBeenCalledTimes(2);

		currentRemoval.resolve();
		await settle();
		expect(rendered.container.textContent).not.toContain('bob');
	});
});
