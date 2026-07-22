import { request } from './_base.svelte';

export interface SshKey {
  id: number;
  title: string;
  public_key: string;
  fingerprint: string;
  created_at: string;
  last_used_at?: string | null;
}

export const sshKeys = {
  list: () => request<SshKey[]>('/users/ssh-keys'),
  create: (title: string, public_key: string) =>
    request<SshKey>('/users/ssh-keys', {
      method: 'POST',
      body: JSON.stringify({ title, public_key }),
    }),
  delete: (id: number) =>
    request<void>(`/users/ssh-keys/${id}`, { method: 'DELETE' }),
};
