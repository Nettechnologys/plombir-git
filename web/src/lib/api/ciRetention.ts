import { request } from './_base.svelte';
export interface CiRetentionPolicy { artifact_retention_days: number; cache_retention_days: number; }
export interface CiCleanupResult { artifacts_deleted: number; caches_deleted: number; failures: number; }
export const ciRetention = {
  get: (owner: string, repo: string) => request<CiRetentionPolicy>(`/repos/${owner}/${repo}/actions/retention`),
  update: (owner: string, repo: string, policy: CiRetentionPolicy) => request<CiRetentionPolicy>(`/repos/${owner}/${repo}/actions/retention`, { method: 'PUT', body: JSON.stringify(policy) }),
  cleanup: (owner: string, repo: string) => request<CiCleanupResult>(`/repos/${owner}/${repo}/actions/retention/expired`, { method: 'DELETE' }),
};
