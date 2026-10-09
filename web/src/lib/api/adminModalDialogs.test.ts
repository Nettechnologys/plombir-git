import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('$lib/stores/auth.svelte', () => ({
	getUser: () => ({ id: 999, username: 'admin' }),
	isAdmin: () => true,
	isAuthReady: () => true,
	isLoggedIn: () => true,
}));

import AdminAuditPage from '../../routes/admin/audit/+page.svelte';
import AdminOrgsPage from '../../routes/admin/orgs/+page.svelte';
import AdminRunnersPage from '../../routes/admin/runners/+page.svelte';
import AdminUsersPage from '../../routes/admin/users/+page.svelte';
import { setTestPage } from '../test/app';
import { admin, resetTestClient, runners } from '../test/client';
import {
	expectEscapeClosesAndRestoresFocus,
	expectModalSurvivesInteraction,
	openDialog,
	openModalFrom,
} from '../test/modalContract';
import { button, element, renderComponent, type RenderedComponent } from '../test/render';

// card_4a99471945dc: the admin dialogs sat inside an overlay that closed on
// any click and on Escape/Enter/Space, so a click into "Display Name" closed
// the edit form and a space typed into it was swallowed.

const timestamp = '2026-08-30T00:00:00Z';
const pagination = { page: 1, per_page: 20, total: 1, total_pages: 1 };

let rendered: RenderedComponent | undefined;

beforeEach(() => {
	resetTestClient();
	setTestPage('/admin', {});
	vi.stubGlobal('confirm', vi.fn(() => true));
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.unstubAllGlobals();
});

function adminUser(id: number, username: string) {
	return {
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
		created_at: timestamp,
	};
}

describe('admin modal dialogs', () => {
	it('keeps the user edit form open while the admin clicks and types into it', async () => {
		admin.listUsers.mockResolvedValue({ data: [adminUser(7, 'bob')], pagination });
		rendered = await renderComponent(AdminUsersPage);
		const opener = button(element(rendered.container, '.actions'), 'Edit');

		await openModalFrom(rendered.container, opener);
		const displayName = element<HTMLInputElement>(rendered.container, '#admin-user-display-name');
		expect(document.activeElement).toBe(displayName);
		await expectModalSurvivesInteraction(rendered.container, displayName);
		await expectModalSurvivesInteraction(rendered.container, element(rendered.container, '#admin-user-bio'));
		await expectEscapeClosesAndRestoresFocus(rendered.container, opener);
	});

	it('keeps the user delete confirmation open and focuses Cancel, not Delete', async () => {
		admin.listUsers.mockResolvedValue({ data: [adminUser(7, 'bob')], pagination });
		rendered = await renderComponent(AdminUsersPage);
		const opener = button(element(rendered.container, '.actions'), 'Delete');

		const dialog = await openModalFrom(rendered.container, opener);
		expect(document.activeElement?.textContent?.trim()).toBe('Cancel');
		await expectModalSurvivesInteraction(rendered.container, element(dialog, 'h2'));
		expect(admin.deleteUser).not.toHaveBeenCalled();
		await expectEscapeClosesAndRestoresFocus(rendered.container, opener);
	});

	it('keeps the organization delete confirmation open on clicks and keys inside', async () => {
		admin.listOrgs.mockResolvedValue({
			data: [{
				id: 3,
				name: 'acme',
				display_name: 'acme',
				visibility: 'public',
				owner_id: 1,
				owner_username: 'owner',
				created_at: timestamp,
			}],
			pagination,
		});
		rendered = await renderComponent(AdminOrgsPage);
		const opener = element<HTMLButtonElement>(rendered.container, 'table .btn-danger');

		const dialog = await openModalFrom(rendered.container, opener);
		await expectModalSurvivesInteraction(rendered.container, element(dialog, 'h2'));
		await expectEscapeClosesAndRestoresFocus(rendered.container, opener);
	});

	it('keeps the runner delete confirmation open on clicks and keys inside', async () => {
		runners.list.mockResolvedValue({
			data: [{ id: 4, name: 'builder', status: 'online', labels: ['linux'], version: '1.0.0', last_seen: timestamp }],
			pagination,
		});
		rendered = await renderComponent(AdminRunnersPage);
		const opener = element<HTMLButtonElement>(rendered.container, 'table .btn-danger');

		const dialog = await openModalFrom(rendered.container, opener);
		await expectModalSurvivesInteraction(rendered.container, element(dialog, 'h2'));
		expect(runners.delete).not.toHaveBeenCalled();
		await expectEscapeClosesAndRestoresFocus(rendered.container, opener);
	});

	it('keeps the audit detail open on clicks and keys inside', async () => {
		const log = {
			id: 12,
			user_id: 1,
			username: 'admin',
			action: 'update_user',
			resource_type: 'user',
			resource_id: 7,
			resource_name: 'bob',
			ip_address: '127.0.0.1',
			details: 'changed display name',
			created_at: timestamp,
		};
		admin.listAuditLogs.mockResolvedValue({ total: 1, page: 1, per_page: 20, logs: [log] });
		admin.getAuditLog.mockResolvedValue(log);
		rendered = await renderComponent(AdminAuditPage);
		const opener = element<HTMLButtonElement>(rendered.container, 'td.actions .btn-sm');

		const dialog = await openModalFrom(rendered.container, opener);
		expect(dialog.textContent).toContain('changed display name');
		await expectModalSurvivesInteraction(rendered.container, element(dialog, '.detail-grid'));
		await expectEscapeClosesAndRestoresFocus(rendered.container, opener);
		expect(openDialog(rendered.container)).toBeNull();
	});
});
