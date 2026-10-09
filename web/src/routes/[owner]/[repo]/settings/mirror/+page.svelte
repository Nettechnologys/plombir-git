<script lang="ts">
  import { page } from '$app/stores';
  import { mirrors, buildMirrorPayload, type RepositoryMirror } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDateTime } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let mirror = $state<RepositoryMirror | null>(null);
  let loading = $state(true);
  let saving = $state(false);
  let syncing = $state(false);
  let deleting = $state(false);
  let error = $state('');
  let success = $state('');
  let url = $state('');
  let username = $state('');
  let password = $state('');
  let clearPassword = $state(false);
  let intervalHours = $state(24);
  const mirrorRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    fillForm(null);
    loading = true;
    saving = false;
    syncing = false;
    deleting = false;
    error = '';
    success = '';
    void loadMirror(expectedOwner, expectedRepo, routeGeneration);
  });

  function fillForm(next: RepositoryMirror | null) {
    mirror = next;
    url = next?.url ?? '';
    username = next?.username ?? '';
    password = '';
    clearPassword = false;
    intervalHours = Math.max(1, Math.round((next?.sync_interval_seconds ?? 86400) / 3600));
  }

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function isBusy() {
    return saving || syncing || deleting;
  }

  async function loadMirror(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = mirrorRequests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      error = '';
      const next = await mirrors.get(expectedOwner, expectedRepo);
      if (mirrorRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        fillForm(next);
      }
    } catch (err: any) {
      if (mirrorRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        if (String(err?.message || '').toLowerCase().includes('no mirror configured')) {
          fillForm(null);
        } else {
          error = err.message || t('settings.mirror.load_failed');
        }
      }
    } finally {
      if (mirrorRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        loading = false;
      }
    }
  }

  function payload() {
    // `has_credentials` is the server's word for "there is a stored password
    // here" — the value itself never comes back, so it is also the only thing
    // that tells an untouched password field apart from an empty one.
    return buildMirrorPayload(
      { url, username, password, clearPassword, intervalHours },
      Boolean(mirror?.has_credentials)
    );
  }

  async function saveMirror(event: SubmitEvent) {
    event.preventDefault();
    if (isBusy()) return;
    if (!url.trim()) {
      error = t('settings.mirror.url_required');
      return;
    }
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const claim = mirrorRequests.begin(expectedOwner, expectedRepo);
    const nextPayload = payload();

    try {
      saving = true;
      error = '';
      success = '';
      const wasConfigured = Boolean(mirror);
      const next = mirror
        ? await mirrors.update(expectedOwner, expectedRepo, nextPayload)
        : await mirrors.create(expectedOwner, expectedRepo, nextPayload);
      if (mirrorRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        fillForm(next);
        success = wasConfigured ? t('settings.mirror.updated') : t('settings.mirror.created');
      }
    } catch (err: any) {
      if (mirrorRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.mirror.save_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) saving = false;
    }
  }

  async function syncMirror() {
    if (isBusy()) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const claim = mirrorRequests.begin(expectedOwner, expectedRepo);
    try {
      syncing = true;
      error = '';
      success = '';
      await mirrors.sync(expectedOwner, expectedRepo);
      if (!mirrorRequests.owns(claim, owner, repo) || !isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      success = t('settings.mirror.sync_started');
      await loadMirror(expectedOwner, expectedRepo, expectedRoute);
    } catch (err: any) {
      if (mirrorRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.mirror.sync_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) syncing = false;
    }
  }

  async function removeMirror() {
    if (isBusy() || !mirror || !(await confirmer.ask({
      title: t('settings.mirror.delete'),
      message: t('settings.mirror.delete_confirm'),
      confirmLabel: t('settings.mirror.delete'),
    }))) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const claim = mirrorRequests.begin(expectedOwner, expectedRepo);

    try {
      deleting = true;
      error = '';
      success = '';
      await mirrors.remove(expectedOwner, expectedRepo);
      if (mirrorRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        fillForm(null);
        success = t('settings.mirror.deleted');
      }
    } catch (err: any) {
      if (mirrorRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.mirror.delete_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) deleting = false;
    }
  }

  function formatDate(value: string | null) {
    return value ? formatDateTime(value) : t('common.never');
  }
</script>

<svelte:head>
  <title>{t('settings.mirror.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="mirror-page">
  <div class="page-header">
    <div>
      <h1>{t('settings.mirror.title')}</h1>
      <p>{t('settings.mirror.desc')}</p>
    </div>
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
      <h2>{mirror ? t('settings.mirror.edit_title') : t('settings.mirror.create_title')}</h2>
      <form class="mirror-form" onsubmit={saveMirror}>
        <div class="form-group">
          <label for="mirror-url">{t('settings.mirror.url')}</label>
          <input id="mirror-url" type="url" bind:value={url} placeholder="https://github.com/org/repo.git" disabled={saving} />
        </div>

        <div class="form-row">
          <div class="form-group">
            <label for="mirror-username">{t('settings.mirror.username')}</label>
            <input id="mirror-username" type="text" bind:value={username} autocomplete="username" disabled={saving} />
          </div>
          <div class="form-group">
            <label for="mirror-password">{t('settings.mirror.password')}</label>
            <input id="mirror-password" type="password" bind:value={password} autocomplete="new-password" disabled={saving || clearPassword} placeholder={mirror?.has_credentials ? t('settings.mirror.password_placeholder') : ''} />
          </div>
        </div>

        <!-- A blank password box means "keep the stored credential", because the
             stored one is never shown and so cannot be edited away. Revoking it
             therefore needs its own control — without this checkbox, deleting
             the whole mirror was the only way to take an access token back. -->
        {#if mirror?.has_credentials}
          <div class="form-group checkbox-group">
            <label for="mirror-clear-password">
              <input id="mirror-clear-password" type="checkbox" bind:checked={clearPassword} disabled={saving} />
              {t('settings.mirror.clear_password')}
            </label>
            <p class="hint">{t('settings.mirror.clear_password_hint')}</p>
          </div>
        {/if}

        <div class="form-group small">
          <label for="mirror-interval">{t('settings.mirror.interval_hours')}</label>
          <input id="mirror-interval" type="number" min="1" step="1" bind:value={intervalHours} disabled={saving} />
        </div>

        <div class="actions">
          <button class="btn btn-primary" type="submit" disabled={saving || !url.trim()}>
            {saving ? t('common.loading') : t('common.save')}
          </button>
          {#if mirror}
            <button class="btn btn-outline" type="button" onclick={syncMirror} disabled={syncing || saving || deleting}>
              {syncing ? t('common.loading') : t('settings.mirror.sync_now')}
            </button>
            <button class="btn btn-danger" type="button" onclick={removeMirror} disabled={deleting || saving || syncing}>
              {deleting ? t('common.loading') : t('settings.mirror.delete')}
            </button>
          {/if}
        </div>
      </form>
    </section>

    {#if mirror}
      <section class="section">
        <h2>{t('settings.mirror.status')}</h2>
        <dl class="status-grid">
          <div>
            <dt>{t('settings.mirror.state')}</dt>
            <dd><span class:error-state={mirror.status === 'error'}>{mirror.status}</span></dd>
          </div>
          <div>
            <dt>{t('settings.mirror.last_sync')}</dt>
            <dd>{formatDate(mirror.last_sync_at)}</dd>
          </div>
          <div>
            <dt>{t('settings.mirror.next_sync')}</dt>
            <dd>{formatDate(mirror.next_sync_at)}</dd>
          </div>
        </dl>

        {#if mirror.last_sync_error}
          <div class="error-detail">{mirror.last_sync_error}</div>
        {/if}
      </section>
    {:else}
      <div class="empty-state">{t('settings.mirror.empty')}</div>
    {/if}
  {/if}
</div>

<ConfirmModal {confirmer} />

<style>
  .mirror-page {
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
    max-width: 180px;
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

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
  }

  .btn {
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.65rem 1rem;
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
    gap: 1rem;
    margin: 0;
  }

  .status-grid div {
    padding: 1rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-secondary);
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

  .error-state {
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
