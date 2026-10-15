import { request } from './_base.svelte';
import { repoPath } from './repoPath';

export interface PushMirror {
  id: number;
  url: string;
  username: string | null;
  /** Whether a password is stored for the remote — the value itself never leaves the server. */
  has_credentials: boolean;
  sync_on_commit: boolean;
  /** `0`: pushed on commit and on request only. */
  sync_interval_seconds: number;
  /** The remote is kept an exact copy: rewritten branches are overwritten, deleted ones removed. */
  force: boolean;
  enabled: boolean;
  next_sync_at: string | null;
  /** The first ref move the remote has not received; `null` when it is up to date. */
  pending_since: string | null;
  last_sync_at: string | null;
  last_success_at: string | null;
  last_sync_error: string | null;
  created_at: string;
  updated_at: string;
}

export interface CreatePushMirrorPayload {
  url: string;
  username?: string;
  password?: string;
  sync_on_commit: boolean;
  sync_interval_seconds: number;
  force: boolean;
}

export interface UpdatePushMirrorPayload {
  url?: string;
  username?: string;
  password?: string;
  sync_on_commit?: boolean;
  sync_interval_seconds?: number;
  force?: boolean;
  enabled?: boolean;
}

export const pushMirrors = {
  list: (owner: string, repo: string) =>
    request<PushMirror[]>(`${repoPath(owner, repo)}/push_mirrors`),
  create: (owner: string, repo: string, payload: CreatePushMirrorPayload) =>
    request<PushMirror>(`${repoPath(owner, repo)}/push_mirrors`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  update: (owner: string, repo: string, id: number, payload: UpdatePushMirrorPayload) =>
    request<PushMirror>(`${repoPath(owner, repo)}/push_mirrors/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  remove: (owner: string, repo: string, id: number) =>
    request<void>(`${repoPath(owner, repo)}/push_mirrors/${id}`, { method: 'DELETE' }),
  sync: (owner: string, repo: string, id: number) =>
    request<{ status: string }>(`${repoPath(owner, repo)}/push_mirrors/${id}/sync`, {
      method: 'POST',
    }),
};
