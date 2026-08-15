import { request } from './_base.svelte';

export interface LabelPayload {
  name: string;
  color: string;
  /** `null` clears the description; leaving the key out keeps whatever is stored. */
  description: string | null;
}

export const labels = {
  list: (owner: string, repo: string) =>
    request<any[]>(`/repos/${owner}/${repo}/labels`),
  get: (owner: string, repo: string, id: number) =>
    request<any>(`/repos/${owner}/${repo}/labels/${id}`),
  create: (owner: string, repo: string, payload: LabelPayload) =>
    request<any>(`/repos/${owner}/${repo}/labels`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  update: (owner: string, repo: string, id: number, payload: Partial<LabelPayload>) =>
    request<any>(`/repos/${owner}/${repo}/labels/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  delete: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/labels/${id}`, { method: 'DELETE' }),
  forIssue: (owner: string, repo: string, issueNumber: number) =>
    request<any[]>(`/repos/${owner}/${repo}/issues/${issueNumber}/labels`),
};
