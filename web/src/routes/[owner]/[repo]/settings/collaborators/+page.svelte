<script lang="ts">
  import { page } from '$app/stores';
  import { collaborators, type Collaborator } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDate } from '$lib/i18n';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let collaboratorList = $state<Collaborator[]>([]);
  let loading = $state(true);
  let error = $state('');
  let success = $state('');
  let userIdentifier = $state('');
  let permission = $state<'read' | 'write' | 'admin'>('read');
  let adding = $state(false);
  let busyRows = $state<Set<string>>(new Set());
  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  // The collaborator's account name, or the bare id when the row outlives the
  // account it points at. A list that answers "who can push here" must name
  // people; a row it cannot name still has to be visible, since the access it
  // grants is real.
  function collaboratorName(collaborator: Collaborator): string {
    return collaborator.username ?? t('settings.collaborators.unnamed_user', {
      userId: collaborator.user_id
    });
  }

  // `$derived`, not a plain constant: labels computed once at mount would stay
  // in the language the page opened in after a switch (card_0d18cf31d13b).
  const permissionOptions = $derived([
    { value: 'read', label: t('orgs.permission.read') },
    { value: 'write', label: t('orgs.permission.write') },
    { value: 'admin', label: t('orgs.permission.admin') }
  ]);

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    collaboratorList = [];
    loading = true;
    error = '';
    success = '';
    userIdentifier = '';
    permission = 'read';
    adding = false;
    busyRows = new Set();
    void loadCollaborators(expectedOwner, expectedRepo);
  });

  function rowKey(id: number | null, identifier = normalizedUserIdentifier()): string {
    return id === null ? `new:${identifier.toLowerCase()}` : `id:${id}`;
  }

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number): boolean {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function isBusy(key: string): boolean {
    return busyRows.has(key);
  }

  function claimMutation(key: string): boolean {
    if (isBusy(key)) return false;
    busyRows = new Set(busyRows).add(key);
    return true;
  }

  function releaseMutation(key: string): void {
    const next = new Set(busyRows);
    next.delete(key);
    busyRows = next;
  }

  async function loadCollaborators(expectedOwner: string, expectedRepo: string) {
    const claim = listRequests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      error = '';
      const next = await collaborators.list(expectedOwner, expectedRepo);
      if (listRequests.owns(claim, owner, repo)) {
        collaboratorList = next;
        error = '';
      }
    } catch (err: any) {
      if (listRequests.owns(claim, owner, repo)) {
        error = err.message || t('settings.collaborators.load_failed');
      }
    } finally {
      if (listRequests.owns(claim, owner, repo)) loading = false;
    }
  }

  function normalizedUserIdentifier(): string {
    return userIdentifier.trim();
  }

  async function handleAdd(event: SubmitEvent) {
    event.preventDefault();

    const identifier = normalizedUserIdentifier();
    if (!identifier) {
      error = t('settings.collaborators.user_required');
      return;
    }
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const expectedPermission = permission;
    const key = rowKey(null, identifier);
    if (!claimMutation(key)) return;

    try {
      adding = true;
      error = '';
      success = '';
      await collaborators.add(expectedOwner, expectedRepo, identifier, expectedPermission);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      userIdentifier = '';
      permission = 'read';
      success = t('settings.collaborators.added');
      await loadCollaborators(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.collaborators.add_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        adding = false;
        releaseMutation(key);
      }
    }
  }

  async function savePermission(collaborator: Collaborator) {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const expectedPermission = collaborator.permission;
    const key = rowKey(collaborator.id);
    if (!claimMutation(key)) return;
    try {
      error = '';
      success = '';
      await collaborators.updatePermission(
        expectedOwner,
        expectedRepo,
        collaborator.id,
        expectedPermission,
      );
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      success = t('settings.collaborators.updated');
      await loadCollaborators(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.collaborators.update_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) releaseMutation(key);
    }
  }

  async function removeCollaborator(collaborator: Collaborator) {
    if (!(await confirmer.ask({
      title: t('settings.collaborators.remove_confirm_title'),
      message: t('settings.collaborators.remove_confirm', { user: collaboratorName(collaborator) }),
      confirmLabel: t('common.remove'),
    })))
      return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const key = rowKey(collaborator.id);
    if (!claimMutation(key)) return;

    try {
      error = '';
      success = '';
      await collaborators.remove(expectedOwner, expectedRepo, collaborator.user_id);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      success = t('settings.collaborators.removed');
      await loadCollaborators(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.collaborators.remove_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) releaseMutation(key);
    }
  }
</script>

<svelte:head>
  <title>{t('settings.collaborators.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="collaborators-page">
  <div class="page-header">
    <div>
      <h1>{t('settings.collaborators.title')}</h1>
      <p>{t('settings.collaborators.desc')}</p>
    </div>
  </div>

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  <section class="section">
    <h2>{t('settings.collaborators.add_title')}</h2>
    <form class="add-form" onsubmit={handleAdd}>
      <div class="form-group">
        <label for="collaborator-user">{t('settings.collaborators.user_identifier')}</label>
        <input
          id="collaborator-user"
          type="text"
          bind:value={userIdentifier}
          placeholder={t('settings.collaborators.user_placeholder')}
          disabled={adding}
        />
      </div>

      <div class="form-group">
        <label for="collaborator-permission">{t('settings.collaborators.permission')}</label>
        <select id="collaborator-permission" bind:value={permission} disabled={adding}>
          {#each permissionOptions as option}
            <option value={option.value}>{option.label}</option>
          {/each}
        </select>
      </div>

      <button class="btn btn-primary" type="submit" disabled={adding || !normalizedUserIdentifier()}>
        {adding ? t('settings.collaborators.adding') : t('settings.collaborators.add')}
      </button>
    </form>
  </section>

  <section class="section">
    <h2>{t('settings.collaborators.current')}</h2>

    {#if loading}
      <div class="loading">{t('common.loading')}</div>
    {:else if collaboratorList.length === 0}
      <div class="empty-state">{t('settings.collaborators.empty')}</div>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{t('settings.collaborators.user')}</th>
              <th>{t('settings.collaborators.permission')}</th>
              <th>{t('settings.collaborators.created')}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each collaboratorList as collaborator (collaborator.id)}
              <tr>
                <td>
                  <span class="user-name">{collaboratorName(collaborator)}</span>
                  {#if collaborator.display_name}
                    <span class="display-name">{collaborator.display_name}</span>
                  {/if}
                </td>
                <td>
                  <select bind:value={collaborator.permission} disabled={isBusy(rowKey(collaborator.id))}>
                    {#each permissionOptions as option}
                      <option value={option.value}>{option.label}</option>
                    {/each}
                  </select>
                </td>
                <td>{formatDate(collaborator.created_at)}</td>
                <td class="actions">
                  <button
                    class="btn btn-outline"
                    onclick={() => savePermission(collaborator)}
                    disabled={isBusy(rowKey(collaborator.id))}
                    aria-busy={isBusy(rowKey(collaborator.id))}
                  >
                    {t('common.save')}
                  </button>
                  <button
                    class="btn btn-danger"
                    onclick={() => removeCollaborator(collaborator)}
                    disabled={isBusy(rowKey(collaborator.id))}
                    aria-busy={isBusy(rowKey(collaborator.id))}
                  >
                    {t('common.delete')}
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</div>

<ConfirmModal {confirmer} />

<style>
  .collaborators-page {
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

  .add-form {
    display: grid;
    grid-template-columns: minmax(160px, 1fr) minmax(140px, 180px) auto;
    align-items: end;
    gap: 1rem;
  }

  .form-group {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  label {
    color: var(--text-primary);
    font-size: 0.9rem;
    font-weight: 500;
  }

  input,
  select {
    min-height: 38px;
    padding: 0.55rem 0.7rem;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 0.9rem;
  }

  input:focus,
  select:focus {
    border-color: var(--accent);
    outline: none;
  }

  .success-box,
  .error-box {
    padding: 0.75rem 1rem;
    border-radius: 6px;
    margin-bottom: 1rem;
    font-size: 0.9rem;
  }

  .success-box {
    background: rgba(40, 167, 69, 0.1);
    color: var(--green, #28a745);
    border: 1px solid rgba(40, 167, 69, 0.3);
  }

  .error-box {
    background: rgba(220, 53, 69, 0.1);
    color: var(--red, #dc3545);
    border: 1px solid rgba(220, 53, 69, 0.3);
  }

  .loading,
  .empty-state {
    padding: 2rem;
    text-align: center;
    color: var(--text-secondary);
    background: var(--bg-secondary);
    border-radius: 6px;
  }

  .table-wrap {
    overflow-x: auto;
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }

  th,
  td {
    padding: 0.75rem;
    border-bottom: 1px solid var(--border);
    text-align: left;
    color: var(--text-primary);
    vertical-align: middle;
  }

  th {
    color: var(--text-secondary);
    font-size: 0.85rem;
    font-weight: 600;
  }

  .user-name {
    font-weight: 600;
  }

  .display-name {
    color: var(--text-secondary);
    font-size: 0.85rem;
    margin-left: 0.4rem;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    white-space: nowrap;
  }

  .btn {
    padding: 0.55rem 0.9rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    cursor: pointer;
    font-size: 0.9rem;
  }

  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn-primary {
    background: var(--accent);
    color: white;
    border-color: var(--accent);
  }

  .btn-outline {
    background: var(--bg-primary);
    color: var(--text-primary);
  }

  .btn-danger {
    background: var(--red, #dc3545);
    color: white;
    border-color: var(--red, #dc3545);
  }

  @media (max-width: 760px) {
    .add-form {
      grid-template-columns: 1fr;
    }

    .actions {
      justify-content: flex-start;
    }
  }
</style>
