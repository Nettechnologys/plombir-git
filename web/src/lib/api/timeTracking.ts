import { request, qs, type PaginatedResponse } from './_base.svelte';
import { repoPath } from './repoPath';

export const timeTracking = {
  list: (owner: string, repo: string, issueNumber: number, page?: number, perPage?: number) =>
    request<PaginatedResponse<any>>(`${repoPath(owner, repo)}/issues/${issueNumber}/time${qs({ page, per_page: perPage })}`),
  add: (owner: string, repo: string, issueNumber: number, data: { duration_minutes: number; description?: string }) =>
    request<any>(`${repoPath(owner, repo)}/issues/${issueNumber}/time`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  total: (owner: string, repo: string, issueNumber: number) =>
    request<{ total_minutes: number; total_formatted: string }>(`${repoPath(owner, repo)}/issues/${issueNumber}/time/total`),
  delete: (owner: string, repo: string, issueNumber: number, id: number) =>
    request<void>(`${repoPath(owner, repo)}/issues/${issueNumber}/time/${id}`, { method: 'DELETE' }),
};
