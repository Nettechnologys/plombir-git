import {
  downloadApiFile,
  request,
  requestBytes,
  qs,
  withApiBase,
  type PaginatedResponse,
} from './_base.svelte';
import { repoPath, repoOwnerPath } from './repoPath';

function encodeRepoPath(path: string): string {
  return path.split('/').map(encodeURIComponent).join('/');
}

/** The LFS object a pointer file names, as the blob API reports it. */
export interface BlobLfs {
  oid: string;
  size: number;
  /** Whether the repository stores the object; `false` means the raw route answers 404. */
  available: boolean;
}

export interface BlobResponse {
  path: string;
  content: string;
  size: number;
  name: string;
  sha: string;
  encoding: string;
  is_binary: boolean;
  lfs?: BlobLfs;
}

interface FileOperationResponse {
  success: boolean;
  file_path: string;
  commit_sha: string;
}

// `GET /branches` answers with objects (`BranchRef` in `repo_content.rs`) so the
// UI can mark the default branch; `GET /tags` answers with bare names, which is
// all a tag picker needs. Both are normalized to `{ name }`-shaped objects here
// so every page consumes one shape.
type BranchRefResponse = { name: string; is_default: boolean };
type TagRefResponse = string;

/** `RefChangeResponse` in `repo_content.rs`: the full ref and the commit it names (or named, for a deletion). */
export interface RefChange {
  ref: string;
  sha: string;
}

export interface Stargazer {
  user_id: number;
  username: string;
  display_name?: string | null;
  avatar_url?: string | null;
  starred_at: string;
}

export interface RepositoryFork {
  id: number;
  owner_id: number;
  owner_name: string;
  name: string;
  description?: string | null;
  is_private: boolean;
  default_branch: string;
  fork_id: number | null;
  stars_count: number;
  forks_count: number;
  org_id: number | null;
  created_at: string;
  updated_at: string;
  deleted_at: string | null;
  origin_repo_id: number | null;
}

export type RepoTreeEntryKind = 'tree' | 'blob' | 'commit';

export interface RepoTreeEntry {
  name: string;
  kind: RepoTreeEntryKind;
  size?: number | null;
}

/**
 * What `GET /commits/{sha}/signature` concluded (`SignatureVerdict` in
 * `crates/rg-http/src/api/repo_content.rs`).
 *
 * `undeterminable` is the one that used to be missing: the server has no public
 * key for the signer, so it checked nothing. Rendering that as a negative
 * verdict accuses a commit nobody has a complaint about (card_61b29791d099).
 */
export type SignatureVerdict = 'valid' | 'invalid' | 'undeterminable' | 'unsigned';

export interface CommitSignature {
  verdict: SignatureVerdict;
  signer_key: string | null;
  signer_name: string | null;
  signer_email: string | null;
  status: string;
}

/**
 * What the signed-in caller may do in a repository, as `GET /repos/{owner}/{name}`
 * reports it (`RepoResponse::viewer_permission`); absent for an anonymous
 * reader. It decides which actions a page offers — every write is still
 * decided by its own route.
 */
export type RepoPermission = 'admin' | 'write' | 'read';

export interface RepositoryDetail {
  id: number;
  owner_id: number;
  name: string;
  description: string | null;
  is_private: boolean;
  default_branch: string;
  stars_count: number;
  forks_count: number;
  created_at: string;
  viewer_permission?: RepoPermission;
  /**
   * The namespace the repository lives in now. Differs from the address that
   * was asked for when a rename or a transfer left it and the API redirected
   * the read here.
   */
  owner_name?: string;
}

/**
 * `PATCH /repos/{owner}/{name}` (card_3625a7b89abb), repository administrators
 * only. Every key is optional and only the ones present change; `description:
 * null` clears it. 400 unknown branch / invalid name / description too long,
 * 409 the name is taken in this namespace.
 */
export interface RepoSettingsPatch {
  description?: string | null;
  is_private?: boolean;
  default_branch?: string;
  name?: string;
}

function normalizeTagRef(tag: TagRefResponse): { name: string } {
  return { name: tag };
}

export const repos = {
  list: (owner: string, page?: number, perPage?: number) =>
    request<PaginatedResponse<{ id: number; name: string; description: string | null; is_private: boolean; created_at: string }>>(
      `${repoOwnerPath(owner)}${qs({ page, per_page: perPage })}`
    ),
  explore: (page?: number, perPage?: number) =>
    request<PaginatedResponse<{ id: number; owner_id: number; name: string; description: string | null; stars_count: number; updated_at: string }>>(
      `/repos/explore${qs({ page, per_page: perPage })}`
    ),
  get: (owner: string, name: string) =>
    request<RepositoryDetail>(`${repoPath(owner, name)}`),
  /** The answer is the updated row; after a rename it carries the new `name`. */
  update: (owner: string, name: string, patch: RepoSettingsPatch) =>
    request<RepositoryDetail>(`${repoPath(owner, name)}`, {
      method: 'PATCH',
      body: JSON.stringify(patch),
    }),
  create: (opts: {
    name: string;
    description?: string;
    is_private?: boolean;
    org?: string;
    auto_init?: boolean;
    default_branch?: string;
    gitignores?: string;
    license?: string;
    readme?: string;
    issue_labels?: string;
  }) =>
    request<{ id: number; name: string }>('/repos', {
      method: 'POST',
      body: JSON.stringify(opts),
    }),
  templates: {
    gitignores: () => request<{ data: { key: string; name: string; description: string }[] }>('/repos/templates/gitignores'),
    licenses: () => request<{ data: { key: string; name: string; description: string }[] }>('/repos/templates/licenses'),
    readmes: () => request<{ data: { key: string; name: string; description: string }[] }>('/repos/templates/readmes'),
    labels: () => request<{ data: { key: string; name: string; description: string }[] }>('/repos/templates/labels'),
  },
  tree: (owner: string, repo: string, ref?: string, path?: string) => {
    return request<{ entries: RepoTreeEntry[] }>(`${repoPath(owner, repo)}/tree${qs({ ref, path })}`);
  },
  blob: (owner: string, repo: string, path: string, ref?: string) => {
    return request<BlobResponse>(`${repoPath(owner, repo)}/blob/${encodeRepoPath(path)}${qs({ ref })}`);
  },
  /** The file's bytes — for an LFS pointer, the object — as an `<img src>`. */
  rawUrl: (owner: string, repo: string, path: string, ref?: string) =>
    withApiBase(`${repoPath(owner, repo)}/raw/${encodeRepoPath(path)}${qs({ ref })}`),
  rawBytes: (owner: string, repo: string, path: string, ref?: string) =>
    requestBytes(`${repoPath(owner, repo)}/raw/${encodeRepoPath(path)}${qs({ ref })}`),
  downloadRaw: (owner: string, repo: string, path: string, ref?: string) =>
    downloadApiFile(
      `${repoPath(owner, repo)}/raw/${encodeRepoPath(path)}${qs({ ref })}`,
      path.split('/').pop() || 'download',
    ),
  saveContent: (
    owner: string,
    repo: string,
    path: string,
    data: { branch?: string; content: string; message: string; sha?: string }
  ) =>
    request<FileOperationResponse>(`${repoPath(owner, repo)}/contents/${encodeRepoPath(path)}`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  deleteContent: (
    owner: string,
    repo: string,
    path: string,
    data: { branch?: string; message: string; sha: string }
  ) =>
    request<FileOperationResponse>(`${repoPath(owner, repo)}/contents/${encodeRepoPath(path)}${qs({
      branch: data.branch,
      message: data.message,
      sha: data.sha,
    })}`, {
      method: 'DELETE',
    }),
  /**
   * `ref` omitted walks the server's HEAD — the repository's own default
   * branch, whatever it is called. `limit` is 1..=100 server-side (default 50);
   * the endpoint has no offset, so a caller pages by passing the last SHA it
   * holds as `ref` (see the commits page).
   */
  log: (owner: string, repo: string, ref?: string, path?: string, limit?: number, skip?: number) => {
    return request<{ commits: { sha: string; message: string; author: string; date: string }[] }>(`${repoPath(owner, repo)}/log${qs({ ref, path, limit, skip: skip || undefined })}`);
  },
  branches: (owner: string, repo: string) =>
    request<BranchRefResponse[]>(`${repoPath(owner, repo)}/branches`),
  tags: (owner: string, repo: string) =>
    request<TagRefResponse[]>(`${repoPath(owner, repo)}/tags`).then((tags) => tags.map(normalizeTagRef)),
  /**
   * Create a branch (card_2060696224ff). `from` is a branch, tag or SHA; the
   * server uses the default branch when it is left out. Held to the same rules
   * as a `git push` of that branch: 400 bad name / unresolvable `from`, 403 a
   * token kept off protected branches, 409 exists or a push rule refuses it.
   */
  createBranch: (owner: string, repo: string, data: { name: string; from?: string }) =>
    request<RefChange>(`${repoPath(owner, repo)}/branches`, {
      method: 'POST',
      body: JSON.stringify(data.from ? { name: data.name, from: data.from } : { name: data.name }),
    }),
  /**
   * Delete a branch. The name travels as ONE path segment, so `feature/x` is
   * sent as `feature%2Fx`. 404 no such branch; 409 the default or a protected
   * branch, or it moved meanwhile.
   */
  deleteBranch: (owner: string, repo: string, branch: string) =>
    request<RefChange>(`${repoPath(owner, repo)}/branches/${encodeURIComponent(branch)}`, { method: 'DELETE' }),
  /** Delete a tag, encoded as one segment like `deleteBranch`. 409 a protected tag. */
  deleteTag: (owner: string, repo: string, tag: string) =>
    request<RefChange>(`${repoPath(owner, repo)}/tags/${encodeURIComponent(tag)}`, { method: 'DELETE' }),
  commitSignature: (owner: string, repo: string, sha: string) =>
    request<CommitSignature>(`${repoPath(owner, repo)}/commits/${sha}/signature`),
  star: (owner: string, repo: string) =>
    request<{ starred: boolean }>(`${repoPath(owner, repo)}/star`, { method: 'PUT' }),
  starred: (owner: string, repo: string) =>
    request<{ starred: boolean }>(`${repoPath(owner, repo)}/starred`, { method: 'GET' }),
  unstar: async (owner: string, repo: string) => {
    const status = await repos.starred(owner, repo);
    if (!status.starred) return { starred: false };
    return repos.star(owner, repo);
  },
  stargazers: (owner: string, repo: string, page?: number, perPage?: number) =>
    request<PaginatedResponse<Stargazer>>(`${repoPath(owner, repo)}/stargazers${qs({ page, per_page: perPage })}`),
  watch: (owner: string, repo: string, state: string) =>
    request<{ watch_state: string }>(`${repoPath(owner, repo)}/watch`, { method: 'PUT', body: JSON.stringify({ state }) }),
  watchStatus: (owner: string, repo: string) =>
    request<{ watch_state: 'not_watching' | 'watching' | 'ignoring' }>(`${repoPath(owner, repo)}/watch`, { method: 'GET' }),
  unwatch: (owner: string, repo: string) =>
    request<{ watch_state: string }>(`${repoPath(owner, repo)}/watch`, { method: 'DELETE' }),
  delete: (owner: string, repo: string) =>
    request<{ deleted: boolean }>(`${repoPath(owner, repo)}`, { method: 'DELETE' }),
  fork: (owner: string, repo: string, opts?: { org?: string }) =>
    request<any>(`${repoPath(owner, repo)}/fork`, {
      method: 'POST',
      ...(opts ? { body: JSON.stringify(opts) } : {}),
    }),
  forks: (owner: string, repo: string, page?: number, perPage?: number) =>
    request<PaginatedResponse<RepositoryFork>>(`${repoPath(owner, repo)}/forks${qs({ page, per_page: perPage })}`),
  transfer: (owner: string, repo: string, newOwner: string) =>
    request<any>(`${repoPath(owner, repo)}/transfer`, { method: 'POST', body: JSON.stringify({ new_owner: newOwner }) }),
  createCommitStatus: (owner: string, repo: string, sha: string, data: { state: string; context: string; description?: string; target_url?: string }) =>
    request<any>(`${repoPath(owner, repo)}/statuses/${sha}`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  listCommitStatuses: (owner: string, repo: string, sha: string) =>
    request<any[]>(`${repoPath(owner, repo)}/commits/${sha}/statuses`),
  getCombinedStatus: (owner: string, repo: string, sha: string) =>
    request<any>(`${repoPath(owner, repo)}/commits/${sha}/status`),
};
