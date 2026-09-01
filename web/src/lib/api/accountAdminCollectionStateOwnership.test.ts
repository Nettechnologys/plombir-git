import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/auth.svelte', () => ({
	getUser: () => ({ id: 999, username: 'admin' }),
	isAdmin: () => true,
	isAuthReady: () => true,
	isLoggedIn: () => true,
}));

import AdminOrgsPage from '../../routes/admin/orgs/+page.svelte';
import AdminRunnersPage from '../../routes/admin/runners/+page.svelte';
import AdminUsersPage from '../../routes/admin/users/+page.svelte';
import OrganizationsPage from '../../routes/orgs/+page.svelte';
import SecurityPage from '../../routes/settings/security/+page.svelte';
import SshKeysPage from '../../routes/settings/ssh-keys/+page.svelte';
import TokensPage from '../../routes/settings/tokens/+page.svelte';
import { setTestPage } from '../test/app';
import {
	admin,
	auth,
	mfa,
	orgs,
	passkeys,
	resetTestClient,
	runners,
	sshKeys,
	tokens,
} from '../test/client';
import {
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
const pagination = (page = 1, totalPages = 1) => ({
	page,
	per_page: 20,
	total: totalPages,
	total_pages: totalPages,
});
const token = (id: number, name: string) => ({
	id,
	name,
	scopes: 'repo',
	created_at: timestamp,
	last_used_at: null,
	expires_at: null,
});
const sshKey = (id: number, title: string) => ({
	id,
	title,
	public_key: `ssh-ed25519 ${title}`,
	fingerprint: `SHA256:${title}`,
	created_at: timestamp,
	last_used_at: null,
});
const passkey = (id: number, name: string) => ({
	id,
	name,
	created_at: timestamp,
	last_used_at: null,
});
const ssoLink = (slug: string, name: string) => ({
	slug,
	name,
	provider_username: `${slug}-user`,
	email: `${slug}@example.test`,
	linked_at: timestamp,
	provider_enabled: true,
});
const adminUser = (id: number, username: string, locked = false) => ({
	id,
	username,
	email: `${username}@example.test`,
	display_name: null,
	bio: null,
	is_admin: false,
	is_active: true,
	auth_provider: 'local',
	login_attempts: locked ? 3 : 0,
	locked_until: locked ? '2099-01-01T00:00:00Z' : null,
	last_login_at: null,
	created_at: timestamp,
});
const adminOrg = (id: number, name: string) => ({
	id,
	name,
	display_name: name,
	visibility: 'public',
	owner_id: id + 100,
	owner_username: `owner-${id}`,
	created_at: timestamp,
});
const runner = (id: number, name: string) => ({
	id,
	name,
	status: 'online',
	labels: ['linux'],
	version: '1.0.0',
	last_seen: timestamp,
});
const organization = (id: number, name: string) => ({
	id,
	name,
	display_name: name,
	description: `${name} description`,
	visibility: 'public',
	created_at: timestamp,
});

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/settings/tokens', {});
	vi.stubGlobal('confirm', vi.fn(() => true));
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

describe('account and admin collection state ownership', () => {
	it('keeps the post-create token list when the initial list finishes last', async () => {
		const initial = deferred<ReturnType<typeof token>[]>();
		const mutation = deferred<{ token: string }>();
		tokens.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([token(2, 'current-token')]);
		tokens.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(TokensPage);

		await input(element(rendered.container, '.create-form input'), 'current-token');
		const form = element<HTMLFormElement>(rendered.container, 'form.create-form');
		await submit(form);
		await submit(form);
		expect(tokens.create).toHaveBeenCalledOnce();

		mutation.resolve({ token: 'secret-token' });
		await settle();
		expect(rendered.container.textContent).toContain('current-token');

		initial.resolve([token(1, 'stale-token')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-token');
		expect(rendered.container.textContent).not.toContain('stale-token');
	});

	it('keeps the post-create SSH key list when the initial list finishes last', async () => {
		const initial = deferred<ReturnType<typeof sshKey>[]>();
		const mutation = deferred<unknown>();
		sshKeys.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([sshKey(2, 'current-key')]);
		sshKeys.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(SshKeysPage);

		await input(element(rendered.container, '#ssh-key-title'), 'current-key');
		await input(element(rendered.container, '#ssh-public-key'), 'ssh-ed25519 current-key');
		const form = element<HTMLFormElement>(rendered.container, 'form.create-form');
		await submit(form);
		await submit(form);
		expect(sshKeys.create).toHaveBeenCalledOnce();

		mutation.resolve({});
		await settle();
		initial.resolve([sshKey(1, 'stale-key')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-key');
		expect(rendered.container.textContent).not.toContain('stale-key');
	});

	it('does not let an older security load overwrite a confirmed passkey registration', async () => {
		const initialBackup = deferred<{ total: number; unused: number }>();
		const initialPasskeys = deferred<ReturnType<typeof passkey>[]>();
		const initialLinks = deferred<ReturnType<typeof ssoLink>[]>();
		const mutation = deferred<unknown>();
		mfa.backup.mockReturnValueOnce(initialBackup.promise);
		passkeys.list.mockReturnValueOnce(initialPasskeys.promise);
		auth.listSsoLinks.mockReturnValueOnce(initialLinks.promise);
		passkeys.register.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(SecurityPage);

		await input(element(rendered.container, 'form.passkey-form input'), 'current-passkey');
		const form = element<HTMLFormElement>(rendered.container, 'form.passkey-form');
		await submit(form);
		await submit(form);
		expect(passkeys.register).toHaveBeenCalledOnce();

		mutation.resolve([passkey(2, 'current-passkey')]);
		await settle();
		expect(rendered.container.textContent).toContain('1 active');

		initialBackup.resolve({ total: 8, unused: 7 });
		initialPasskeys.resolve([passkey(1, 'stale-passkey')]);
		initialLinks.resolve([ssoLink('current-sso', 'Current SSO')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-passkey');
		expect(rendered.container.textContent).toContain('Current SSO');
		expect(rendered.container.textContent).not.toContain('stale-passkey');
	});

	it('keeps the newest admin-user page when pagination responses reverse', async () => {
		const pageTwo = deferred<{ data: ReturnType<typeof adminUser>[]; pagination: ReturnType<typeof pagination> }>();
		admin.listUsers
			.mockResolvedValueOnce({ data: [adminUser(1, 'page-one-user')], pagination: pagination(1, 3) })
			.mockReturnValueOnce(pageTwo.promise)
			.mockResolvedValueOnce({ data: [adminUser(3, 'current-page-user')], pagination: pagination(3, 3) });
		rendered = await renderComponent(AdminUsersPage);

		const next = element<HTMLButtonElement>(rendered.container, '.pagination button:last-child');
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();
		expect(admin.listUsers).toHaveBeenCalledTimes(3);
		expect(admin.listUsers).toHaveBeenLastCalledWith(3, 20);
		expect(rendered.container.textContent).toContain('current-page-user');

		pageTwo.resolve({ data: [adminUser(2, 'stale-page-user')], pagination: pagination(2, 3) });
		await settle();
		expect(rendered.container.textContent).toContain('current-page-user');
		expect(rendered.container.textContent).not.toContain('stale-page-user');
	});

	it('shares one admin-user row claim between unlock, edit and delete controls', async () => {
		const mutation = deferred<unknown>();
		admin.listUsers.mockResolvedValue({
			data: [adminUser(7, 'locked-user', true)],
			pagination: pagination(),
		});
		admin.unlockUser.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(AdminUsersPage);

		await click(element(rendered.container, '.actions .btn-sm'));
		const rowButtons = Array.from(rendered.container.querySelectorAll<HTMLButtonElement>('.actions button'));
		expect(rowButtons.every((candidate) => candidate.disabled)).toBe(true);
		const deleteButton = rowButtons.find((candidate) => candidate.textContent?.trim() === 'Delete');
		expect(deleteButton).toBeDefined();
		await click(deleteButton!);
		expect(admin.deleteUser).not.toHaveBeenCalled();
		expect(rendered.container.querySelector('[role="dialog"]')).toBeNull();

		mutation.resolve({});
		await settle();
	});

	it('keeps the newest admin-organization page when pagination responses reverse', async () => {
		const pageTwo = deferred<{ data: ReturnType<typeof adminOrg>[]; pagination: ReturnType<typeof pagination> }>();
		admin.listOrgs
			.mockResolvedValueOnce({ data: [adminOrg(1, 'page-one-org')], pagination: pagination(1, 3) })
			.mockReturnValueOnce(pageTwo.promise)
			.mockResolvedValueOnce({ data: [adminOrg(3, 'current-page-org')], pagination: pagination(3, 3) });
		rendered = await renderComponent(AdminOrgsPage);

		const next = element<HTMLButtonElement>(rendered.container, '.pagination button:last-child');
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();
		expect(admin.listOrgs).toHaveBeenCalledTimes(3);
		expect(admin.listOrgs).toHaveBeenLastCalledWith(3, 20);
		expect(rendered.container.textContent).toContain('current-page-org');

		pageTwo.resolve({ data: [adminOrg(2, 'stale-page-org')], pagination: pagination(2, 3) });
		await settle();
		expect(rendered.container.textContent).toContain('current-page-org');
		expect(rendered.container.textContent).not.toContain('stale-page-org');
	});

	it('keeps the post-register runner list when the initial list finishes last', async () => {
		const initial = deferred<{ data: ReturnType<typeof runner>[]; pagination: ReturnType<typeof pagination> }>();
		const mutation = deferred<{ id: number; token: string }>();
		runners.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce({ data: [runner(2, 'current-runner')], pagination: pagination() });
		runners.register.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(AdminRunnersPage);

		await input(element(rendered.container, '.form-grid input'), 'current-runner');
		await input(element(rendered.container, '.form-grid label:nth-child(2) input'), 'owner/project');
		const register = element<HTMLButtonElement>(rendered.container, '.form-grid .btn-primary');
		await click(register);
		await click(register);
		expect(runners.register).toHaveBeenCalledOnce();
		expect(runners.register).toHaveBeenCalledWith({
			repository: 'owner/project',
			name: 'current-runner',
			labels: undefined,
		});

		mutation.resolve({ id: 2, token: 'runner-secret' });
		await settle();
		initial.resolve({ data: [runner(1, 'stale-runner')], pagination: pagination() });
		await settle();
		expect(rendered.container.textContent).toContain('current-runner');
		expect(rendered.container.textContent).not.toContain('stale-runner');
	});

	it('keeps the newest admin-runner page when pagination responses reverse', async () => {
		const pageTwo = deferred<{
			data: ReturnType<typeof runner>[];
			pagination: ReturnType<typeof pagination>;
		}>();
		runners.list
			.mockResolvedValueOnce({ data: [runner(1, 'page-one-runner')], pagination: pagination(1, 3) })
			.mockReturnValueOnce(pageTwo.promise)
			.mockResolvedValueOnce({ data: [runner(3, 'current-page-runner')], pagination: pagination(3, 3) });
		rendered = await renderComponent(AdminRunnersPage);

		const next = element<HTMLButtonElement>(rendered.container, '.pagination button:last-child');
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		next.dispatchEvent(new MouseEvent('click', { bubbles: true }));
		await settle();
		expect(runners.list).toHaveBeenCalledTimes(3);
		expect(runners.list).toHaveBeenLastCalledWith(3, 20);
		expect(rendered.container.textContent).toContain('current-page-runner');

		pageTwo.resolve({ data: [runner(2, 'stale-page-runner')], pagination: pagination(2, 3) });
		await settle();
		expect(rendered.container.textContent).toContain('current-page-runner');
		expect(rendered.container.textContent).not.toContain('stale-page-runner');
	});

	it('keeps the post-create account organization list when the initial list finishes last', async () => {
		const initial = deferred<ReturnType<typeof organization>[]>();
		const mutation = deferred<{ name: string }>();
		orgs.list
			.mockReturnValueOnce(initial.promise)
			.mockResolvedValueOnce([organization(2, 'current-org')]);
		orgs.create.mockReturnValueOnce(mutation.promise);
		rendered = await renderComponent(OrganizationsPage);

		await click(element(rendered.container, 'button.header-action'));
		await input(element(rendered.container, '#name'), 'current-org');
		const form = element<HTMLFormElement>(rendered.container, 'form.create-panel');
		await submit(form);
		await submit(form);
		expect(orgs.create).toHaveBeenCalledOnce();

		mutation.resolve({ name: 'current-org' });
		await settle();
		initial.resolve([organization(1, 'stale-org')]);
		await settle();
		expect(rendered.container.textContent).toContain('current-org');
		expect(rendered.container.textContent).not.toContain('stale-org');
	});
});
