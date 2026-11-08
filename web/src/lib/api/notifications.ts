import { request, qs, type PaginatedResponse } from './_base.svelte';
import { repoPath } from './repoPath';

/** Why a notification row reached the account (card_349c2b6a0d7c); `null` for repository-watch rows. */
export type NotificationReason = 'review_requested' | 'assigned' | 'mention' | 'ci_failed' | 'participating';

export interface NotificationRow {
  id: number;
  user_id: number;
  event_type: string;
  title: string;
  body: string | null;
  repo_id: number | null;
  is_read: boolean;
  created_at: string;
  reason?: NotificationReason | string | null;
  subject_type?: 'issue' | 'pull_request' | null;
  /** A relative app path such as `/owner/repo/pulls/7`; may be absent. */
  link?: string | null;
}

/** The six mail categories `GET /users/me/notification-settings` reports. */
export const EMAIL_NOTIFICATION_KEYS = [
  'review_requested',
  'mention',
  'assigned',
  'ci_failed',
  'participating',
  'ci_triggered',
] as const;

export type EmailNotificationKey = (typeof EMAIL_NOTIFICATION_KEYS)[number];
export type EmailNotificationSettings = Record<EmailNotificationKey, boolean>;

export const NOTIFICATION_EVENTS = [
  'review_requested',
  'mention',
  'assigned',
  'ci_failed',
  'participating',
  'ci_triggered',
  'new_device',
  'token_created',
  'token_revoked',
  'agent_token_created',
  'agent_token_revoked',
  'ssh_key_added',
  'ssh_key_removed',
  'signing_key_added',
  'signing_key_removed',
  'deploy_key_added',
  'deploy_key_removed',
  'passkey_added',
  'passkey_removed',
  'mfa_enabled',
  'mfa_replaced',
  'mfa_disabled',
  'backup_codes_regenerated',
  'password_changed',
  'password_reset',
  'email_changed',
  'external_login_linked',
  'external_login_unlinked',
  'admin_password_reset',
  'admin_factors_reset',
  'runner_token_created',
  'runner_token_revoked',
  'notification_settings_changed',
  'agent_profile_changed',
  'bot_created',
  'bot_deleted',
  'repository_access_changed',
  'organization_access_changed',
  'repository_security_changed',
  'organization_security_changed',
  'webhook_changed',
  'mirror_changed',
  'ci_security_changed',
  'admin_security_changed',
] as const;
export type NotificationEvent = (typeof NOTIFICATION_EVENTS)[number];
export type NotificationChannel = 'email' | 'telegram';
export interface DeliverySettings {
  email: EmailNotificationSettings;
  events: Record<NotificationEvent, Record<NotificationChannel, boolean>>;
  locale: 'en' | 'ru' | 'zh-CN';
  revision: number;
  telegram: { token_configured: boolean; chat_id: number | null; message_thread_id: number | null; confirmed: boolean };
  capabilities: { email: boolean; email_reason: string | null };
}
/** Accept the old read shape during a rolling frontend/backend upgrade. */
export type NotificationSettings = { email: EmailNotificationSettings } & Partial<Omit<DeliverySettings, 'email'>>;
export interface DeliverySettingsPatch {
  locale?: DeliverySettings['locale'];
  events?: Partial<Record<NotificationEvent, Partial<Record<NotificationChannel, boolean>>>>;
  telegram?: { action: 'keep' | 'remove' } | { action: 'set'; token?: string; chat_id: number; message_thread_id: number | null };
}
export interface NotificationTestResult {
  event_id: string;
  channels: Record<NotificationChannel, { status: 'queued' | 'inflight' | 'sent' | 'failed' | 'unavailable' | 'cancelled'; reason: string | null }>;
}

/** Why the caller follows an issue or a pull request. */
export type SubscriptionReason = 'author' | 'commented' | 'mention' | 'assigned' | 'review_requested' | 'manual';

export interface ThreadSubscription {
  subscribed: boolean;
  reason: SubscriptionReason | string | null;
}

/** `issues` or `pulls`: the two kinds of thread a subscription addresses. */
export type ThreadKind = 'issues' | 'pulls';

export const notifications = {
  list: (userId?: number, unreadOnly?: boolean, page?: number, perPage?: number) =>
    request<PaginatedResponse<NotificationRow>>(`/notifications${qs({ user_id: userId, unread_only: unreadOnly, page, per_page: perPage })}`),
  unreadCount: (userId?: number) =>
    request<any>(`/notifications/unread-count${userId ? `?user_id=${userId}` : ''}`),
  markRead: (id: number) =>
    request<any>(`/notifications/${id}/read`, { method: 'POST' }),
  markAllRead: (userId?: number) =>
    request<any>(`/notifications/mark-all-read${userId ? `?user_id=${userId}` : ''}`, { method: 'POST' }),
  delete: (id: number) =>
    request<any>(`/notifications/${id}`, { method: 'DELETE' }),
  /** Which notification kinds are also mailed (card_349c2b6a0d7c). */
  settings: () => request<NotificationSettings>('/users/me/notification-settings'),
  /** Any subset of the six keys; the answer is the full stored object. */
  updateSettings: (email: Partial<EmailNotificationSettings>) =>
    request<NotificationSettings>('/users/me/notification-settings', {
      method: 'PUT',
      body: JSON.stringify({ email }),
    }),
  updateDelivery: (patch: DeliverySettingsPatch) => request<DeliverySettings>('/users/me/notification-settings', {
    method: 'PUT', body: JSON.stringify(patch),
  }),
  testDelivery: (event: NotificationEvent) => request<NotificationTestResult>('/users/me/notification-settings/test', {
    method: 'POST', body: JSON.stringify({ event }),
  }),
  testResult: (eventId: string) => request<NotificationTestResult>(`/users/me/notification-settings/test${qs({ event_id: eventId })}`),
  // The two thread kinds are spelled out rather than built by a helper, so
  // every route stays statically visible to the API alignment checks.
  subscription: (owner: string, repo: string, kind: ThreadKind, number: number) =>
    kind === 'issues'
      ? request<ThreadSubscription>(`${repoPath(owner, repo)}/issues/${number}/subscription`)
      : request<ThreadSubscription>(`${repoPath(owner, repo)}/pulls/${number}/subscription`),
  /** Follow the thread; the answer carries `reason: 'manual'`. */
  subscribe: (owner: string, repo: string, kind: ThreadKind, number: number) =>
    kind === 'issues'
      ? request<ThreadSubscription>(`${repoPath(owner, repo)}/issues/${number}/subscription`, { method: 'PUT' })
      : request<ThreadSubscription>(`${repoPath(owner, repo)}/pulls/${number}/subscription`, { method: 'PUT' }),
  /** Unfollow; an explicit unfollow survives commenting again. */
  unsubscribe: (owner: string, repo: string, kind: ThreadKind, number: number) =>
    kind === 'issues'
      ? request<ThreadSubscription>(`${repoPath(owner, repo)}/issues/${number}/subscription`, { method: 'DELETE' })
      : request<ThreadSubscription>(`${repoPath(owner, repo)}/pulls/${number}/subscription`, { method: 'DELETE' }),
};
