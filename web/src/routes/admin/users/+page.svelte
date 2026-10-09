<script lang="ts">
  import { isAuthReady, isLoggedIn, isAdmin, getUser } from '$lib/stores/auth.svelte';
  import { goto } from '$app/navigation';
  import { createT, formatDate } from '$lib/i18n';
  import { admin, buildAdminUserPayload, type AdminUser } from '$lib/api/client.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { untrack } from 'svelte';
  import Modal from '$lib/components/Modal.svelte';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();

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
  let busyUserIds = $state<Set<number>>(new Set());
  let showCreate = $state(false);
  let newUsername = $state('');
  let newEmail = $state('');
  let newDisplayName = $state('');
  let newIsAdmin = $state(false);
  let creating = $state(false);
  // A generated password is shown once, right after it was made, for the
  // administrator to hand over; its holder has to replace it at first sign-in.
  let issuedPassword = $state<{ username: string; password: string } | null>(null);
  const listRequests = new LatestRequestFence<string>();

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) { goto('/login'); return; }
    if (!isAdmin()) { goto('/dashboard'); return; }
    // Pagination owns its own reload. Do not make this auth effect subscribe to
    // page/perPage merely because loadUsers reads their request snapshot.
    untrack(() => void loadUsers());
  });

  function pageRequestKey(expectedPage: number, expectedPerPage: number): string {
    return `${expectedPage}:${expectedPerPage}`;
  }

  function isUserBusy(id: number): boolean {
    return busyUserIds.has(id);
  }

  function claimUser(id: number): boolean {
    if (isUserBusy(id)) return false;
    busyUserIds = new Set(busyUserIds).add(id);
    return true;
  }

  function releaseUser(id: number): void {
    const next = new Set(busyUserIds);
    next.delete(id);
    busyUserIds = next;
  }

  async function loadUsers() {
    const expectedPage = page;
    const expectedPerPage = perPage;
    const identity = pageRequestKey(expectedPage, expectedPerPage);
    const claim = listRequests.begin(identity);
    loading = true;
    error = '';
    try {
      const result = await admin.listUsers(expectedPage, expectedPerPage);
      if (listRequests.owns(claim, pageRequestKey(page, perPage))) {
        users = result.data;
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

  function openEdit(u: AdminUser) {
    if (isUserBusy(u.id)) return;
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
    const target = selectedUser;
    const userId = target.id;
    const payload = buildAdminUserPayload({
      display_name: editDisplayName,
      bio: editBio,
      is_admin: editIsAdmin,
      is_active: editIsActive,
    });
    if (!claimUser(userId)) return;
    saving = true;
    error = '';
    try {
      await admin.updateUser(userId, payload);
      if (selectedUser?.id === userId) closeEdit();
      await loadUsers();
    } catch (e: any) {
      if (selectedUser?.id === userId) error = e.message;
    } finally {
      saving = false;
      releaseUser(userId);
    }
  }

  function confirmDelete(u: AdminUser) {
    if (isUserBusy(u.id)) return;
    deleteTarget = u;
    showDeleteConfirm = true;
  }

  async function handleDelete() {
    if (!deleteTarget) return;
    const userId = deleteTarget.id;
    if (!claimUser(userId)) return;
    saving = true;
    error = '';
    try {
      await admin.deleteUser(userId);
      if (deleteTarget?.id === userId) {
        deleteTarget = null;
        showDeleteConfirm = false;
      }
      if (selectedUser?.id === userId) selectedUser = null;
      await loadUsers();
    } catch (e: any) {
      if (deleteTarget?.id === userId) error = e.message;
    } finally {
      saving = false;
      releaseUser(userId);
    }
  }

  async function handleCreate(e: Event) {
    e.preventDefault();
    creating = true;
    error = '';
    try {
      const created = await admin.createUser({
        username: newUsername.trim(),
        email: newEmail.trim(),
        display_name: newDisplayName.trim() || undefined,
        is_admin: newIsAdmin,
      });
      issuedPassword = { username: created.user.username, password: created.temporary_password };
      newUsername = '';
      newEmail = '';
      newDisplayName = '';
      newIsAdmin = false;
      showCreate = false;
      await loadUsers();
    } catch (e: any) {
      error = e.message;
    } finally {
      creating = false;
    }
  }

  async function handleResetPassword(user: AdminUser) {
    if (!(await confirmer.ask({
      title: t('admin.users.reset_password'),
      message: t('admin.users.reset_password_confirm', { username: user.username }),
      confirmLabel: t('admin.users.reset_password'),
    }))) return;
    const userId = user.id;
    if (!claimUser(userId)) return;
    try {
      error = '';
      const { temporary_password } = await admin.resetUserPassword(userId);
      issuedPassword = { username: user.username, password: temporary_password };
    } catch (e: any) {
      error = e.message;
    } finally {
      releaseUser(userId);
    }
  }

  function isLocked(user: AdminUser) {
    return !!user.locked_until && new Date(user.locked_until).getTime() > Date.now();
  }

  async function handleUnlock(user: AdminUser) {
    const userId = user.id;
    if (!claimUser(userId)) return;
    try {
      error = '';
      await admin.unlockUser(userId);
      await loadUsers();
    } catch (e: any) {
      error = e.message;
    } finally {
      releaseUser(userId);
    }
  }

  function prevPage() {
    if (page > 1) { page--; loadUsers(); }
  }

  function nextPage() {
    if (page < totalPages) { page++; loadUsers(); }
  }

</script>

<svelte:head>
  <title>{t('admin.users.title')} · {t('admin.settings.admin')} · Plombir Git</title>
</svelte:head>

<div class="container">
  <div class="header">
    <a href="/admin" class="back">← {t('admin.back')}</a>
    <h1>{t('admin.users.title')}</h1>
    <p class="meta">{total} {t('admin.users.total')}</p>
  </div>

  {#if error}
    <div class="error">{error}</div>
  {/if}

  {#if issuedPassword}
    <div class="issued-password" role="status">
      <p>
        {t('admin.users.temp_password_before')} <strong>{issuedPassword.username}</strong>
        {t('admin.users.temp_password_after')}
      </p>
      <code class="temporary-password">{issuedPassword.password}</code>
      <button class="btn-sm" onclick={() => (issuedPassword = null)}>{t('admin.users.done')}</button>
    </div>
  {/if}

  <div class="create-user">
    {#if showCreate}
      <form class="create-user-form" onsubmit={handleCreate}>
        <input id="admin-new-username" type="text" placeholder={t('admin.users.username_placeholder')} bind:value={newUsername} required autocomplete="off" />
        <input id="admin-new-email" type="email" placeholder={t('admin.users.email_placeholder')} bind:value={newEmail} required autocomplete="off" />
        <input type="text" placeholder={t('admin.users.display_name_placeholder')} bind:value={newDisplayName} />
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={newIsAdmin} />
          {t('admin.users.administrator')}
        </label>
        <button type="submit" class="btn-sm" disabled={creating}>{creating ? t('admin.users.creating') : t('admin.users.create')}</button>
        <button type="button" class="btn-sm" onclick={() => (showCreate = false)}>{t('common.cancel', 'Cancel')}</button>
      </form>
    {:else}
      <button class="btn-sm open-create-user" onclick={() => (showCreate = true)}>{t('admin.users.new')}</button>
    {/if}
  </div>

  {#if loading}
    <p class="loading">{t('common.loading')}</p>
  {:else}
    <div class="table-wrap">
      <table class="users-table">
        <thead>
          <tr>
            <th>{t('admin.users.columns.username')}</th>
            <th>{t('admin.users.columns.email')}</th>
            <th>{t('admin.users.columns.admin')}</th>
            <th>{t('admin.users.columns.active')}</th>
            <th>{t('admin.users.columns.provider')}</th>
            <th>{t('admin.users.columns.login')}</th>
            <th>{t('admin.users.columns.created')}</th>
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
                  <span class="badge locked" title={t('admin.users.locked_until', { date: formatDate(u.locked_until || '') })}>{t('admin.users.locked')}</span>
                {:else if u.login_attempts > 0}
                  <span class="badge warning">{t('admin.users.failed_attempts', { count: u.login_attempts })}</span>
                {:else}
                  <span class="badge active" title={u.last_login_at ? t('admin.users.last_login', { date: formatDate(u.last_login_at) }) : t('admin.users.no_login')}>{t('admin.users.login_ok')}</span>
                {/if}
              </td>
              <td class="date">{formatDate(u.created_at)}</td>
              <td class="actions">
                {#if isLocked(u) || u.login_attempts > 0}
                  <button class="btn-sm" disabled={isUserBusy(u.id)} onclick={() => handleUnlock(u)}>
                    {isUserBusy(u.id) ? t('admin.users.working') : t('admin.users.unlock')}
                  </button>
                {/if}
                <button class="btn-sm" disabled={isUserBusy(u.id)} onclick={() => openEdit(u)}>{t('common.edit')}</button>
                {#if u.auth_provider === 'local' && u.id !== getUser()?.id}
                  <button class="btn-sm reset-password" disabled={isUserBusy(u.id)} onclick={() => handleResetPassword(u)}>{t('admin.users.reset_password')}</button>
                {/if}
                {#if u.id !== getUser()?.id}
                  <button class="btn-danger" disabled={isUserBusy(u.id)} onclick={() => confirmDelete(u)}>{t('common.delete')}</button>
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
        <button onclick={prevPage} disabled={page <= 1}>{t('common.prev_arrow')}</button>
        <span>{t('common.page_info', { page, total: totalPages })}</span>
        <button onclick={nextPage} disabled={page >= totalPages}>{t('common.next_arrow')}</button>
      </div>
    {/if}
  {/if}
</div>

<!-- Edit modal -->
{#if selectedUser}
  <Modal onclose={closeEdit} labelledby="admin-user-edit-title">
    <div class="modal">
      <h2 id="admin-user-edit-title">{t('admin.users.edit', { username: selectedUser.username })}</h2>

      {#if error}
        <div class="error">{error}</div>
      {/if}

      <div class="form-group">
        <label for="admin-user-display-name">{t('admin.users.display_name')}</label>
        <input id="admin-user-display-name" type="text" bind:value={editDisplayName} />
      </div>

      <div class="form-group">
        <label for="admin-user-bio">{t('admin.users.bio')}</label>
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
        <button class="btn-primary" onclick={handleSave} disabled={saving || isUserBusy(selectedUser.id)}>
          {saving ? t('common.loading') : t('common.save')}
        </button>
        <button class="btn-secondary" onclick={closeEdit}>{t('common.cancel')}</button>
      </div>
    </div>
  </Modal>
{/if}

<!-- Delete confirm modal -->
{#if showDeleteConfirm && deleteTarget}
  <Modal onclose={() => showDeleteConfirm = false} labelledby="admin-user-delete-title">
    <div class="modal">
      <h2 id="admin-user-delete-title">{t('admin.users.delete_confirm')}</h2>
      <p>
        {t('admin.users.delete_warning', { username: deleteTarget.username })}
      </p>
      {#if error}
        <div class="error">{error}</div>
      {/if}
      <div class="modal-actions">
        <button class="btn-danger" onclick={handleDelete} disabled={saving || isUserBusy(deleteTarget.id)}>
          {saving ? t('common.loading') : t('common.delete')}
        </button>
        <button class="btn-secondary" onclick={() => showDeleteConfirm = false} data-autofocus>{t('common.cancel')}</button>
      </div>
    </div>
  </Modal>
{/if}

<ConfirmModal {confirmer} />

<style>
  .issued-password {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 12px;
    margin-bottom: 16px;
    padding: 12px 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-secondary);
  }

  .issued-password p { margin: 0; flex-basis: 100%; }

  .temporary-password { font-size: 15px; user-select: all; }

  .create-user { margin-bottom: 16px; }

  .create-user-form {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
  }

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
  /* The panel itself is lib/components/Modal.svelte; `.modal` scopes its content. */
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

  /* Phone widths (card_c30077df5603): the create form and the issued password
     wrap; the table already scrolls inside .table-wrap. */
  @media (max-width: 700px) {
    .create-user-form,
    .issued-password,
    .actions { flex-wrap: wrap; }
  }
</style>
