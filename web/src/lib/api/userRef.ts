/**
 * Turning what a person typed into the key the API resolves an account by.
 *
 * The endpoints that hand out access — repository collaborators, organization
 * members, team members — all accept `user_id`, `username` or `email`, and the
 * only one a human is ever going to have at hand is the name. So the forms ask
 * for "user", not "User ID", and this decides which key that answer becomes.
 *
 * A bare run of digits is read as an id, which keeps working for anyone who
 * genuinely has one; anything containing `@` is an e-mail; everything else is
 * a username. `null` means the field was empty, which is the one case the
 * caller has to refuse locally instead of sending.
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
