import { request, qs, type PaginatedResponse } from './_base.svelte';

export type DiffLine = {
  kind: 'meta' | 'context' | 'addition' | 'deletion';
  content: string;
  old_line: number | null;
  new_line: number | null;
};

export type FileDiff = {
  path: string;
  status: string;
  additions: number;
  deletions: number;
  patch: string | null;
  lines: DiffLine[];
};

export type PrDiff = {
  base_branch: string;
  head_branch: string;
  files_changed: FileDiff[];
  stats: { total_additions: number; total_deletions: number; files_changed: number };
};

export type MergeQueueEntry = {
  id: number;
  position: number;
  pr_id: number;
  pr_number: number;
  title: string;
  strategy: string;
  status: 'queued' | 'running';
  enqueued_by_id: number;
  created_at: string;
};

export const pulls = {
  template: (owner: string, repo: string) =>
    request<{ content: string; file_name: string } | undefined>(`/repos/${owner}/${repo}/pull_request_template`),
  list: (owner: string, repo: string, state?: string, page?: number, perPage?: number) => {
    return request<PaginatedResponse<any>>(`/repos/${owner}/${repo}/pulls${qs({ state, page, per_page: perPage })}`);
  },
  get: (owner: string, repo: string, number: number) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}`),
  create: (owner: string, repo: string, data: { title: string; body?: string; head_branch: string; base_branch: string; draft?: boolean }) =>
    request<any>(`/repos/${owner}/${repo}/pulls`, {
      method: 'POST',
      body: JSON.stringify({
        title: data.title,
        body: data.body,
        head: data.head_branch,
        base: data.base_branch,
        draft: data.draft ?? false,
      }),
    }),
  update: (owner: string, repo: string, number: number, data: { title?: string; body?: string; state?: string; draft?: boolean }) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  diff: (owner: string, repo: string, number: number) =>
    request<PrDiff>(`/repos/${owner}/${repo}/pulls/${number}/diff`),
  merge: (owner: string, repo: string, number: number, strategy: string) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/merge`, {
      method: 'POST',
      body: JSON.stringify({ strategy }),
    }),
  enableAutoMerge: (owner: string, repo: string, number: number, strategy: string) =>
    request<{ status: 'disabled' | 'pending' | 'merged'; reason?: string; merge?: any }>(
      `/repos/${owner}/${repo}/pulls/${number}/auto-merge`,
      { method: 'PUT', body: JSON.stringify({ strategy }) },
    ),
  disableAutoMerge: (owner: string, repo: string, number: number) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/auto-merge`, { method: 'DELETE' }),
  mergeQueue: (owner: string, repo: string) =>
    request<MergeQueueEntry[]>(`/repos/${owner}/${repo}/merge-queue`),
  enqueueMerge: (owner: string, repo: string, number: number, strategy: string) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/merge-queue`, {
      method: 'PUT',
      body: JSON.stringify({ strategy }),
    }),
  cancelQueuedMerge: (owner: string, repo: string, number: number) =>
    request<void>(`/repos/${owner}/${repo}/pulls/${number}/merge-queue`, { method: 'DELETE' }),
};

export const reviews = {
  list: (owner: string, repo: string, number: number) =>
    request<any[]>(`/repos/${owner}/${repo}/pulls/${number}/reviews`),
  submit: (owner: string, repo: string, number: number, body: string, verdict: string) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/reviews`, {
      method: 'POST',
      body: JSON.stringify({ body, action: verdict }),
    }),
  comments: (owner: string, repo: string, number: number) =>
    request<any[]>(`/repos/${owner}/${repo}/pulls/${number}/comments`),
  timeline: (owner: string, repo: string, number: number) =>
    request<Array<{
      id: string;
      kind: string;
      actor: { id: number; username: string } | null;
      created_at: string;
      body: string | null;
      metadata: Record<string, any>;
    }>>(`/repos/${owner}/${repo}/pulls/${number}/timeline`),
  addComment: (owner: string, repo: string, number: number, data: {
    body: string;
    path: string;
    line?: number;
    start_line?: number;
    side?: 'LEFT' | 'RIGHT';
    start_side?: 'LEFT' | 'RIGHT';
    review_id?: number;
    commit_id?: string;
    reply_to_id?: number;
    suggestion?: string;
  }) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/comments`, {
      method: 'POST',
      body: JSON.stringify(data),
    }),
  setThreadResolved: (owner: string, repo: string, number: number, commentId: number, resolved: boolean) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/comments/${commentId}/resolution`, {
      method: 'PATCH',
      body: JSON.stringify({ resolved }),
    }),
  applySuggestion: (owner: string, repo: string, number: number, commentId: number) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/comments/${commentId}/suggestion/apply`, {
      method: 'POST',
    }),
  applySuggestions: (owner: string, repo: string, number: number, commentIds: number[]) =>
    request<{ comments: any[]; commit_sha: string }>(`/repos/${owner}/${repo}/pulls/${number}/suggestions/apply`, {
      method: 'POST',
      body: JSON.stringify({ comment_ids: commentIds }),
    }),
  requestedReviewers: (owner: string, repo: string, number: number) =>
    request<Array<{ id: number; reviewer_id: number; username: string; requested_by_id: number; created_at: string }>>(
      `/repos/${owner}/${repo}/pulls/${number}/reviewers`,
    ),
  requestReviewer: (owner: string, repo: string, number: number, username: string) =>
    request<{ id: number; reviewer_id: number; username: string; requested_by_id: number; created_at: string }>(
      `/repos/${owner}/${repo}/pulls/${number}/reviewers`,
      { method: 'POST', body: JSON.stringify({ username }) },
    ),
  removeRequestedReviewer: (owner: string, repo: string, number: number, username: string) =>
    request<void>(`/repos/${owner}/${repo}/pulls/${number}/reviewers/${encodeURIComponent(username)}`, {
      method: 'DELETE',
    }),
};
