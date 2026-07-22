import { request } from './_base.svelte';

export interface StartImportPayload {
  platform: 'github' | 'gitlab' | 'gitea' | 'git';
  source_url: string;
  target_owner: string;
  target_name?: string;
  auth_token?: string;
  import_repo?: boolean;
  import_issues?: boolean;
  import_pull_requests?: boolean;
  import_wiki?: boolean;
  import_releases?: boolean;
  import_labels?: boolean;
  import_milestones?: boolean;
}

export interface ImportTask {
  id: number;
  user_id: number;
  repo_id: number | null;
  platform: string;
  source_url: string;
  target_owner: string;
  target_name: string;
  status: string;
  progress: number;
  stage: string | null;
  error: string | null;
  stats: string | null;
  created_at: string;
  updated_at: string;
}

export const imports = {
  list: () =>
    request<ImportTask[]>('/imports'),
  start: (payload: StartImportPayload) =>
    request<ImportTask>('/imports', {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  get: (id: number) =>
    request<ImportTask>(`/imports/${id}`),
  remove: (id: number) =>
    request<void>(`/imports/${id}`, { method: 'DELETE' }),
};
