import { request } from './_base.svelte';

export interface CiSecret { name: string; environment: string | null; created_at: string; updated_at: string; }
export const ciSecrets = {
  list: (owner: string, repo: string) => request<CiSecret[]>(`/repos/${owner}/${repo}/actions/secrets`),
  put: (owner: string, repo: string, name: string, value: string, environment: string | null = null) => request<CiSecret>(`/repos/${owner}/${repo}/actions/secrets/${encodeURIComponent(name)}`, { method: 'PUT', body: JSON.stringify({ value, environment }) }),
  delete: (owner: string, repo: string, name: string, environment: string | null = null) => request<void>(`/repos/${owner}/${repo}/actions/secrets/${encodeURIComponent(name)}${environment ? `?environment=${encodeURIComponent(environment)}` : ''}`, { method: 'DELETE' }),
};
