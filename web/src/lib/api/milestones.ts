import { request } from './_base.svelte';

export interface Milestone {
  id: number;
  repo_id: number;
  title: string;
  description: string | null;
  state: 'open' | 'closed';
  due_date: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreateMilestonePayload {
  title: string;
  description?: string;
  due_date?: string;
  state?: Milestone['state'];
}

export interface UpdateMilestonePayload {
  title?: string;
  description?: string | null;
  state?: Milestone['state'];
  due_date?: string | null;
}

export const milestones = {
  list: (owner: string, repo: string, state?: string) => {
    const params = new URLSearchParams();
    if (state) params.set('state', state);
    const qs = params.toString() ? `?${params.toString()}` : '';
    return request<Milestone[]>(`/repos/${owner}/${repo}/milestones${qs}`);
  },
  get: (owner: string, repo: string, id: number) =>
    request<Milestone>(`/repos/${owner}/${repo}/milestones/${id}`),
  create: (owner: string, repo: string, data: CreateMilestonePayload) =>
    request<Milestone>(`/repos/${owner}/${repo}/milestones`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  update: (owner: string, repo: string, id: number, data: UpdateMilestonePayload) =>
    request<Milestone>(`/repos/${owner}/${repo}/milestones/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  delete: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/milestones/${id}`, { method: 'DELETE' }),
};
