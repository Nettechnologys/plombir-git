import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

vi.mock('./_base.svelte', () => ({ API_BASE: '/api/v1' }));

import { connectNotificationWebSocket } from './websockets';

class FakeWebSocket {
	static instances: FakeWebSocket[] = [];

	closeCalls = 0;
	onopen: ((event: Event) => void) | null = null;
	onmessage: ((event: MessageEvent) => void) | null = null;
	onerror: ((event: Event) => void) | null = null;
	onclose: ((event: CloseEvent) => void) | null = null;

	constructor(readonly url: string) {
		FakeWebSocket.instances.push(this);
	}

	open() {
		this.onopen?.(new Event('open'));
	}

	close() {
		this.closeCalls += 1;
		this.onclose?.(new CloseEvent('close'));
	}

	message(data: unknown) {
		this.onmessage?.(new MessageEvent('message', { data: JSON.stringify(data) }));
	}

	failHandshake() {
		this.onerror?.(new Event('error'));
		this.close();
	}
}

describe('connectNotificationWebSocket reconnect policy', () => {
	beforeEach(() => {
		FakeWebSocket.instances = [];
		vi.useFakeTimers();
		vi.stubGlobal('WebSocket', FakeWebSocket);
	});

	afterEach(() => {
		vi.useRealTimers();
		vi.unstubAllGlobals();
	});

	it('does not retry a handshake that never reached open', async () => {
		connectNotificationWebSocket(vi.fn(), vi.fn(), () => true);
		FakeWebSocket.instances[0].failHandshake();

		await vi.advanceTimersByTimeAsync(10_000);

		expect(FakeWebSocket.instances).toHaveLength(1);
	});

	it('retries an established connection after five seconds while still logged in', async () => {
		connectNotificationWebSocket(vi.fn(), vi.fn(), () => true);
		FakeWebSocket.instances[0].open();
		FakeWebSocket.instances[0].close();

		await vi.advanceTimersByTimeAsync(4_999);
		expect(FakeWebSocket.instances).toHaveLength(1);

		await vi.advanceTimersByTimeAsync(1);
		expect(FakeWebSocket.instances).toHaveLength(2);
	});

	it('does not retry when the live auth state says the user logged out', async () => {
		const shouldReconnect = vi.fn(() => false);
		connectNotificationWebSocket(vi.fn(), vi.fn(), shouldReconnect);
		FakeWebSocket.instances[0].open();
		FakeWebSocket.instances[0].close();

		await vi.advanceTimersByTimeAsync(5_000);

		expect(shouldReconnect).toHaveBeenCalledOnce();
		expect(FakeWebSocket.instances).toHaveLength(1);
	});

	it('closes the current socket and suppresses callbacks after an idempotent disconnect', () => {
		const onMessage = vi.fn();
		const connection = connectNotificationWebSocket(onMessage);
		const socket = FakeWebSocket.instances[0];
		socket.open();

		connection.disconnect();
		connection.disconnect();
		socket.message({ event_type: 'push' });

		expect(socket.closeCalls).toBe(1);
		expect(onMessage).not.toHaveBeenCalled();
	});

	it('cancels a reconnect already scheduled by an established socket', async () => {
		const connection = connectNotificationWebSocket(vi.fn());
		FakeWebSocket.instances[0].open();
		FakeWebSocket.instances[0].close();

		connection.disconnect();
		await vi.advanceTimersByTimeAsync(5_000);

		expect(FakeWebSocket.instances).toHaveLength(1);
	});
});
