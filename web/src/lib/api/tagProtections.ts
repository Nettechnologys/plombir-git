import { request } from './_base.svelte';

export interface TagProtection { id: number; pattern: string; allowed_user_ids: number[]; created_at: string; updated_at: string; }

/**
 * A tag-protection rule as the API accepts it.
 *
 * `allowed_user_ids` is the rule's exemption list, and it is never optional on
 * the wire: `tag_push_allowed_by_rule` lets a push through only for an actor
 * named in it, so an empty list means the pattern is closed to everybody —
 * including the repository owner. A body that leaves the key out therefore
 * creates a rule nothing can be excepted from.
 *
 * `pattern` is create-only: `PATCH .../tags/protection/{id}` reads the allow
 * list and nothing else, so a changed pattern would be silently discarded.
 */
export interface TagProtectionPayload {
  pattern?: string;
  allowed_user_ids: number[];
}

export const tagProtections = {
  list: (owner: string, repo: string) => request<TagProtection[]>(`/repos/${owner}/${repo}/tags/protection`),
  create: (owner: string, repo: string, payload: TagProtectionPayload & { pattern: string }) =>
    request<TagProtection>(`/repos/${owner}/${repo}/tags/protection`, { method: 'POST', body: JSON.stringify(payload) }),
  update: (owner: string, repo: string, id: number, payload: Omit<TagProtectionPayload, 'pattern'>) =>
    request<TagProtection>(`/repos/${owner}/${repo}/tags/protection/${id}`, { method: 'PATCH', body: JSON.stringify(payload) }),
  delete: (owner: string, repo: string, id: number) => request<void>(`/repos/${owner}/${repo}/tags/protection/${id}`, { method: 'DELETE' }),
};
