<script lang="ts">
  import { repos } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';

  const t = createT();
  
  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let branch = $state('main');
  
  let commits = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  const logRequests = new LatestRepositoryResourceRequestFence<string>();

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedBranch = branch;
    if (!expectedOwner || !expectedRepo) return;
    void loadCommits(expectedOwner, expectedRepo, expectedBranch);
  });

  async function loadCommits(expectedOwner: string, expectedRepo: string, expectedBranch: string) {
    const claim = logRequests.begin(expectedOwner, expectedRepo, expectedBranch);
    loading = true;
    error = '';
    commits = [];
    try {
      const result = await repos.log(expectedOwner, expectedRepo, expectedBranch);
      if (!logRequests.owns(claim, owner, repo, branch)) return;
      if (result?.commits && Array.isArray(result.commits)) {
        commits = result.commits;
      } else {
        error = 'Invalid response format from server';
      }
    } catch (e: any) {
      if (logRequests.owns(claim, owner, repo, branch)) {
        error = e.message || 'Failed to load commits';
      }
    } finally {
      if (logRequests.owns(claim, owner, repo, branch)) loading = false;
    }
  }
</script>

<svelte:head>
  <title>{owner}/{repo} · {t('repo.tabs.commits')} · ForgeKeep</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="commits" />

  <h1>{t('repo.tabs.commits')}</h1>
  
  {#if error}
    <div class="error-banner">
      <p>{error}</p>
      <button onclick={() => window.location.reload()}>Retry</button>
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
      {#each commits as commit}
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
  {/if}
</div>

<style>
  h1 {
    font-size: 24px;
    margin-bottom: 16px;
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
