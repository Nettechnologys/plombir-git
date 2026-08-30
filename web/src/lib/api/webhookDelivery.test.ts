import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import WebhooksPage from '../../routes/[owner]/[repo]/settings/webhooks/+page.svelte';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import {
  reloadDeliveriesAfterRedelivery,
  webhookDeliveryOutcome,
} from './webhookDelivery';
import type { WebhookDelivery } from './webhooks';
import { setTestPage } from '../test/app';
import { resetTestClient, webhooks } from '../test/client';
import { button, click, element, renderComponent, settle, type RenderedComponent } from '../test/render';

let rendered: RenderedComponent | undefined;

const hook = {
	id: 10,
	url: 'https://example.com/hook',
	content_type: 'json',
	events: 'push',
	active: true,
};

function delivery(overrides: Partial<WebhookDelivery> = {}): WebhookDelivery {
  return {
    id: 1,
    webhook_id: 10,
    event: 'push',
    delivery_id: '11111111-1111-1111-1111-111111111111',
    response_status: 200,
    request_payload: '{"ref":"refs/heads/main"}',
    response_body: null,
    duration_ms: 25,
    created_at: '2026-08-15T12:00:00Z',
    ...overrides,
  };
}

beforeEach(() => {
	resetTestClient();
	setTestPage('/alice/demo/settings/webhooks', { owner: 'alice', repo: 'demo' });
	const source = delivery();
	const replay = delivery({ id: 3, delivery_id: '33333333-3333-3333-3333-333333333333' });
	webhooks.list.mockResolvedValue([hook]);
	webhooks.get.mockResolvedValue(hook);
	webhooks.deliveries
		.mockResolvedValueOnce([source])
		.mockResolvedValue([replay, source]);
});

afterEach(async () => {
	await rendered?.destroy();
	rendered = undefined;
	vi.clearAllTimers();
	vi.useRealTimers();
});

describe('webhook delivery status', () => {
  it('distinguishes a pending row from a transport failure with no HTTP status', () => {
    expect(webhookDeliveryOutcome(delivery({ response_status: null, duration_ms: null }))).toBe('pending');
    expect(webhookDeliveryOutcome(delivery({ response_status: null, duration_ms: 30 }))).toBe('failure');
  });

  it('treats only 2xx responses as successful deliveries', () => {
    expect(webhookDeliveryOutcome(delivery({ response_status: 204 }))).toBe('success');
    expect(webhookDeliveryOutcome(delivery({ response_status: 302 }))).toBe('failure');
    expect(webhookDeliveryOutcome(delivery({ response_status: 500 }))).toBe('failure');
  });
});

describe('redelivery refresh', () => {
  it('waits for a new row that matches the selected delivery rather than any concurrent event', async () => {
    const source = delivery();
    const unrelated = delivery({ id: 2, delivery_id: '22222222-2222-2222-2222-222222222222', event: 'issue.opened' });
    const replay = delivery({ id: 3, delivery_id: '33333333-3333-3333-3333-333333333333' });
    const snapshots = [
      [unrelated, source],
      [replay, unrelated, source],
    ];
    const sleeps: number[] = [];
    let reads = 0;

    const refreshed = await reloadDeliveriesAfterRedelivery(
      async () => snapshots[Math.min(reads++, snapshots.length - 1)],
      [source],
      source,
      {
        delays: [0, 10, 20],
        sleep: async (milliseconds) => { sleeps.push(milliseconds); },
      },
    );

    expect(refreshed[0]).toEqual(replay);
    expect(reads).toBe(2);
    expect(sleeps).toEqual([10]);
  });

  it('returns the latest server state when the replay row outlives the refresh window', async () => {
    const source = delivery();
    const latest = [delivery({ id: 2, event: 'issue.closed' }), source];

    await expect(reloadDeliveriesAfterRedelivery(
      async () => latest,
      [source],
      source,
      { delays: [0], sleep: async () => {} },
    )).resolves.toEqual(latest);
  });

  it('cancels an outstanding delay without starting another load', async () => {
    vi.useFakeTimers();
    const source = delivery();
    const controller = new AbortController();
    const load = vi.fn(async () => [source]);

    const polling = reloadDeliveriesAfterRedelivery(load, [source], source, {
      delays: [0, 10_000],
      signal: controller.signal,
    });
    await vi.advanceTimersByTimeAsync(0);

    expect(load).toHaveBeenCalledOnce();
    expect(vi.getTimerCount()).toBe(1);

    controller.abort();
    controller.abort();
    await expect(polling).rejects.toMatchObject({ name: 'AbortError' });
    expect(vi.getTimerCount()).toBe(0);

    await vi.advanceTimersByTimeAsync(10_000);
    expect(load).toHaveBeenCalledOnce();
  });
});

describe('the webhook settings page', () => {
	it('loads and renders the recorded delivery diagnostics', async () => {
		rendered = await renderComponent(WebhooksPage);
		await click(button(rendered.container, 'View deliveries'));

		expect(webhooks.get).toHaveBeenCalledWith('alice', 'demo', 10);
		expect(webhooks.deliveries).toHaveBeenCalledWith('alice', 'demo', 10);
		const item = element(rendered.container, '.delivery-item');
		expect(item.textContent).toContain('HTTP 200');
		expect(item.textContent).toContain('11111111-1111-1111-1111-111111111111');
		expect(item.textContent).toContain('25 ms');
		expect(item.textContent).toContain('{"ref":"refs/heads/main"}');
	});

	it('redelivers from the rendered row and reloads the persisted replay', async () => {
		rendered = await renderComponent(WebhooksPage);
		await click(button(rendered.container, 'View deliveries'));
		await click(button(rendered.container, 'Redeliver'));

		expect(webhooks.redeliver).toHaveBeenCalledWith('alice', 'demo', 10, 1);
		expect(rendered.container.textContent).toContain('33333333-3333-3333-3333-333333333333');
	});

	it('cancels pending polling when the page is destroyed', async () => {
		vi.useFakeTimers();
		const source = delivery();
		webhooks.deliveries.mockReset();
		webhooks.deliveries.mockResolvedValue([source]);

		rendered = await renderComponent(WebhooksPage);
		await click(button(rendered.container, 'View deliveries'));
		await click(button(rendered.container, 'Redeliver'));

		expect(webhooks.deliveries).toHaveBeenCalledTimes(2);
		expect(vi.getTimerCount()).toBe(1);

		await rendered.destroy();
		rendered = undefined;
		expect(vi.getTimerCount()).toBe(0);
		await vi.advanceTimersByTimeAsync(60_000);

		expect(webhooks.deliveries).toHaveBeenCalledTimes(2);
	});

	it('cancels pending polling when the repository route changes', async () => {
		vi.useFakeTimers();
		const source = delivery();
		webhooks.deliveries.mockReset();
		webhooks.deliveries.mockResolvedValue([source]);

		rendered = await renderComponent(WebhooksPage);
		await click(button(rendered.container, 'View deliveries'));
		await click(button(rendered.container, 'Redeliver'));

		expect(webhooks.deliveries).toHaveBeenCalledTimes(2);
		expect(vi.getTimerCount()).toBe(1);

		setTestPage('/bob/other/settings/webhooks', { owner: 'bob', repo: 'other' });
		await settle();
		expect(vi.getTimerCount()).toBe(0);
		await vi.advanceTimersByTimeAsync(60_000);

		expect(webhooks.deliveries).toHaveBeenCalledTimes(2);
	});

	it('does not publish an old hook refresh after another hook is opened', async () => {
		const source = delivery();
		const replay = delivery({ id: 3, delivery_id: '33333333-3333-3333-3333-333333333333' });
		const otherHook = { ...hook, id: 20, url: 'https://example.com/other' };
		const otherDelivery = delivery({
			id: 20,
			webhook_id: 20,
			delivery_id: '20202020-2020-2020-2020-202020202020',
		});
		let resolveOldPoll!: (deliveries: WebhookDelivery[]) => void;
		const oldPoll = new Promise<WebhookDelivery[]>((resolve) => { resolveOldPoll = resolve; });
		let firstHookReads = 0;

		webhooks.list.mockResolvedValue([hook, otherHook]);
		webhooks.get.mockImplementation(async (_owner: string, _repo: string, hookId: number) => (
			hookId === hook.id ? hook : otherHook
		));
		webhooks.deliveries.mockReset();
		webhooks.deliveries.mockImplementation(async (_owner: string, _repo: string, hookId: number) => {
			if (hookId === hook.id) {
				firstHookReads += 1;
				return firstHookReads === 1 ? [source] : oldPoll;
			}
			return [otherDelivery];
		});

		rendered = await renderComponent(WebhooksPage);
		await click(button(rendered.container, 'View deliveries'));
		await click(button(rendered.container, 'Redeliver'));
		await click(button(rendered.container, 'View deliveries'));

		expect(rendered.container.textContent).toContain(otherHook.url);
		expect(rendered.container.textContent).toContain(otherDelivery.delivery_id);

		resolveOldPoll([replay, source]);
		await Promise.resolve();
		await Promise.resolve();

		expect(rendered.container.textContent).toContain(otherDelivery.delivery_id);
		expect(rendered.container.textContent).not.toContain(replay.delivery_id);
	});

	it('still reports a genuine refresh failure for the current hook', async () => {
		const source = delivery();
		webhooks.deliveries.mockReset();
		webhooks.deliveries
			.mockResolvedValueOnce([source])
			.mockRejectedValueOnce(new Error('refresh backend unavailable'));

		rendered = await renderComponent(WebhooksPage);
		await click(button(rendered.container, 'View deliveries'));
		await click(button(rendered.container, 'Redeliver'));

		expect(rendered.container.textContent).toContain('refresh backend unavailable');
	});

  it.each([
    'deliveries_title',
    'view_deliveries',
    'hide_deliveries',
    'refresh_deliveries',
    'deliveries_empty',
    'deliveries_load_failed',
    'redeliver',
    'redelivering',
    'redelivery_triggered',
    'redelivery_failed',
    'redelivery_refresh_failed',
    'delivery_pending',
    'delivery_failed',
    'delivery_id',
    'delivery_duration',
    'request_payload',
    'response_details',
    'not_recorded',
  ])('has a real label in both catalogs: settings.webhooks.%s', (key) => {
    expect(en.settings.webhooks).toHaveProperty(key);
    expect(zhCN.settings.webhooks).toHaveProperty(key);
  });
});
