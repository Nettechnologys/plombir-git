<script lang="ts">
  import { goto } from '$app/navigation';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDateTime } from '$lib/i18n';
  import { runners } from '$lib/api/client.svelte';
  import { isAdmin, isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';
  import { untrack } from 'svelte';
  import Modal from '$lib/components/Modal.svelte';

  const t = createT();

  let runnerList = $state<any[]>([]);
  let page = $state(1);
  let perPage = $state(20);
  let total = $state(0);
  let totalPages = $state(1);
  let loading = $state(true);
  let error = $state('');
  let deleteTarget = $state<any | null>(null);
  let deleting = $state(false);

  let newRunnerName = $state('');
  let newRunnerRepository = $state('');
  let newRunnerLabels = $state<string[]>([]);
  let saving = $state(false);
  let registeredRunner = $state<{ id: number; token: string; name: string } | null>(null);
  const listRequests = new LatestRequestFence<string>();

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    if (!isAdmin()) {
      goto('/dashboard');
      return;
    }
    // Pagination owns its own reload; keep page/perPage out of this auth effect.
    untrack(() => void loadRunners());
  });

  function pageRequestKey(expectedPage: number, expectedPerPage: number): string {
    return `${expectedPage}:${expectedPerPage}`;
  }

  async function loadRunners() {
    const expectedPage = page;
    const expectedPerPage = perPage;
    const identity = pageRequestKey(expectedPage, expectedPerPage);
    const claim = listRequests.begin(identity);
    loading = true;
    error = '';
    try {
      const result = await runners.list(expectedPage, expectedPerPage);
      if (listRequests.owns(claim, pageRequestKey(page, perPage))) {
        runnerList = result.data;
        total = result.pagination?.total ?? runnerList.length;
        totalPages = result.pagination?.total_pages ?? 1;
      }
    } catch (e: any) {
      if (listRequests.owns(claim, pageRequestKey(page, perPage))) {
        error = e.message || t('errors.load_failed');
      }
    } finally {
      if (listRequests.owns(claim, pageRequestKey(page, perPage))) loading = false;
    }
  }

  async function handleRegister() {
    if (saving) return;
    const runnerName = newRunnerName.trim();
    const runnerRepository = newRunnerRepository.trim();
    if (!runnerName || !runnerRepository) return;
    const runnerLabels = [...newRunnerLabels];
    saving = true;
    error = '';
    try {
      // Empty rows are editor placeholders. Every other value is already one
      // structural label and must stay byte-for-byte intact: commas and
      // whitespace are legal label content and participate in CI tag matching.
      const nonEmptyLabels = runnerLabels.filter((label) => label !== '');
      const labels = nonEmptyLabels.length > 0 ? nonEmptyLabels : undefined;
      const response = await runners.register({
        repository: runnerRepository,
        name: runnerName,
        labels,
      });
      registeredRunner = { id: response.id, token: response.token, name: runnerName };
      if (
        newRunnerName.trim() === runnerName &&
        newRunnerRepository.trim() === runnerRepository &&
        newRunnerLabels.length === runnerLabels.length &&
        newRunnerLabels.every((label, index) => label === runnerLabels[index])
      ) {
        newRunnerName = '';
        newRunnerRepository = '';
        newRunnerLabels = [];
      }
      await loadRunners();
    } catch (e: any) {
      error = e.message || t('errors.save_failed');
    } finally {
      saving = false;
    }
  }

  function addRunnerLabel() {
    newRunnerLabels = [...newRunnerLabels, ''];
  }

  function removeRunnerLabel(index: number) {
    newRunnerLabels = newRunnerLabels.filter((_, itemIndex) => itemIndex !== index);
  }

  async function copyRunnerToken() {
    if (!registeredRunner) return;
    await navigator.clipboard.writeText(registeredRunner.token);
  }

  function confirmDelete(runner: any) {
    deleteTarget = runner;
  }

  async function handleDelete() {
    if (!deleteTarget) return;
    if (deleting) return;
    const runnerId = deleteTarget.id;
    deleting = true;
    error = '';
    try {
      await runners.delete(runnerId);
      if (deleteTarget?.id === runnerId) deleteTarget = null;
      await loadRunners();
    } catch (e: any) {
      if (deleteTarget?.id === runnerId) {
        error = e.message || t('errors.delete_failed');
      }
    } finally {
      deleting = false;
    }
  }

  function prevPage() {
    if (page > 1) {
      page -= 1;
      loadRunners();
    }
  }

  function nextPage() {
    if (page < totalPages) {
      page += 1;
      loadRunners();
    }
  }
</script>

<svelte:head>
  <title>{t('admin.runners.title')} · Plombir Git</title>
</svelte:head>

<div class="container">
  <div class="header">
    <a href="/admin" class="back">← {t('admin.back')}</a>
    <h1>{t('admin.runners.title')}</h1>
    <p class="meta">{total} {t('admin.runners.total')}</p>
  </div>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  {#if registeredRunner}
    <div class="token-banner">
      <div>
        <strong>{t('admin.runners.token_title', { name: registeredRunner.name })}</strong>
        <p>{t('admin.runners.token_help')}</p>
        <code>{registeredRunner.token}</code>
      </div>
      <button class="btn-secondary" type="button" onclick={copyRunnerToken}>
        {t('common.copy')}
      </button>
    </div>
  {/if}

  <section class="panel">
    <h2>{t('admin.runners.register')}</h2>
    <div class="form-grid">
      <label>
        <span>{t('admin.runners.name')}</span>
        <input type="text" bind:value={newRunnerName} placeholder="linux-runner-01" />
      </label>
      <label>
        <span>{t('admin.runners.repository')}</span>
        <input type="text" bind:value={newRunnerRepository} placeholder={t('admin.runners.repository_placeholder')} />
      </label>
      <div class="runner-label-field">
        <span id="runner-labels-label" class="field-label">{t('admin.runners.labels')}</span>
        <span class="field-hint">{t('admin.runners.labels_hint')}</span>
        <div class="runner-label-editor" aria-labelledby="runner-labels-label">
          {#if newRunnerLabels.length === 0}
            <p class="runner-labels-empty">{t('admin.runners.labels_empty')}</p>
          {/if}
          {#each newRunnerLabels as _, index}
            <label class="runner-label-row" for={`runner-label-${index}`}>
              <span>{t('admin.runners.label_name', { number: index + 1 })}</span>
              <div class="runner-label-control">
                <input
                  id={`runner-label-${index}`}
                  type="text"
                  bind:value={newRunnerLabels[index]}
                  placeholder="gpu,a100"
                  disabled={saving}
                />
                <button
                  class="btn-secondary remove-runner-label"
                  type="button"
                  onclick={() => removeRunnerLabel(index)}
                  disabled={saving}
                  aria-label={t('admin.runners.remove_label', { number: index + 1 })}
                >
                  {t('common.delete')}
                </button>
              </div>
            </label>
          {/each}
          <button
            class="btn-secondary add-runner-label"
            type="button"
            onclick={addRunnerLabel}
            disabled={saving}
          >
            {t('admin.runners.add_label')}
          </button>
        </div>
      </div>
      <button
        class="btn-primary"
        onclick={handleRegister}
        disabled={saving || !newRunnerName.trim() || !newRunnerRepository.trim()}
      >
        {saving ? t('common.loading') : t('admin.runners.register')}
      </button>
    </div>
  </section>

  {#if loading}
    <p class="loading">{t('common.loading')}</p>
  {:else if runnerList.length === 0}
    <p class="empty">{t('admin.runners.empty')}</p>
  {:else}
    <div class="table-wrap">
      <table class="runners-table">
        <thead>
          <tr>
            <th>{t('admin.runners.name')}</th>
            <th>{t('admin.runners.repository')}</th>
            <th>{t('admin.runners.status')}</th>
            <th>{t('admin.runners.labels')}</th>
            <th>{t('admin.runners.version')}</th>
            <th>{t('admin.runners.last_seen')}</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each runnerList as runner (runner.id)}
            <tr>
              <td class="name">{runner.name}</td>
              <td class="muted">{runner.repository || t('admin.runners.unscoped')}</td>
              <td>
                <span class="badge" class:online={runner.status === 'online'}>{runner.status}</span>
              </td>
              <td>
                <div class="labels">
                  {#each runner.labels as label}
                    <span>{label}</span>
                  {/each}
                </div>
              </td>
              <td class="muted">{runner.version || '-'}</td>
              <td class="muted">{runner.last_seen ? formatDateTime(runner.last_seen) : t('common.never')}</td>
              <td class="actions">
                <button class="btn-danger" onclick={() => confirmDelete(runner)}>{t('common.delete')}</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    {#if totalPages > 1}
      <div class="pagination">
        <button onclick={prevPage} disabled={page <= 1}>{t('common.previous')}</button>
        <span>{t('admin.runners.page_info', { page, total: totalPages })}</span>
        <button onclick={nextPage} disabled={page >= totalPages}>{t('common.next')}</button>
      </div>
    {/if}
  {/if}
</div>

{#if deleteTarget}
  <Modal onclose={() => deleteTarget = null} labelledby="admin-runner-delete-title" width="420px">
    <div class="modal">
      <h2 id="admin-runner-delete-title">{t('admin.runners.delete_confirm')}</h2>
      <p>{t('admin.runners.delete_warning', { name: deleteTarget.name })}</p>
      <div class="modal-actions">
        <button class="btn-danger" onclick={handleDelete} disabled={deleting}>
          {deleting ? t('common.loading') : t('common.delete')}
        </button>
        <button class="btn-secondary" onclick={() => deleteTarget = null} data-autofocus>{t('common.cancel')}</button>
      </div>
    </div>
  </Modal>
{/if}

<style>
  .header { margin-bottom: 1.5rem; }
  .back { color: var(--text-secondary); text-decoration: none; font-size: 0.9rem; }
  .back:hover { color: var(--accent); text-decoration: none; }
  h1 { margin: 0.5rem 0 0; }
  h2 { margin: 0 0 1rem; font-size: 1rem; }
  .meta, .loading, .empty, .muted { color: var(--text-secondary); }
  .empty { font-style: italic; }
  .error { color: #f85149; background: rgba(248, 81, 73, 0.1); padding: 0.5rem 0.75rem; border-radius: 6px; margin-bottom: 1rem; }
  .panel, .token-banner { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 8px; padding: 1rem; margin-bottom: 1rem; }
  .token-banner { display: flex; align-items: flex-start; justify-content: space-between; gap: 1rem; }
  .token-banner p { color: var(--text-secondary); margin: 0.25rem 0 0.75rem; }
  .token-banner code { display: block; max-width: 100%; overflow-x: auto; padding: 0.5rem; background: var(--bg-primary); border: 1px solid var(--border); border-radius: 6px; }
  .form-grid { display: grid; grid-template-columns: minmax(160px, 1fr) minmax(220px, 1.5fr); gap: 0.75rem; align-items: end; }
  label { display: flex; flex-direction: column; gap: 0.35rem; }
  label span { color: var(--text-secondary); font-size: 0.85rem; font-weight: 600; }
  input { padding: 0.5rem 0.65rem; border: 1px solid var(--border); border-radius: 6px; background: var(--bg-primary); color: var(--text-primary); }
  .runner-label-field { display: flex; flex-direction: column; gap: 0.35rem; grid-column: 1 / -1; }
  .field-label { color: var(--text-secondary); font-size: 0.85rem; font-weight: 600; }
  .field-hint, .runner-labels-empty { color: var(--text-secondary); font-size: 0.85rem; }
  .runner-labels-empty { margin: 0; }
  .runner-label-editor { display: flex; flex-direction: column; gap: 0.65rem; padding: 0.75rem; border: 1px solid var(--border); border-radius: 6px; }
  .runner-label-row { gap: 0.25rem; }
  .runner-label-control { display: flex; align-items: center; gap: 0.5rem; }
  .runner-label-control input { flex: 1; min-width: 0; }
  .add-runner-label { align-self: flex-start; }
  .form-grid > .btn-primary { justify-self: start; }
  .table-wrap { overflow-x: auto; }
  .runners-table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
  .runners-table th { text-align: left; padding: 0.6rem 0.75rem; border-bottom: 2px solid var(--border); color: var(--text-secondary); font-weight: 600; }
  .runners-table td { padding: 0.6rem 0.75rem; border-bottom: 1px solid var(--border); color: var(--text-primary); vertical-align: top; }
  .runners-table tr:hover td { background: var(--bg-hover); }
  .name { font-weight: 600; }
  .labels { display: flex; flex-wrap: wrap; gap: 0.35rem; }
  .labels span { padding: 0.1rem 0.4rem; border: 1px solid var(--border); border-radius: 999px; color: var(--text-secondary); font-size: 0.8rem; }
  .actions { text-align: right; }
  .badge { display: inline-block; padding: 0.1rem 0.45rem; border-radius: 999px; font-size: 0.8rem; background: rgba(139, 148, 158, 0.15); color: var(--text-secondary); border: 1px solid var(--border); }
  .badge.online { background: rgba(63, 185, 80, 0.15); color: #3fb950; border-color: #3fb950; }
  .pagination { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; }
  .pagination button, .btn-secondary { background: var(--bg-primary); border: 1px solid var(--border); color: var(--text-primary); border-radius: 6px; padding: 0.45rem 0.8rem; cursor: pointer; }
  .pagination button:disabled, .btn-primary:disabled, .btn-danger:disabled { opacity: 0.6; cursor: not-allowed; }
  .btn-primary { background: var(--accent); border: 1px solid var(--accent); color: #fff; border-radius: 6px; padding: 0.5rem 0.9rem; cursor: pointer; font-weight: 600; }
  .btn-danger { background: rgba(248, 81, 73, 0.15); border: 1px solid #f85149; color: #f85149; border-radius: 4px; padding: 0.25rem 0.6rem; font-size: 0.8rem; cursor: pointer; }
  .btn-danger:hover { background: rgba(248, 81, 73, 0.25); }
  /* The panel itself is lib/components/Modal.svelte; `.modal` scopes its content. */
  .modal p { color: var(--text-secondary); margin: 0 0 1rem; }
  .modal-actions { display: flex; gap: 0.75rem; justify-content: flex-end; margin-top: 1.25rem; }

  @media (max-width: 720px) {
    .form-grid { grid-template-columns: 1fr; }
    .token-banner { flex-direction: column; }
  }
</style>
