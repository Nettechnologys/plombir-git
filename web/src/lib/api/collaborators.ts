import { request } from './_base.svelte';
import { buildUserRef } from './userRef';
import { repoPath } from './repoPath';

export interface Collaborator {
  id: number;
  repo_id: number;
  user_id: number;
  /** `null` when the collaborator row outlives the account it names. */
  username: string | null;
  display_name: string | null;
  permission: 'read' | 'write' | 'admin' | string;
  created_at: string;
}

export const collaborators = {
  list: (owner: string, repo: string) =>
    request<Collaborator[]>(`${repoPath(owner, repo)}/collaborators`),
  add: (owner: string, repo: string, userIdentifier: number | string, permission: string) => {
    // Same three keys, same reading of them, as every other place that hands
    // out access — see `buildUserRef`.
    const user = buildUserRef(userIdentifier);
    if (user === null) return Promise.reject(new Error('A user is required.'));
    return request<any>(`${repoPath(owner, repo)}/collaborators`, {
      method: 'POST',
      body: JSON.stringify({ ...user, permission }),
    });
  },
  updatePermission: (owner: string, repo: string, id: number, permission: string) =>
    request<any>(`${repoPath(owner, repo)}/collaborators/${id}`, {
      method: 'PATCH',
      body: JSON.stringify({ permission }),
    }),
  // `{id}` here is the collaborator's **user** id, while `{id}` on the PATCH
  // above is the `repo_collaborators` row id — the same URL position, two
  // different keys. axum refuses to mount the two verbs with different segment
  // names, so the URL cannot carry the distinction and callers must: pass
  // `collaborator.user_id`, not `collaborator.id`.
  //
  // Passing the wrong one is at least no longer silent: a delete that matches
  // no row answers 404 instead of 204, so this rejects rather than pretending.
  remove: (owner: string, repo: string, id: number) =>
    request<void>(`${repoPath(owner, repo)}/collaborators/${id}`, { method: 'DELETE' }),
};
