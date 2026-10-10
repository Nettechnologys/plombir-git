import { request } from './_base.svelte';

export type SigningKeyKind = 'gpg' | 'ssh';

export interface SigningKey {
  id: number;
  title: string;
  kind: SigningKeyKind;
  public_key: string;
  fingerprint: string;
  created_at: string;
}

export const signingKeys = {
  list: () => request<SigningKey[]>('/users/signing-keys'),
  create: (title: string, kind: SigningKeyKind, public_key: string) =>
    request<SigningKey>('/users/signing-keys', {
      method: 'POST',
      body: JSON.stringify({ title, kind, public_key }),
    }),
  delete: (id: number) => request<void>(`/users/signing-keys/${id}`, { method: 'DELETE' }),
};
