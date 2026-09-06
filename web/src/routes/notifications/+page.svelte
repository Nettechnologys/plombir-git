<script lang="ts">
  import { notifications, connectNotificationWebSocket } from '$lib/api/client.svelte';
  import { getUser, isLoggedIn } from '$lib/stores/auth.svelte';
  import { createT, formatDateTime } from '$lib/i18n';

  const t = createT();

  let notifs = $state<any[]>([]);
  let unreadCount = $state(0);
  let loading = $state(true);
  let loadError = $state('');
  let filterUnread = $state(false);
  let wsConnected = $state(false);
  let marking = $state(false);
  let loadGeneration = 0;

  type LoadClaim = Readonly<{
    generation: number;
    userId: number | undefined;
    unreadOnly: boolean;
  }>;

  function ownsLoad(claim: LoadClaim): boolean {
    return claim.generation === loadGeneration
      && claim.userId === getUser()?.id
      && claim.unreadOnly === filterUnread;
  }

  async function load() {
    const claim: LoadClaim = {
      generation: ++loadGeneration,
      userId: getUser()?.id,
      unreadOnly: filterUnread,
    };
    loading = true;
    loadError = '';
    try {
      const [listData, countData] = await Promise.all([
        notifications.list(claim.userId, claim.unreadOnly),
        notifications.unreadCount(claim.userId),
      ]);
      if (!ownsLoad(claim)) return;
      notifs = listData.data;
      unreadCount = countData.unread_count || 0;
    } catch (e) {
      if (!ownsLoad(claim)) return;
      loadError = e instanceof Error ? e.message : t('errors.load_failed');
      console.error('Failed to load notifications:', e);
    } finally {
      if (ownsLoad(claim)) loading = false;
    }
  }

  async function markRead(id: number) {
    if (marking) return;
    marking = true;
    try {
      await notifications.markRead(id);
      await load();
    } catch (e) {
      console.error('Failed to mark as read:', e);
    } finally {
      marking = false;
    }
  }

  async function markAllRead() {
    if (marking) return;
    marking = true;
    try {
      const userId = getUser()?.id;
      await notifications.markAllRead(userId);
      await load();
    } catch (e) {
      console.error('Failed to mark all as read:', e);
    } finally {
      marking = false;
    }
  }

  function eventIcon(type: string): string {
    switch (type) {
      case 'push': return '📦';
      case 'ci_triggered': return '🔧';
      case 'issue': return '❗';
      case 'pr': case 'pull_request': return '🔀';
      case 'review': return '👀';
      case 'pipeline': return '🔧';
      default: return '🔔';
    }
  }

  function setupWebSocket() {
    return connectNotificationWebSocket(
      (event) => {
        if (event.event_type === 'push' || event.event_type === 'ci_triggered') {
          unreadCount++;
          void load();
        }
      },
      () => {
        wsConnected = false;
      },
      () => isLoggedIn() === true,
      (connected) => {
        wsConnected = connected;
      },
    );
  }

  void load();
  $effect(() => {
    if (!isLoggedIn()) {
      wsConnected = false;
      return;
    }

    const connection = setupWebSocket();
    return () => {
      connection.disconnect();
      wsConnected = false;
      loadGeneration++;
    };
  });
</script>

<div class="container">
  <div class="header">
    <h1>{t('notifications.title')} {unreadCount > 0 ? `(${unreadCount})` : ''}</h1>
    <div class="actions">
      <span class="ws-status" class:connected={wsConnected}>
        {wsConnected ? `🟢 ${t('nav.live')}` : `🔴 ${t('nav.offline')}`}
      </span>
      <label class="filter">
        <input type="checkbox" bind:checked={filterUnread} onchange={load} />
        {t('notifications.unread_only')}
      </label>
      {#if unreadCount > 0}
        <button class="btn-sm" onclick={markAllRead} disabled={marking} aria-busy={marking}>
          {t('notifications.mark_all_read')}
        </button>
      {/if}
    </div>
  </div>

  {#if loading}
    <p>{t('common.loading')}</p>
  {:else if loadError}
    <div class="error-banner" role="alert">{loadError}</div>
  {:else if notifs.length === 0}
    <div class="empty-state">
      <p>{t('notifications.empty')}</p>
      {#if wsConnected}
        <p class="hint">{t('notifications.hint')}</p>
      {/if}
    </div>
  {:else}
    <div class="notif-list">
      {#each notifs as notif}
        <div class="notif-item" class:unread={!notif.is_read}>
          <div class="notif-icon">{eventIcon(notif.event_type)}</div>
          <div class="notif-content">
            <div class="notif-title">{notif.title}</div>
            {#if notif.body}<div class="notif-body">{notif.body}</div>{/if}
            <div class="notif-meta">
              <span class="notif-type">{notif.event_type}</span>
              <span class="notif-time">{formatDateTime(notif.created_at)}</span>
            </div>
          </div>
          <div class="notif-actions">
            {#if !notif.is_read}
              <button
                class="btn-xs"
                onclick={() => markRead(notif.id)}
                disabled={marking}
                aria-busy={marking}
              >
                {t('notifications.mark_read')}
              </button>
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 1.5rem; }
  h1 { color: var(--text-primary); margin: 0; }
  .actions { display: flex; align-items: center; gap: 1rem; }
  .ws-status { font-size: 0.8rem; color: var(--text-secondary); }
  .ws-status.connected { color: #22c55e; }
  .filter { display: flex; align-items: center; gap: 0.3rem; color: var(--text-secondary); font-size: 0.9rem; cursor: pointer; }
  .btn-sm { background: var(--accent); color: white; border: none; border-radius: 4px; padding: 0.4rem 0.8rem; cursor: pointer; font-size: 0.85rem; }
  .btn-xs { background: transparent; color: var(--accent); border: 1px solid var(--accent); border-radius: 4px; padding: 0.2rem 0.5rem; cursor: pointer; font-size: 0.75rem; }
  .empty-state { text-align: center; padding: 3rem; color: var(--text-secondary); }
  .hint { font-size: 0.85rem; color: var(--text-secondary); margin-top: 0.5rem; }
  .notif-list { display: flex; flex-direction: column; gap: 0; }
  .notif-item { display: flex; align-items: flex-start; gap: 0.75rem; padding: 0.75rem 1rem; border-bottom: 1px solid var(--border); background: var(--bg-secondary); }
  .notif-item:first-child { border-radius: 8px 8px 0 0; }
  .notif-item:last-child { border-radius: 0 0 8px 8px; border-bottom: none; }
  .notif-item.unread { border-left: 3px solid var(--accent); }
  .notif-icon { font-size: 1.2rem; flex-shrink: 0; margin-top: 0.1rem; }
  .notif-content { flex: 1; min-width: 0; }
  .notif-title { color: var(--text-primary); font-weight: 500; }
  .notif-item.unread .notif-title { font-weight: 700; }
  .notif-body { color: var(--text-secondary); font-size: 0.85rem; margin-top: 0.2rem; }
  .notif-meta { display: flex; gap: 0.75rem; margin-top: 0.3rem; font-size: 0.8rem; }
  .notif-type { color: var(--accent); text-transform: uppercase; font-size: 0.7rem; font-weight: 600; }
  .notif-time { color: var(--text-secondary); }
  .notif-actions { flex-shrink: 0; }
</style>
