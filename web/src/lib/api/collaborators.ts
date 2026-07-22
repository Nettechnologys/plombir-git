import { request } from './_base.svelte';

export const collaborators = {
  list: (owner: string, repo: string) =>
    request<any[]>(`/repos/${owner}/${repo}/collaborators`),
  add: (owner: string, repo: string, userIdentifier: number | string, permission: string) => {
    const raw = String(userIdentifier).trim();
    const numericId = typeof userIdentifier === 'number' || /^\d+$/.test(raw) ? Number(raw) : null;
    const payload =
      numericId && Number.isInteger(numericId) && numericId > 0
        ? { user_id: numericId, permission }
        : raw.includes('@')
          ? { email: raw, permission }
          : { username: raw, permission };
    return request<any>(`/repos/${owner}/${repo}/collaborators`, {
      method: 'POST',
      body: JSON.stringify(payload),
    });
  },
  updatePermission: (owner: string, repo: string, id: number, permission: string) =>
    request<any>(`/repos/${owner}/${repo}/collaborators/${id}`, {
      method: 'PATCH',
      body: JSON.stringify({ permission }),
    }),
  remove: (owner: string, repo: string, userId: number) =>
    request<void>(`/repos/${owner}/${repo}/collaborators/${userId}`, { method: 'DELETE' }),
};
