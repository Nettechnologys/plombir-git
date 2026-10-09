import { request, qs, type PaginatedResponse } from './_base.svelte';

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

export interface NotificationSettings {
  email: EmailNotificationSettings;
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
  // The two thread kinds are spelled out rather than built by a helper, so
  // every route stays statically visible to the API alignment checks.
  subscription: (owner: string, repo: string, kind: ThreadKind, number: number) =>
    kind === 'issues'
      ? request<ThreadSubscription>(`/repos/${owner}/${repo}/issues/${number}/subscription`)
      : request<ThreadSubscription>(`/repos/${owner}/${repo}/pulls/${number}/subscription`),
  /** Follow the thread; the answer carries `reason: 'manual'`. */
  subscribe: (owner: string, repo: string, kind: ThreadKind, number: number) =>
    kind === 'issues'
      ? request<ThreadSubscription>(`/repos/${owner}/${repo}/issues/${number}/subscription`, { method: 'PUT' })
      : request<ThreadSubscription>(`/repos/${owner}/${repo}/pulls/${number}/subscription`, { method: 'PUT' }),
  /** Unfollow; an explicit unfollow survives commenting again. */
  unsubscribe: (owner: string, repo: string, kind: ThreadKind, number: number) =>
    kind === 'issues'
      ? request<ThreadSubscription>(`/repos/${owner}/${repo}/issues/${number}/subscription`, { method: 'DELETE' })
      : request<ThreadSubscription>(`/repos/${owner}/${repo}/pulls/${number}/subscription`, { method: 'DELETE' }),
};
