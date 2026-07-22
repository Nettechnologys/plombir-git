<script lang="ts">
  import { isAuthReady, isLoggedIn, isAdmin } from '$lib/stores/auth.svelte';
  import { goto } from '$app/navigation';
  import { createT, formatDate, formatDateTime } from '$lib/i18n';
  import { admin, type AuditLogEntry } from '$lib/api/client.svelte';

  const t = createT();

  let logs = $state<AuditLogEntry[]>([]);
  let page = $state(0);
  let perPage = $state(20);
  let total = $state(0);
  let totalPages = $state(1);
  let loading = $state(true);
  let error = $state('');

  // Filter state
  let actionFilter = $state('');
  let resourceFilter = $state('');

  // Detail modal
  let selectedLog = $state<AuditLogEntry | null>(null);
  let detailLoading = $state(false);

  // Predefined action groups for the filter dropdown
  const actionGroups = [
    { value: '', label: () => t('admin.audit.filter_all') },
    { value: 'user.login', label: 'user.login' },
    { value: 'user.register', label: 'user.register' },
    { value: 'repo.create', label: 'repo.create' },
    { value: 'repo.delete', label: 'repo.delete' },
    { value: 'repo.fork', label: 'repo.fork' },
    { value: 'repo.transfer', label: 'repo.transfer' },
    { value: 'org.create', label: 'org.create' },
    { value: 'org.update', label: 'org.update' },
    { value: 'org.delete', label: 'org.delete' },
    { value: 'org.add_member', label: 'org.add_member' },
    { value: 'org.remove_member', label: 'org.remove_member' },
    { value: 'admin.update_user', label: 'admin.update_user' },
    { value: 'admin.delete_user', label: 'admin.delete_user' },
    { value: 'admin.delete_org', label: 'admin.delete_org' },
  ];

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) { goto('/login'); return; }
    if (!isAdmin()) { goto('/dashboard'); return; }
    loadLogs();
  });

  async function loadLogs() {
    loading = true;
    error = '';
    try {
      const result = await admin.listAuditLogs({
        page: page + 1, // L-4: Backend now uses 1-based page numbering
        per_page: perPage,
        action: actionFilter || undefined,
        resource_type: resourceFilter || undefined,
      });
      logs = result.logs;
      total = result.total;
      totalPages = Math.max(1, Math.ceil(result.total / result.per_page));
    } catch (e: any) {
      error = e.message || t('errors.load_failed');
    } finally {
      loading = false;
    }
  }

  function applyFilter() {
    page = 0;
    loadLogs();
  }

  function clearFilters() {
    actionFilter = '';
    resourceFilter = '';
    page = 0;
    loadLogs();
  }

  async function openDetail(log: AuditLogEntry) {
    detailLoading = true;
    error = '';
    selectedLog = log;
    try {
      selectedLog = await admin.getAuditLog(log.id);
    } catch (e: any) {
      error = e.message || t('errors.load_failed');
    } finally {
      detailLoading = false;
    }
  }

  function closeDetail() {
    selectedLog = null;
    detailLoading = false;
  }

  function closeDetailByKey(e: KeyboardEvent) {
    if (e.key === 'Escape' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      closeDetail();
    }
  }

  function prevPage() {
    if (page > 0) { page--; loadLogs(); }
  }

  function nextPage() {
    if (page < totalPages - 1) { page++; loadLogs(); }
  }

  function formatAction(action: string): string {
    return action;
  }

  function formatResourceType(rt: string | null): string {
    if (!rt) return '—';
    const map: Record<string, string> = {
      user: 'User',
      repo: 'Repository',
      org: 'Organization',
    };
    return map[rt] || rt;
  }
</script>

<div class="container">
  <div class="header">
    <a href="/admin" class="back">← {t('admin.back')}</a>
    <h1>{t('admin.audit.title')}</h1>
    <p class="meta">{total} {t('admin.audit.total')}</p>
  </div>

  <!-- Filters -->
  <div class="filters">
    <select bind:value={actionFilter} onchange={applyFilter}>
      {#each actionGroups as g}
        <option value={g.value}>{typeof g.label === 'function' ? g.label() : t(g.label)}</option>
      {/each}
    </select>
    <select bind:value={resourceFilter} onchange={applyFilter}>
      <option value="">{t('admin.audit.fields.resource_type')}: All</option>
      <option value="user">User</option>
      <option value="repo">Repository</option>
      <option value="org">Organization</option>
    </select>
    {#if actionFilter || resourceFilter}
      <button class="btn-sm" onclick={clearFilters}>Clear filters</button>
    {/if}
  </div>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  {#if loading}
    <p class="loading">{t('common.loading')}</p>
  {:else if logs.length === 0}
    <p class="empty">No audit records found.</p>
  {:else}
    <div class="table-wrap">
      <table class="audit-table">
        <thead>
          <tr>
            <th>{t('admin.audit.fields.time')}</th>
            <th>{t('admin.audit.fields.user')}</th>
            <th>{t('admin.audit.fields.action')}</th>
            <th>{t('admin.audit.fields.resource')}</th>
            <th>{t('admin.audit.fields.ip')}</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each logs as log}
            <tr>
              <td class="time-cell" title={formatDateTime(log.created_at)}>
                {formatDate(log.created_at)}
              </td>
              <td class="user-cell">
                {#if log.username}
                  <span class="username-text">{log.username}</span>
                {:else if log.user_id}
                  <span class="user-id">#{log.user_id}</span>
                {:else}
                  <span class="anonymous">—</span>
                {/if}
              </td>
              <td>
                <span class="action-badge" data-action={log.action}>
                  {formatAction(log.action)}
                </span>
              </td>
              <td class="resource-cell">
                {#if log.resource_name}
                  <span class="resource-link">{formatResourceType(log.resource_type)}: {log.resource_name}</span>
                {:else}
                  <span class="text-muted">—</span>
                {/if}
              </td>
              <td class="ip-cell">
                {log.ip_address || '—'}
              </td>
              <td class="actions">
                <button class="btn-sm" onclick={() => openDetail(log)}>
                  {t('admin.audit.fields.details')}
                </button>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    <!-- Pagination -->
    {#if totalPages > 1}
      <div class="pagination">
        <button onclick={prevPage} disabled={page <= 0}>← Prev</button>
        <span>Page {page + 1} of {totalPages}</span>
        <button onclick={nextPage} disabled={page >= totalPages - 1}>Next →</button>
      </div>
    {/if}
  {/if}
</div>

<!-- Detail modal -->
  {#if selectedLog}
    <div
      class="modal-overlay"
      onclick={closeDetail}
      role="button"
      tabindex="0"
      onkeydown={closeDetailByKey}
    >
      <div class="modal" role="dialog" aria-modal="true" tabindex="-1">
      <h2>{t('admin.audit.detail_title', { id: selectedLog.id })}</h2>

      <div class="detail-grid">
        <div class="detail-row">
          <span class="detail-label">{t('admin.audit.fields.time')}</span>
          <span class="detail-value">{formatDateTime(selectedLog.created_at)}</span>
        </div>
        <div class="detail-row">
          <span class="detail-label">{t('admin.audit.fields.user')}</span>
          <span class="detail-value">
            {#if selectedLog.username}
              {selectedLog.username} (#{selectedLog.user_id})
            {:else}
              {selectedLog.user_id ? `#${selectedLog.user_id}` : '—'}
            {/if}
          </span>
        </div>
        <div class="detail-row">
          <span class="detail-label">{t('admin.audit.fields.action')}</span>
          <span class="detail-value"><span class="action-badge" data-action={selectedLog.action}>{selectedLog.action}</span></span>
        </div>
        <div class="detail-row">
          <span class="detail-label">{t('admin.audit.fields.resource_type')}</span>
          <span class="detail-value">{formatResourceType(selectedLog.resource_type)}</span>
        </div>
        {#if selectedLog.resource_name}
          <div class="detail-row">
            <span class="detail-label">{t('admin.audit.fields.resource')}</span>
            <span class="detail-value">{selectedLog.resource_name} (#{selectedLog.resource_id})</span>
          </div>
        {/if}
        <div class="detail-row">
          <span class="detail-label">{t('admin.audit.fields.ip')}</span>
          <span class="detail-value">{selectedLog.ip_address || '—'}</span>
        </div>
        <div class="detail-row detail-row-full">
          <span class="detail-label">{t('admin.audit.fields.details')}</span>
          <span class="detail-value">
            {detailLoading ? t('common.loading') : selectedLog.details || t('admin.audit.no_details')}
          </span>
        </div>
      </div>

      <div class="modal-actions">
        <button class="btn-secondary" onclick={closeDetail}>{t('common.cancel')}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .header { margin-bottom: 1rem; }
  .back { color: var(--text-secondary); text-decoration: none; font-size: 0.9rem; }
  .back:hover { color: var(--accent); text-decoration: none; }
  h1 { margin: 0.5rem 0 0; }
  .meta { color: var(--text-secondary); margin: 0; }

  /* Filters */
  .filters {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    margin-bottom: 1rem;
    flex-wrap: wrap;
  }
  .filters select {
    background: var(--bg-secondary);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.4rem 0.75rem;
    font-size: 0.85rem;
  }
  .filters .btn-sm {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    color: var(--text-secondary);
    border-radius: 4px;
    padding: 0.3rem 0.6rem;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .filters .btn-sm:hover { color: var(--text-primary); background: var(--bg-hover); }

  .error { color: #f85149; background: rgba(248, 81, 73, 0.1); padding: 0.5rem 0.75rem; border-radius: 6px; margin-bottom: 1rem; }
  .loading { color: var(--text-secondary); }
  .empty { color: var(--text-secondary); text-align: center; padding: 2rem 0; }

  /* Table */
  .table-wrap { overflow-x: auto; }
  .audit-table { width: 100%; border-collapse: collapse; font-size: 0.875rem; }
  .audit-table th {
    text-align: left;
    padding: 0.6rem 0.75rem;
    border-bottom: 2px solid var(--border);
    color: var(--text-secondary);
    font-weight: 600;
    white-space: nowrap;
  }
  .audit-table td {
    padding: 0.6rem 0.75rem;
    border-bottom: 1px solid var(--border);
    color: var(--text-primary);
  }
  .audit-table tr:hover td { background: var(--bg-hover); }

  .time-cell {
    white-space: nowrap;
    color: var(--text-secondary);
    font-size: 0.8rem;
    font-family: var(--font-mono, monospace);
  }
  .user-cell .username-text { font-weight: 500; }
  .user-cell .user-id { color: var(--text-secondary); }
  .user-cell .anonymous { color: var(--text-secondary); }
  .resource-cell { max-width: 240px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .resource-link { color: var(--accent); }
  .text-muted { color: var(--text-secondary); }
  .ip-cell {
    font-family: var(--font-mono, monospace);
    font-size: 0.8rem;
    color: var(--text-secondary);
    white-space: nowrap;
  }
  .actions { display: flex; gap: 0.5rem; }

  /* Action badge */
  .action-badge {
    display: inline-block;
    padding: 0.15rem 0.5rem;
    border-radius: 8px;
    font-size: 0.78rem;
    font-weight: 500;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    white-space: nowrap;
    font-family: var(--font-mono, monospace);
  }
  .action-badge[data-action^="user."] { border-color: #58a6ff; color: #58a6ff; background: rgba(88, 166, 255, 0.1); }
  .action-badge[data-action^="repo."] { border-color: #3fb950; color: #3fb950; background: rgba(63, 185, 80, 0.1); }
  .action-badge[data-action^="org."] { border-color: #d2a8ff; color: #d2a8ff; background: rgba(210, 168, 255, 0.1); }
  .action-badge[data-action^="admin."] { border-color: #f85149; color: #f85149; background: rgba(248, 81, 73, 0.1); }

  /* Pagination */
  .pagination { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; }
  .pagination button {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    color: var(--text-primary);
    border-radius: 6px;
    padding: 0.4rem 0.8rem;
    cursor: pointer;
  }
  .pagination button:disabled { opacity: 0.5; cursor: not-allowed; }
  .pagination span { color: var(--text-secondary); font-size: 0.9rem; }

  .btn-sm {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    color: var(--text-primary);
    border-radius: 4px;
    padding: 0.25rem 0.6rem;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .btn-sm:hover { background: var(--bg-hover); }

  /* Modal */
  .modal-overlay {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .modal {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 12px;
    padding: 1.5rem;
    width: 560px;
    max-width: 90vw;
    max-height: 80vh;
    overflow-y: auto;
  }
  .modal h2 { margin: 0 0 1rem; font-size: 1.1rem; }

  .detail-grid { margin-bottom: 1rem; }
  .detail-row {
    display: flex;
    padding: 0.5rem 0;
    border-bottom: 1px solid var(--border);
    gap: 1rem;
  }
  .detail-row:last-child { border-bottom: none; }
  .detail-label {
    min-width: 100px;
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--text-secondary);
    flex-shrink: 0;
  }
  .detail-value {
    font-size: 0.9rem;
    color: var(--text-primary);
    word-break: break-all;
  }
  .detail-row-full {
    flex-direction: column;
    gap: 0.25rem;
  }

  .modal-actions {
    display: flex;
    gap: 0.75rem;
    justify-content: flex-end;
    margin-top: 1rem;
  }
  .btn-secondary {
    background: var(--bg-primary);
    border: 1px solid var(--border);
    color: var(--text-primary);
    border-radius: 6px;
    padding: 0.5rem 1rem;
    cursor: pointer;
    font-size: 0.9rem;
  }
</style>
