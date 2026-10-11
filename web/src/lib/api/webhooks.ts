import { request } from './_base.svelte';
import { pathSegment, repoPath } from './repoPath';

export interface RepositoryWebhook {
  id: number;
  repo_id: number;
  scope_kind: 'repo' | 'org' | 'system';
  scope_id: number;
  url: string;
  content_type: 'json' | 'form' | string;
  /** Whether an HMAC secret is configured. The value itself never leaves the server. */
  has_secret: boolean;
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
  content_type?: 'json' | 'form' | 'slack' | 'discord' | 'matrix';
  secret?: string;
  active?: boolean;
  events: string[];
}

export const webhooks = {
  list: (owner: string, repo: string) =>
    request<RepositoryWebhook[]>(`${repoPath(owner, repo)}/hooks`),
  create: (owner: string, repo: string, payload: WebhookPayload) =>
    request<RepositoryWebhook>(`${repoPath(owner, repo)}/hooks`, {
      method: 'POST',
      body: JSON.stringify(payload),
    }),
  get: (owner: string, repo: string, id: number) =>
    request<RepositoryWebhook>(`${repoPath(owner, repo)}/hooks/${id}`),
  update: (owner: string, repo: string, id: number, payload: Partial<WebhookPayload>) =>
    request<RepositoryWebhook>(`${repoPath(owner, repo)}/hooks/${id}`, {
      method: 'PATCH',
      body: JSON.stringify(payload),
    }),
  remove: (owner: string, repo: string, id: number) =>
    request<void>(`${repoPath(owner, repo)}/hooks/${id}`, { method: 'DELETE' }),
  deliveries: (owner: string, repo: string, id: number) =>
    request<WebhookDelivery[]>(`${repoPath(owner, repo)}/hooks/${id}/deliveries`),
  redeliver: (owner: string, repo: string, id: number, deliveryId: number) =>
    request<{ message: string }>(`${repoPath(owner, repo)}/hooks/${id}/deliveries/${deliveryId}/redeliver`, { method: 'POST' }),
};

export const organizationWebhooks = {
  list: (name: string) => request<RepositoryWebhook[]>(`/orgs/${pathSegment(name)}/hooks`),
  create: (name: string, payload: WebhookPayload) => request<RepositoryWebhook>(`/orgs/${pathSegment(name)}/hooks`, {
    method: 'POST', body: JSON.stringify(payload),
  }),
  get: (name: string, id: number) => request<RepositoryWebhook>(`/orgs/${pathSegment(name)}/hooks/${id}`),
  update: (name: string, id: number, payload: Partial<WebhookPayload>) => request<RepositoryWebhook>(`/orgs/${pathSegment(name)}/hooks/${id}`, {
    method: 'PATCH', body: JSON.stringify(payload),
  }),
  remove: (name: string, id: number) => request<void>(`/orgs/${pathSegment(name)}/hooks/${id}`, { method: 'DELETE' }),
  deliveries: (name: string, id: number) => request<WebhookDelivery[]>(`/orgs/${pathSegment(name)}/hooks/${id}/deliveries`),
  redeliver: (name: string, id: number, deliveryId: number) => request<{ message: string }>(`/orgs/${pathSegment(name)}/hooks/${id}/deliveries/${deliveryId}/redeliver`, { method: 'POST' }),
};

export const systemWebhooks = {
  list: () => request<RepositoryWebhook[]>('/admin/hooks'),
  create: (payload: WebhookPayload) => request<RepositoryWebhook>('/admin/hooks', {
    method: 'POST', body: JSON.stringify(payload),
  }),
  get: (id: number) => request<RepositoryWebhook>(`/admin/hooks/${id}`),
  update: (id: number, payload: Partial<WebhookPayload>) => request<RepositoryWebhook>(`/admin/hooks/${id}`, {
    method: 'PATCH', body: JSON.stringify(payload),
  }),
  remove: (id: number) => request<void>(`/admin/hooks/${id}`, { method: 'DELETE' }),
  deliveries: (id: number) => request<WebhookDelivery[]>(`/admin/hooks/${id}/deliveries`),
  redeliver: (id: number, deliveryId: number) => request<{ message: string }>(`/admin/hooks/${id}/deliveries/${deliveryId}/redeliver`, { method: 'POST' }),
};
