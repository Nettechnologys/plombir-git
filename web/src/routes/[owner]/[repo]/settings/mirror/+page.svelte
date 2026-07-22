<script lang="ts">
  import { page } from '$app/stores';
  import { mirrors, type RepositoryMirror } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();
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
  let intervalHours = $state(24);

  $effect(() => {
    loadMirror();
  });

  function fillForm(next: RepositoryMirror | null) {
    mirror = next;
    url = next?.url ?? '';
    username = next?.username ?? '';
    password = '';
    intervalHours = Math.max(1, Math.round((next?.sync_interval_seconds ?? 86400) / 3600));
  }

  async function loadMirror() {
    try {
      loading = true;
      error = '';
      const next = await mirrors.get(owner, repo);
      fillForm(next);
    } catch (err: any) {
      if (String(err?.message || '').toLowerCase().includes('no mirror configured')) {
        fillForm(null);
      } else {
        error = err.message || t('settings.mirror.load_failed');
      }
    } finally {
      loading = false;
    }
  }

  function payload() {
    const trimmedUrl = url.trim();
    const trimmedUsername = username.trim();
    const trimmedPassword = password.trim();

    return {
      url: trimmedUrl,
      username: trimmedUsername || undefined,
      password: trimmedPassword || undefined,
      sync_interval_seconds: Math.max(1, Math.round(intervalHours)) * 3600,
    };
  }

  async function saveMirror(event: SubmitEvent) {
    event.preventDefault();
    if (!url.trim()) {
      error = t('settings.mirror.url_required');
      return;
    }

    try {
      saving = true;
      error = '';
      success = '';
      const wasConfigured = Boolean(mirror);
      const next = mirror
        ? await mirrors.update(owner, repo, payload())
        : await mirrors.create(owner, repo, payload());
      fillForm(next);
      success = wasConfigured ? t('settings.mirror.updated') : t('settings.mirror.created');
    } catch (err: any) {
      error = err.message || t('settings.mirror.save_failed');
    } finally {
      saving = false;
    }
  }

  async function syncMirror() {
    try {
      syncing = true;
      error = '';
      success = '';
      await mirrors.sync(owner, repo);
      success = t('settings.mirror.sync_started');
      await loadMirror();
    } catch (err: any) {
      error = err.message || t('settings.mirror.sync_failed');
    } finally {
      syncing = false;
    }
  }

  async function removeMirror() {
    if (!mirror || !confirm(t('settings.mirror.delete_confirm'))) return;

    try {
      deleting = true;
      error = '';
      success = '';
      await mirrors.remove(owner, repo);
      fillForm(null);
      success = t('settings.mirror.deleted');
    } catch (err: any) {
      error = err.message || t('settings.mirror.delete_failed');
    } finally {
      deleting = false;
    }
  }

  function formatDate(value: string | null) {
    return value ? new Date(value).toLocaleString() : t('common.never');
  }
</script>

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
            <input id="mirror-password" type="password" bind:value={password} autocomplete="new-password" disabled={saving} placeholder={mirror ? t('settings.mirror.password_placeholder') : ''} />
          </div>
        </div>

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
