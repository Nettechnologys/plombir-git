import { request, qs, type PaginatedResponse } from './_base.svelte';
import { formatHeadRef } from '../pullHeadRef';

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

/** One commit of a compare, in the shape `GET /log` already answers with. */
export type CompareCommit = {
  sha: string;
  message: string;
  author: string;
  date: string;
};

/**
 * `GET /repos/{owner}/{name}/compare?base=&head=` — not served by the backend
 * yet (card_87f9b1c97489). `PrDiff` plus the commits reachable from head and not
 * from base, oldest first, so the compare page renders the same diff the pull
 * request will.
 */
export type CompareResult = PrDiff & {
  commits: CompareCommit[];
  total_commits?: number;
  merge_base_sha?: string | null;
};

/** `POST /pulls/{n}/merge` 200: the merge result plus the head-branch report when deletion was asked for. */
export type MergeOutcome = Record<string, unknown> & {
  head_branch_deleted?: boolean;
  head_branch_kept?: string;
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
  /**
   * Open a pull request. `head_owner` names the fork holding `head_branch`
   * (card_87f9b1c97489); it is sent as `head: "<owner>:<branch>"`, the form
   * `CreatePrRequest` resolves to a fork of this repository. Omitted, or equal
   * to `owner`, the head is a branch of this repository and goes out bare.
   */
  create: (
    owner: string,
    repo: string,
    data: { title: string; body?: string; head_branch: string; head_owner?: string | null; base_branch: string; draft?: boolean },
  ) =>
    request<any>(`/repos/${owner}/${repo}/pulls`, {
      method: 'POST',
      body: JSON.stringify({
        title: data.title,
        body: data.body,
        head: formatHeadRef({ owner: data.head_owner ?? null, branch: data.head_branch }, owner),
        base: data.base_branch,
        draft: data.draft ?? false,
      }),
    }),
  /**
   * What a pull request from `head` into `base` would contain, before it exists.
   *
   * `head` is a bare branch of this repository or `"<owner>:<branch>"` of one of
   * its forks — the same form `create` sends. The answer is the PR diff shape
   * (`PrDiff`) plus the commits `head` has that `base` does not.
   */
  compare: (owner: string, repo: string, base: string, head: string) =>
    request<CompareResult>(
      `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/compare${qs({ base, head })}`,
    ),
  update: (owner: string, repo: string, number: number, data: { title?: string; body?: string; state?: string; draft?: boolean }) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }),
  diff: (owner: string, repo: string, number: number) =>
    request<PrDiff>(`/repos/${owner}/${repo}/pulls/${number}/diff`),
  /**
   * Merge. With `deleteHeadBranch` the server deletes the head branch once the
   * merge has landed and reports it beside the merge: `head_branch_deleted`,
   * and when the branch had to stay, the reason in `head_branch_kept`
   * (card_2060696224ff). A kept branch is not a failed merge.
   */
  /**
   * Delete an open or closed pull request — repository administrators only
   * (card_ee4f318c50f1). A merged one, or one the queue is merging, is `409`.
   */
  delete: (owner: string, repo: string, number: number) =>
    request<void>(`/repos/${owner}/${repo}/pulls/${number}`, { method: 'DELETE' }),
  merge: (owner: string, repo: string, number: number, strategy: string, opts: { deleteHeadBranch?: boolean } = {}) =>
    request<MergeOutcome>(`/repos/${owner}/${repo}/pulls/${number}/merge`, {
      method: 'POST',
      body: JSON.stringify({ strategy, delete_head_branch: opts.deleteHeadBranch ?? false }),
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
  /**
   * Let a fork PR's head run CI under this repository's secrets.
   *
   * `trigger_pull_request_ci` refuses a fork head until `ci_approved_sha`
   * matches it, because a pipeline runs under the *base* repository's id and is
   * handed that repository's CI secrets. Only somebody with write access can
   * lift that, and until this client existed the only way to do it was `curl`
   * with a token (card_3c0751fbf09d).
   *
   * The server starts the run it unblocks, so the caller should reload the
   * pull request afterwards. The approval is recorded against the head commit
   * and does not survive the next push to the fork.
   */
  approveCi: (owner: string, repo: string, number: number) =>
    request<any>(
      `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/pulls/${number}/ci-approval`,
      { method: 'POST' },
    ),
};

export const reviews = {
  list: (owner: string, repo: string, number: number) =>
    request<any[]>(`/repos/${owner}/${repo}/pulls/${number}/reviews`),
  submit: (owner: string, repo: string, number: number, body: string, verdict: string) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/reviews`, {
      method: 'POST',
      body: JSON.stringify({ body, action: verdict }),
    }),
  /**
   * Withdraw a standing review.
   *
   * `current_approvers` reads the `dismissed_at` stamp on the review row
   * and nothing else, so this is the only thing that takes a stale approval
   * back off a protected branch's counter (card_dc0f5d58e5f4). Behind
   * `RepoWrite`; until this client existed the only way to reach it was `curl`
   * with a token (card_1714b4dacad5).
   *
   * The response is the dismissed review itself, carrying `dismissed_at` /
   * `dismissed_by`. The dismissal also writes a `review_dismiss` timeline
   * event, so a caller showing the timeline wants to reload it too.
   */
  dismiss: (owner: string, repo: string, number: number, id: number, message: string) =>
    request<any>(
      `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/pulls/${number}/reviews/${id}/dismiss`,
      { method: 'POST', body: JSON.stringify({ message }) },
    ),
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
  /**
   * Edit a review comment (card_60961272e1ba): its author or a repository
   * administrator, 403 for anyone else. The answer is the comment row.
   */
  editComment: (owner: string, repo: string, number: number, commentId: number, body: string) =>
    request<any>(`/repos/${owner}/${repo}/pulls/${number}/comments/${commentId}`, {
      method: 'PATCH',
      body: JSON.stringify({ body }),
    }),
  /** Same rule as `editComment`; 204, or 409 when others replied to it. */
  deleteComment: (owner: string, repo: string, number: number, commentId: number) =>
    request<void>(`/repos/${owner}/${repo}/pulls/${number}/comments/${commentId}`, { method: 'DELETE' }),
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
