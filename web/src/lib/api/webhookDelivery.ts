import type { WebhookDelivery } from './webhooks';

export type WebhookDeliveryOutcome = 'pending' | 'success' | 'failure';

// A delivery row is written after the detached outbound request finishes. The
// server bounds that request at 30 seconds, so the final refresh window extends
// just past that bound while returning immediately when the replay row appears.
export const REDELIVERY_REFRESH_DELAYS_MS = [0, 250, 750, 1_500, 3_000, 6_000, 12_000, 12_000] as const;

export function webhookDeliveryOutcome(delivery: WebhookDelivery): WebhookDeliveryOutcome {
  if (delivery.duration_ms === null) return 'pending';
  return delivery.response_status !== null
    && delivery.response_status >= 200
    && delivery.response_status < 300
    ? 'success'
    : 'failure';
}

function isNewReplay(
  candidate: WebhookDelivery,
  source: WebhookDelivery,
  knownIds: ReadonlySet<number>,
): boolean {
  return !knownIds.has(candidate.id)
    && candidate.event === source.event
    && candidate.request_payload === source.request_payload;
}

export interface DeliveryRefreshOptions {
  delays?: readonly number[];
  signal?: AbortSignal;
  sleep?: (milliseconds: number, signal?: AbortSignal) => Promise<void>;
}

function abortReason(signal: AbortSignal): unknown {
  return signal.reason ?? new DOMException('Webhook delivery refresh was cancelled', 'AbortError');
}

function throwIfAborted(signal?: AbortSignal): void {
  if (signal?.aborted) throw abortReason(signal);
}

function sleepUntil(milliseconds: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal?.removeEventListener('abort', onAbort);
      resolve();
    }, milliseconds);
    const onAbort = () => {
      clearTimeout(timer);
      signal?.removeEventListener('abort', onAbort);
      reject(abortReason(signal!));
    };

    if (signal?.aborted) {
      onAbort();
    } else {
      signal?.addEventListener('abort', onAbort, { once: true });
    }
  });
}

/**
 * Reload until the detached replay has produced its persisted delivery row.
 *
 * Matching both event and the exact recorded payload avoids treating an
 * unrelated delivery that happened concurrently as completion of this replay.
 */
export async function reloadDeliveriesAfterRedelivery(
  load: () => Promise<WebhookDelivery[]>,
  previous: readonly WebhookDelivery[],
  source: WebhookDelivery,
  options: DeliveryRefreshOptions = {},
): Promise<WebhookDelivery[]> {
  const delays = options.delays ?? REDELIVERY_REFRESH_DELAYS_MS;
  const sleep = options.sleep ?? sleepUntil;
  const knownIds = new Set(previous.map((delivery) => delivery.id));
  let latest = [...previous];

  for (const delay of delays) {
    throwIfAborted(options.signal);
    if (delay > 0) await sleep(delay, options.signal);
    throwIfAborted(options.signal);
    latest = await load();
    throwIfAborted(options.signal);
    if (latest.some((candidate) => isNewReplay(candidate, source, knownIds))) {
      return latest;
    }
  }

  return latest;
}
