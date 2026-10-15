<script lang="ts">
  import { page } from '$app/stores';
  import { pushMirrors, type PushMirror } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDateTime } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let mirrors = $state<PushMirror[]>([]);
  let loading = $state(true);
  let creating = $state(false);
  /** The id of the mirror a row action is running for. */
  let busyId = $state<number | null>(null);
  let error = $state('');
  let success = $state('');
  let url = $state('');
  let username = $state('');
  let password = $state('');
  let syncOnCommit = $state(true);
  let intervalHours = $state(8);
  let force = $state(false);
  const requests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    mirrors = [];
    resetForm();
    loading = true;
    creating = false;
    busyId = null;
    error = '';
    success = '';
    void load(expectedOwner, expectedRepo, routeGeneration);
  });

  function resetForm() {
    url = '';
    username = '';
    password = '';
    syncOnCommit = true;
    intervalHours = 8;
    force = false;
  }

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function isBusy() {
    return creating || busyId !== null;
  }

  async function load(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = requests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      const next = await pushMirrors.list(expectedOwner, expectedRepo);
      if (requests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        mirrors = next;
      }
    } catch (err: any) {
      if (requests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.push_mirrors.load_failed');
      }
    } finally {
      if (requests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        loading = false;
      }
    }
  }

  async function addMirror(event: SubmitEvent) {
    event.preventDefault();
    if (isBusy()) return;
    if (!url.trim()) {
      error = t('settings.push_mirrors.url_required');
      return;
    }
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const claim = requests.begin(expectedOwner, expectedRepo);
    try {
      creating = true;
      error = '';
      success = '';
      await pushMirrors.create(expectedOwner, expectedRepo, {
        url: url.trim(),
        username: username.trim() || undefined,
        password: password || undefined,
        sync_on_commit: syncOnCommit,
        sync_interval_seconds: Math.max(0, Math.round(intervalHours)) * 3600,
        force,
      });
      if (!requests.owns(claim, owner, repo) || !isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      resetForm();
      success = t('settings.push_mirrors.created');
      await load(expectedOwner, expectedRepo, expectedRoute);
    } catch (err: any) {
      if (requests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.push_mirrors.save_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) creating = false;
    }
  }

  async function rowAction(
    mirror: PushMirror,
    action: () => Promise<unknown>,
    done: string,
    failed: string,
  ) {
    if (isBusy()) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const claim = requests.begin(expectedOwner, expectedRepo);
    try {
      busyId = mirror.id;
      error = '';
      success = '';
      await action();
      if (!requests.owns(claim, owner, repo) || !isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      success = done;
      await load(expectedOwner, expectedRepo, expectedRoute);
    } catch (err: any) {
      if (requests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || failed;
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) busyId = null;
    }
  }

  function syncNow(mirror: PushMirror) {
    const [o, r] = [owner, repo];
    return rowAction(
      mirror,
      () => pushMirrors.sync(o, r, mirror.id),
      t('settings.push_mirrors.sync_started'),
      t('settings.push_mirrors.sync_failed'),
    );
  }

  function toggle(mirror: PushMirror) {
    const [o, r] = [owner, repo];
    return rowAction(
      mirror,
      () => pushMirrors.update(o, r, mirror.id, { enabled: !mirror.enabled }),
      t('settings.push_mirrors.updated'),
      t('settings.push_mirrors.save_failed'),
    );
  }

  async function remove(mirror: PushMirror) {
    if (isBusy() || !(await confirmer.ask({
      title: t('settings.push_mirrors.delete'),
      message: t('settings.push_mirrors.delete_confirm', { url: mirror.url }),
      confirmLabel: t('settings.push_mirrors.delete'),
    }))) return;
    const [o, r] = [owner, repo];
    await rowAction(
      mirror,
      () => pushMirrors.remove(o, r, mirror.id),
      t('settings.push_mirrors.deleted'),
      t('settings.push_mirrors.delete_failed'),
    );
  }

  function formatDate(value: string | null) {
    return value ? formatDateTime(value) : t('common.never');
  }
</script>

<svelte:head>
  <title>{t('settings.push_mirrors.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="push-mirrors-page">
  <div class="page-header">
    <h1>{t('settings.push_mirrors.title')}</h1>
    <p>{t('settings.push_mirrors.desc')}</p>
  </div>

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  {#if loading}
    <div class="loading">{t('common.loading')}</div>
  {:else}
    <section class="section">
      {#if mirrors.length === 0}
        <div class="empty-state">{t('settings.push_mirrors.empty')}</div>
      {:else}
        <ul class="mirror-list">
          {#each mirrors as mirror (mirror.id)}
            <li class="mirror" class:disabled={!mirror.enabled}>
              <div class="mirror-head">
                <code class="mirror-url">{mirror.url}</code>
                <div class="row-actions">
                  <button class="btn btn-outline" type="button" onclick={() => syncNow(mirror)} disabled={isBusy() || !mirror.enabled}>
                    {busyId === mirror.id ? t('common.loading') : t('settings.push_mirrors.sync_now')}
                  </button>
                  <button class="btn btn-outline" type="button" onclick={() => toggle(mirror)} disabled={isBusy()}>
                    {mirror.enabled ? t('settings.push_mirrors.disable') : t('settings.push_mirrors.enable')}
                  </button>
                  <button class="btn btn-danger" type="button" onclick={() => remove(mirror)} disabled={isBusy()}>
                    {t('settings.push_mirrors.delete')}
                  </button>
                </div>
              </div>
              <dl class="status-grid">
                <div>
                  <dt>{t('settings.push_mirrors.last_success')}</dt>
                  <dd>{formatDate(mirror.last_success_at)}</dd>
                </div>
                <div>
                  <dt>{t('settings.push_mirrors.behind_since')}</dt>
                  <dd class:lagging={Boolean(mirror.pending_since)}>
                    {mirror.pending_since ? formatDateTime(mirror.pending_since) : t('settings.push_mirrors.up_to_date')}
                  </dd>
                </div>
                <div>
                  <dt>{t('settings.push_mirrors.mode')}</dt>
                  <dd>
                    {mirror.sync_on_commit ? t('settings.push_mirrors.on_commit') : t('settings.push_mirrors.interval_only')}
                    {#if mirror.force}· <strong>{t('settings.push_mirrors.force_short')}</strong>{/if}
                  </dd>
                </div>
              </dl>
              {#if mirror.last_sync_error}
                <div class="error-detail">{mirror.last_sync_error}</div>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
    </section>

    <section class="section">
      <h2>{t('settings.push_mirrors.add_title')}</h2>
      <form class="mirror-form" onsubmit={addMirror}>
        <div class="form-group">
          <label for="push-mirror-url">{t('settings.push_mirrors.url')}</label>
          <input id="push-mirror-url" type="url" bind:value={url} placeholder="https://github.com/org/repo.git" disabled={creating} />
        </div>
        <div class="form-row">
          <div class="form-group">
            <label for="push-mirror-username">{t('settings.push_mirrors.username')}</label>
            <input id="push-mirror-username" type="text" bind:value={username} autocomplete="off" disabled={creating} />
          </div>
          <div class="form-group">
            <label for="push-mirror-password">{t('settings.push_mirrors.password')}</label>
            <input id="push-mirror-password" type="password" bind:value={password} autocomplete="new-password" disabled={creating} />
          </div>
        </div>
        <div class="form-group checkbox-group">
          <label for="push-mirror-on-commit">
            <input id="push-mirror-on-commit" type="checkbox" bind:checked={syncOnCommit} disabled={creating} />
            {t('settings.push_mirrors.sync_on_commit')}
          </label>
        </div>
        <div class="form-group small">
          <label for="push-mirror-interval">{t('settings.push_mirrors.interval_hours')}</label>
          <input id="push-mirror-interval" type="number" min="0" step="1" bind:value={intervalHours} disabled={creating} />
          <p class="hint">{t('settings.push_mirrors.interval_hint')}</p>
        </div>
        <div class="form-group checkbox-group">
          <label for="push-mirror-force">
            <input id="push-mirror-force" type="checkbox" bind:checked={force} disabled={creating} />
            {t('settings.push_mirrors.force')}
          </label>
          <p class="hint">{t('settings.push_mirrors.force_hint')}</p>
        </div>
        <div class="actions">
          <button class="btn btn-primary" type="submit" disabled={isBusy() || !url.trim()}>
            {creating ? t('common.loading') : t('settings.push_mirrors.add')}
          </button>
        </div>
      </form>
    </section>
  {/if}
</div>

<ConfirmModal {confirmer} />

<style>
  .push-mirrors-page {
    max-width: 900px;
  }

  .page-header {
    margin-bottom: 2rem;
  }

  h1 {
    font-size: 1.75rem;
    margin: 0 0 0.5rem;
    color: var(--text-primary);
  }

  h2 {
    font-size: 1.1rem;
    margin: 0 0 1rem;
    color: var(--text-primary);
  }

  p {
    margin: 0;
    color: var(--text-secondary);
    font-size: 0.95rem;
  }

  .section {
    margin-bottom: 2.5rem;
    padding-bottom: 2rem;
    border-bottom: 1px solid var(--border);
  }

  .mirror-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  .mirror {
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-secondary);
  }

  .mirror.disabled {
    opacity: 0.7;
  }

  .mirror-head {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    margin-bottom: 0.75rem;
  }

  .mirror-url {
    overflow-wrap: anywhere;
  }

  .row-actions,
  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
  }

  .mirror-form {
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  .form-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 1rem;
  }

  .form-group {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .form-group.small {
    max-width: 260px;
  }

  .checkbox-group label {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    font-weight: 500;
  }

  .checkbox-group input {
    width: auto;
    padding: 0;
  }

  .hint {
    font-size: 0.85rem;
    color: var(--text-secondary);
  }

  label {
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--text-primary);
  }

  input {
    padding: 0.65rem 0.75rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-primary);
    color: var(--text-primary);
    font-size: 0.95rem;
  }

  input:focus {
    outline: none;
    border-color: var(--accent);
  }

  .btn {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.5rem 0.85rem;
    cursor: pointer;
    font-weight: 600;
    background: var(--bg-secondary);
    color: var(--text-primary);
  }

  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn-primary {
    background: var(--accent);
    border-color: var(--accent);
    color: white;
  }

  .btn-danger {
    background: var(--red-dim);
    border-color: var(--red-dim);
    color: white;
  }

  .status-grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 0.75rem;
    margin: 0;
  }

  dt {
    margin-bottom: 0.35rem;
    color: var(--text-secondary);
    font-size: 0.8rem;
    font-weight: 600;
    text-transform: uppercase;
  }

  dd {
    margin: 0;
    color: var(--text-primary);
  }

  .lagging {
    color: var(--red);
    font-weight: 600;
  }

  .success-box,
  .error-box,
  .empty-state,
  .loading,
  .error-detail {
    padding: 1rem;
    border-radius: 6px;
    margin-bottom: 1rem;
  }

  .error-detail {
    margin: 0.75rem 0 0;
  }

  .success-box {
    background: rgba(63, 185, 80, 0.12);
    color: var(--green);
  }

  .error-box,
  .error-detail {
    background: rgba(248, 81, 73, 0.12);
    color: var(--red);
  }

  .empty-state,
  .loading {
    background: var(--bg-secondary);
    color: var(--text-secondary);
    border: 1px solid var(--border);
  }

  @media (max-width: 720px) {
    .form-row,
    .status-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
