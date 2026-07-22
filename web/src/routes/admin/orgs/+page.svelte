<script lang="ts">
  import { isAuthReady, isLoggedIn, isAdmin } from '$lib/stores/auth.svelte';
  import { goto } from '$app/navigation';
  import { createT, formatDate } from '$lib/i18n';
  import { admin, type AdminOrg } from '$lib/api/client.svelte';

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

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) { goto('/login'); return; }
    if (!isAdmin()) { goto('/dashboard'); return; }
    loadOrgs();
  });

  async function loadOrgs() {
    loading = true;
    error = '';
    try {
      const result = await admin.listOrgs(page, perPage);
      orgs = result.data;
      total = result.pagination?.total ?? 0;
      totalPages = result.pagination?.total_pages ?? 1;
    } catch (e: any) {
      error = e.message || t('errors.load_failed');
    } finally {
      loading = false;
    }
  }

  function confirmDelete(org: AdminOrg) {
    deleteTarget = org;
    showDeleteConfirm = true;
  }

  async function handleDelete() {
    if (!deleteTarget) return;
    deleting = true;
    error = '';
    try {
      await admin.deleteOrg(deleteTarget.name);
      deleteTarget = null;
      showDeleteConfirm = false;
      await loadOrgs();
    } catch (e: any) {
      error = e.message;
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

  function closeDeleteDialogByKey(e: KeyboardEvent) {
    if (e.key === 'Escape' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      showDeleteConfirm = false;
    }
  }
</script>

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
    <p class="empty">{t('orgs.no_repos')}</p>
  {:else}
    <div class="table-wrap">
      <table class="orgs-table">
        <thead>
          <tr>
            <th>Name</th>
            <th>Display Name</th>
            <th>Visibility</th>
            <th>Owner ID</th>
            <th>Created</th>
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
              <td class="owner">#{org.owner_id}</td>
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
        <button onclick={prevPage} disabled={page <= 1}>← Prev</button>
        <span>Page {page} of {totalPages}</span>
        <button onclick={nextPage} disabled={page >= totalPages}>Next →</button>
      </div>
    {/if}
  {/if}
</div>

<!-- Delete confirm -->
  {#if showDeleteConfirm && deleteTarget}
    <div
      class="modal-overlay"
      onclick={() => showDeleteConfirm = false}
      role="button"
      tabindex="0"
      onkeydown={closeDeleteDialogByKey}
    >
      <div class="modal" role="dialog" aria-modal="true" tabindex="-1">
      <h2>{t('admin.orgs.delete_confirm')}</h2>
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
        <button class="btn-secondary" onclick={() => showDeleteConfirm = false}>{t('common.cancel')}</button>
      </div>
    </div>
  </div>
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
  .modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.7); display: flex; align-items: center; justify-content: center; z-index: 1000; }
  .modal { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 12px; padding: 1.5rem; width: 420px; max-width: 90vw; }
  .modal h2 { margin: 0 0 1rem; font-size: 1.1rem; }
  .modal p { color: var(--text-secondary); margin: 0 0 1rem; }
  .modal-actions { display: flex; gap: 0.75rem; justify-content: flex-end; margin-top: 1.25rem; }
  .btn-secondary { background: var(--bg-primary); border: 1px solid var(--border); color: var(--text-primary); border-radius: 6px; padding: 0.5rem 1rem; cursor: pointer; font-size: 0.9rem; }
</style>
