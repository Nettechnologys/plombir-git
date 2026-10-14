import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import AdminUsersPage from '../../routes/admin/users/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { admin, auth, resetTestClient } from '../test/client';
import { answerConfirm, button, click, renderComponent, settle, type RenderedComponent } from '../test/render';

// Decision #5: an administrator can take every second factor off an account
// whose owner is locked out — offered only for an account that has one, and
// never for the administrator's own.

let rendered: RenderedComponent | undefined;

const user = (id: number, username: string, mfa_enabled: boolean) => ({
	id,
	username,
	email: `${username}@example.com`,
	display_name: null,
	bio: null,
	is_admin: id === 1,
	is_active: true,
	auth_provider: 'local',
	mfa_enabled,
	login_attempts: 0,
	locked_until: null,
	last_login_at: null,
	created_at: '2026-10-14T12:00:00Z',
});

beforeEach(async () => {
	resetTestClient();
	auth.me.mockResolvedValue({ id: 1, username: 'admin', email: 'admin@example.com', is_admin: true, display_name: 'Admin' });
	admin.listUsers.mockResolvedValue({
		data: [user(1, 'admin', true), user(7, 'locked', true), user(8, 'plain', false)],
		pagination: { total: 3, total_pages: 1 },
	});
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

describe('admin MFA reset', () => {
	it('is offered for another account with a second factor and asks before resetting', async () => {
		admin.resetUserMfa.mockResolvedValue(user(7, 'locked', false));
		rendered = await renderComponent(AdminUsersPage);
		await settle();

		const rows = [...rendered.container.querySelectorAll('tbody tr')];
		const offered = rows.map((row) => row.querySelector('.reset-mfa') !== null);
		expect(offered).toEqual([false, true, false]);

		await click(button(rows[1], 'Reset MFA'));
		expect(await answerConfirm()).toContain('locked');
		expect(admin.resetUserMfa).toHaveBeenCalledWith(7);
	});

	it('does nothing when the confirmation is declined', async () => {
		rendered = await renderComponent(AdminUsersPage);
		await settle();
		const row = rendered.container.querySelectorAll('tbody tr')[1];
		await click(button(row, 'Reset MFA'));
		await answerConfirm(false);
		expect(admin.resetUserMfa).not.toHaveBeenCalled();
	});
});
