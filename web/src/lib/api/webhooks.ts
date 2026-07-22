import { request } from './_base.svelte';

export interface RepositoryWebhook {
  id: number;
  repo_id: number;
  url: string;
  content_type: 'json' | 'form' | string;
  secret: string | null;
  active: boolean;
  events: string;
  created_at: string;
  updated_at: string;
}

export interface WebhookDelivery {
  id: number;
  webhook_id: number;
  event: string;
  delivery_id: string;
  response_status: number | null;
  request_payload: string | null;
  response_body: string | null;
  duration_ms: number | null;
  created_at: string;
}

export interface WebhookPayload {
  url: string;
  content_type?: 'json' | 'form';
  secret?: string;
  active?: boolean;
  events: string[];
}

export const webhooks = {
  list: (owner: string, repo: string) =>
    request<RepositoryWebhook[]>(`/repos/${owner}/${repo}/hooks`),
  create: (owner: string, repo: string, payload: WebhookPayload) =>
    request<RepositoryWebhook>(`/repos/${owner}/${repo}/hooks`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  get: (owner: string, repo: string, id: number) =>
    request<RepositoryWebhook>(`/repos/${owner}/${repo}/hooks/${id}`),
  update: (owner: string, repo: string, id: number, payload: Partial<WebhookPayload>) =>
    request<RepositoryWebhook>(`/repos/${owner}/${repo}/hooks/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  remove: (owner: string, repo: string, id: number) =>
    request<void>(`/repos/${owner}/${repo}/hooks/${id}`, { method: 'DELETE' }),
  deliveries: (owner: string, repo: string, id: number) =>
    request<WebhookDelivery[]>(`/repos/${owner}/${repo}/hooks/${id}/deliveries`),
  redeliver: (owner: string, repo: string, id: number, deliveryId: number) =>
    request<{ message: string }>(`/repos/${owner}/${repo}/hooks/${id}/deliveries/${deliveryId}/redeliver`, { method: 'POST' }),
};
