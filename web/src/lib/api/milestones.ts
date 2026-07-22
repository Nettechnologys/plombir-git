import { request } from './_base.svelte';

export const milestones = {
  list: (owner: string, repo: string, state?: string) => {
    const params = new URLSearchParams();
    if (state) params.set('state', state);
    const qs = params.toString() ? `?${params.toString()}` : '';
    return request<any[]>(`/repos/${owner}/${repo}/milestones${qs}`);
  },
  get: (owner: string, repo: string, id: number) =>
    request<any>(`/repos/${owner}/${repo}/milestones/${id}`),
  create: (owner: string, repo: string, data: { title: string; description?: string; due_date?: string; state?: string }) =>
    request<any>(`/repos/${owner}/${repo}/milestones`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  update: (owner: string, repo: string, id: number, data: { title?: string; description?: string; state?: string; due_date?: string }) =>
    request<any>(`/repos/${owner}/${repo}/milestones/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  delete: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/milestones/${id}`, { method: 'DELETE' }),
};
