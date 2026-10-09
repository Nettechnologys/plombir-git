<script lang="ts">
  import { page } from '$app/stores';
  import {
    lfsStorage,
    type LfsObject,
    type LfsPruneOutcome,
    type LfsUsage,
  } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDateTime } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let usage = $state<LfsUsage | null>(null);
  let objects = $state<LfsObject[]>([]);
  let nextCursor = $state('');
  let loading = $state(true);
  let loadingMore = $state(false);
  let error = $state('');
  let orphans = $state<LfsObject[] | null>(null);
  let graceHours = $state(24);
  let scanning = $state(false);
  let selected = $state<Set<string>>(new Set());
  let removing = $state(false);
  let outcome = $state<LfsPruneOutcome | null>(null);
  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    usage = null;
    objects = [];
    nextCursor = '';
    loading = true;
    loadingMore = false;
    error = '';
    orphans = null;
    scanning = false;
    selected = new Set();
    removing = false;
    outcome = null;
    void load(expectedOwner, expectedRepo);
  });

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number): boolean {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
  }

  async function load(expectedOwner: string, expectedRepo: string, cursor?: string) {
    const claim = listRequests.begin(expectedOwner, expectedRepo);
    try {
      error = '';
      const [nextUsage, page] = await Promise.all([
        cursor ? Promise.resolve(usage) : lfsStorage.usage(expectedOwner, expectedRepo),
        lfsStorage.objects(expectedOwner, expectedRepo, cursor),
      ]);
      if (listRequests.owns(claim, owner, repo)) {
        usage = nextUsage;
        objects = cursor ? [...objects, ...page.objects] : page.objects;
        nextCursor = page.next_cursor;
      }
    } catch (err: any) {
      if (listRequests.owns(claim, owner, repo)) {
        error = err.message || t('settings.lfs_storage.load_failed');
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
    await load(owner, repo, nextCursor);
  }

  async function findOrphans() {
    if (scanning) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    scanning = true;
    error = '';
    outcome = null;
    try {
      const found = await lfsStorage.orphans(expectedOwner, expectedRepo);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      orphans = found.objects;
      graceHours = found.grace_hours;
      selected = new Set(found.objects.map((object) => object.oid));
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.lfs_storage.load_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) scanning = false;
    }
  }

  function toggle(oid: string, checked: boolean) {
    const next = new Set(selected);
    if (checked) next.add(oid);
    else next.delete(oid);
    selected = next;
  }

  function removeSelected() {
    return remove([...selected]);
  }

  /**
   * Ask the server to remove `oids`. It re-reads the refs and keeps, with the
   * reason, anything still referenced or too recent — so removing one object
   * from the full list is as safe as removing it from the orphan list.
   */
  async function remove(oids: string[]) {
    if (removing || oids.length === 0) return;
    if (!(await confirmer.ask({
      title: t('settings.lfs_storage.remove_confirm_title'),
      message: t('settings.lfs_storage.remove_confirm', { count: oids.length }),
      confirmLabel: t('settings.lfs_storage.remove'),
    }))) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    removing = true;
    error = '';
    try {
      const result = await lfsStorage.prune(expectedOwner, expectedRepo, oids);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      outcome = result;
      const deleted = new Set(result.deleted);
      orphans = (orphans ?? []).filter((object) => !deleted.has(object.oid));
      selected = new Set([...selected].filter((oid) => !deleted.has(oid)));
      await load(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = t('settings.lfs_storage.remove_failed', { message: err.message || String(err) });
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) removing = false;
    }
  }
</script>

<svelte:head><title>{t('settings.lfs_storage.title')} · {owner}/{repo}</title></svelte:head>

<div class="lfs-storage-page">
  <header>
    <h1>{t('settings.lfs_storage.title')}</h1>
    <p>{t('settings.lfs_storage.desc')}</p>
    {#if usage}
      <p class="usage">
        {t('settings.lfs_storage.usage', { count: usage.object_count, size: formatSize(usage.total_bytes) })}
      </p>
    {/if}
  </header>

  {#if error}<div class="message error" role="alert">{error}</div>{/if}
  {#if outcome}
    <div class="message success outcome">
      <p>{t('settings.lfs_storage.removed', { deleted: outcome.deleted.length })}</p>
      {#each outcome.kept as kept}
        <p class="kept">{t('settings.lfs_storage.kept', { oid: kept.oid.slice(0, 12), reason: kept.reason })}</p>
      {/each}
    </div>
  {/if}

  <section>
    <button class="btn btn-secondary" type="button" disabled={scanning} aria-busy={scanning} onclick={findOrphans}>
      {scanning ? t('settings.lfs_storage.scanning') : t('settings.lfs_storage.find_orphans')}
    </button>
    {#if orphans !== null}
      <div class="orphans">
        <h2>{t('settings.lfs_storage.orphans_title')}</h2>
        {#if orphans.length === 0}
          <div class="empty-state">{t('settings.lfs_storage.no_orphans')}</div>
        {:else}
          <p>{t('settings.lfs_storage.orphans_desc', { hours: graceHours })}</p>
          <ul class="orphan-list">
            {#each orphans as object (object.oid)}
              <li>
                <label>
                  <input
                    type="checkbox"
                    checked={selected.has(object.oid)}
                    onchange={(event) => toggle(object.oid, (event.currentTarget as HTMLInputElement).checked)}
                  />
                  <code>{object.oid.slice(0, 12)}</code>
                  <span>{formatSize(object.size)}</span>
                  <span>{formatDateTime(object.created_at)}</span>
                </label>
              </li>
            {/each}
          </ul>
          <button
            class="btn btn-danger"
            type="button"
            disabled={removing || selected.size === 0}
            aria-busy={removing}
            onclick={removeSelected}
          >{t('settings.lfs_storage.remove_selected')}</button>
        {/if}
      </div>
    {/if}
  </section>

  <section>
    {#if loading}
      <p>{t('common.loading')}</p>
    {:else if objects.length === 0}
      <div class="empty-state">{t('settings.lfs_storage.empty')}</div>
    {:else}
      <table class="object-table">
        <thead>
          <tr>
            <th>{t('settings.lfs_storage.oid')}</th>
            <th>{t('settings.lfs_storage.size')}</th>
            <th>{t('settings.lfs_storage.created_at')}</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each objects as object (object.oid)}
            <tr>
              <td>
                <code title={object.oid}>{object.oid.slice(0, 12)}</code>
                {#if !object.uploaded}<span class="pending">{t('settings.lfs_storage.not_uploaded')}</span>{/if}
              </td>
              <td>{formatSize(object.size)}</td>
              <td>{formatDateTime(object.created_at)}</td>
              <td class="actions">
                <button
                  class="btn btn-danger btn-sm"
                  type="button"
                  disabled={removing}
                  onclick={() => remove([object.oid])}
                >{t('settings.lfs_storage.remove')}</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
      {#if nextCursor}
        <button class="btn more" type="button" disabled={loadingMore} aria-busy={loadingMore} onclick={loadMore}>
          {t('settings.lfs_storage.more')}
        </button>
      {/if}
    {/if}
  </section>
</div>

<ConfirmModal {confirmer} />

<style>
  .lfs-storage-page { max-width: 880px; }
  header, section { margin-bottom: 28px; }
  h1 { margin-bottom: 6px; }
  header p { color: var(--text-secondary); }
  .usage { font-weight: 600; color: var(--text-primary); }
  .message, .empty-state { margin: 12px 0 18px; padding: 12px 14px; border: 1px solid var(--border); border-radius: var(--radius); }
  .message p { margin: 0 0 4px; }
  .error { color: var(--red); }
  .success { color: var(--green); }
  .kept { color: var(--text-secondary); }
  .orphans { margin-top: 16px; }
  .orphan-list { list-style: none; padding: 0; display: grid; gap: 6px; margin-bottom: 12px; }
  .orphan-list label { display: flex; gap: 12px; align-items: center; }
  .object-table { width: 100%; border-collapse: collapse; }
  .object-table th, .object-table td { padding: 8px 12px; border-bottom: 1px solid var(--border); text-align: left; }
  code { font-family: var(--font-mono, monospace); }
  .pending { margin-left: 8px; color: var(--text-secondary); font-size: 12px; }
  .actions { text-align: right; }
  .more { margin-top: 12px; }
</style>
