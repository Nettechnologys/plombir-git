<script lang="ts">
  import { repos } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import Dropdown from '$lib/components/Dropdown.svelte';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';

  type Commit = { sha: string; message: string; author: string; date: string };
  type Branch = { name: string; is_default: boolean };

  const t = createT();
  const PAGE_SIZE = 50;

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  // The URL is the only source of the ref. Without `?ref=` the request names
  // no ref at all and the server walks HEAD — the repository's own default
  // branch. This page used to send a hard-coded `main`, so a repository whose
  // default is `master` answered "ref not found" and a new empty repository
  // an error instead of "No commits yet" (card_2e320f5287d7).
  let ref = $derived($page.url.searchParams.get('ref') || '');

  let commits = $state<Commit[]>([]);
  let branches = $state<Branch[]>([]);
  let loading = $state(true);
  let loadingMore = $state(false);
  let hasMore = $state(false);
  let error = $state('');
  let loadMoreError = $state('');
  const logRequests = new LatestRepositoryResourceRequestFence<string>();
  const branchRequests = new LatestRepositoryResourceRequestFence<string>();

  let currentRefLabel = $derived(ref || branches.find((b) => b.is_default)?.name || 'HEAD');

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRef = ref;
    if (!expectedOwner || !expectedRepo) return;
    void loadCommits(expectedOwner, expectedRepo, expectedRef);
  });

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    if (!expectedOwner || !expectedRepo) return;
    void loadBranches(expectedOwner, expectedRepo);
  });

  async function loadBranches(expectedOwner: string, expectedRepo: string) {
    const claim = branchRequests.begin(expectedOwner, expectedRepo, 'branches');
    branches = [];
    try {
      const result = await repos.branches(expectedOwner, expectedRepo);
      if (branchRequests.owns(claim, owner, repo, 'branches')) branches = result || [];
    } catch {
      // The picker is a convenience: without the list the page still shows
      // the history of the ref in the URL, and the label still names it.
    }
  }

  async function loadCommits(expectedOwner: string, expectedRepo: string, expectedRef: string) {
    const claim = logRequests.begin(expectedOwner, expectedRepo, expectedRef);
    loading = true;
    loadingMore = false;
    hasMore = false;
    error = '';
    loadMoreError = '';
    commits = [];
    try {
      const result = await repos.log(expectedOwner, expectedRepo, expectedRef || undefined, undefined, PAGE_SIZE);
      if (!logRequests.owns(claim, owner, repo, ref)) return;
      if (result?.commits && Array.isArray(result.commits)) {
        commits = result.commits;
        hasMore = result.commits.length === PAGE_SIZE;
      } else {
        error = 'Invalid response format from server';
      }
    } catch (e: any) {
      if (logRequests.owns(claim, owner, repo, ref)) {
        error = e.message || 'Failed to load commits';
      }
    } finally {
      if (logRequests.owns(claim, owner, repo, ref)) loading = false;
    }
  }

  // The next page is the same walk of the same ref, `skip`ped past what is
  // shown: a cursor of "the last commit's history" would drop the commits of a
  // merged side branch the walk had not reached yet (card_2e320f5287d7).
  async function loadMore() {
    const last = commits[commits.length - 1];
    if (!last || loadingMore || !hasMore) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRef = ref;
    const claim = logRequests.begin(expectedOwner, expectedRepo, expectedRef);
    loadingMore = true;
    loadMoreError = '';
    try {
      const result = await repos.log(
        expectedOwner,
        expectedRepo,
        expectedRef || undefined,
        undefined,
        PAGE_SIZE,
        commits.length
      );
      if (!logRequests.owns(claim, owner, repo, ref)) return;
      const nextPage = Array.isArray(result?.commits) ? result.commits : [];
      // A history that moved between the two pages can repeat a commit at
      // the seam; the list stays a list of distinct commits.
      const seen = new Set(commits.map((commit) => commit.sha));
      const fresh = nextPage.filter((commit) => !seen.has(commit.sha));
      commits = [...commits, ...fresh];
      hasMore = nextPage.length === PAGE_SIZE;
    } catch (e: any) {
      if (logRequests.owns(claim, owner, repo, ref)) {
        loadMoreError = e.message || 'Failed to load commits';
      }
    } finally {
      if (logRequests.owns(claim, owner, repo, ref)) loadingMore = false;
    }
  }

  function commitsHref(nextRef: string): string {
    const query = nextRef ? `?${new URLSearchParams({ ref: nextRef }).toString()}` : '';
    return `/${owner}/${repo}/commits${query}`;
  }

  function selectBranch(name: string, close: () => void) {
    close();
    if (name === ref) return;
    void goto(commitsHref(name), { replaceState: true, keepFocus: true, noScroll: true });
  }
</script>

<svelte:head>
  <title>{owner}/{repo} · {t('repo.tabs.commits')} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="commits" />

  <div class="commits-header">
    <h1>{t('repo.tabs.commits')}</h1>
    <Dropdown ariaLabel={t('repo.select_branch')} triggerClass="btn-outline" placement="right">
      {#snippet trigger()}
        🌿 {currentRefLabel} <span aria-hidden="true">▾</span>
      {/snippet}
      {#snippet menu(close)}
        {#each branches as b (b.name)}
          <button
            class="dropdown-item"
            class:active={b.name === ref || (!ref && b.is_default)}
            onclick={() => selectBranch(b.name, close)}
            role="menuitem"
          >
            {b.name} {b.is_default ? t('repo.browser.default_branch') : ''}
          </button>
        {/each}
      {/snippet}
    </Dropdown>
  </div>
  
  {#if error}
    <div class="error-banner">
      <p>{error}</p>
      <button onclick={() => loadCommits(owner, repo, ref)}>{t('common.retry')}</button>
    </div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if !error && commits.length === 0}
    <!-- Same guard as the repository home: a load that failed left this list
         empty for a reason that is not "no commits yet" — a repository whose
         HEAD lost its branch answers 409 and has a full history
         (card_9e11f76dddd1). -->
    <div class="empty">
      <p>{t('repo.commits_empty', 'No commits yet.')}</p>
    </div>
  {:else}
    <div class="commits-list">
      {#each commits as commit (commit.sha)}
        <div class="commit-item">
          <div class="commit-icon">📝</div>
          <div class="commit-body">
            <div class="commit-message">
              <a href={`/${owner}/${repo}/commits/${commit.sha}`}>{commit.message}</a>
            </div>
            <div class="commit-meta">
              <span class="commit-author">{commit.author}</span>
              <span class="commit-date">{formatDate(commit.date)}</span>
              <span class="commit-sha">{commit.sha.substring(0, 7)}</span>
            </div>
          </div>
        </div>
      {/each}
    </div>
    {#if loadMoreError}
      <div class="error-banner" role="alert">
        <p>{loadMoreError}</p>
      </div>
    {/if}
    {#if hasMore}
      <div class="load-more">
        <button type="button" class="btn-outline" onclick={loadMore} disabled={loadingMore} aria-busy={loadingMore}>
          {loadingMore ? t('common.loading') : t('common.load_more')}
        </button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .commits-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 16px;
  }

  h1 {
    font-size: 24px;
    margin: 0;
  }

  .load-more {
    display: flex;
    justify-content: center;
    margin-top: 16px;
  }

  .error-banner {
    background: var(--error-bg, #fee);
    border: 1px solid var(--error-border, #fcc);
    border-radius: 6px;
    padding: 12px 16px;
    margin-bottom: 16px;
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .error-banner button {
    padding: 6px 12px;
    background: var(--primary);
    color: white;
    border: none;
    border-radius: 4px;
    cursor: pointer;
  }

  .empty {
    text-align: center;
    padding: 60px 24px;
    color: var(--text-secondary);
  }

  .commits-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .commit-item {
    display: flex;
    gap: 12px;
    padding: 12px 16px;
    border: 1px solid var(--border);
    border-radius: 6px;
    transition: border-color 0.15s;
  }

  .commit-item:hover {
    border-color: var(--primary);
  }

  .commit-icon {
    font-size: 24px;
    flex-shrink: 0;
  }

  .commit-body {
    flex: 1;
    min-width: 0;
  }

  .commit-message {
    font-weight: 500;
    margin-bottom: 4px;
  }

  .commit-message a {
    color: inherit;
    text-decoration: none;
  }

  .commit-message a:hover {
    color: var(--primary);
  }

  .commit-meta {
    display: flex;
    gap: 12px;
    font-size: 13px;
    color: var(--text-secondary);
  }

  .commit-sha {
    font-family: monospace;
    background: var(--bg-secondary, #f5f5f5);
    padding: 2px 6px;
    border-radius: 3px;
    font-size: 12px;
  }
</style>
