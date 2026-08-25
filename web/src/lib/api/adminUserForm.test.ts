import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import AdminUsersPage from '../../routes/admin/users/+page.svelte';
import { buildAdminUserPayload, type AdminUserFormState } from './adminUserForm';
import { fetchUser, logout } from '../stores/auth.svelte';
import { admin, auth, resetTestClient } from '../test/client';
import { button, click, element, input, renderComponent, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const targetUser = {
	id: 7,
	username: 'target',
	email: 'target@example.com',
	display_name: 'Target User',
	bio: 'Maintainer of things',
	is_admin: false,
	is_active: true,
	auth_provider: 'local',
	login_attempts: 0,
	locked_until: null,
	last_login_at: null,
	created_at: '2026-08-15T12:00:00Z',
};

beforeEach(async () => {
	resetTestClient();
	auth.me.mockResolvedValue({
		id: 1,
		username: 'admin',
		email: 'admin@example.com',
		is_admin: true,
		display_name: 'Admin',
	});
	admin.listUsers.mockResolvedValue({
		data: [targetUser],
		pagination: { total: 1, total_pages: 1 },
	});
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

function formState(overrides: Partial<AdminUserFormState> = {}): AdminUserFormState {
  return {
    display_name: 'Target User',
    bio: 'Maintainer of things',
    is_admin: false,
    is_active: true,
    ...overrides
  };
}

/** What the network actually carries — `JSON.stringify` drops an `undefined` value. */
function wire(payload: unknown): Record<string, unknown> {
  return JSON.parse(JSON.stringify(payload));
}

describe('buildAdminUserPayload', () => {
  it('sends an emptied display name as null, so an admin can actually remove it', () => {
    const body = wire(buildAdminUserPayload(formState({ display_name: '  ' })));

    expect(body).toHaveProperty('display_name');
    expect(body.display_name).toBeNull();
  });

  it('sends an emptied bio as null', () => {
    const body = wire(buildAdminUserPayload(formState({ bio: '' })));

    expect(body).toHaveProperty('bio');
    expect(body.bio).toBeNull();
  });

  it('drops no key on the way to the wire, whatever the form holds', () => {
    const body = wire(buildAdminUserPayload(formState({ display_name: '', bio: '' })));

    expect(Object.keys(body).sort()).toEqual(['bio', 'display_name', 'is_active', 'is_admin']);
  });

  it('still carries what the admin typed, and the two flags', () => {
    const body = wire(buildAdminUserPayload(formState({ is_admin: true, is_active: false })));

    expect(body).toEqual({
      display_name: 'Target User',
      bio: 'Maintainer of things',
      is_admin: true,
      is_active: false
    });
  });

	it('submits explicit clears from the rendered admin editor', async () => {
		rendered = await renderComponent(AdminUsersPage);
		await click(button(rendered.container, 'Edit'));
		await input(element(rendered.container, '#admin-user-display-name'), '   ');
		await input(element(rendered.container, '#admin-user-bio'), '');
		await click(element(rendered.container, '.modal-actions .btn-primary'));

		expect(admin.updateUser).toHaveBeenCalledWith(7, {
			display_name: null,
			bio: null,
			is_admin: false,
			is_active: true,
		});
	});
});
