// Security audit finding #6: the administrator's way back into an account
// whose second factor its owner can no longer pass, and the password every
// enable now carries.
//
// Two halves. The client half pins the wire shape of `admin.resetUserMfa` and
// `mfa.enable` against a mocked transport. The page half renders the admin
// users page and drives the "Reset MFA" control through its confirmation,
// its request and its reload — and checks that it is offered only for an
// account that has a factor to reset, and never for the administrator's own.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

vi.mock('$lib/stores/auth.svelte', () => ({
	getUser: () => ({ id: 999, username: 'admin' }),
	isAdmin: () => true,
	isAuthReady: () => true,
	isLoggedIn: () => true,
}));

import AdminUsersPage from '../../routes/admin/users/+page.svelte';
import { setTestPage } from '../test/app';
import { admin as routeAdmin, resetTestClient } from '../test/client';
import { button, click, renderComponent, settle, type RenderedComponent } from '../test/render';
import { admin } from './admin';
import { mfa } from './mfa';

const timestamp = '2026-08-30T00:00:00Z';
const pagination = () => ({ page: 1, per_page: 20, total: 1, total_pages: 1 });
const adminUser = (id: number, username: string, mfaEnabled: boolean) => ({
	id,
	username,
	email: `${username}@example.test`,
	display_name: null,
	bio: null,
	is_admin: false,
	is_active: true,
	auth_provider: 'local',
	login_attempts: 0,
	locked_until: null,
	last_login_at: null,
	mfa_enabled: mfaEnabled,
	created_at: timestamp,
});

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	base.request.mockReset();
	setTestPage('/admin/users', {});
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

describe('admin MFA reset client', () => {
	it('posts to the reset route of the named account and carries no body', async () => {
		base.request.mockResolvedValueOnce(adminUser(7, 'locked-out', false));
		const result = await admin.resetUserMfa(7);
		expect(base.request).toHaveBeenCalledWith('/admin/users/7/mfa/reset', { method: 'POST' });
		expect(result.mfa_enabled).toBe(false);
	});

	it('sends the password with the code on every enable', async () => {
		base.request.mockResolvedValueOnce({ enabled: true, backup_codes: ['a', 'b'] });
		await mfa.enable('123456', 'hunter2');
		expect(base.request).toHaveBeenCalledWith('/users/mfa/enable', {
			method: 'POST',
			body: JSON.stringify({ code: '123456', password: 'hunter2' }),
		});
	});
});

describe('admin users page: Reset MFA', () => {
	it('confirms, resets the account and reloads the list', async () => {
		vi.stubGlobal('confirm', vi.fn(() => true));
		routeAdmin.listUsers.mockResolvedValue({
			data: [adminUser(7, 'locked-out', true)],
			pagination: pagination(),
		});
		routeAdmin.resetUserMfa.mockResolvedValue(adminUser(7, 'locked-out', false));
		rendered = await renderComponent(AdminUsersPage);

		await click(button(rendered.container, 'Reset MFA'));
		await settle();

		expect(window.confirm).toHaveBeenCalledOnce();
		expect(String(vi.mocked(window.confirm).mock.calls[0][0])).toContain('locked-out');
		expect(routeAdmin.resetUserMfa).toHaveBeenCalledWith(7);
		expect(routeAdmin.listUsers).toHaveBeenCalledTimes(2);
	});

	it('does nothing when the confirmation is declined', async () => {
		vi.stubGlobal('confirm', vi.fn(() => false));
		routeAdmin.listUsers.mockResolvedValue({
			data: [adminUser(7, 'locked-out', true)],
			pagination: pagination(),
		});
		rendered = await renderComponent(AdminUsersPage);

		await click(button(rendered.container, 'Reset MFA'));
		await settle();

		expect(routeAdmin.resetUserMfa).not.toHaveBeenCalled();
		expect(routeAdmin.listUsers).toHaveBeenCalledOnce();
	});

	it('offers the control only for an account that has a factor, and never for your own', async () => {
		routeAdmin.listUsers.mockResolvedValue({
			data: [
				adminUser(7, 'no-factor', false),
				adminUser(999, 'admin', true),
				adminUser(8, 'with-factor', true),
			],
			pagination: pagination(),
		});
		rendered = await renderComponent(AdminUsersPage);

		const rows = Array.from(rendered.container.querySelectorAll('.users-table tbody tr'));
		expect(rows).toHaveLength(3);
		const offers = (row: Element) =>
			Array.from(row.querySelectorAll('button')).some(
				(candidate) => candidate.textContent?.trim() === 'Reset MFA',
			);
		expect(offers(rows[0])).toBe(false);
		expect(offers(rows[1])).toBe(false);
		expect(offers(rows[2])).toBe(true);
	});

	it('surfaces the server refusal and releases the row', async () => {
		vi.stubGlobal('confirm', vi.fn(() => true));
		routeAdmin.listUsers.mockResolvedValue({
			data: [adminUser(7, 'locked-out', true)],
			pagination: pagination(),
		});
		routeAdmin.resetUserMfa.mockRejectedValueOnce(new Error('MFA is not enabled for this account'));
		rendered = await renderComponent(AdminUsersPage);

		await click(button(rendered.container, 'Reset MFA'));
		await settle();

		expect(rendered.container.textContent).toContain('MFA is not enabled for this account');
		expect(button(rendered.container, 'Reset MFA').disabled).toBe(false);
		expect(routeAdmin.listUsers).toHaveBeenCalledOnce();
	});
});
