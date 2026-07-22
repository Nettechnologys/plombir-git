import { request } from './_base.svelte';

export const labels = {
  list: (owner: string, repo: string) =>
    request<any[]>(`/repos/${owner}/${repo}/labels`),
  get: (owner: string, repo: string, id: number) =>
    request<any>(`/repos/${owner}/${repo}/labels/${id}`),
  create: (owner: string, repo: string, name: string, color: string, description?: string) =>
    request<any>(`/repos/${owner}/${repo}/labels`, {
      method: 'POST',
      body: JSON.stringify({ name, color, description }),
    }),
  update: (owner: string, repo: string, id: number, data: { name?: string; color?: string; description?: string }) =>
    request<any>(`/repos/${owner}/${repo}/labels/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  delete: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/labels/${id}`, { method: 'DELETE' }),
  forIssue: (owner: string, repo: string, issueNumber: number) =>
    request<any[]>(`/repos/${owner}/${repo}/issues/${issueNumber}/labels`),
};
