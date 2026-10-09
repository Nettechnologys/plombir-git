<script lang="ts">
  import { isAuthReady, isLoggedIn, isAdmin } from '$lib/stores/auth.svelte';
  import { goto } from '$app/navigation';
  import { createT, formatDate } from '$lib/i18n';
  import { admin, type AdminOrg } from '$lib/api/client.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { untrack } from 'svelte';
  import Modal from '$lib/components/Modal.svelte';

  const t = createT();

  let orgs = $state<AdminOrg[]>([]);
  let page = $state(1);
  let perPage = $state(20);
  let totalPages = $state(1);
  let total = $state(0);
  let loading = $state(true);
  let error = $state('');
  let deleteTarget = $state<AdminOrg | null>(null);
  let showDeleteConfirm = $state(false);
  let deleting = $state(false);
  const listRequests = new LatestRequestFence<string>();

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) { goto('/login'); return; }
    if (!isAdmin()) { goto('/dashboard'); return; }
    // Pagination owns its own reload; keep page/perPage out of this auth effect.
    untrack(() => void loadOrgs());
  });

  function pageRequestKey(expectedPage: number, expectedPerPage: number): string {
    return `${expectedPage}:${expectedPerPage}`;
  }

  async function loadOrgs() {
    const expectedPage = page;
    const expectedPerPage = perPage;
    const identity = pageRequestKey(expectedPage, expectedPerPage);
    const claim = listRequests.begin(identity);
    loading = true;
    error = '';
    try {
      const result = await admin.listOrgs(expectedPage, expectedPerPage);
      if (listRequests.owns(claim, pageRequestKey(page, perPage))) {
        orgs = result.data;
        total = result.pagination?.total ?? 0;
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

  function confirmDelete(org: AdminOrg) {
    deleteTarget = org;
    showDeleteConfirm = true;
  }

  async function handleDelete() {
    if (!deleteTarget) return;
    if (deleting) return;
    const targetName = deleteTarget.name;
    deleting = true;
    error = '';
    try {
      await admin.deleteOrg(targetName);
      if (deleteTarget?.name === targetName) {
        deleteTarget = null;
        showDeleteConfirm = false;
      }
      await loadOrgs();
    } catch (e: any) {
      if (deleteTarget?.name === targetName) error = e.message;
    } finally {
      deleting = false;
    }
  }

  function prevPage() {
    if (page > 1) { page--; loadOrgs(); }
  }

  function nextPage() {
    if (page < totalPages) { page++; loadOrgs(); }
  }

</script>

<svelte:head>
  <title>{t('admin.orgs.title')} · {t('admin.settings.admin')} · Plombir Git</title>
</svelte:head>

<div class="container">
  <div class="header">
    <a href="/admin" class="back">← {t('admin.back')}</a>
    <h1>{t('admin.orgs.title')}</h1>
    <p class="meta">{total} {t('admin.orgs.total')}</p>
  </div>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  {#if loading}
    <p class="loading">{t('common.loading')}</p>
  {:else if orgs.length === 0}
    <p class="empty">{t('orgs.no_orgs')}</p>
  {:else}
    <div class="table-wrap">
      <table class="orgs-table">
        <thead>
          <tr>
            <th>{t('admin.orgs.columns.name')}</th>
            <th>{t('admin.orgs.columns.display_name')}</th>
            <th>{t('admin.orgs.columns.visibility')}</th>
            <th>{t('admin.orgs.columns.owner')}</th>
            <th>{t('admin.orgs.columns.created')}</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each orgs as org}
            <tr>
              <td class="name">
                <a href={`/orgs/${org.name}`}>{org.name}</a>
              </td>
              <td class="display-name">{org.display_name || '—'}</td>
              <td>
                <span class="badge" class:private={org.visibility === 'private'}>
                  {org.visibility}
                </span>
              </td>
              <td class="owner">
                {#if org.owner_username}
                  <span class="owner-name">{org.owner_username}</span>
                {:else}
                  <span class="user-id">#{org.owner_id}</span>
                {/if}
              </td>
              <td class="date">{formatDate(org.created_at)}</td>
              <td class="actions">
                <button class="btn-danger" onclick={() => confirmDelete(org)}>{t('common.delete')}</button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    {#if totalPages > 1}
      <div class="pagination">
        <button onclick={prevPage} disabled={page <= 1}>{t('common.prev_arrow')}</button>
        <span>{t('common.page_info', { page, total: totalPages })}</span>
        <button onclick={nextPage} disabled={page >= totalPages}>{t('common.next_arrow')}</button>
      </div>
    {/if}
  {/if}
</div>

<!-- Delete confirm -->
{#if showDeleteConfirm && deleteTarget}
  <Modal onclose={() => showDeleteConfirm = false} labelledby="admin-org-delete-title" width="420px">
    <div class="modal">
      <h2 id="admin-org-delete-title">{t('admin.orgs.delete_confirm')}</h2>
      <p>
        {t('admin.orgs.delete_warning', { name: deleteTarget.name })}
      </p>
      {#if error}
        <div class="error">{error}</div>
      {/if}
      <div class="modal-actions">
        <button class="btn-danger" onclick={handleDelete} disabled={deleting}>
          {deleting ? t('common.loading') : t('common.delete')}
        </button>
        <button class="btn-secondary" onclick={() => showDeleteConfirm = false} data-autofocus>{t('common.cancel')}</button>
      </div>
    </div>
  </Modal>
{/if}

<style>
  .header { margin-bottom: 1.5rem; }
  .back { color: var(--text-secondary); text-decoration: none; font-size: 0.9rem; }
  .back:hover { color: var(--accent); text-decoration: none; }
  h1 { margin: 0.5rem 0 0; }
  .meta { color: var(--text-secondary); margin: 0; }
  .error { color: #f85149; background: rgba(248, 81, 73, 0.1); padding: 0.5rem 0.75rem; border-radius: 6px; margin-bottom: 1rem; }
  .loading { color: var(--text-secondary); }
  .empty { color: var(--text-secondary); font-style: italic; }

  .table-wrap { overflow-x: auto; }
  .orgs-table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
  .orgs-table th { text-align: left; padding: 0.6rem 0.75rem; border-bottom: 2px solid var(--border); color: var(--text-secondary); font-weight: 600; }
  .orgs-table td { padding: 0.6rem 0.75rem; border-bottom: 1px solid var(--border); color: var(--text-primary); }
  .orgs-table tr:hover td { background: var(--bg-hover); }
  .name a { color: var(--accent); font-weight: 500; text-decoration: none; }
  .name a:hover { text-decoration: underline; }
  .display-name { color: var(--text-secondary); }
  .owner { color: var(--text-secondary); }
  .owner-name { color: var(--text-primary); }
  .user-id { font-family: monospace; }
  .date { color: var(--text-secondary); white-space: nowrap; }
  .actions { text-align: right; }

  .badge { display: inline-block; padding: 0.1rem 0.4rem; border-radius: 8px; font-size: 0.8rem; background: rgba(63, 185, 80, 0.15); color: #3fb950; border: 1px solid #3fb950; }
  .badge.private { background: rgba(248, 81, 73, 0.15); color: #f85149; border-color: #f85149; }

  .pagination { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; }
  .pagination button { background: var(--bg-secondary); border: 1px solid var(--border); color: var(--text-primary); border-radius: 6px; padding: 0.4rem 0.8rem; cursor: pointer; }
  .pagination button:disabled { opacity: 0.5; cursor: not-allowed; }
  .pagination span { color: var(--text-secondary); font-size: 0.9rem; }

  .btn-danger { background: rgba(248, 81, 73, 0.15); border: 1px solid #f85149; color: #f85149; border-radius: 4px; padding: 0.25rem 0.6rem; font-size: 0.8rem; cursor: pointer; }
  .btn-danger:hover { background: rgba(248, 81, 73, 0.25); }
  .btn-danger:disabled { opacity: 0.6; cursor: not-allowed; }

  /* Modal */
  /* The panel itself is lib/components/Modal.svelte; `.modal` scopes its content. */
  .modal h2 { margin: 0 0 1rem; font-size: 1.1rem; }
  .modal p { color: var(--text-secondary); margin: 0 0 1rem; }
  .modal-actions { display: flex; gap: 0.75rem; justify-content: flex-end; margin-top: 1.25rem; }
  .btn-secondary { background: var(--bg-primary); border: 1px solid var(--border); color: var(--text-primary); border-radius: 6px; padding: 0.5rem 1rem; cursor: pointer; font-size: 0.9rem; }
</style>
