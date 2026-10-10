import { request, qs, type PaginatedResponse } from './_base.svelte';
import { repoPath } from './repoPath';

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
    request<PaginatedResponse<any>>(`${repoPath(owner, repo)}/pipelines${qs({ page, per_page: perPage })}`),
  get: (owner: string, repo: string, id: number) =>
    request<any>(`${repoPath(owner, repo)}/pipelines/${id}`),
  workflowDispatchSchema: (owner: string, repo: string, ref?: string) =>
    request<WorkflowDispatchSchemaResponse>(
      `${repoPath(owner, repo)}/pipelines/workflow-dispatch${qs({ ref })}`,
    ),
  trigger: (owner: string, repo: string, ref?: string, inputs: Record<string, string> = {}) =>
    request<TriggerPipelineResponse>(`${repoPath(owner, repo)}/pipelines`, {
      method: 'POST',
      body: JSON.stringify({ ref, inputs }),
    }),
  retry: (owner: string, repo: string, id: number) =>
    request<any>(`${repoPath(owner, repo)}/pipelines/${id}/retry`, { method: 'POST' }),
  cancel: (owner: string, repo: string, id: number) =>
    request<any>(`${repoPath(owner, repo)}/pipelines/${id}/cancel`, { method: 'POST' }),
  job: (owner: string, repo: string, pipelineId: number, jobId: number) =>
    request<any>(`${repoPath(owner, repo)}/pipelines/${pipelineId}/jobs/${jobId}`),
  jobLog: (owner: string, repo: string, pipelineId: number, jobId: number, offset = 0, limit = 262144) =>
    request<{ content: string; offset: number; next_offset: number; total_length: number }>(
      `${repoPath(owner, repo)}/pipelines/${pipelineId}/jobs/${jobId}/log${qs({ offset, limit })}`,
    ),
  play: (owner: string, repo: string, pipelineId: number, jobId: number) =>
    request<any>(`${repoPath(owner, repo)}/pipelines/${pipelineId}/jobs/${jobId}/play`, { method: 'POST' }),
  approve: (owner: string, repo: string, pipelineId: number, jobId: number) =>
    request<any>(`${repoPath(owner, repo)}/pipelines/${pipelineId}/jobs/${jobId}/approve`, { method: 'POST' }),
};
