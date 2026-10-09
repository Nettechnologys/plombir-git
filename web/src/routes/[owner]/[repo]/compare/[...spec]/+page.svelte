<script lang="ts">
  // `/{owner}/{repo}/compare/{base}...{head}` — what a pull request from `head`
  // into `base` would carry, before anyone opens it (card_87f9b1c97489). `head`
  // is a branch of this repository or `{fork owner}:{branch}` of one of its
  // forks; a spec with no `...` names only the head and compares it against the
  // default branch.
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import DiffFileList from '$lib/components/DiffFileList.svelte';
  import { pulls, repos } from '$lib/api/client.svelte';
  import type { CompareResult } from '$lib/api/pulls';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { formatHeadRef, newPullHref, parseCompareSpec, type CompareSpec } from '$lib/pullHeadRef';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let rawSpec = $derived($page.params.spec ?? '');
  let spec = $derived(parseCompareSpec(rawSpec));

  let base = $state('');
  let result = $state<CompareResult | null>(null);
  let loading = $state(true);
  let error = $state('');
  const compareRequests = new LatestRepositoryResourceRequestFence<string>();

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedSpec = rawSpec;
    const parsed = spec;
    result = null;
    error = '';
    base = parsed?.base ?? '';
    if (!parsed) {
      loading = false;
      error = t('pulls.compare.invalid_spec');
      return;
    }
    void loadCompare(expectedOwner, expectedRepo, expectedSpec, parsed);
  });

  function owns(claim: ReturnType<typeof compareRequests.begin>) {
    return compareRequests.owns(claim, owner, repo, rawSpec);
  }

  async function loadCompare(
    expectedOwner: string,
    expectedRepo: string,
    expectedSpec: string,
    parsed: CompareSpec,
  ) {
    const claim = compareRequests.begin(expectedOwner, expectedRepo, expectedSpec);
    loading = true;
    try {
      let baseBranch = parsed.base;
      if (!baseBranch) {
        const info = await repos.get(expectedOwner, expectedRepo);
        if (!owns(claim)) return;
        baseBranch = info?.default_branch || 'main';
        base = baseBranch;
      }
      const next = await pulls.compare(
        expectedOwner,
        expectedRepo,
        baseBranch,
        formatHeadRef(parsed.head, expectedOwner),
      );
      if (!owns(claim)) return;
      result = next;
    } catch (e: any) {
      if (owns(claim)) error = e?.message || t('pulls.compare.unavailable');
    } finally {
      if (owns(claim)) loading = false;
    }
  }

  function headLabel(head: CompareSpec['head']) {
    return head.owner && head.owner !== owner ? `${head.owner}:${head.branch}` : head.branch;
  }

  function commitHref(sha: string) {
    const commitOwner = spec?.head.owner || owner;
    return `/${encodeURIComponent(commitOwner)}/${encodeURIComponent(repo)}/commits/${encodeURIComponent(sha)}`;
  }

  function firstLine(message: string) {
    return (message || '').split('\n', 1)[0];
  }
</script>

<svelte:head>
  <title>{t('pulls.compare.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="pulls" />

  <div class="compare-header">
    <div>
      <h1>{t('pulls.compare.title')}</h1>
      {#if spec}
        <p class="compare-refs">
          <span class="ref-label">{t('pulls.compare.base')}</span>
          <code class="branch-label base-ref">{base || '…'}</code>
          <span class="arrow">←</span>
          <span class="ref-label">{t('pulls.compare.head')}</span>
          <code class="branch-label head-ref">{headLabel(spec.head)}</code>
        </p>
      {/if}
    </div>
    {#if spec && base && result && result.commits.length > 0}
      <a class="btn-primary create-pr-link" href={newPullHref(owner, repo, base, spec.head)}>
        {t('pulls.compare.create_pull')}
      </a>
    {/if}
  </div>

  {#if error}
    <div class="error-banner" role="alert">{error}</div>
  {:else if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if result}
    {#if result.commits.length === 0}
      <div class="empty compare-empty">
        <p>{t('pulls.compare.nothing_to_compare', { base, head: spec ? headLabel(spec.head) : '' })}</p>
      </div>
    {:else}
      <section class="compare-commits gh-card">
        <h2>{t('pulls.compare.commits', { count: result.total_commits ?? result.commits.length })}</h2>
        <ul class="commit-list">
          {#each result.commits as commit (commit.sha)}
            <li class="commit-row">
              <a class="commit-message" href={commitHref(commit.sha)}>{firstLine(commit.message)}</a>
              <span class="commit-meta">
                {commit.author} · {formatDate(commit.date)} · <code>{commit.sha.slice(0, 7)}</code>
              </span>
            </li>
          {/each}
        </ul>
      </section>
      {#if result.files_changed.length > 0}
        <DiffFileList files={result.files_changed} stats={result.stats} />
      {:else}
        <p class="text-secondary">{t('repo.browser.no_diff')}</p>
      {/if}
    {/if}
  {/if}
</div>

<style>
  .compare-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    flex-wrap: wrap;
    margin-bottom: 16px;
  }
  h1 { font-size: 22px; margin: 0 0 8px; }
  h2 { font-size: 15px; margin: 0 0 10px; }
  .compare-refs {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    margin: 0;
    font-size: 13px;
    color: var(--text-secondary);
  }
  .arrow { color: var(--text-muted); }
  .branch-label {
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--accent);
    overflow-wrap: anywhere;
  }
  .btn-primary {
    display: inline-block;
    padding: 6px 16px;
    background: var(--accent);
    color: #fff;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    text-decoration: none;
  }
  .btn-primary:hover { background: var(--accent-hover); text-decoration: none; }
  .compare-commits { padding: 12px 16px; margin-bottom: 16px; }
  .commit-list { list-style: none; margin: 0; padding: 0; }
  .commit-row {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 0;
    border-bottom: 1px solid var(--border-light);
  }
  .commit-row:last-child { border-bottom: none; }
  .commit-message { font-weight: 600; color: var(--text-primary); overflow-wrap: anywhere; }
  .commit-meta { font-size: 12px; color: var(--text-muted); }
  .empty { text-align: center; padding: 48px; color: var(--text-secondary); }
</style>
