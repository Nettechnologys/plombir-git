import { describe, expect, it } from 'vitest';

import webhooksPageSource from '../../routes/[owner]/[repo]/settings/webhooks/+page.svelte?raw';
import en from '../i18n/translations/en.json';
import zhCN from '../i18n/translations/zh-CN.json';
import {
  reloadDeliveriesAfterRedelivery,
  webhookDeliveryOutcome,
} from './webhookDelivery';
import type { WebhookDelivery } from './webhooks';

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
});

describe('the webhook settings page', () => {
  it('has production callers for the three formerly disconnected client methods', () => {
    expect(webhooksPageSource).toContain('webhooks.get(');
    expect(webhooksPageSource).toContain('webhooks.deliveries(');
    expect(webhooksPageSource).toContain('webhooks.redeliver(');
  });

  it('blocks duplicate redelivery and reloads the asynchronously persisted outcome', () => {
    expect(webhooksPageSource).toContain('if (!selectedHook || redeliveringId !== null) return;');
    expect(webhooksPageSource).toContain('disabled={redeliveringId !== null || deliveriesLoading}');
    expect(webhooksPageSource).toContain('reloadDeliveriesAfterRedelivery(');
  });

  it('renders the recorded diagnostics instead of reducing a delivery to a success flag', () => {
    for (const field of ['delivery_id', 'created_at', 'response_status', 'duration_ms', 'request_payload', 'response_body']) {
      expect(webhooksPageSource).toContain(`delivery.${field}`);
    }
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
