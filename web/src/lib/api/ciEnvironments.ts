import { request } from './_base.svelte';
import type { AllowedUser } from './userRef';

export interface CiEnvironment {
  id: number; name: string; protected: boolean; required_approvals: number;
  allowed_approver_ids: number[]; allowed_approvers: AllowedUser[]; created_at: string; updated_at: string;
}

/**
 * An environment as the API accepts it.
 *
 * `allowed_approvers` is the approver list, one entry per person: a username
 * or a bare id (an e-mail is refused). The API still accepts the numeric
 * `allowed_approver_ids`; the form sends names because approving a deployment
 * is handing out access, and a number is not something the owner of a
 * repository can look up — there is no `/users/{username}` to look it up in.
 *
 * The list is always sent, empty included: `POST` and `PUT` both carry the
 * whole environment, so an omitted list would not mean "leave it alone", and a
 * form that cannot clear it could only ever add approvers.
 */
export interface CiEnvironmentPayload {
  name: string; protected: boolean; required_approvals: number; allowed_approvers: string[];
}
export const ciEnvironments = {
  list: (owner: string, repo: string) => request<CiEnvironment[]>(`/repos/${owner}/${repo}/actions/environments`),
  create: (owner: string, repo: string, payload: CiEnvironmentPayload) => request<CiEnvironment>(`/repos/${owner}/${repo}/actions/environments`, { method: 'POST', body: JSON.stringify(payload) }),
  update: (owner: string, repo: string, id: number, payload: CiEnvironmentPayload) => request<CiEnvironment>(`/repos/${owner}/${repo}/actions/environments/${id}`, { method: 'PUT', body: JSON.stringify(payload) }),
  delete: (owner: string, repo: string, id: number) => request<void>(`/repos/${owner}/${repo}/actions/environments/${id}`, { method: 'DELETE' }),
};
