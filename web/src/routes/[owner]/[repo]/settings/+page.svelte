<script lang="ts">
  import { onDestroy } from 'svelte';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import Modal from '$lib/components/Modal.svelte';
  import {
    buildRepoSettingsPatch,
    repoStorage,
    repoSettingsFormState,
    repos,
    type RepoStorageReport,
    type RepoSettingsFormState,
    type RepositoryDetail,
  } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { isRepoAdmin } from '$lib/repoPermission';
  import { createT } from '$lib/i18n';

  const t = createT();

  let { data } = $props();

  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
  
  let repository = $state<RepositoryDetail | null>(null);
  let loading = $state(true);
  let error = $state('');

  // Settings form (card_3625a7b89abb): repository administrators only — the
  // field comes from the server, which still refuses everyone else.
  let isAdmin = $derived(isRepoAdmin(repository?.viewer_permission));
  let branchNames = $state<string[]>([]);
  let form = $state<RepoSettingsFormState>({ name: '', description: '', isPrivate: false, defaultBranch: '' });
  let saving = $state(false);
  let saveError = $state('');
  let saveNotice = $state('');
  // The general form saves the description and the default branch; visibility
  // and the name have their own confirmed actions below, so this patch never
  // carries them.
  let generalPatch = $derived(
    repository
      ? buildRepoSettingsPatch(repository, { ...form, name: repository.name, isPrivate: repository.is_private })
      : {},
  );
  let generalChanged = $derived(Object.keys(generalPatch).length > 0);
  let branchOptions = $derived(
    repository && !branchNames.includes(repository.default_branch)
      ? [repository.default_branch, ...branchNames]
      : branchNames,
  );

  // Storage read-out (security audit #10): the ceilings every upload path
  // enforces are only actionable if the owner can see how close the repository
  // is. One compact section, loaded alongside the repository itself.
  let storage = $state<RepoStorageReport | null>(null);
  let storageError = $state('');

  function formatSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
    if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
    return `${(bytes / (1024 * 1024 * 1024)).toFixed(2)} GiB`;
  }

  async function loadStorage(expectedOwner: string, expectedRepo: string) {
    const expectedRoute = routeGeneration;
    try {
      const report = await repoStorage.get(expectedOwner, expectedRepo);
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        storage = report;
        storageError = '';
      }
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        storageError = err?.message || t('settings.storage.load_failed');
      }
    }
  }

  let visibilityOpen = $state(false);
  let visibilityBusy = $state(false);
  let visibilityError = $state('');

  let renameTo = $state('');
  let renameOpen = $state(false);
  let renaming = $state(false);
  let renameError = $state('');
  let renameTarget = $derived(renameTo.trim());
  
  // Transfer state
  let newOwner = $state('');
  let transferring = $state(false);
  let transferError = $state('');
  let transferSuccess = $state('');
  let transferRedirectTimer: ReturnType<typeof setTimeout> | undefined;
  
  // Delete state
  let deleteConfirm = $state('');
  let deleting = $state(false);
  let deleteError = $state('');
  let repositoryPath = $derived(`${owner}/${repo}`);
  const repositoryRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;
  
  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    if (transferRedirectTimer !== undefined) {
      clearTimeout(transferRedirectTimer);
      transferRedirectTimer = undefined;
    }
    repository = null;
    loading = true;
    error = '';
    branchNames = [];
    form = { name: '', description: '', isPrivate: false, defaultBranch: '' };
    saving = false;
    saveError = '';
    saveNotice = '';
    visibilityOpen = false;
    visibilityBusy = false;
    visibilityError = '';
    renameTo = '';
    renameOpen = false;
    renaming = false;
    renameError = '';
    newOwner = '';
    transferring = false;
    transferError = '';
    transferSuccess = '';
    deleteConfirm = '';
    deleting = false;
    deleteError = '';
    storage = null;
    storageError = '';
    void loadRepository(expectedOwner, expectedRepo);
  });

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function adopt(next: RepositoryDetail) {
    repository = next;
    form = repoSettingsFormState(next);
    renameTo = next.name;
  }
  
  async function loadRepository(expectedOwner: string, expectedRepo: string) {
    const claim = repositoryRequests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      const response = await repos.get(expectedOwner, expectedRepo);
      if (!repositoryRequests.owns(claim, owner, repo)) return;
      adopt(response);
      // Not gated on admin: the endpoint is a read, and the page shows the
      // section to whoever can see the repository.
      void loadStorage(expectedOwner, expectedRepo);
      error = '';
      if (isRepoAdmin(response.viewer_permission)) {
        // Only a picker: the current default stays selectable even when the
        // branch list cannot be read.
        const branches = await Promise.resolve(repos.branches(expectedOwner, expectedRepo)).catch(() => []);
        if (repositoryRequests.owns(claim, owner, repo)) {
          branchNames = (branches ?? []).map((branch: { name: string }) => branch.name);
        }
      }
    } catch (err: any) {
      if (repositoryRequests.owns(claim, owner, repo)) {
        error = err.message || t('settings.load_failed');
      }
    } finally {
      if (repositoryRequests.owns(claim, owner, repo)) loading = false;
    }
  }

  async function handleSave(event: Event) {
    event.preventDefault();
    if (!repository || saving || !generalChanged) return;
    const patch = generalPatch;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    saving = true;
    saveError = '';
    saveNotice = '';
    try {
      const updated = await repos.update(expectedOwner, expectedRepo, patch);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute) || !repository) return;
      const next = { ...repository, ...updated };
      repository = next;
      form = { ...form, description: next.description ?? '', defaultBranch: next.default_branch };
      saveNotice = t('settings.general_form.saved');
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        saveError = err?.message || t('settings.general_form.save_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) saving = false;
    }
  }

  function openVisibility() {
    visibilityError = '';
    visibilityOpen = true;
  }

  function closeVisibility() {
    if (visibilityBusy) return;
    visibilityOpen = false;
    visibilityError = '';
  }

  async function confirmVisibility() {
    if (!repository || visibilityBusy) return;
    const makePrivate = !repository.is_private;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    visibilityBusy = true;
    visibilityError = '';
    try {
      const updated = await repos.update(expectedOwner, expectedRepo, { is_private: makePrivate });
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute) || !repository) return;
      repository = { ...repository, ...updated };
      form = { ...form, isPrivate: repository.is_private };
      visibilityOpen = false;
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        visibilityError = err?.message || t('settings.visibility.failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) visibilityBusy = false;
    }
  }

  function openRename() {
    if (!repository || !renameTarget || renameTarget === repository.name) return;
    renameError = '';
    renameOpen = true;
  }

  function closeRename() {
    if (renaming) return;
    renameOpen = false;
    renameError = '';
  }

  async function confirmRename() {
    const name = renameTarget;
    if (!name || renaming) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    renaming = true;
    renameError = '';
    try {
      const updated = await repos.update(expectedOwner, expectedRepo, { name });
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      renameOpen = false;
      // There is no redirect from the old name: the page moves with the repository.
      await goto(`/${expectedOwner}/${updated?.name || name}/settings`);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        renameError = err?.message || t('settings.rename.failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) renaming = false;
    }
  }
  
  // Transfer and delete confirm in a `Modal` rather than `window.confirm()`
  // (card_270a0a77fd79): the native dialog cannot say which repository and
  // which destination it is about, is styled by the browser, and a browser
  // set to suppress dialogs answers it `false` without asking anybody.
  let transferConfirmOpen = $state(false);
  // The repository's name goes in as text, not through `{@html}`: the sentence
  // is split around its placeholder and the name is rendered between.
  const deleteInstruction = $derived(
    t('settings.delete.confirm_instruction', { repo: '\u0001' }).split('\u0001'),
  );
  let deleteConfirmOpen = $state(false);

  function askTransfer() {
    if (!newOwner.trim() || transferring) return;
    transferConfirmOpen = true;
  }

  function askDelete() {
    if (deleteConfirm !== repositoryPath || deleting) return;
    deleteConfirmOpen = true;
  }

  async function handleTransfer() {
    const destinationOwner = newOwner.trim();
    transferConfirmOpen = false;
    if (!destinationOwner) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    
    try {
      transferring = true;
      transferError = '';
      transferSuccess = '';
      
      await repos.transfer(expectedOwner, expectedRepo, destinationOwner);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      transferSuccess = t('settings.transfer.success');
      // Redirect to new repo URL
      if (transferRedirectTimer !== undefined) clearTimeout(transferRedirectTimer);
      transferRedirectTimer = setTimeout(() => {
        transferRedirectTimer = undefined;
        if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
          goto(`/${destinationOwner}/${expectedRepo}`);
        }
      }, 1500);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        transferError = err.message || t('settings.transfer.failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) transferring = false;
    }
  }

  onDestroy(() => {
    if (transferRedirectTimer !== undefined) {
      clearTimeout(transferRedirectTimer);
      transferRedirectTimer = undefined;
    }
  });
  
  async function handleDelete() {
    deleteConfirmOpen = false;
    if (deleteConfirm !== repositoryPath) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    
    try {
      deleting = true;
      deleteError = '';
      
      await repos.delete(expectedOwner, expectedRepo);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      
      // Redirect to dashboard
      goto('/dashboard');
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        deleteError = err.message || t('settings.delete.failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) deleting = false;
    }
}
</script>

<svelte:head>
  <title>{t('settings.general')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="settings-page">
  <h1>{t('settings.general')}</h1>
  
  {#if loading}
    <div class="loading">{t('common.loading')}</div>
  {:else if error}
    <div class="error">{error}</div>
  {:else if repository && isAdmin}
    <!-- General settings -->
    <section class="section">
      <h2>{t('settings.repository_info.title', 'Repository Information')}</h2>
      <form class="general-form" onsubmit={handleSave}>
        <div class="form-group">
          <label for="repo-description">{t('settings.repository_info.description', 'Description')}</label>
          <textarea
            id="repo-description"
            rows="3"
            maxlength="2000"
            bind:value={form.description}
            placeholder={t('settings.general_form.description_placeholder')}
            disabled={saving}
          ></textarea>
        </div>
        <div class="form-group">
          <label for="repo-default-branch">{t('settings.general_form.default_branch')}</label>
          <select id="repo-default-branch" bind:value={form.defaultBranch} disabled={saving}>
            {#each branchOptions as branchName (branchName)}
              <option value={branchName}>{branchName}</option>
            {/each}
          </select>
          <p class="hint">{t('settings.general_form.default_branch_hint')}</p>
        </div>
        {#if saveError}
          <div class="error-box save-error" role="alert">{saveError}</div>
        {/if}
        {#if saveNotice}
          <div class="success-box" role="status">{saveNotice}</div>
        {/if}
        <button type="submit" class="btn btn-primary save-general" disabled={saving || !generalChanged}>
          {saving ? t('common.saving') : t('settings.general_form.save')}
        </button>
      </form>
    </section>

    <!-- Visibility -->
    <section class="section">
      <h2>{t('settings.repository_info.visibility', 'Visibility')}</h2>
      <p class="section-desc">
        <span class="badge" class:private={repository.is_private}>
          {repository.is_private ? t('settings.repository_info.private', 'Private') : t('settings.repository_info.public', 'Public')}
        </span>
        {repository.is_private ? t('settings.visibility.now_private') : t('settings.visibility.now_public')}
      </p>
      <button class="btn btn-warning toggle-visibility" onclick={openVisibility}>
        {repository.is_private ? t('settings.visibility.make_public') : t('settings.visibility.make_private')}
      </button>
    </section>

    <!-- Rename -->
    <section class="section">
      <h2>{t('settings.rename.title')}</h2>
      <p class="section-desc">{t('settings.rename.desc')}</p>
      <div class="form-group">
        <label for="repo-rename">{t('settings.rename.new_name')}</label>
        <div class="input-row">
          <input id="repo-rename" type="text" bind:value={renameTo} disabled={renaming} />
          <button
            class="btn btn-warning open-rename"
            onclick={openRename}
            disabled={renaming || !renameTarget || renameTarget === repository.name}
          >
            {t('settings.rename.button')}
          </button>
        </div>
      </div>
    </section>
  {:else if repository}
    <!-- Repository Info -->
    <section class="section">
      <h2>{t('settings.repository_info.title', 'Repository Information')}</h2>
      <div class="info-grid">
        <div class="info-item">
          <span class="info-label">{t('settings.repository_info.name', 'Name')}</span>
          <div class="info-value">{repository.name}</div>
        </div>
        <div class="info-item">
          <span class="info-label">{t('settings.repository_info.description', 'Description')}</span>
          <div class="info-value">{repository.description || '-'}</div>
        </div>
        <div class="info-item">
          <span class="info-label">{t('settings.repository_info.visibility', 'Visibility')}</span>
          <div class="info-value">
            <span class="badge" class:private={repository.is_private}>
              {repository.is_private ? t('settings.repository_info.private', 'Private') : t('settings.repository_info.public', 'Public')}
            </span>
          </div>
        </div>
      </div>
      <p class="section-desc admin-only-note">{t('settings.general_form.admin_only')}</p>
    </section>
  {/if}

  {#if repository}
    <!-- Storage (security audit #10): usage per store against the enforced budget. -->
    <section class="section storage-section">
      <h2>{t('settings.storage.title')}</h2>
      {#if storage}
        <p class="section-desc">
          {t('settings.storage.usage', {
            used: formatSize(storage.usage.total_bytes),
            limit: formatSize(storage.limits.repo_quota_bytes),
          })}
        </p>
        <p class="storage-breakdown">
          {t('settings.storage.breakdown', {
            lfs: formatSize(storage.usage.lfs_bytes),
            releases: formatSize(storage.usage.release_bytes),
            attachments: formatSize(storage.usage.attachment_bytes),
            ci: formatSize(storage.usage.ci_cache_bytes),
            packages: formatSize(storage.usage.package_bytes),
            oci: formatSize(storage.usage.oci_bytes),
          })}
        </p>
      {:else if storageError}
        <p class="section-desc storage-error">{storageError}</p>
      {:else}
        <p class="section-desc">{t('common.loading')}</p>
      {/if}
    </section>
  {/if}

  {#if repository && isAdmin}
    <!-- Transfer Ownership -->
    <section class="section transfer-section">
      <h2>{t('settings.transfer.title')}</h2>
      <p class="section-desc">{t('settings.transfer.desc')}</p>
      
      <div class="warning-box">
        <span class="warning-icon">⚠️</span>
        <p>{t('settings.transfer.warning')}</p>
      </div>
      
      {#if transferError}
        <div class="error-box">{transferError}</div>
      {/if}
      
      {#if transferSuccess}
        <div class="success-box">{transferSuccess}</div>
      {/if}
      
      <div class="form-group">
        <label for="new-owner">{t('settings.transfer.new_owner')}</label>
        <div class="input-row">
          <input 
            id="new-owner"
            type="text" 
            bind:value={newOwner}
            placeholder={t('settings.transfer.new_owner_placeholder')}
            disabled={transferring}
          />
          <button 
            class="btn btn-warning transfer-repo"
            onclick={askTransfer}
            disabled={!newOwner.trim() || transferring}
          >
            {transferring ? t('settings.transfer.confirming') : t('settings.transfer.confirm')}
          </button>
        </div>
      </div>
    </section>
    
    <!-- Danger Zone -->
    <section class="section danger-zone">
      <h2>{t('settings.danger_zone')}</h2>
      
      <div class="danger-box">
        <h3>{t('settings.delete.title')}</h3>
        <p>{t('settings.delete.desc')}</p>
        
        {#if deleteError}
          <div class="error-box">{deleteError}</div>
        {/if}
        
        <div class="form-group">
          <label for="delete-confirm">{deleteInstruction[0]}<strong>{repositoryPath}</strong>{deleteInstruction[1] ?? ''}</label>
          <input 
            id="delete-confirm"
            type="text" 
            bind:value={deleteConfirm}
            placeholder={t('settings.delete.confirm_placeholder')}
            disabled={deleting}
          />
        </div>
        
        <button 
          class="btn btn-danger"
          onclick={askDelete}
          disabled={deleteConfirm !== repositoryPath || deleting}
        >
          {deleting ? t('settings.delete.confirming') : t('settings.delete.confirm_button')}
        </button>
      </div>
    </section>
  {/if}
</div>

{#if transferConfirmOpen && repository}
  <Modal onclose={() => (transferConfirmOpen = false)} labelledby="transfer-confirm-title">
    <h2 id="transfer-confirm-title">{t('settings.transfer.title')}</h2>
    <p class="confirm-target"><strong>{repositoryPath}</strong> → <strong>{newOwner.trim()}/{repo}</strong></p>
    <p>{t('settings.transfer.warning')}</p>
    <div class="modal-actions">
      <button class="btn btn-warning confirm-transfer" onclick={handleTransfer} disabled={transferring}>
        {t('settings.transfer.confirm')}
      </button>
      <button class="btn" onclick={() => (transferConfirmOpen = false)} data-autofocus>{t('common.cancel')}</button>
    </div>
  </Modal>
{/if}

{#if deleteConfirmOpen && repository}
  <Modal onclose={() => (deleteConfirmOpen = false)} labelledby="delete-confirm-title">
    <h2 id="delete-confirm-title">{t('settings.delete.title')}</h2>
    <p class="confirm-target"><strong>{repositoryPath}</strong></p>
    <p>{t('settings.delete.desc')}</p>
    <div class="modal-actions">
      <button class="btn btn-danger confirm-delete-repo" onclick={handleDelete} disabled={deleting}>
        {t('settings.delete.confirm_button')}
      </button>
      <button class="btn" onclick={() => (deleteConfirmOpen = false)} data-autofocus>{t('common.cancel')}</button>
    </div>
  </Modal>
{/if}

{#if visibilityOpen && repository}
  <Modal onclose={closeVisibility} labelledby="visibility-title">
    <h2 id="visibility-title">
      {repository.is_private ? t('settings.visibility.confirm_public_title') : t('settings.visibility.confirm_private_title')}
    </h2>
    <p>{repository.is_private ? t('settings.visibility.confirm_public_body') : t('settings.visibility.confirm_private_body')}</p>
    {#if visibilityError}
      <div class="error-box visibility-error" role="alert">{visibilityError}</div>
    {/if}
    <div class="modal-actions">
      <button class="btn btn-warning confirm-visibility" onclick={confirmVisibility} disabled={visibilityBusy}>
        {visibilityBusy ? t('common.saving') : repository.is_private ? t('settings.visibility.make_public') : t('settings.visibility.make_private')}
      </button>
      <button class="btn btn-secondary" onclick={closeVisibility} disabled={visibilityBusy} data-autofocus>{t('common.cancel')}</button>
    </div>
  </Modal>
{/if}

{#if renameOpen && repository}
  <Modal onclose={closeRename} labelledby="rename-title">
    <h2 id="rename-title">{t('settings.rename.confirm_title')}</h2>
    <p>{t('settings.rename.confirm_body', { from: `${owner}/${repository.name}`, to: `${owner}/${renameTarget}` })}</p>
    <p class="rename-warning">{t('settings.rename.no_redirect')}</p>
    {#if renameError}
      <div class="error-box rename-error" role="alert">{renameError}</div>
    {/if}
    <div class="modal-actions">
      <button class="btn btn-warning confirm-rename" onclick={confirmRename} disabled={renaming}>
        {renaming ? t('settings.rename.renaming') : t('settings.rename.button')}
      </button>
      <button class="btn btn-secondary" onclick={closeRename} disabled={renaming} data-autofocus>{t('common.cancel')}</button>
    </div>
  </Modal>
{/if}

<style>
  .settings-page {
    max-width: 800px;
  }
  
  h1 {
    font-size: 1.75rem;
    margin-bottom: 2rem;
    color: var(--text-primary);
  }
  
  h2 {
    font-size: 1.25rem;
    margin-bottom: 1rem;
    color: var(--text-primary);
  }
  
  .section {
    margin-bottom: 2.5rem;
    padding-bottom: 2rem;
    border-bottom: 1px solid var(--border);
  }
  
  .section-desc {
    color: var(--text-secondary);
    margin-bottom: 1.5rem;
    font-size: 0.9rem;
  }
  
.info-grid {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }
  
  .info-item {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .info-label {
    font-size: 0.85rem;
    color: var(--text-secondary);
    font-weight: 500;
  }
  
  .info-value {
    color: var(--text-primary);
    font-size: 0.95rem;
  }
  
  .badge {
    display: inline-block;
    padding: 0.25rem 0.75rem;
    border-radius: 12px;
    font-size: 0.8rem;
    font-weight: 600;
    background: var(--green, #28a745);
    color: white;
  }
  
  .badge.private {
    background: var(--orange, #ff8800);
  }
  
  .warning-box {
    display: flex;
    gap: 0.75rem;
    padding: 1rem;
    background: rgba(255, 165, 0, 0.1);
    border: 1px solid var(--orange, #ff8800);
    border-radius: 6px;
    margin-bottom: 1.5rem;
  }
  
  .warning-icon {
    font-size: 1.25rem;
    flex-shrink: 0;
  }
  
  .warning-box p {
    color: var(--text-primary);
    font-size: 0.9rem;
    margin: 0;
  }
  
  .form-group {
    margin-top: 1.5rem;
  }
  
  .form-group label {
    display: block;
    margin-bottom: 0.5rem;
    color: var(--text-primary);
    font-weight: 500;
    font-size: 0.9rem;
  }
  
  .input-row {
    display: flex;
    gap: 0.75rem;
  }
  
  input[type="text"] {
    flex: 1;
    padding: 0.6rem 0.75rem;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 0.9rem;
  }
  
  input[type="text"]:focus {
    outline: none;
    border-color: var(--accent);
  }
  
  .btn {
    padding: 0.6rem 1.25rem;
    border: none;
    border-radius: 6px;
    font-size: 0.9rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
  }
  
  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  
  .btn-warning {
    background: var(--orange, #ff8800);
    color: white;
  }
  
  .btn-warning:hover:not(:disabled) {
    background: var(--orange-dark, #cc6600);
  }
  
  .btn-danger {
    background: var(--red, #ff4444);
    color: white;
  }
  
  .btn-danger:hover:not(:disabled) {
    background: var(--red-dark, #cc0000);
  }
  
  .danger-zone {
    border-bottom: none;
  }
  
  .danger-box {
    border: 1px solid var(--red, #ff4444);
    background: rgba(255, 0, 0, 0.05);
    border-radius: 6px;
    padding: 1.5rem;
  }
  
  .danger-box h3 {
    color: var(--red, #ff4444);
    margin-bottom: 0.5rem;
    font-size: 1.1rem;
  }
  
  .danger-box p {
    color: var(--text-secondary);
    font-size: 0.9rem;
    margin-bottom: 1rem;
  }
  
  .error-box {
    padding: 0.75rem;
    background: rgba(255, 0, 0, 0.1);
    border: 1px solid var(--red, #ff4444);
    border-radius: 6px;
    color: var(--red, #ff4444);
    font-size: 0.9rem;
    margin-bottom: 1rem;
  }
  
  .success-box {
    padding: 0.75rem;
    background: rgba(0, 255, 0, 0.1);
    border: 1px solid var(--green, #28a745);
    border-radius: 6px;
    color: var(--green, #28a745);
    font-size: 0.9rem;
    margin-bottom: 1rem;
  }
  
  .loading, .error {
    padding: 2rem;
    text-align: center;
    color: var(--text-secondary);
  }
  
  .error {
    color: var(--red, #ff4444);
  }
  textarea,
  select {
    width: 100%;
    box-sizing: border-box;
    padding: 0.6rem 0.75rem;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 0.9rem;
    font-family: inherit;
  }

  .general-form .form-group:first-child {
    margin-top: 0;
  }

  .hint {
    margin: 0.4rem 0 0;
    color: var(--text-secondary);
    font-size: 0.8rem;
  }

  .general-form > .btn {
    margin-top: 1.25rem;
  }

  .btn-primary {
    background: var(--accent);
    color: white;
  }

  .btn-secondary {
    background: none;
    color: var(--text-primary);
    border: 1px solid var(--border);
  }

  .section-desc .badge {
    margin-right: 0.5rem;
  }

  .admin-only-note {
    margin-top: 1.5rem;
    margin-bottom: 0;
  }

  .rename-warning {
    color: var(--red, #ff4444);
    font-weight: 500;
  }

  .modal-actions {
    display: flex;
    gap: 0.75rem;
    margin-top: 1rem;
  }
</style>
