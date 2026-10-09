<script lang="ts">
  import { page } from '$app/stores';
  import { lfsLocks, type LfsLock } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDateTime } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let locks = $state<LfsLock[]>([]);
  let nextCursor = $state('');
  let loading = $state(true);
  let loadingMore = $state(false);
  let unlockingId = $state<string | null>(null);
  let error = $state('');
  let success = $state('');
  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    locks = [];
    nextCursor = '';
    loading = true;
    loadingMore = false;
    unlockingId = null;
    error = '';
    success = '';
    void loadLocks(expectedOwner, expectedRepo);
  });

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number): boolean {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  async function loadLocks(expectedOwner: string, expectedRepo: string, cursor?: string) {
    const claim = listRequests.begin(expectedOwner, expectedRepo);
    try {
      error = '';
      const next = await lfsLocks.list(expectedOwner, expectedRepo, cursor);
      if (listRequests.owns(claim, owner, repo)) {
        locks = cursor ? [...locks, ...next.locks] : next.locks;
        nextCursor = next.next_cursor;
      }
    } catch (err: any) {
      if (listRequests.owns(claim, owner, repo)) {
        error = err.message || t('settings.lfs_locks.load_failed');
      }
    } finally {
      if (listRequests.owns(claim, owner, repo)) {
        loading = false;
        loadingMore = false;
      }
    }
  }

  async function loadMore() {
    if (loadingMore || !nextCursor) return;
    loadingMore = true;
    await loadLocks(owner, repo, nextCursor);
  }

  async function forceUnlock(lock: LfsLock) {
    if (unlockingId !== null) return;
    if (!(await confirmer.ask({
      title: t('settings.lfs_locks.force_unlock'),
      message: t('settings.lfs_locks.force_confirm', { path: lock.path, owner: lock.owner.name }),
      confirmLabel: t('settings.lfs_locks.force_unlock'),
    }))) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    unlockingId = lock.id;
    error = '';
    success = '';
    try {
      await lfsLocks.forceUnlock(expectedOwner, expectedRepo, lock.id);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      locks = locks.filter((candidate) => candidate.id !== lock.id);
      success = t('settings.lfs_locks.unlocked', { path: lock.path });
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = t('settings.lfs_locks.unlock_failed', { message: err.message || String(err) });
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) unlockingId = null;
    }
  }
</script>

<svelte:head><title>{t('settings.lfs_locks.title')} · {owner}/{repo}</title></svelte:head>

<div class="lfs-locks-page">
  <header>
    <h1>{t('settings.lfs_locks.title')}</h1>
    <p>{t('settings.lfs_locks.desc')}</p>
  </header>

  {#if error}<div class="message error" role="alert">{error}</div>{/if}
  {#if success}<div class="message success">{success}</div>{/if}

  {#if loading}
    <p>{t('common.loading')}</p>
  {:else if locks.length === 0}
    <div class="empty-state">{t('settings.lfs_locks.empty')}</div>
  {:else}
    <table class="lock-table">
      <thead>
        <tr>
          <th>{t('settings.lfs_locks.path')}</th>
          <th>{t('settings.lfs_locks.owner')}</th>
          <th>{t('settings.lfs_locks.locked_at')}</th>
          <th></th>
        </tr>
      </thead>
      <tbody>
        {#each locks as lock (lock.id)}
          <tr>
            <td><code>{lock.path}</code></td>
            <td>{lock.owner.name}</td>
            <td>{formatDateTime(lock.locked_at)}</td>
            <td class="actions">
              <button
                class="btn btn-danger"
                type="button"
                disabled={unlockingId !== null}
                aria-busy={unlockingId === lock.id}
                onclick={() => forceUnlock(lock)}
              >{t('settings.lfs_locks.force_unlock')}</button>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
    {#if nextCursor}
      <button class="btn more" type="button" disabled={loadingMore} aria-busy={loadingMore} onclick={loadMore}>
        {t('settings.lfs_locks.more')}
      </button>
    {/if}
  {/if}
</div>

<ConfirmModal {confirmer} />

<style>
  .lfs-locks-page { max-width: 880px; }
  header { margin-bottom: 28px; }
  h1 { margin-bottom: 6px; }
  header p { color: var(--text-secondary); }
  .message, .empty-state { margin-bottom: 18px; padding: 12px 14px; border: 1px solid var(--border); border-radius: var(--radius); }
  .error { color: var(--red); }
  .success { color: var(--green); }
  .lock-table { width: 100%; border-collapse: collapse; }
  .lock-table th, .lock-table td { padding: 10px 12px; border-bottom: 1px solid var(--border); text-align: left; }
  .lock-table code { font-family: var(--font-mono, monospace); overflow-wrap: anywhere; }
  .actions { text-align: right; }
  .more { margin-top: 12px; }
</style>
