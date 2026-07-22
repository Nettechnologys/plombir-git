import { request } from './_base.svelte';

export interface CiSecret { name: string; created_at: string; updated_at: string; }
export const ciSecrets = {
  list: (owner: string, repo: string) => request<CiSecret[]>(`/repos/${owner}/${repo}/actions/secrets`),
  put: (owner: string, repo: string, name: string, value: string) => request<CiSecret>(`/repos/${owner}/${repo}/actions/secrets/${encodeURIComponent(name)}`, { method: 'PUT', body: JSON.stringify({ value }) }),
  delete: (owner: string, repo: string, name: string) => request<void>(`/repos/${owner}/${repo}/actions/secrets/${encodeURIComponent(name)}`, { method: 'DELETE' }),
};
