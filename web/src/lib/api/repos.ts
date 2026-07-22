import { request, qs, type PaginatedResponse } from './_base.svelte';

function encodeRepoPath(path: string): string {
  return path.split('/').map(encodeURIComponent).join('/');
}

interface FileOperationResponse {
  success: boolean;
  file_path: string;
  commit_sha: string;
}

type BranchRefResponse = string | { name: string; is_default?: boolean };
type TagRefResponse = string | { name: string };

function normalizeBranchRef(branch: BranchRefResponse): { name: string; is_default: boolean } {
  if (typeof branch === 'string') return { name: branch, is_default: false };
  return { name: branch.name, is_default: Boolean(branch.is_default) };
}

function normalizeTagRef(tag: TagRefResponse): { name: string } {
  if (typeof tag === 'string') return { name: tag };
  return { name: tag.name };
}

export const repos = {
  list: (owner: string, page?: number, perPage?: number) =>
    request<PaginatedResponse<{ id: number; name: string; description: string | null; is_private: boolean; created_at: string }>>(
      `/repos/${owner}${qs({ page, per_page: perPage })}`
    ),
  explore: (page?: number, perPage?: number) =>
    request<PaginatedResponse<{ id: number; owner_id: number; name: string; description: string | null; stars_count: number; updated_at: string }>>(
      `/repos/explore${qs({ page, per_page: perPage })}`
    ),
  get: (owner: string, name: string) =>
    request<{
      id: number;
      name: string;
      description: string | null;
      is_private: boolean;
      default_branch: string;
      stars_count: number;
      created_at: string;
    }>(`/repos/${owner}/${name}`),
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
    return request<{ entries: { name: string; kind: string; size?: number }[] }>(`/repos/${owner}/${repo}/tree${qs({ ref, path })}`);
  },
  blob: (owner: string, repo: string, path: string, ref?: string) => {
    return request<{ path: string; content: string; size: number; name: string; sha: string; encoding: string; is_binary: boolean }>(`/repos/${owner}/${repo}/blob/${encodeRepoPath(path)}${qs({ ref })}`);
  },
  saveContent: (
    owner: string,
    repo: string,
    path: string,
    data: { branch?: string; content: string; message: string; sha?: string }
  ) =>
    request<FileOperationResponse>(`/repos/${owner}/${repo}/contents/${encodeRepoPath(path)}`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  deleteContent: (
    owner: string,
    repo: string,
    path: string,
    data: { branch?: string; message: string; sha: string }
  ) =>
    request<FileOperationResponse>(`/repos/${owner}/${repo}/contents/${encodeRepoPath(path)}${qs({
      branch: data.branch,
      message: data.message,
      sha: data.sha,
    })}`, {
      method: 'DELETE',
    }),
  log: (owner: string, repo: string, ref?: string, path?: string) => {
    return request<{ commits: { sha: string; message: string; author: string; date: string }[] }>(`/repos/${owner}/${repo}/log${qs({ ref, path })}`);
  },
  branches: (owner: string, repo: string) =>
    request<BranchRefResponse[]>(`/repos/${owner}/${repo}/branches`).then((branches) => branches.map(normalizeBranchRef)),
  tags: (owner: string, repo: string) =>
    request<TagRefResponse[]>(`/repos/${owner}/${repo}/tags`).then((tags) => tags.map(normalizeTagRef)),
  commitSignature: (owner: string, repo: string, sha: string) =>
    request<{ verified: boolean; signer_key: string | null; signer_name: string | null; signer_email: string | null; status: string }>(`/repos/${owner}/${repo}/commits/${sha}/signature`),
  star: (owner: string, repo: string) =>
    request<{ starred: boolean }>(`/repos/${owner}/${repo}/star`, { method: 'PUT' }),
  starred: (owner: string, repo: string) =>
    request<{ starred: boolean }>(`/repos/${owner}/${repo}/starred`, { method: 'GET' }),
  unstar: async (owner: string, repo: string) => {
    const status = await repos.starred(owner, repo);
    if (!status.starred) return { starred: false };
    return repos.star(owner, repo);
  },
  stargazers: (owner: string, repo: string, page?: number, perPage?: number) =>
    request<PaginatedResponse<any>>(`/repos/${owner}/${repo}/stargazers${qs({ page, per_page: perPage })}`),
  watch: (owner: string, repo: string, state: string) =>
    request<{ watch_state: string }>(`/repos/${owner}/${repo}/watch`, { method: 'PUT', body: JSON.stringify({ state }) }),
  watchStatus: (owner: string, repo: string) =>
    request<{ watch_state: 'not_watching' | 'watching' | 'ignoring' }>(`/repos/${owner}/${repo}/watch`, { method: 'GET' }),
  unwatch: (owner: string, repo: string) =>
    request<{ watch_state: string }>(`/repos/${owner}/${repo}/watch`, { method: 'DELETE' }),
  delete: (owner: string, repo: string) =>
    request<{ deleted: boolean }>(`/repos/${owner}/${repo}`, { method: 'DELETE' }),
  fork: (owner: string, repo: string) =>
    request<any>(`/repos/${owner}/${repo}/fork`, { method: 'POST' }),
  forks: (owner: string, repo: string, page?: number, perPage?: number) => {
    const params = new URLSearchParams();
    if (page) params.set('page', String(page));
    if (perPage) params.set('per_page', String(perPage));
    const qs = params.toString() ? `?${params.toString()}` : '';
    return request<PaginatedResponse<any>>(`/repos/${owner}/${repo}/forks${qs}`);
  },
  transfer: (owner: string, repo: string, newOwner: string) =>
    request<any>(`/repos/${owner}/${repo}/transfer`, { method: 'POST', body: JSON.stringify({ new_owner: newOwner }) }),
  createCommitStatus: (owner: string, repo: string, sha: string, data: { state: string; context: string; description?: string; target_url?: string }) =>
    request<any>(`/repos/${owner}/${repo}/statuses/${sha}`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  listCommitStatuses: (owner: string, repo: string, sha: string) =>
    request<any[]>(`/repos/${owner}/${repo}/commits/${sha}/statuses`),
  getCombinedStatus: (owner: string, repo: string, sha: string) =>
    request<any>(`/repos/${owner}/${repo}/commits/${sha}/status`),
};
