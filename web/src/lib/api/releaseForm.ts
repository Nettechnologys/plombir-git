/** The state the release edit form holds. */
export interface ReleaseUpdateFormState {
  title: string;
  body: string;
  is_draft: boolean;
  is_prerelease: boolean;
}

/** The body of `PATCH /repos/{owner}/{repo}/releases/{id}`. */
export interface ReleaseUpdatePayload {
  title: string;
  body: string;
  is_draft: boolean;
  is_prerelease: boolean;
}

/**
 * Build the request body for a release update.
 *
 * The release notes are always sent, emptied included. `update_release` writes
 * whatever `body` carries and skips the field when it is absent, so
 * `body.trim() || undefined` — a dropped key, since `JSON.stringify` discards
 * `undefined` — meant release notes could be rewritten but never removed:
 * deleting the text and saving answered `200` and left the old notes published.
 */
export function buildReleaseUpdatePayload(form: ReleaseUpdateFormState): ReleaseUpdatePayload {
  return {
    title: form.title.trim(),
    body: form.body.trim(),
    is_draft: form.is_draft,
    is_prerelease: form.is_prerelease
  };
}
