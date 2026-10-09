import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

// card_349c2b6a0d7c: mail categories, thread subscriptions and notification
// rows that point somewhere. The pages' client namespace is the shared mock,
// but the calls under test are pointed at the REAL client, whose transport is
// this mock — so each assertion reads the request as it leaves the browser.
const base = vi.hoisted(() => ({
	downloadApiFile: vi.fn(),
	getToken: vi.fn(() => 'test-token'),
	request: vi.fn(),
	qs: vi.fn(() => ''),
	withApiBase: vi.fn((path: string) => `/api/v1${path}`),
}));

vi.mock('./_base.svelte', () => base);

import NotificationSettingsPage from '../../routes/settings/notifications/+page.svelte';
import NotificationsPage from '../../routes/notifications/+page.svelte';
import ThreadSubscription from '../components/ThreadSubscription.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import { ApiError } from './error';
import { notifications } from './notifications';
import { navigation, setTestPage } from '../test/app';
import {
	auth,
	connectNotificationWebSocket,
	notifications as routeNotifications,
	resetTestClient,
} from '../test/client';
import { click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const SERVER_DEFAULTS = {
	review_requested: true,
	mention: true,
	assigned: true,
	ci_failed: true,
	participating: true,
	ci_triggered: false,
};

function callsTo(path: string, method = 'GET') {
	return base.request.mock.calls.filter(([url, init]) => url === path && (init?.method ?? 'GET') === method);
}

function toggleCategory(container: ParentNode, key: string, checked: boolean) {
	const box = element<HTMLInputElement>(container, `[data-category="${key}"] input[type="checkbox"]`);
	box.checked = checked;
	box.dispatchEvent(new Event('change', { bubbles: true }));
	return box;
}

async function signIn() {
	auth.me.mockResolvedValue({ id: 1, username: 'alice', email: 'alice@example.com', is_admin: false, display_name: null });
	await fetchUser();
}

beforeEach(async () => {
	vi.clearAllMocks();
	resetTestClient();
	base.request.mockReset();
	connectNotificationWebSocket.mockReturnValue({ disconnect: vi.fn() });
	routeNotifications.settings.mockImplementation(notifications.settings);
	routeNotifications.updateSettings.mockImplementation(notifications.updateSettings);
	routeNotifications.subscription.mockImplementation(notifications.subscription);
	routeNotifications.subscribe.mockImplementation(notifications.subscribe);
	routeNotifications.unsubscribe.mockImplementation(notifications.unsubscribe);
	routeNotifications.markRead.mockImplementation(notifications.markRead);
	await signIn();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	document.body.innerHTML = '';
	await logout();
});

describe('notification settings page', () => {
	it('renders what the server stores', async () => {
		base.request.mockResolvedValue({ email: { ...SERVER_DEFAULTS, mention: false } });
		setTestPage('/settings/notifications', {});
		rendered = await renderComponent(NotificationSettingsPage);

		expect(callsTo('/users/me/notification-settings')).toHaveLength(1);
		const checked = Object.fromEntries(
			Array.from(rendered.container.querySelectorAll<HTMLElement>('[data-category]')).map((row) => [
				row.dataset.category,
				row.querySelector<HTMLInputElement>('input')!.checked,
			]),
		);
		expect(checked).toEqual({ ...SERVER_DEFAULTS, mention: false });
		expect(element(rendered.container, '[data-category="ci_triggered"]').textContent).toContain(
			'A push started CI in a repository you own',
		);
	});

	it('sends only the toggled key and shows the state the server answered with', async () => {
		base.request.mockImplementation(async (_path: string, init?: RequestInit) =>
			init?.method === 'PUT'
				? { email: { ...SERVER_DEFAULTS, ci_triggered: true, assigned: false } }
				: { email: SERVER_DEFAULTS },
		);
		setTestPage('/settings/notifications', {});
		rendered = await renderComponent(NotificationSettingsPage);

		toggleCategory(rendered.container, 'ci_triggered', true);
		await settle();

		const puts = callsTo('/users/me/notification-settings', 'PUT');
		expect(puts).toHaveLength(1);
		expect(JSON.parse(puts[0][1].body)).toEqual({ email: { ci_triggered: true } });
		// `assigned` changed elsewhere; the page shows the server's answer.
		expect(element<HTMLInputElement>(rendered.container, '[data-category="assigned"] input').checked).toBe(false);
		expect(element<HTMLInputElement>(rendered.container, '[data-category="ci_triggered"] input').checked).toBe(true);
		expect(rendered.container.querySelector('[role="status"]')?.textContent).toContain('Saved');
	});

	it('puts the switch back and shows the refusal when saving fails', async () => {
		base.request.mockResolvedValue({ email: SERVER_DEFAULTS });
		routeNotifications.updateSettings.mockRejectedValue(new ApiError('settings are read-only right now', 503));
		setTestPage('/settings/notifications', {});
		rendered = await renderComponent(NotificationSettingsPage);

		const box = toggleCategory(rendered.container, 'mention', false);
		await settle();

		expect(routeNotifications.updateSettings).toHaveBeenCalledWith({ mention: false });
		expect(box.checked).toBe(true);
		expect(element(rendered.container, '.save-error').textContent).toContain('settings are read-only right now');
	});
});

describe('thread subscription button', () => {
	function subscriptionServer(initial: { subscribed: boolean; reason: string | null }) {
		base.request.mockImplementation(async (_path: string, init?: RequestInit) => {
			if (init?.method === 'DELETE') return { subscribed: false, reason: 'commented' };
			if (init?.method === 'PUT') return { subscribed: true, reason: 'manual' };
			return initial;
		});
	}

	it('reads, unfollows and follows an issue, saying why', async () => {
		subscriptionServer({ subscribed: true, reason: 'commented' });
		rendered = await renderComponent(ThreadSubscription, { owner: 'alice', repo: 'demo', kind: 'issues', number: 5 });

		const path = '/repos/alice/demo/issues/5/subscription';
		expect(callsTo(path)).toHaveLength(1);
		const button = element<HTMLButtonElement>(rendered.container, '.btn-subscription');
		expect(button.textContent?.trim()).toBe('Unsubscribe');
		expect(element(rendered.container, '.subscription-reason').textContent).toContain('because you commented');

		await click(button);
		expect(callsTo(path, 'DELETE')).toHaveLength(1);
		expect(button.textContent?.trim()).toBe('Subscribe');
		expect(element(rendered.container, '.subscription-reason').textContent).toContain("You're not receiving");

		await click(button);
		expect(callsTo(path, 'PUT')).toHaveLength(1);
		expect(button.textContent?.trim()).toBe('Unsubscribe');
		expect(element(rendered.container, '.subscription-reason').textContent).toContain('because you subscribed');
	});

	it('addresses a pull request under /pulls', async () => {
		subscriptionServer({ subscribed: false, reason: null });
		rendered = await renderComponent(ThreadSubscription, { owner: 'alice', repo: 'demo', kind: 'pulls', number: 7 });

		await click(element(rendered.container, '.btn-subscription'));
		await click(element(rendered.container, '.btn-subscription'));

		expect(callsTo('/repos/alice/demo/pulls/7/subscription')).toHaveLength(1);
		expect(callsTo('/repos/alice/demo/pulls/7/subscription', 'PUT')).toHaveLength(1);
		expect(callsTo('/repos/alice/demo/pulls/7/subscription', 'DELETE')).toHaveLength(1);
	});

	it('is not offered to a signed-out reader', async () => {
		await logout();
		subscriptionServer({ subscribed: true, reason: 'author' });
		rendered = await renderComponent(ThreadSubscription, { owner: 'alice', repo: 'demo', kind: 'issues', number: 5 });

		expect(rendered.container.querySelector('.btn-subscription')).toBeNull();
		expect(base.request).not.toHaveBeenCalled();
	});
});

describe('notifications page', () => {
	const row = (overrides: Record<string, unknown>) => ({
		id: 9,
		user_id: 1,
		event_type: 'pull_request',
		title: 'Review requested on #7',
		body: '',
		repo_id: 3,
		is_read: false,
		created_at: '2026-10-09T08:00:00Z',
		reason: 'review_requested',
		subject_type: 'pull_request',
		link: '/alice/demo/pulls/7',
		...overrides,
	});

	beforeEach(() => {
		routeNotifications.unreadCount.mockResolvedValue({ unread_count: 1 });
	});

	it('follows the row link inside the app, marks the row read and labels the reason', async () => {
		routeNotifications.list.mockResolvedValue({ data: [row({})] });
		base.request.mockResolvedValue({});
		rendered = await renderComponent(NotificationsPage);

		const item = element(rendered.container, '.notif-item');
		expect(item.querySelector('.notif-icon')?.textContent).toBe('🔀');
		expect(item.querySelector('.notif-reason')?.textContent).toBe('Review requested');
		const link = element<HTMLAnchorElement>(item, 'a.notif-link');
		expect(link.getAttribute('href')).toBe('/alice/demo/pulls/7');

		const event = new MouseEvent('click', { bubbles: true, cancelable: true, button: 0 });
		link.dispatchEvent(event);
		await settle();

		expect(event.defaultPrevented).toBe(true);
		expect(callsTo('/notifications/9/read', 'POST')).toHaveLength(1);
		expect(navigation.goto).toHaveBeenCalledWith('/alice/demo/pulls/7');
	});

	it('leaves a row without a usable link as plain text', async () => {
		routeNotifications.list.mockResolvedValue({
			data: [
				row({ id: 1, link: null, reason: null, subject_type: null, event_type: 'push', title: 'Pushed to main' }),
				row({ id: 2, link: 'https://elsewhere.example/x', title: 'Off-site' }),
			],
		});
		rendered = await renderComponent(NotificationsPage);

		expect(rendered.container.querySelector('a.notif-link')).toBeNull();
		expect(rendered.container.textContent).toContain('Pushed to main');
		expect(rendered.container.querySelectorAll('.notif-reason')).toHaveLength(1);
	});
});
