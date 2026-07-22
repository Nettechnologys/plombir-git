import { request } from './_base.svelte';

export interface TagProtection { id: number; pattern: string; allowed_user_ids: number[]; created_at: string; updated_at: string; }
export const tagProtections = {
  list: (owner: string, repo: string) => request<TagProtection[]>(`/repos/${owner}/${repo}/tags/protection`),
  create: (owner: string, repo: string, pattern: string) => request<TagProtection>(`/repos/${owner}/${repo}/tags/protection`, { method: 'POST', body: JSON.stringify({ pattern, allowed_user_ids: [] }) }),
  delete: (owner: string, repo: string, id: number) => request<void>(`/repos/${owner}/${repo}/tags/protection/${id}`, { method: 'DELETE' }),
};
