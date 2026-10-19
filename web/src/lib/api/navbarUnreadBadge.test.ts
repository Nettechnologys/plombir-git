import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import Navbar from '../components/Navbar.svelte';
import NotificationsPage from '../../routes/notifications/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { UNREAD_REFRESH_MS } from '../stores/unreadNotifications.svelte';
import { auth, connectNotificationWebSocket, notifications, resetTestClient } from '../test/client';
import { click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

// card_a9dd3d9eccfe: a new notification was visible only to someone who opened
// /notifications on their own — the link in the bar carried no count.

let rendered: RenderedComponent[] = [];
let pushes: Array<(event: { event_type: string; data: unknown }) => void> = [];
const disconnect = vi.fn();

beforeEach(async () => {
	resetTestClient();
	await logout();
	resetTestClient();
	pushes = [];
	disconnect.mockReset();
	connectNotificationWebSocket.mockImplementation((onMessage: (typeof pushes)[number]) => {
		pushes.push(onMessage);
		return { disconnect };
	});
	auth.me.mockResolvedValueOnce({ id: 7, username: 'bob', email: 'bob@example.test', is_admin: false, display_name: null });
	await fetchUser();
});

afterEach(async () => {
	for (const component of rendered) await component.destroy();
	rendered = [];
	vi.useRealTimers();
	document.body.innerHTML = '';
});

const badge = (container: ParentNode) => container.querySelector('.notifications-link .unread-badge');

describe('the navigation bar', () => {
	it('shows the unread count, and a live access notice raises it without a reload', async () => {
		notifications.unreadCount.mockResolvedValue({ unread_count: 0 });
		const bar = await renderComponent(Navbar);
		rendered.push(bar);
		await settle();
		expect(notifications.unreadCount).toHaveBeenCalledWith(7);
		expect(badge(bar.container)).toBeNull();

		// `POST /collaborators` wrote the row and pushed it over the socket.
		notifications.unreadCount.mockResolvedValue({ unread_count: 1 });
		pushes[0]({ event_type: 'access', data: { title: 'alice gave you write access to alice/app' } });
		await settle();
		expect(badge(bar.container)?.textContent).toBe('1');
		expect(badge(bar.container)?.getAttribute('aria-label')).toBe('1 unread notifications');
	});

	it('a requested review reaches the open notifications page without a reload', async () => {
		notifications.unreadCount.mockResolvedValue({ unread_count: 0 });
		notifications.list.mockResolvedValue({ data: [], pagination: { page: 1, per_page: 20, total: 0, total_pages: 0 } });
		const page = await renderComponent(NotificationsPage);
		rendered.push(page);
		await settle();
		expect(page.container.textContent).not.toContain('please review');

		// card_6abf2e3c4287: thread notifications are announced by their reason.
		notifications.unreadCount.mockResolvedValue({ unread_count: 1 });
		notifications.list.mockResolvedValue({
			data: [
				{
					id: 9,
					event_type: 'pull_request',
					reason: 'review_requested',
					title: 'alice/app #3: please review',
					is_read: false,
					created_at: '2026-10-19T00:00:00Z',
				},
			],
			pagination: { page: 1, per_page: 20, total: 1, total_pages: 1 },
		});
		for (const push of pushes) push({ event_type: 'review_requested', data: { payload: { reason: 'review_requested' } } });
		await settle();
		expect(page.container.textContent).toContain('please review');
	});

	it('re-reads the count periodically for notifications whose push was missed', async () => {
		vi.useFakeTimers({ toFake: ['setInterval', 'clearInterval'] });
		notifications.unreadCount.mockResolvedValue({ unread_count: 0 });
		const bar = await renderComponent(Navbar);
		rendered.push(bar);
		await settle();
		notifications.unreadCount.mockResolvedValue({ unread_count: 120 });
		vi.advanceTimersByTime(UNREAD_REFRESH_MS);
		await settle();
		expect(badge(bar.container)?.textContent).toBe('99+');
	});

	it('goes dark once everything is marked read on the notifications page', async () => {
		notifications.unreadCount.mockResolvedValue({ unread_count: 2 });
		notifications.list.mockResolvedValue({
			data: [
				{ id: 1, event_type: 'access', title: 'one', is_read: false, created_at: '2026-10-19T00:00:00Z' },
				{ id: 2, event_type: 'mention', title: 'two', is_read: false, created_at: '2026-10-19T00:00:00Z' },
			],
			pagination: { page: 1, per_page: 20, total: 2, total_pages: 1 },
		});
		const bar = await renderComponent(Navbar);
		rendered.push(bar);
		const page = await renderComponent(NotificationsPage);
		rendered.push(page);
		await settle();
		expect(badge(bar.container)?.textContent).toBe('2');

		notifications.markAllRead.mockResolvedValue({});
		notifications.unreadCount.mockResolvedValue({ unread_count: 0 });
		notifications.list.mockResolvedValue({ data: [], pagination: { page: 1, per_page: 20, total: 0, total_pages: 0 } });
		const markAll = Array.from(page.container.querySelectorAll<HTMLButtonElement>('button')).find((button) =>
			/mark all/i.test(button.textContent ?? ''),
		);
		await click(markAll!);
		await settle();
		expect(notifications.markAllRead).toHaveBeenCalled();
		expect(badge(bar.container)).toBeNull();
	});

	it('stops watching when the reader signs out', async () => {
		notifications.unreadCount.mockResolvedValue({ unread_count: 3 });
		const bar = await renderComponent(Navbar);
		rendered.push(bar);
		await settle();
		expect(badge(bar.container)?.textContent).toBe('3');
		await logout();
		await settle();
		expect(disconnect).toHaveBeenCalled();
		expect(element(bar.container, 'a[href="/login"]')).toBeTruthy();
	});
});
