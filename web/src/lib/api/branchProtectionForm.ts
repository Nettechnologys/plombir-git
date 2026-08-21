import type { BranchProtectionPayload } from './branchProtections';

/**
 * The state the branch-protection settings form holds. The two list fields are
 * raw comma-separated text, exactly as typed into the inputs.
 */
export interface BranchProtectionFormState {
  branch_name: string;
  require_pr: boolean;
  require_status_check: boolean;
  required_status_checks: string;
  require_approval: boolean;
  required_approvals: number | string;
  allow_force_push: boolean;
  require_signed_commits: boolean;
  allowed_push_users: string;
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
    required_status_checks: parseStringList(form.required_status_checks),
    require_approval: form.require_approval,
    required_approvals: Number(form.required_approvals || 1),
    allow_force_push: form.allow_force_push,
    require_signed_commits: form.require_signed_commits,
    allowed_push_users: parseStringList(form.allowed_push_users)
  };
}
