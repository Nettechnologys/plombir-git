<script lang="ts">
  import { isAuthReady, isLoggedIn, isAdmin, getUser } from '$lib/stores/auth.svelte';
  import { goto } from '$app/navigation';
  import { createT, formatDate } from '$lib/i18n';
  import { admin, type AdminUser } from '$lib/api/client.svelte';

  const t = createT();

  let users = $state<AdminUser[]>([]);
  let page = $state(1);
  let perPage = $state(20);
  let totalPages = $state(1);
  let total = $state(0);
  let loading = $state(true);
  let error = $state('');
  let selectedUser = $state<AdminUser | null>(null);
  let editDisplayName = $state('');
  let editBio = $state('');
  let editIsAdmin = $state(false);
  let editIsActive = $state(true);
  let saving = $state(false);
  let showDeleteConfirm = $state(false);
  let deleteTarget = $state<AdminUser | null>(null);
  let unlockingUserId = $state<number | null>(null);

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) { goto('/login'); return; }
    if (!isAdmin()) { goto('/dashboard'); return; }
    loadUsers();
  });

  async function loadUsers() {
    loading = true;
    error = '';
    try {
      const result = await admin.listUsers(page, perPage);
      users = result.data;
      total = result.pagination?.total ?? 0;
      totalPages = result.pagination?.total_pages ?? 1;
    } catch (e: any) {
      error = e.message || t('errors.load_failed');
    } finally {
      loading = false;
    }
  }

  function openEdit(u: AdminUser) {
    selectedUser = u;
    editDisplayName = u.display_name || '';
    editBio = u.bio || '';
    editIsAdmin = u.is_admin;
    editIsActive = u.is_active;
    showDeleteConfirm = false;
  }

  function closeEdit() {
    selectedUser = null;
    showDeleteConfirm = false;
  }

  async function handleSave() {
    if (!selectedUser) return;
    saving = true;
    error = '';
    try {
      await admin.updateUser(selectedUser.id, {
        display_name: editDisplayName || undefined,
        bio: editBio || undefined,
        is_admin: editIsAdmin,
        is_active: editIsActive,
      });
      closeEdit();
      await loadUsers();
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  function confirmDelete(u: AdminUser) {
    deleteTarget = u;
    showDeleteConfirm = true;
  }

  async function handleDelete() {
    if (!deleteTarget) return;
    saving = true;
    error = '';
    try {
      await admin.deleteUser(deleteTarget.id);
      deleteTarget = null;
      showDeleteConfirm = false;
      selectedUser = null;
      await loadUsers();
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  function isLocked(user: AdminUser) {
    return !!user.locked_until && new Date(user.locked_until).getTime() > Date.now();
  }

  async function handleUnlock(user: AdminUser) {
    try {
      unlockingUserId = user.id;
      error = '';
      await admin.unlockUser(user.id);
      await loadUsers();
    } catch (e: any) {
      error = e.message;
    } finally {
      unlockingUserId = null;
    }
  }

  function prevPage() {
    if (page > 1) { page--; loadUsers(); }
  }

  function nextPage() {
    if (page < totalPages) { page++; loadUsers(); }
  }

  function closeEditByKey(e: KeyboardEvent) {
    if (e.key === 'Escape' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      closeEdit();
    }
  }

  function closeDeleteByKey(e: KeyboardEvent) {
    if (e.key === 'Escape' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      showDeleteConfirm = false;
    }
  }
</script>

<div class="container">
  <div class="header">
    <a href="/admin" class="back">← {t('admin.back')}</a>
    <h1>{t('admin.users.title')}</h1>
    <p class="meta">{total} {t('admin.users.total')}</p>
  </div>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  {#if loading}
    <p class="loading">{t('common.loading')}</p>
  {:else}
    <div class="table-wrap">
      <table class="users-table">
        <thead>
          <tr>
            <th>Username</th>
            <th>Email</th>
            <th>Admin</th>
            <th>Active</th>
            <th>Provider</th>
            <th>Login</th>
            <th>Created</th>
            <th></th>
          </tr>
        </thead>
        <tbody>
          {#each users as u}
            <tr>
              <td class="username">{u.username}</td>
              <td class="email">{u.email}</td>
              <td>
                <span class="badge" class:admin={u.is_admin}>
                  {u.is_admin ? '✓' : '—'}
                </span>
              </td>
              <td>
                <span class="badge" class:active={u.is_active} class:inactive={!u.is_active}>
                  {u.is_active ? '✓' : '✗'}
                </span>
              </td>
              <td><span class="badge">{u.auth_provider}</span></td>
              <td class="login-state">
                {#if isLocked(u)}
                  <span class="badge locked" title={`Locked until ${formatDate(u.locked_until || '')}`}>Locked</span>
                {:else if u.login_attempts > 0}
                  <span class="badge warning">{u.login_attempts} failed</span>
                {:else}
                  <span class="badge active" title={u.last_login_at ? `Last login ${formatDate(u.last_login_at)}` : 'No completed login recorded'}>OK</span>
                {/if}
              </td>
              <td class="date">{formatDate(u.created_at)}</td>
              <td class="actions">
                {#if isLocked(u) || u.login_attempts > 0}
                  <button class="btn-sm" disabled={unlockingUserId === u.id} onclick={() => handleUnlock(u)}>
                    {unlockingUserId === u.id ? 'Unlocking...' : 'Unlock'}
                  </button>
                {/if}
                <button class="btn-sm" onclick={() => openEdit(u)}>{t('common.edit')}</button>
                {#if u.id !== getUser()?.id}
                  <button class="btn-danger" onclick={() => confirmDelete(u)}>{t('common.delete')}</button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>

    <!-- Pagination -->
    {#if totalPages > 1}
      <div class="pagination">
        <button onclick={prevPage} disabled={page <= 1}>← Prev</button>
        <span>Page {page} of {totalPages}</span>
        <button onclick={nextPage} disabled={page >= totalPages}>Next →</button>
      </div>
    {/if}
  {/if}
</div>

<!-- Edit modal -->
{#if selectedUser}
  <div
    class="modal-overlay"
    onclick={closeEdit}
    role="button"
    tabindex="0"
    onkeydown={closeEditByKey}
  >
    <div class="modal" role="dialog" aria-modal="true" tabindex="-1">
      <h2>{t('admin.users.edit', { username: selectedUser.username })}</h2>

      {#if error}
        <div class="error">{error}</div>
      {/if}

      <div class="form-group">
        <label for="admin-user-display-name">Display Name</label>
        <input id="admin-user-display-name" type="text" bind:value={editDisplayName} />
      </div>

      <div class="form-group">
        <label for="admin-user-bio">Bio</label>
        <textarea id="admin-user-bio" bind:value={editBio} rows="3"></textarea>
      </div>

      <div class="form-group">
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={editIsAdmin} />
          {t('admin.users.is_admin')}
        </label>
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={editIsActive} />
          {t('admin.users.is_active')}
        </label>
      </div>

      <div class="modal-actions">
        <button class="btn-primary" onclick={handleSave} disabled={saving}>
          {saving ? t('common.loading') : t('common.save')}
        </button>
        <button class="btn-secondary" onclick={closeEdit}>{t('common.cancel')}</button>
      </div>
    </div>
  </div>
  {/if}

<!-- Delete confirm modal -->
{#if showDeleteConfirm && deleteTarget}
  <div
    class="modal-overlay"
    onclick={() => showDeleteConfirm = false}
    role="button"
    tabindex="0"
    onkeydown={closeDeleteByKey}
  >
    <div class="modal" role="dialog" aria-modal="true" tabindex="-1">
      <h2>{t('admin.users.delete_confirm')}</h2>
      <p>
        {t('admin.users.delete_warning', { username: deleteTarget.username })}
      </p>
      {#if error}
        <div class="error">{error}</div>
      {/if}
      <div class="modal-actions">
        <button class="btn-danger" onclick={handleDelete} disabled={saving}>
          {saving ? t('common.loading') : t('common.delete')}
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

  .table-wrap { overflow-x: auto; }
  .users-table { width: 100%; border-collapse: collapse; font-size: 0.9rem; }
  .users-table th { text-align: left; padding: 0.6rem 0.75rem; border-bottom: 2px solid var(--border); color: var(--text-secondary); font-weight: 600; }
  .users-table td { padding: 0.6rem 0.75rem; border-bottom: 1px solid var(--border); color: var(--text-primary); }
  .users-table tr:hover td { background: var(--bg-hover); }
  .username { font-weight: 500; }
  .email { color: var(--text-secondary); font-size: 0.85rem; }
  .date { color: var(--text-secondary); font-size: 0.85rem; white-space: nowrap; }
  .actions { display: flex; gap: 0.5rem; }
  .login-state { white-space: nowrap; }

  .badge { display: inline-block; padding: 0.1rem 0.4rem; border-radius: 8px; font-size: 0.8rem; background: var(--bg-secondary); border: 1px solid var(--border); }
  .badge.admin { background: rgba(255, 213, 0, 0.15); border-color: #ffd500; color: #ffd500; }
  .badge.active { color: #3fb950; border-color: #3fb950; }
  .badge.inactive { color: #f85149; border-color: #f85149; }
  .badge.locked { color: #f85149; border-color: #f85149; background: rgba(248, 81, 73, 0.1); }
  .badge.warning { color: #d29922; border-color: #d29922; }

  .pagination { display: flex; align-items: center; gap: 1rem; margin-top: 1rem; }
  .pagination button { background: var(--bg-secondary); border: 1px solid var(--border); color: var(--text-primary); border-radius: 6px; padding: 0.4rem 0.8rem; cursor: pointer; }
  .pagination button:disabled { opacity: 0.5; cursor: not-allowed; }
  .pagination span { color: var(--text-secondary); font-size: 0.9rem; }

  .btn-sm { background: var(--bg-secondary); border: 1px solid var(--border); color: var(--text-primary); border-radius: 4px; padding: 0.25rem 0.6rem; font-size: 0.8rem; cursor: pointer; }
  .btn-sm:hover { background: var(--bg-hover); }
  .btn-sm:disabled { opacity: 0.6; cursor: wait; }
  .btn-danger { background: rgba(248, 81, 73, 0.15); border: 1px solid #f85149; color: #f85149; border-radius: 4px; padding: 0.25rem 0.6rem; font-size: 0.8rem; cursor: pointer; }
  .btn-danger:hover { background: rgba(248, 81, 73, 0.25); }

  /* Modal */
  .modal-overlay { position: fixed; inset: 0; background: rgba(0,0,0,0.7); display: flex; align-items: center; justify-content: center; z-index: 1000; }
  .modal { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 12px; padding: 1.5rem; width: 480px; max-width: 90vw; }
  .modal h2 { margin: 0 0 1rem; font-size: 1.1rem; }
  .modal p { color: var(--text-secondary); margin: 0 0 1rem; }
  .form-group { margin-bottom: 1rem; }
  .form-group label { display: block; font-size: 0.85rem; font-weight: 600; color: var(--text-secondary); margin-bottom: 0.4rem; }
  .form-group input[type="text"], .form-group textarea { width: 100%; box-sizing: border-box; background: var(--bg-primary); color: var(--text-primary); border: 1px solid var(--border); border-radius: 6px; padding: 0.5rem 0.75rem; font-size: 0.9rem; }
  .form-group textarea { resize: vertical; }
  .checkbox-label { display: flex; align-items: center; gap: 0.5rem; font-size: 0.9rem; font-weight: normal; cursor: pointer; color: var(--text-primary); }
  .checkbox-label input { width: auto; }
  .modal-actions { display: flex; gap: 0.75rem; justify-content: flex-end; margin-top: 1.25rem; }
  .btn-primary { background: var(--accent); color: white; border: none; border-radius: 6px; padding: 0.5rem 1rem; cursor: pointer; font-size: 0.9rem; }
  .btn-primary:disabled { opacity: 0.6; cursor: not-allowed; }
  .btn-secondary { background: var(--bg-primary); border: 1px solid var(--border); color: var(--text-primary); border-radius: 6px; padding: 0.5rem 1rem; cursor: pointer; font-size: 0.9rem; }
</style>
