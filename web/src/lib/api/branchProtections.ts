import { request } from './_base.svelte';

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
  allowed_push_user_ids: string | null;
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
