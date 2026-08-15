import type { UpdateUserData } from './admin';

/** The state the admin's user-edit dialog holds. */
export interface AdminUserFormState {
  display_name: string;
  bio: string;
  is_admin: boolean;
  is_active: boolean;
}

/**
 * Build the request body for `PATCH /admin/users/{id}`.
 *
 * Both text fields are always sent, and an emptied one is sent as `null`.
 * `UpdateUserRequest` puts `display_name` and `bio` behind
 * `clearable::double_option` so that `null` clears the column and an absent key
 * leaves it — and `editDisplayName || undefined` could produce only the second,
 * because `JSON.stringify` drops an `undefined` value. An admin asked to remove
 * somebody's display name or bio (a takedown, a leaked real name) cleared the
 * field, saved, read `200`, and the old value was still there.
 */
export function buildAdminUserPayload(form: AdminUserFormState): UpdateUserData {
  return {
    display_name: form.display_name.trim() || null,
    bio: form.bio.trim() || null,
    is_admin: form.is_admin,
    is_active: form.is_active
  };
}
