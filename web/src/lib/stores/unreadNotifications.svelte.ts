/// How many notifications the signed-in reader has not read, for the badge in
/// the navigation bar.
///
/// A notification used to be visible only to someone who opened
/// `/notifications` on their own: the link in the bar was bare text, and the
/// unread count was asked for by that page alone (card_a9dd3d9eccfe). The count
/// is re-read from the server rather than counted here, so a notification read
/// in another tab, or an unread row that was folded into an older one, never
/// leaves the badge wrong.

import { connectNotificationWebSocket, notifications } from '$lib/api/client.svelte';

/// Live pushes cover pushes, CI and access changes; issue and pull-request
/// notifications reach the inbox without one, so the count is also re-read on
/// this period while the tab is visible.
export const UNREAD_REFRESH_MS = 60_000;

let unread = $state(0);
let generation = 0;

export function getUnreadCount(): number {
  return unread;
}

/// The notifications page reads the exact count after each load and after
/// marking rows read; handing it here keeps the badge from disagreeing with the
/// page in front of the reader.
export function setUnreadCount(count: number): void {
  unread = Number.isFinite(count) && count > 0 ? Math.floor(count) : 0;
}

async function refresh(owner: number, userId: number) {
  try {
    const answer = await notifications.unreadCount(userId);
    if (owner === generation) setUnreadCount(Number(answer?.unread_count));
  } catch {
    // Keep the last known count; the next push or period asks again.
  }
}

/// Track `userId`'s unread count: now, on every live notification, and
/// periodically. Returns the function that stops tracking.
export function watchUnreadNotifications(userId: number): () => void {
  const owner = ++generation;
  unread = 0;
  void refresh(owner, userId);
  const socket = connectNotificationWebSocket(
    (event) => {
      if (event?.event_type) void refresh(owner, userId);
    },
    undefined,
    () => owner === generation,
  );
  const timer = setInterval(() => {
    if (typeof document === 'undefined' || document.visibilityState !== 'hidden') {
      void refresh(owner, userId);
    }
  }, UNREAD_REFRESH_MS);
  return () => {
    clearInterval(timer);
    socket?.disconnect();
    if (owner === generation) {
      generation += 1;
      unread = 0;
    }
  };
}
