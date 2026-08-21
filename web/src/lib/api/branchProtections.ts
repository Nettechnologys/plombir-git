import { request } from './_base.svelte';
import type { AllowedUser } from './userRef';

export interface BranchProtectionRule {
  id: number;
  repo_id: number;
  branch_name: string;
  require_pr: boolean;
  require_status_check: boolean;
  required_status_checks: string | null;
  require_approval: boolean;
  required_approvals: number | null;
  allow_force_push: boolean;
  require_signed_commits: boolean;
  /** The stored JSON mirror of the allow-list, kept as it always was. */
  allowed_push_user_ids: string | null;
  /** The same allow-list with each person named — what a screen renders. */
  allowed_push_users: AllowedUser[];
  created_at: string;
  updated_at: string;
}

export interface BranchProtectionPayload {
  branch_name?: string;
  require_pr?: boolean;
  require_status_check?: boolean;
  required_status_checks?: string[];
  require_approval?: boolean;
  required_approvals?: number;
  allow_force_push?: boolean;
  require_signed_commits?: boolean;
  /**
   * The direct-push exceptions, one entry per person: a username, an e-mail,
   * or a bare id. The API still accepts `allowed_push_user_ids`, but a number
   * is not something the owner of a repository can look up anywhere on this
   * instance — which is why the form asks for names.
   */
  allowed_push_users?: string[];
  allowed_push_user_ids?: number[];
}

export const branchProtections = {
  list: (owner: string, repo: string) =>
    request<BranchProtectionRule[]>(`/repos/${owner}/${repo}/branches/protection`),
  create: (owner: string, repo: string, payload: BranchProtectionPayload & { branch_name: string }) =>
    request<BranchProtectionRule>(`/repos/${owner}/${repo}/branches/protection`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  update: (owner: string, repo: string, id: number, payload: Omit<BranchProtectionPayload, 'branch_name'>) =>
    request<BranchProtectionRule>(`/repos/${owner}/${repo}/branches/protection/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  remove: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/branches/protection/${id}`, { method: 'DELETE' }),
};
