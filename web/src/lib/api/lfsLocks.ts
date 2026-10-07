import { request, qs } from './_base.svelte';

/** A Git LFS lock, as the locking API spells it. */
export interface LfsLock {
  id: string;
  path: string;
  locked_at: string;
  owner: { name: string };
}

interface LfsLockPage {
  locks: LfsLock[];
  next_cursor: string;
}

export const lfsLocks = {
  /** One page of a repository's locks; `next_cursor` is empty on the last. */
  list: (owner: string, repo: string, cursor?: string) =>
    request<LfsLockPage>(`/repos/${owner}/${repo}/lfs/locks${qs({ cursor, limit: 100 })}`),
  /** Take someone else's lock off. The server allows it to repository administrators only. */
  forceUnlock: (owner: string, repo: string, id: string) =>
    request<{ lock: LfsLock }>(`/repos/${owner}/${repo}/lfs/locks/${encodeURIComponent(id)}/unlock`, {
      method: 'POST',
      body: JSON.stringify({ force: true }),
    }),
};
