import { request, qs } from './_base.svelte';
import { repoPath } from './repoPath';

/** One object in a repository's LFS store. */
export interface LfsObject {
  oid: string;
  size: number;
  /** `false` for an object announced to the batch API whose bytes never arrived. */
  uploaded: boolean;
  created_at: string;
}

export interface LfsUsage {
  object_count: number;
  total_bytes: number;
}

export interface LfsPruneOutcome {
  deleted: string[];
  kept: { oid: string; reason: string }[];
}

export const lfsStorage = {
  usage: (owner: string, repo: string) => request<LfsUsage>(`${repoPath(owner, repo)}/lfs/usage`),
  objects: (owner: string, repo: string, cursor?: string) =>
    request<{ objects: LfsObject[]; next_cursor: string }>(
      `${repoPath(owner, repo)}/lfs/objects${qs({ cursor, limit: 100 })}`,
    ),
  /** Objects no ref's history points at, old enough to remove. Reads the whole history. */
  orphans: (owner: string, repo: string) =>
    request<{ objects: LfsObject[]; grace_hours: number }>(`${repoPath(owner, repo)}/lfs/orphans`, {
      method: 'GET',
      timeoutMs: 300_000,
    } as RequestInit),
  prune: (owner: string, repo: string, oids: string[]) =>
    request<LfsPruneOutcome>(`${repoPath(owner, repo)}/lfs/orphans/prune`, {
      method: 'POST',
      body: JSON.stringify({ oids }),
      timeoutMs: 300_000,
    } as RequestInit),
};
