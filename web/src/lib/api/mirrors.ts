import { request } from './_base.svelte';

export interface RepositoryMirror {
  id: number;
  repo_id: number;
  url: string;
  username: string | null;
  sync_interval_seconds: number;
  next_sync_at: string | null;
  last_sync_at: string | null;
  last_sync_error: string | null;
  status: string;
  created_at: string;
  updated_at: string;
}

export interface MirrorPayload {
  url: string;
  username?: string;
  password?: string;
  sync_interval_seconds: number;
}

export const mirrors = {
  get: (owner: string, repo: string) =>
    request<RepositoryMirror>(`/repos/${owner}/${repo}/mirror`),
  create: (owner: string, repo: string, payload: MirrorPayload) =>
    request<RepositoryMirror>(`/repos/${owner}/${repo}/mirror`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  update: (owner: string, repo: string, payload: Partial<MirrorPayload> & { status?: string }) =>
    request<RepositoryMirror>(`/repos/${owner}/${repo}/mirror`, {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  remove: (owner: string, repo: string) =>
    request<void>(`/repos/${owner}/${repo}/mirror`, { method: 'DELETE' }),
  sync: (owner: string, repo: string) =>
    request<{ status: string }>(`/repos/${owner}/${repo}/mirror/sync`, { method: 'POST' }),
};
