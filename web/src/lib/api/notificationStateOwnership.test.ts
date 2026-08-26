import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import NotificationsPage from '../../routes/notifications/+page.svelte';
import { fetchUser, logout } from '../stores/auth.svelte';
import {
	auth,
	connectNotificationWebSocket,
	notifications,
	resetTestClient,
} from '../test/client';
import {
	check,
	click,
	element,
	renderComponent,
	settle,
	type RenderedComponent,
} from '../test/render';

type Deferred<T> = {
	promise: Promise<T>;
	resolve: (value: T) => void;
	reject: (reason?: unknown) => void;
};

function deferred<T>(): Deferred<T> {
	let resolve!: (value: T) => void;
	let reject!: (reason?: unknown) => void;
	const promise = new Promise<T>((resolvePromise, rejectPromise) => {
		resolve = resolvePromise;
		reject = rejectPromise;
	});
	return { promise, resolve, reject };
}

const timestamp = '2026-08-26T00:00:00Z';
const notification = (id: number, title: string, isRead: boolean) => ({
	id,
	event_type: 'issue',
	title,
	body: '',
	created_at: timestamp,
	is_read: isRead,
});
const list = (...data: ReturnType<typeof notification>[]) => ({ data });
const count = (unreadCount: number) => ({ unread_count: unreadCount });

let rendered: RenderedComponent | undefined;

beforeEach(async () => {
	resetTestClient();
	auth.me.mockResolvedValue({
		id: 1,
		username: 'alice',
		email: 'alice@example.com',
		is_admin: false,
		display_name: 'Alice',
	});
	connectNotificationWebSocket.mockReturnValue(null);
	await fetchUser();
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	await logout();
});

describe('notification async state ownership', () => {
	it('publishes list and count together and rejects a late response from the old filter', async () => {
		const oldList = deferred<ReturnType<typeof list>>();
		const oldCount = deferred<ReturnType<typeof count>>();
		const currentList = deferred<ReturnType<typeof list>>();
		const currentCount = deferred<ReturnType<typeof count>>();
		notifications.list
			.mockReturnValueOnce(oldList.promise)
			.mockReturnValueOnce(currentList.promise);
		notifications.unreadCount
			.mockReturnValueOnce(oldCount.promise)
			.mockReturnValueOnce(currentCount.promise);

		rendered = await renderComponent(NotificationsPage);
		await check(element(rendered.container, 'input[type="checkbox"]'), true);
		expect(notifications.list).toHaveBeenNthCalledWith(1, 1, false);
		expect(notifications.list).toHaveBeenNthCalledWith(2, 1, true);

		currentList.resolve(list(notification(2, 'current unread', false)));
		await settle();
		expect(rendered.container.textContent).not.toContain('current unread');
		expect(rendered.container.textContent).toContain('Loading');

		currentCount.resolve(count(1));
		await settle();
		expect(rendered.container.textContent).toContain('current unread');
		expect(rendered.container.textContent).toContain('(1)');

		oldList.resolve(list(notification(1, 'stale all', false)));
		oldCount.resolve(count(7));
		await settle();
		expect(rendered.container.textContent).toContain('current unread');
		expect(rendered.container.textContent).not.toContain('stale all');
		expect(rendered.container.textContent).toContain('(1)');
	});

	for (const mutationCase of [
		{ name: 'markRead', selector: '.btn-xs', invoke: notifications.markRead },
		{ name: 'markAllRead', selector: '.btn-sm', invoke: notifications.markAllRead },
	]) {
		it(`keeps ${mutationCase.name} after an older WebSocket refresh finishes`, async () => {
			let onWebSocketMessage: ((event: { event_type: string }) => void) | undefined;
			connectNotificationWebSocket.mockImplementation((onMessage: typeof onWebSocketMessage) => {
				onWebSocketMessage = onMessage;
				return {
					addEventListener: () => undefined,
				} as unknown as WebSocket;
			});
			const mutation = deferred<unknown>();
			const staleList = deferred<ReturnType<typeof list>>();
			const staleCount = deferred<ReturnType<typeof count>>();
			notifications.list
				.mockResolvedValueOnce(list(notification(1, 'old unread', false)))
				.mockReturnValueOnce(staleList.promise)
				.mockResolvedValueOnce(list(notification(1, 'current read', true)));
			notifications.unreadCount
				.mockResolvedValueOnce(count(1))
				.mockReturnValueOnce(staleCount.promise)
				.mockResolvedValueOnce(count(0));
			mutationCase.invoke.mockReturnValueOnce(mutation.promise);

			rendered = await renderComponent(NotificationsPage);
			const action = element<HTMLButtonElement>(rendered.container, mutationCase.selector);
			await click(action);
			expect(action.disabled).toBe(true);
			expect(action.getAttribute('aria-busy')).toBe('true');
			await click(action);
			expect(mutationCase.invoke).toHaveBeenCalledOnce();

			onWebSocketMessage?.({ event_type: 'push' });
			await settle();
			mutation.resolve(undefined);
			await settle();
			expect(rendered.container.textContent).toContain('current read');
			expect(rendered.container.textContent).not.toContain('(1)');

			staleList.resolve(list(notification(1, 'stale unread', false)));
			staleCount.resolve(count(1));
			await settle();
			expect(rendered.container.textContent).toContain('current read');
			expect(rendered.container.textContent).not.toContain('stale unread');
			expect(rendered.container.textContent).not.toContain('(1)');
		});
	}

	it('does not let a stale failure clear the current request loading state', async () => {
		const staleList = deferred<ReturnType<typeof list>>();
		const staleCount = deferred<ReturnType<typeof count>>();
		const currentList = deferred<ReturnType<typeof list>>();
		const currentCount = deferred<ReturnType<typeof count>>();
		notifications.list
			.mockReturnValueOnce(staleList.promise)
			.mockReturnValueOnce(currentList.promise);
		notifications.unreadCount
			.mockReturnValueOnce(staleCount.promise)
			.mockReturnValueOnce(currentCount.promise);

		rendered = await renderComponent(NotificationsPage);
		await check(element(rendered.container, 'input[type="checkbox"]'), true);
		staleList.reject(new Error('stale failure'));
		staleCount.resolve(count(9));
		await settle();
		expect(rendered.container.textContent).toContain('Loading');
		expect(rendered.container.querySelector('[role="alert"]')).toBeNull();

		currentList.resolve(list(notification(2, 'current result', false)));
		currentCount.resolve(count(1));
		await settle();
		expect(rendered.container.textContent).toContain('current result');
		expect(rendered.container.textContent).not.toContain('stale failure');
	});
});
