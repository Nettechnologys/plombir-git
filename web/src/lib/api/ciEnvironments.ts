import { request } from './_base.svelte';

export interface CiEnvironment {
  id: number; name: string; protected: boolean; required_approvals: number;
  allowed_approver_ids: number[]; created_at: string; updated_at: string;
}
export interface CiEnvironmentPayload {
  name: string; protected: boolean; required_approvals: number; allowed_approver_ids: number[];
}
export const ciEnvironments = {
  list: (owner: string, repo: string) => request<CiEnvironment[]>(`/repos/${owner}/${repo}/actions/environments`),
  create: (owner: string, repo: string, payload: CiEnvironmentPayload) => request<CiEnvironment>(`/repos/${owner}/${repo}/actions/environments`, { method: 'POST', body: JSON.stringify(payload) }),
  update: (owner: string, repo: string, id: number, payload: CiEnvironmentPayload) => request<CiEnvironment>(`/repos/${owner}/${repo}/actions/environments/${id}`, { method: 'PUT', body: JSON.stringify(payload) }),
  delete: (owner: string, repo: string, id: number) => request<void>(`/repos/${owner}/${repo}/actions/environments/${id}`, { method: 'DELETE' }),
};
