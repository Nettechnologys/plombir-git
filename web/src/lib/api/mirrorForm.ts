import type { MirrorPayload } from './mirrors';

/**
 * The state the mirror settings form holds.
 *
 * `password` is what the operator typed *now* — the stored credential never
 * leaves the server, so the input is always blank on load and `clearPassword`
 * is the only way to say "take the stored one off".
 */
export interface MirrorFormState {
  url: string;
  username: string;
  password: string;
  /** The operator ticked "remove the stored credential". */
  clearPassword: boolean;
  intervalHours: number | string;
}

/**
 * Which `password` key, if any, the request carries.
 *
 * The API reads an empty string as "clear the credential" and an absent key as
 * "leave it as it is" (`crates/rg-http/src/api/mirrors.rs`), and until the
 * form grew a checkbox it could only ever produce the second: `password:
 * trimmedPassword || undefined` turned a blank input into a dropped key, so an
 * access token to somebody else's repository could be replaced but never
 * revoked — deleting the whole mirror was the only way off.
 *
 * Blank input now means whatever the form is actually showing:
 *
 * - checkbox ticked → `''`, the explicit clear;
 * - something typed → that, the replacement;
 * - nothing stored → `''`, which stores nothing and matches the empty input;
 * - something stored and untouched → the key is left out.
 *
 * That last branch is the one deliberate "leave it alone" in this file, and it
 * is not the blank-equals-absent coincidence the checkbox exists to break: a
 * stored credential is the one piece of state the form is not allowed to
 * display, so an untouched input cannot be read as "no credential". Both other
 * meanings of a blank input are reachable, and each is chosen, not inferred.
 */
function passwordField(
  typedPassword: string,
  clearPassword: boolean,
  hasStoredCredential: boolean
): { password?: string } {
  if (clearPassword) return { password: '' };
  if (typedPassword) return { password: typedPassword };
  return hasStoredCredential ? {} : { password: '' };
}

/**
 * Build the request body for a mirror create or update.
 *
 * Everything the form displays is sent on every save, `username` included: an
 * emptied username is an empty string, not a dropped key, so the remote can go
 * back to being anonymous. Only the credential the form cannot show is ever
 * omitted — see [`passwordField`].
 *
 * `hasStoredCredential` is the server's own `has_credentials` flag, so the
 * "leave it alone" branch is keyed to a credential that really exists rather
 * than to whether this happens to be an update call.
 */
export function buildMirrorPayload(
  form: MirrorFormState,
  hasStoredCredential: boolean
): MirrorPayload {
  return {
    url: form.url.trim(),
    username: form.username.trim(),
    ...passwordField(form.password.trim(), form.clearPassword, hasStoredCredential),
    sync_interval_seconds: syncIntervalSeconds(form.intervalHours)
  };
}

/**
 * Hours as typed into the number input, as a whole number of seconds.
 *
 * A blank or otherwise unusable field falls back to the one-hour floor the API
 * enforces anyway. It used to reach `Math.max(1, Math.round(NaN))`, and `NaN`
 * serialises as `null` — an interval the server reads as "no interval given"
 * and quietly leaves alone, which is the same silent no-op this file is about.
 */
function syncIntervalSeconds(intervalHours: number | string): number {
  const hours = Number(intervalHours);
  return (Number.isFinite(hours) ? Math.max(1, Math.round(hours)) : 1) * 3600;
}
