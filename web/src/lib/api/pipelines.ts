import { request, qs, type PaginatedResponse } from './_base.svelte';

export const pipelines = {
  list: (owner: string, repo: string, page?: number, perPage?: number) =>
    request<PaginatedResponse<any>>(`/repos/${owner}/${repo}/pipelines${qs({ page, per_page: perPage })}`),
  get: (owner: string, repo: string, id: number) =>
    request<any>(`/repos/${owner}/${repo}/pipelines/${id}`),
  trigger: (owner: string, repo: string, ref?: string) =>
    request<any>(`/repos/${owner}/${repo}/pipelines`, {
      method: 'POST',
      body: JSON.stringify({ ref }),
    }),
  retry: (owner: string, repo: string, id: number) =>
    request<any>(`/repos/${owner}/${repo}/pipelines/${id}/retry`, { method: 'POST' }),
  cancel: (owner: string, repo: string, id: number) =>
    request<any>(`/repos/${owner}/${repo}/pipelines/${id}/cancel`, { method: 'POST' }),
  job: (owner: string, repo: string, pipelineId: number, jobId: number) =>
    request<any>(`/repos/${owner}/${repo}/pipelines/${pipelineId}/jobs/${jobId}`),
  play: (owner: string, repo: string, pipelineId: number, jobId: number) =>
    request<any>(`/repos/${owner}/${repo}/pipelines/${pipelineId}/jobs/${jobId}/play`, { method: 'POST' }),
  approve: (owner: string, repo: string, pipelineId: number, jobId: number) =>
    request<any>(`/repos/${owner}/${repo}/pipelines/${pipelineId}/jobs/${jobId}/approve`, { method: 'POST' }),
};
