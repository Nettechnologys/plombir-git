import { request } from './_base.svelte';
import { repoPath } from './repoPath';

/** `environment` is `null` for a repository-wide secret, read by every job. */
export interface CiSecret { name: string; environment: string | null; created_at: string; updated_at: string; }
const scopeQuery = (environment: string | null) => (environment ? `?environment=${encodeURIComponent(environment)}` : '');
export const ciSecrets = {
  list: (owner: string, repo: string) => request<CiSecret[]>(`${repoPath(owner, repo)}/actions/secrets`),
  put: (owner: string, repo: string, name: string, value: string, environment: string | null = null) => request<CiSecret>(`${repoPath(owner, repo)}/actions/secrets/${encodeURIComponent(name)}`, { method: 'PUT', body: JSON.stringify({ value, environment }) }),
  delete: (owner: string, repo: string, name: string, environment: string | null = null) => request<void>(`${repoPath(owner, repo)}/actions/secrets/${encodeURIComponent(name)}${scopeQuery(environment)}`, { method: 'DELETE' }),
};
