// The allow-list of a tag rule and the one of a branch rule are the same
// field in two places — a comma-separated list of user ids — so they parse the
// same way, including the part that matters: an emptied field is `[]`, never
// `undefined`. See `parseNumberList` for why that distinction is the whole bug.
import { parseNumberList } from './branchProtectionForm';
import type { TagProtectionPayload } from './tagProtections';

/**
 * The state the tag-protection settings form holds. `allowed_user_ids` is raw
 * comma-separated text, exactly as typed into the input.
 */
export interface TagProtectionFormState {
  pattern: string;
  allowed_user_ids: string;
}

/**
 * Build the request body for a create (`includePattern`) or update call.
 *
 * The allow-list is always sent. It is the only way to except anybody from a
 * protected pattern — `tag_push_allowed_by_rule` admits an actor named in the
 * list and nobody else — so a form that cannot send it can only ever produce
 * the one state "this pattern is closed to everyone", with deleting the rule
 * as the sole way back out.
 *
 * `pattern` is omitted on update because `PATCH` cannot change it: sending it
 * would let the form show an edit that the server never made.
 */
export function buildTagProtectionPayload(
  form: TagProtectionFormState,
  includePattern: boolean
): TagProtectionPayload {
  return {
    ...(includePattern ? { pattern: form.pattern.trim() } : {}),
    allowed_user_ids: parseNumberList(form.allowed_user_ids)
  };
}
