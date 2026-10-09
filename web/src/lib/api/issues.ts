import { request, qs, type PaginatedResponse } from './_base.svelte';

function parseIssueLabels(labels: string | string[] | undefined | null): string[] {
  if (Array.isArray(labels)) {
    return labels;
  }

  if (!labels || typeof labels !== 'string') {
    return [];
  }

  try {
    const parsed = JSON.parse(labels);
    if (Array.isArray(parsed)) {
      return parsed.filter((value) => typeof value === 'string');
    }
  } catch {
    // Older rows may contain comma-separated label names.
  }

  return labels
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean);
}

function normalizeIssue<T extends { labels?: string | string[] | null }>(issue: T): Omit<T, 'labels'> & { labels: string[] } {
  return {
    ...issue,
    labels: parseIssueLabels(issue.labels),
  };
}

export type IssueTemplate = {
  name: string;
  title: string;
  about: string;
  labels: string[];
  assignees: string[];
  ref: string;
  content: string;
  file_name: string;
};

export type IssueConfig = {
  blank_issues_enabled: boolean;
  contact_links: Array<{ name: string; url: string; about: string }>;
};

export interface Issue {
  id: number;
  repo_id: number;
  number: number;
  title: string;
  body: string | null;
  state: 'open' | 'closed';
  author_id: number;
  author?: string | null;
  /** When the author is a bot account, the person it acts for. */
  author_bot_owner?: string | null;
  assignee_id: number | null;
  /** The assignee's username, when the issue is on someone whose account resolves. */
  assignee?: string | null;
  milestone_id: number | null;
  labels: string[];
  created_at: string;
  updated_at: string;
  closed_at: string | null;
}

/** A comment as `GET /issues/{number}/comments` lists it. */
export interface IssueComment {
  id: number;
  issue_id: number;
  author_id: number;
  /** Absent on the row an edit answers with; the listing carries it. */
  author?: string | null;
  author_bot_owner?: string | null;
  body: string;
  created_at: string;
  /** Later than `created_at` once the comment was edited. */
  updated_at: string;
}

export interface IssueUpdatePayload {
  title?: string;
  body?: string;
  state?: Issue['state'];
  labels?: string[];
  assignee_id?: number | null;
  milestone_id?: number | null;
}

/**
 * Build the label half of an issue-list query.
 *
 * An array asks for the names it holds, one repeated `label` key each, so a
 * label whose own name contains a comma — `release, urgent` is a name the
 * backend creates and stores — addresses exactly itself. A plain string keeps
 * the older `labels=a,b` spelling, where the comma is the separator and such a
 * name cannot be expressed at all.
 */
function labelFilterQuery(labels: string | string[] | undefined): string[] {
  if (!Array.isArray(labels)) {
    return [];
  }
  return labels
    .filter((name) => name !== '')
    .map((name) => `label=${encodeURIComponent(name)}`);
}

export const issues = {
  templates: (owner: string, repo: string) =>
    request<IssueTemplate[]>(`/repos/${owner}/${repo}/issue_templates`),
  templateConfig: (owner: string, repo: string) =>
    request<IssueConfig>(`/repos/${owner}/${repo}/issue_config`),
  list: (owner: string, repo: string, state?: string, page?: number, perPage?: number, labels?: string | string[]) => {
    const scalar = qs({ state, page, per_page: perPage, labels: Array.isArray(labels) ? undefined : labels });
    const structural = labelFilterQuery(labels);
    const query = structural.length > 0
      ? `${scalar === '' ? '?' : `${scalar}&`}${structural.join('&')}`
      : scalar;
    return request<PaginatedResponse<Issue>>(`/repos/${owner}/${repo}/issues${query}`)
      .then((response) => ({
        ...response,
        data: response.data.map(normalizeIssue),
      }));
  },
  get: (owner: string, repo: string, number: number) =>
    request<Issue>(`/repos/${owner}/${repo}/issues/${number}`).then(normalizeIssue),
  create: (owner: string, repo: string, title: string, body?: string, labels?: string[]) =>
    request<Issue>(`/repos/${owner}/${repo}/issues`, {
      method: 'POST',
      body: JSON.stringify({ title, body, labels }),
    }).then(normalizeIssue),
  update: (owner: string, repo: string, number: number, data: IssueUpdatePayload) =>
    request<Issue>(`/repos/${owner}/${repo}/issues/${number}`, {
      method: 'PATCH',
      body: JSON.stringify(data),
    }).then(normalizeIssue),
  comments: (owner: string, repo: string, number: number) =>
    request<any[]>(`/repos/${owner}/${repo}/issues/${number}/comments`),
  labels: (owner: string, repo: string, number: number) =>
    request<any[]>(`/repos/${owner}/${repo}/issues/${number}/labels`),
  addComment: (owner: string, repo: string, number: number, body: string) =>
    request<any>(`/repos/${owner}/${repo}/issues/${number}/comments`, {
      method: 'POST',
      body: JSON.stringify({ body }),
    }),
  /**
   * Edit a comment (card_60961272e1ba): its author or a repository
   * administrator, 403 for anyone else. The answer is the comment row — it
   * carries no `author` name, so a caller merges it over the one it shows.
   */
  editComment: (owner: string, repo: string, commentId: number, body: string) =>
    request<IssueComment>(`/repos/${owner}/${repo}/issues/comments/${commentId}`, {
      method: 'PATCH',
      body: JSON.stringify({ body }),
    }),
  /** Same rule as `editComment`; 204. */
  deleteComment: (owner: string, repo: string, commentId: number) =>
    request<void>(`/repos/${owner}/${repo}/issues/comments/${commentId}`, { method: 'DELETE' }),
  /**
   * Delete the issue with its comments and attachments; repository
   * administrators only (403 otherwise), 204. Pull requests have no such
   * route on purpose.
   */
  delete: (owner: string, repo: string, number: number) =>
    request<void>(`/repos/${owner}/${repo}/issues/${number}`, { method: 'DELETE' }),
};
