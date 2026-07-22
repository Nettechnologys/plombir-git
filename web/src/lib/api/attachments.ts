import { getToken, request, withApiBase } from './_base.svelte';

export type Attachment = {
  id: number;
  uuid: string;
  name: string;
  size: number;
  content_type: string;
  download_count: number;
  created_at: string;
  browser_download_url: string;
};

export type AttachmentTarget = 'issues' | 'pulls' | 'issues/comments' | 'pulls/comments';

function path(owner: string, repo: string, target: AttachmentTarget, targetId: number): string {
  return `/repos/${owner}/${repo}/${target}/${targetId}/assets`;
}

export const attachments = {
  list: (owner: string, repo: string, target: AttachmentTarget, targetId: number) =>
    request<Attachment[]>(path(owner, repo, target, targetId)),

  upload: async (
    owner: string,
    repo: string,
    target: AttachmentTarget,
    targetId: number,
    file: File,
  ): Promise<Attachment> => {
    const form = new FormData();
    form.append('attachment', file, file.name);
    const headers: Record<string, string> = {};
    const token = getToken();
    if (token) headers.Authorization = `Bearer ${token}`;
    const response = await fetch(withApiBase(path(owner, repo, target, targetId)), {
      method: 'POST',
      headers,
      credentials: 'include',
      body: form,
    });
    if (!response.ok) {
      const body = await response.json().catch(() => ({}));
      throw new Error(body?.error?.message || body?.message || `HTTP ${response.status}`);
    }
    return response.json();
  },

  remove: (owner: string, repo: string, target: AttachmentTarget, targetId: number, id: number) =>
    request<void>(`${path(owner, repo, target, targetId)}/${id}`, { method: 'DELETE' }),
};
