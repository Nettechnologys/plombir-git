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

export const issues = {
  templates: (owner: string, repo: string) =>
    request<IssueTemplate[]>(`/repos/${owner}/${repo}/issue_templates`),
  templateConfig: (owner: string, repo: string) =>
    request<IssueConfig>(`/repos/${owner}/${repo}/issue_config`),
  list: (owner: string, repo: string, state?: string, page?: number, perPage?: number, labels?: string) => {
    return request<PaginatedResponse<any>>(`/repos/${owner}/${repo}/issues${qs({ state, page, per_page: perPage, labels })}`)
      .then((response) => ({
        ...response,
        data: response.data.map(normalizeIssue),
      }));
  },
  get: (owner: string, repo: string, number: number) =>
    request<any>(`/repos/${owner}/${repo}/issues/${number}`).then(normalizeIssue),
  create: (owner: string, repo: string, title: string, body?: string, labels?: string[]) =>
    request<any>(`/repos/${owner}/${repo}/issues`, {
      method: 'POST',
      body: JSON.stringify({ title, body, labels }),
    }).then(normalizeIssue),
  update: (owner: string, repo: string, number: number, data: Record<string, any>) =>
    request<any>(`/repos/${owner}/${repo}/issues/${number}`, {
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
};
