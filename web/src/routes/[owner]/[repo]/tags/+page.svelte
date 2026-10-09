<script lang="ts">
  // Tags of a repository, with delete (card_2060696224ff). Same rules as the
  // branches page: write controls for a `write`/`admin` viewer, the server's refusal
  // shown inline, and a tag a protection pattern covers disabled up front.
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import Modal from '$lib/components/Modal.svelte';
  import { repos, tagProtections } from '$lib/api/client.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { viewerPermission } from '$lib/viewerPermission.svelte';
  import { protectedBy } from '$lib/refPattern';
  import { createT } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let tags = $state<Array<{ name: string }>>([]);
  let protections = $state<string[]>([]);
  let loading = $state(true);
  let error = $state('');
  let notice = $state('');

  let pendingDelete = $state<string | null>(null);
  let deleting = $state(false);
  let deleteError = $state('');

  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  const permission = viewerPermission(() => owner, () => repo);
  let canWrite = $derived(permission.canWrite);

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    tags = [];
    protections = [];
    error = '';
    notice = '';
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
      const [nextTags, rules] = await Promise.all([
        repos.tags(expectedOwner, expectedRepo),
        Promise.resolve(tagProtections.list(expectedOwner, expectedRepo)).catch(() => []),
      ]);
      if (!current()) return;
      tags = nextTags ?? [];
      protections = (rules ?? []).map((rule: { pattern: string }) => rule.pattern);
      error = '';
    } catch (e: any) {
      if (current()) error = e?.message || t('repo.tags.load_failed');
    } finally {
      if (current()) loading = false;
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
      await repos.deleteTag(expectedOwner, expectedRepo, name);
      if (!isCurrent(expectedOwner, expectedRepo, expectedRoute)) return;
      pendingDelete = null;
      notice = t('repo.tags.deleted', { name });
      await load(expectedOwner, expectedRepo, expectedRoute);
    } catch (e: any) {
      if (isCurrent(expectedOwner, expectedRepo, expectedRoute)) {
        deleteError = e?.message || t('repo.tags.delete_failed');
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
  <title>{t('repo.tags.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="code" />

  <div class="refs-toolbar">
    <h1>{t('repo.tags.title')}</h1>
    <div class="refs-links">
      <a href={`/${owner}/${repo}/branches`}>{t('repo.branches.title')}</a>
      <a href={`/${owner}/${repo}/releases`}>{t('repo.tabs.releases')}</a>
    </div>
  </div>

  {#if notice}
    <div class="notice" role="status">{notice}</div>
  {/if}

  {#if error}
    <div class="error-banner" role="alert">{error}</div>
  {:else if loading && tags.length === 0}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if tags.length === 0}
    <div class="empty"><p>{t('repo.tags.empty')}</p></div>
  {:else}
    <ul class="ref-list gh-list">
      {#each tags as tag (tag.name)}
        {@const isProtected = protectedBy(tag.name, protections)}
        <li class="ref-row gh-list-item" data-tag={tag.name}>
          <div class="ref-name">
            <a href={browseHref(tag.name)}><code>{tag.name}</code></a>
            {#if isProtected}<span class="badge protected-badge">{t('repo.tags.protected')}</span>{/if}
          </div>
          {#if canWrite}
            <button
              class="btn-danger btn-sm delete-tag"
              onclick={() => askDelete(tag.name)}
              disabled={isProtected}
              title={isProtected ? t('repo.tags.protected_hint') : undefined}
            >
              {t('repo.tags.delete')}
            </button>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

{#if pendingDelete}
  <Modal onclose={closeDelete} labelledby="delete-tag-title">
    <h2 id="delete-tag-title">{t('repo.tags.delete_title')}</h2>
    <p>{t('repo.tags.delete_confirm', { name: pendingDelete })}</p>
    {#if deleteError}
      <div class="error-banner delete-error" role="alert">{deleteError}</div>
    {/if}
    <div class="form-actions">
      <button class="btn-danger confirm-delete" onclick={confirmDelete} disabled={deleting}>
        {deleting ? t('repo.tags.deleting') : t('repo.tags.delete')}
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
  .form-actions { display: flex; gap: 8px; margin-top: 8px; }
  .ref-list { list-style: none; margin: 0; padding: 0; }
  .ref-row { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 10px 16px; border-bottom: 1px solid var(--border-light); flex-wrap: wrap; }
  .ref-row:last-child { border-bottom: none; }
  .ref-name { display: flex; align-items: center; gap: 8px; min-width: 0; overflow-wrap: anywhere; }
  .badge { padding: 1px 6px; border: 1px solid var(--border); border-radius: 10px; font-size: 11px; color: var(--text-secondary); }
  .btn-secondary { padding: 4px 12px; background: none; color: var(--text-primary); border: 1px solid var(--border); border-radius: var(--radius); font-size: 13px; cursor: pointer; }
  .btn-danger { padding: 4px 12px; background: none; color: var(--red); border: 1px solid var(--red); border-radius: var(--radius); font-size: 13px; cursor: pointer; }
  .btn-danger:disabled { opacity: 0.5; cursor: not-allowed; }
  .empty { text-align: center; padding: 48px; color: var(--text-secondary); }
</style>
