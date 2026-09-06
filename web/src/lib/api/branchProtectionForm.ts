import type { BranchProtectionPayload } from './branchProtections';

/** The state the branch-protection settings form holds. */
export interface BranchProtectionFormState {
  branch_name: string;
  require_pr: boolean;
  require_status_check: boolean;
  /** One row per CI job name. Job names may themselves contain commas. */
  required_status_checks: string[];
  require_approval: boolean;
  required_approvals: number | string;
  allow_force_push: boolean;
  require_signed_commits: boolean;
  allowed_push_users: string;
}

export type StoredStringListParseResult =
  | { kind: 'parsed'; value: string[] }
  | { kind: 'unavailable' };

function isStringList(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((item) => typeof item === 'string');
}

/**
 * Decode a JSON-backed string list for an editable form without inventing an
 * empty value when the stored representation is unreadable.
 *
 * `NULL` and `[]` are both honest spellings of "no named entries". A present
 * value that is not a JSON array of strings is different: returning an empty
 * field for it would let the next full-form save overwrite the damaged value
 * with `[]` before the operator had even been told that anything was wrong.
 */
export function parseStoredStringList(value: string | null): StoredStringListParseResult {
  if (value === null) return { kind: 'parsed', value: [] };

  try {
    const parsed: unknown = JSON.parse(value);
    if (isStringList(parsed)) {
      return { kind: 'parsed', value: parsed };
    }
  } catch {
    // The typed result keeps a decode failure distinct from an honest empty list.
  }

  return { kind: 'unavailable' };
}

/**
 * Comma-separated text to a list of names.
 *
 * An emptied field is an empty list, **not** `undefined`: `JSON.stringify`
 * drops an `undefined` value, so the request would leave the key out entirely,
 * and the API reads a missing key as "leave this column alone". The operator
 * would clear the field, save, read `200`, and find the old list still there.
 */
export function parseStringList(value: string): string[] {
  return value
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

/**
 * Build the request body for a create (`includeBranch`) or update call.
 *
 * Every field the form displays is sent on every save. Nothing is omitted on
 * the grounds of being empty or inactive: a key the body leaves out means
 * "keep whatever is stored", which is a state this form has no way to show and
 * therefore must never send. Revoking a direct-push grant or dropping back to
 * "any green CI" is expressed by clearing the field, so clearing the field has
 * to reach the server as `[]`.
 */
export function buildBranchProtectionPayload(
  form: BranchProtectionFormState,
  includeBranch: boolean
): BranchProtectionPayload {
  return {
    ...(includeBranch ? { branch_name: form.branch_name.trim() } : {}),
    require_pr: form.require_pr,
    require_status_check: form.require_status_check,
    // Empty editor rows are placeholders, not check names. Apart from that
    // distinction the strings stay byte-for-byte intact: native and matrix CI
    // job names may contain commas or significant whitespace.
    required_status_checks: form.required_status_checks.filter((item) => item !== ''),
    require_approval: form.require_approval,
    required_approvals: Number(form.required_approvals || 1),
    allow_force_push: form.allow_force_push,
    require_signed_commits: form.require_signed_commits,
    allowed_push_users: parseStringList(form.allowed_push_users)
  };
}
