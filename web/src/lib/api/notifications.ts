import { request, qs, type PaginatedResponse } from './_base.svelte';

export const notifications = {
  list: (userId?: number, unreadOnly?: boolean, page?: number, perPage?: number) =>
    request<PaginatedResponse<any>>(`/notifications${qs({ user_id: userId, unread_only: unreadOnly, page, per_page: perPage })}`),
  unreadCount: (userId?: number) =>
    request<any>(`/notifications/unread-count${userId ? `?user_id=${userId}` : ''}`),
  markRead: (id: number) =>
    request<any>(`/notifications/${id}/read`, { method: 'POST' }),
  markAllRead: (userId?: number) =>
    request<any>(`/notifications/mark-all-read${userId ? `?user_id=${userId}` : ''}`, { method: 'POST' }),
  delete: (id: number) =>
    request<any>(`/notifications/${id}`, { method: 'DELETE' }),
};
