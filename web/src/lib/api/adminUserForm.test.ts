import { describe, expect, it } from 'vitest';

// The page's own source, pulled in by vite so the check below needs no node
// filesystem API (and no `@types/node` for `npm run check`).
import adminUsersPageSource from '../../routes/admin/users/+page.svelte?raw';
import { buildAdminUserPayload, type AdminUserFormState } from './adminUserForm';

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

  it('is the builder the admin page itself uses', () => {
    const page = adminUsersPageSource;

    expect(page).toContain('buildAdminUserPayload');
    expect(page).toContain('admin.updateUser');
    // A second, page-local body builder would make every assertion above a
    // statement about dead code.
    expect(page).not.toMatch(/display_name:\s*editDisplayName\s*\|\|/);
    expect(page).not.toMatch(/bio:\s*editBio\s*\|\|/);
  });
});
