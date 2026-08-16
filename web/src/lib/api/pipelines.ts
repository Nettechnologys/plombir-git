import { request, qs, type PaginatedResponse } from './_base.svelte';

export interface TriggerPipelineResponse {
  id: number;
  status: string;
  commit_sha: string;
  ref_name: string;
}

export type WorkflowDispatchInputType = 'boolean' | 'choice' | 'number' | 'environment' | 'string';

export interface WorkflowDispatchInput {
  name: string;
  description: string | null;
  required: boolean;
  type: WorkflowDispatchInputType;
  default: string | null;
  options: string[];
}

export interface WorkflowDispatchWorkflow {
  path: string;
  name: string;
  inputs: WorkflowDispatchInput[];
}

export interface WorkflowDispatchSchemaResponse {
  ref_name: string;
  commit_sha: string;
  inputs: WorkflowDispatchInput[];
  workflows: WorkflowDispatchWorkflow[];
}

export const pipelines = {
  list: (owner: string, repo: string, page?: number, perPage?: number) =>
    request<PaginatedResponse<any>>(`/repos/${owner}/${repo}/pipelines${qs({ page, per_page: perPage })}`),
  get: (owner: string, repo: string, id: number) =>
    request<any>(`/repos/${owner}/${repo}/pipelines/${id}`),
  workflowDispatchSchema: (owner: string, repo: string, ref?: string) =>
    request<WorkflowDispatchSchemaResponse>(
      `/repos/${owner}/${repo}/pipelines/workflow-dispatch${qs({ ref })}`,
    ),
  trigger: (owner: string, repo: string, ref?: string, inputs: Record<string, string> = {}) =>
    request<TriggerPipelineResponse>(`/repos/${owner}/${repo}/pipelines`, {
      method: 'POST',
      body: JSON.stringify({ ref, inputs }),
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
