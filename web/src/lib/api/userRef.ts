/**
 * Turning what a person typed into the key the API resolves an account by.
 *
 * The endpoints that hand out access — repository collaborators, organization
 * members, team members — accept `user_id` or `username`, and the only one a
 * human is ever going to have at hand is the name. So the forms ask for "user",
 * not "User ID", and this decides which key that answer becomes.
 *
 * A bare run of digits is read as an id, which keeps working for anyone who
 * genuinely has one; anything containing `@` is sent as an e-mail, which the
 * server refuses with the reason (addresses are not confirmed on the instance,
 * so an address names whoever registered it first); everything else is a
 * username. `null` means the field was empty, which is the one case the caller
 * has to refuse locally instead of sending.
 */
export type UserRefPayload = { user_id: number } | { username: string } | { email: string };

export function buildUserRef(value: string | number): UserRefPayload | null {
  const raw = String(value).trim();
  if (!raw) return null;

  if (/^\d+$/.test(raw)) {
    const userId = Number(raw);
    return Number.isSafeInteger(userId) && userId > 0 ? { user_id: userId } : null;
  }

  return raw.includes('@') ? { email: raw } : { username: raw };
}

/**
 * One person on an allow-list, as the API names them back.
 *
 * The exception lists of branch and tag protection store ids, because that is
 * what the push gate compares against — so the id stays on the wire and the
 * name travels beside it. `username` is `null` for an id that resolves to no
 * account: the grant is real and has to stay on screen, unnamed, rather than
 * shortening a list that answers "who may push here".
 */
export interface AllowedUser {
  user_id: number;
  username: string | null;
  display_name: string | null;
}

/** How one entry of an allow-list is written back into the form's text field. */
export function allowedUserLabel(user: AllowedUser): string {
  return user.username ?? String(user.user_id);
}
