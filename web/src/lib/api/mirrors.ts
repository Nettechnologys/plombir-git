import { request } from './_base.svelte';
import { repoPath } from './repoPath';

export interface RepositoryMirror {
  id: number;
  repo_id: number;
  url: string;
  username: string | null;
  /** Whether a password is stored for the remote — the value itself never leaves the server. */
  has_credentials: boolean;
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
    request<RepositoryMirror>(`${repoPath(owner, repo)}/mirror`),
  create: (owner: string, repo: string, payload: MirrorPayload) =>
    request<RepositoryMirror>(`${repoPath(owner, repo)}/mirror`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  update: (owner: string, repo: string, payload: Partial<MirrorPayload> & { status?: string }) =>
    request<RepositoryMirror>(`${repoPath(owner, repo)}/mirror`, {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  remove: (owner: string, repo: string) =>
    request<void>(`${repoPath(owner, repo)}/mirror`, { method: 'DELETE' }),
  sync: (owner: string, repo: string) =>
    request<{ status: string }>(`${repoPath(owner, repo)}/mirror/sync`, { method: 'POST' }),
};
