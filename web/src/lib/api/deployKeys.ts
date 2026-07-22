import { request } from './_base.svelte';

export interface DeployKey {
  id: number;
  title: string;
  public_key: string;
  fingerprint: string;
  read_only: boolean;
  created_by_id: number;
  created_at: string;
  last_used_at?: string | null;
}

export const deployKeys = {
  list: (owner: string, repo: string) =>
    request<DeployKey[]>(`/repos/${owner}/${repo}/keys`),
  create: (owner: string, repo: string, title: string, public_key: string, read_only: boolean) =>
    request<DeployKey>(`/repos/${owner}/${repo}/keys`, {
      method: 'POST',
      body: JSON.stringify({ title, public_key, read_only }),
    }),
  delete: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/keys/${id}`, { method: 'DELETE' }),
};
