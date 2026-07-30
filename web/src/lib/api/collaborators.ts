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
  // `{id}` here is the collaborator's **user** id, while `{id}` on the PATCH
  // above is the `repo_collaborators` row id — the same URL position, two
  // different keys. axum refuses to mount the two verbs with different segment
  // names, so the URL cannot carry the distinction and callers must: pass
  // `collaborator.user_id`, not `collaborator.id`.
  remove: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/collaborators/${id}`, { method: 'DELETE' }),
};
