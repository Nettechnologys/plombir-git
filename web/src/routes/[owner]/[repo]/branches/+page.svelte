<script lang="ts">
  // Branches of a repository: list, create, delete (card_2060696224ff).
  //
  // Write controls are offered to a viewer whose `viewer_permission` is
  // `write` or `admin` (card_3625a7b89abb) — they used to go to anyone signed
  // in — and the server's answer (403/409 with its message) is shown inline.
  // The default branch and branches a protection rule covers are refused by the
  // server for every caller, so their Delete is disabled up front.
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import { branchProtections, repos } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { viewerPermission } from '$lib/viewerPermission.svelte';
  import { protectedBy } from '$lib/refPattern';
  import { compareHref } from '$lib/pullHeadRef';
  import { createT } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let branches = $state<Array<{ name: string; is_default: boolean }>>([]);
  let protections = $state<string[]>([]);
  let loading = $state(true);
  let error = $state('');
  let notice = $state('');

  let showCreate = $state(false);
  let newName = $state('');
  let newFrom = $state('');
  let creating = $state(false);
  let createError = $state('');

  let pendingDelete = $state<string | null>(null);
  let deleting = $state(false);
  let deleteError = $state('');

  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  const permission = viewerPermission(() => owner, () => repo);
  let canWrite = $derived(permission.canWrite);
  let defaultBranch = $derived(branches.find((branch) => branch.is_default)?.name ?? '');

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    branches = [];
    protections = [];
    error = '';
    notice = '';
    showCreate = false;
    newName = '';
    newFrom = '';
    creating = false;
    createError = '';
    pendingDelete = null;
    deleting = false;
    deleteError = '';
    void load(expectedOwner, expectedRepo, routeGeneration);
  });

  function isCurrent(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  async function load(expectedOwner = owner, expectedRepo = repo, expectedRoute = routeGeneration) {
    const claim = listRequests.begin(expectedOwner, expectedRepo);
    const current = () => listRequests.owns(claim, owner, repo) && isCurrent(expectedOwner, expectedRepo, expectedRoute);
    loading = true;
    try {
      const [nextBranches, rules] = await Promise.all([
        repos.branches(expectedOwner, expectedRepo),
        // Only a marker: a repository whose rules cannot be read still lists
        // its branches, and the server still refuses what it protects.
        Promise.resolve(branchProtections.list(expectedOwner, expectedRepo)).catch(() => []),
      ]);
      if (!current()) return;
      branches = nextBranches ?? [];
      protections = (rules ?? []).map((rule: { branch_name: string }) => rule.branch_name);
      error = '';
    } catch (e: any) {
      if (current()) error = e?.message || t('repo.branches.load_failed');
    } finally {
      if (current()) loading = false;
    }
  }

  function openCreate() {
    showCreate = !showCreate;
    createError = '';
    if (showCreate && !newFrom) newFrom = defaultBranch;
  }

  async function handleCreate(event: Event) {
    event.preventDefault();
    if (creating) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const name = newName.trim();
    const from = newFrom.trim();
    if (!name) return;
    creating = true;
    createError = '';
    try {
      await repos.createBranch(expectedOwner, expectedRepo, { name, from: from || undefined });
      if (!isCurrent(expectedOwner, expectedRepo, expectedRoute)) return;
      notice = t('repo.branches.created', { name });
      showCreate = false;
      newName = '';
      newFrom = '';
      await load(expectedOwner, expectedRepo, expectedRoute);
    } catch (e: any) {
      if (isCurrent(expectedOwner, expectedRepo, expectedRoute)) {
        createError = e?.message || t('repo.branches.create_failed');
      }
    } finally {
      if (isCurrent(expectedOwner, expectedRepo, expectedRoute)) creating = false;
    }
  }

  function askDelete(name: string) {
    pendingDelete = name;
    deleteError = '';
  }

  function closeDelete() {
    if (deleting) return;
    pendingDelete = null;
    deleteError = '';
  }

  async function confirmDelete() {
    const name = pendingDelete;
    if (!name || deleting) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    deleting = true;
    deleteError = '';
    try {
      await repos.deleteBranch(expectedOwner, expectedRepo, name);
      if (!isCurrent(expectedOwner, expectedRepo, expectedRoute)) return;
      pendingDelete = null;
      notice = t('repo.branches.deleted', { name });
      await load(expectedOwner, expectedRepo, expectedRoute);
    } catch (e: any) {
      if (isCurrent(expectedOwner, expectedRepo, expectedRoute)) {
        deleteError = e?.message || t('repo.branches.delete_failed');
      }
    } finally {
      if (isCurrent(expectedOwner, expectedRepo, expectedRoute)) deleting = false;
    }
  }

  function browseHref(name: string) {
    return `/${owner}/${repo}?${new URLSearchParams({ ref: name }).toString()}`;
  }
</script>

<svelte:head>
  <title>{t('repo.branches.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="code" />

  <div class="refs-toolbar">
    <h1>{t('repo.branches.title')}</h1>
    <div class="refs-links">
      <a href={`/${owner}/${repo}/tags`}>{t('repo.tags.title')}</a>
      {#if canWrite}
        <button class="btn-primary new-branch-btn" onclick={openCreate}>{t('repo.branches.new')}</button>
      {/if}
    </div>
  </div>

  {#if notice}
    <div class="notice" role="status">{notice}</div>
  {/if}

  {#if showCreate && canWrite}
    <form class="create-branch gh-card" onsubmit={handleCreate}>
      <label>
        {t('repo.branches.name')}
        <input type="text" bind:value={newName} required disabled={creating} placeholder={t('repo.branches.name_placeholder')} />
      </label>
      <label>
        {t('repo.branches.from')}
        <input type="text" list="branch-from-options" bind:value={newFrom} disabled={creating} placeholder={defaultBranch} />
        <datalist id="branch-from-options">
          {#each branches as branch (branch.name)}
            <option value={branch.name}></option>
          {/each}
        </datalist>
        <span class="hint">{t('repo.branches.from_hint')}</span>
      </label>
      {#if createError}
        <div class="error-banner create-error" role="alert">{createError}</div>
      {/if}
      <div class="form-actions">
        <button type="submit" class="btn-primary" disabled={creating || !newName.trim()}>
          {creating ? t('repo.branches.creating') : t('repo.branches.create')}
        </button>
        <button type="button" class="btn-secondary" onclick={() => (showCreate = false)} disabled={creating}>
          {t('common.cancel')}
        </button>
      </div>
    </form>
  {/if}

  {#if error}
    <div class="error-banner" role="alert">{error}</div>
  {:else if loading && branches.length === 0}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if branches.length === 0}
    <div class="empty"><p>{t('repo.branches.empty')}</p></div>
  {:else}
    <ul class="ref-list gh-list">
      {#each branches as branch (branch.name)}
        {@const isProtected = protectedBy(branch.name, protections)}
        <li class="ref-row gh-list-item" data-branch={branch.name}>
          <div class="ref-name">
            <a href={browseHref(branch.name)}><code>{branch.name}</code></a>
            {#if branch.is_default}<span class="badge default-badge">{t('repo.branches.default')}</span>{/if}
            {#if isProtected}<span class="badge protected-badge">{t('repo.branches.protected')}</span>{/if}
          </div>
          <div class="ref-actions">
            {#if !branch.is_default && defaultBranch}
              <a class="btn-secondary btn-sm" href={compareHref(owner, repo, defaultBranch, { owner: null, branch: branch.name })}>
                {t('repo.branches.compare')}
              </a>
            {/if}
            {#if canWrite && !branch.is_default}
              <button
                class="btn-danger btn-sm delete-branch"
                onclick={() => askDelete(branch.name)}
                disabled={isProtected}
                title={isProtected ? t('repo.branches.protected_hint') : undefined}
              >
                {t('repo.branches.delete')}
              </button>
            {/if}
          </div>
        </li>
      {/each}
    </ul>
  {/if}
</div>

{#if pendingDelete}
  <Modal onclose={closeDelete} labelledby="delete-branch-title">
    <h2 id="delete-branch-title">{t('repo.branches.delete_title')}</h2>
    <p>{t('repo.branches.delete_confirm', { name: pendingDelete })}</p>
    {#if deleteError}
      <div class="error-banner delete-error" role="alert">{deleteError}</div>
    {/if}
    <div class="form-actions">
      <button class="btn-danger confirm-delete" onclick={confirmDelete} disabled={deleting}>
        {deleting ? t('repo.branches.deleting') : t('repo.branches.delete')}
      </button>
      <button class="btn-secondary" onclick={closeDelete} disabled={deleting} data-autofocus>{t('common.cancel')}</button>
    </div>
  </Modal>
{/if}

<style>
  .refs-toolbar { display: flex; align-items: center; justify-content: space-between; gap: 12px; flex-wrap: wrap; margin-bottom: 16px; }
  h1 { font-size: 22px; margin: 0; }
  h2 { font-size: 18px; margin: 0 0 12px; }
  .refs-links { display: flex; align-items: center; gap: 12px; }
  .notice { padding: 8px 12px; margin-bottom: 12px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--bg-secondary); }
  .create-branch { display: flex; flex-direction: column; gap: 12px; padding: 16px; margin-bottom: 16px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; font-weight: 600; }
  .hint { font-weight: 400; color: var(--text-muted); font-size: 12px; }
  .form-actions { display: flex; gap: 8px; margin-top: 8px; }
  .ref-list { list-style: none; margin: 0; padding: 0; }
  .ref-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 16px; border-bottom: 1px solid var(--border-light); flex-wrap: wrap; }
  .ref-row:last-child { border-bottom: none; }
  .ref-name { display: flex; align-items: center; gap: 8px; min-width: 0; overflow-wrap: anywhere; }
  .ref-actions { display: flex; align-items: center; gap: 8px; }
  .badge { padding: 1px 6px; border: 1px solid var(--border); border-radius: 10px; font-size: 11px; color: var(--text-secondary); }
  .btn-primary { padding: 6px 16px; background: var(--accent); color: #fff; border: 1px solid var(--accent); border-radius: var(--radius); font-size: 14px; font-weight: 600; cursor: pointer; }
  .btn-primary:disabled { opacity: 0.5; }
  .btn-secondary { padding: 4px 12px; background: none; color: var(--text-primary); border: 1px solid var(--border); border-radius: var(--radius); font-size: 13px; cursor: pointer; text-decoration: none; }
  .btn-danger { padding: 4px 12px; background: none; color: var(--red); border: 1px solid var(--red); border-radius: var(--radius); font-size: 13px; cursor: pointer; }
  .btn-danger:disabled { opacity: 0.5; cursor: not-allowed; }
  .empty { text-align: center; padding: 48px; color: var(--text-secondary); }
</style>
