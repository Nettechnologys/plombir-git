import { request } from './_base.svelte';

export const tokens = {
  list: () =>
    request<Array<{
      id: number;
      name: string;
      scopes: string;
      expires_at?: string | null;
      last_used_at?: string | null;
      created_at: string;
    }>>('/users/tokens'),
  create: (name: string, scopes?: string, expires_at?: string) =>
    request<{ id: number; name: string; token: string; scopes: string; expires_at?: string; created_at: string }>('/users/tokens', {
      method: 'POST',
      body: JSON.stringify({ name, scopes, expires_at }),
    }),
  delete: (id: number) =>
    request<void>(`/users/tokens/${id}`, { method: 'DELETE' }),
};
