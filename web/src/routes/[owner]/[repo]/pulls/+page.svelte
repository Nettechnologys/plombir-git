<script lang="ts">
  import { page } from '$app/stores';
  import { untrack } from 'svelte';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import BotBadge from '$lib/components/BotBadge.svelte';
  import { pulls, repos } from '$lib/api/client.svelte';
  import type { RepositoryFork } from '$lib/api/repos';
  import { getUser } from '$lib/stores/auth.svelte';
  import { compareHref, parseHeadRef } from '$lib/pullHeadRef';
  import {
    LatestRepositoryRequestFence,
    LatestRepositoryResourceRequestFence,
  } from '$lib/asyncStateOwnership';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let prList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let filterState = $state('open');
  let showCreate = $state(false);
  let newTitle = $state('');
  let newBody = $state('');
  let newHead = $state('');
  // Filled from the branch list's default marker once it loads (or from the
  // compare page's `?base=`): a literal `main` sat in the select on a
  // repository without such a branch and was sent as the base.
  let newBase = $state('');
  let newDraft = $state(false);
  let branches = $state<any[]>([]);
  let branchesLoading = $state(false);
  let branchesError = $state('');
  let templateLoaded = $state(false);
  let creating = $state(false);
  const pullListRequests = new LatestRepositoryResourceRequestFence<string>();
  const branchRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  // The head of a pull request may live in a fork (card_87f9b1c97489). The
  // server reads `head: "<fork owner>:<branch>"`, looks the fork up as
  // `<fork owner>/<this repo's name>` and accepts it only when it is a direct
  // fork of this repository — exactly what `GET /forks` lists, so that list,
  // narrowed to forks that kept the name, is the set of heads worth offering.
  // `''` names this repository itself.
  const FORK_PAGE_SIZE = 100;
  const MAX_FORK_PAGES = 10;
  let headRepoOwner = $state('');
  let forks = $state<RepositoryFork[]>([]);
  let forksError = $state('');
  // A head owner the URL asked for that the fork list did not (yet) name.
  let requestedHeadOwner = $state('');
  let headBranches = $state<any[]>([]);
  let headBranchesLoading = $state(false);
  let headBranchesError = $state('');
  const forkRequests = new LatestRepositoryRequestFence();
  const headBranchRequests = new LatestRepositoryResourceRequestFence<string>();

  let headIsFork = $derived(headRepoOwner !== '' && headRepoOwner !== owner);
  let headBranchOptions = $derived(headIsFork ? headBranches : branches);
  let headBranchesBusy = $derived(headIsFork ? headBranchesLoading : branchesLoading);
  let headRepoOptions = $derived.by(() => {
    const me = getUser()?.username ?? '';
    const owners = forks.map((fork) => fork.owner_name);
    if (requestedHeadOwner && requestedHeadOwner !== owner && !owners.includes(requestedHeadOwner)) {
      owners.push(requestedHeadOwner);
    }
    // The caller's own forks first: they are the ones a person opening a pull
    // request from the UI almost always means.
    return [...new Set(owners)]
      .filter((name) => name && name !== owner)
      .sort((a, b) => Number(b === me) - Number(a === me))
      .map((name) => ({ owner: name, mine: name === me }));
  });
  let compareLink = $derived(
    newHead && newBase
      ? compareHref(owner, repo, newBase, { owner: headIsFork ? headRepoOwner : null, branch: newHead })
      : '',
  );

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    filterState = 'open';
    prList = [];
    branches = [];
    branchesLoading = false;
    branchesError = '';
    showCreate = false;
    newTitle = '';
    newBody = '';
    newHead = '';
    newBase = '';
    newDraft = false;
    templateLoaded = false;
    creating = false;
    error = '';
    headRepoOwner = '';
    forks = [];
    forksError = '';
    requestedHeadOwner = '';
    headBranches = [];
    headBranchesLoading = false;
    headBranchesError = '';
    headBranchRequests.begin(expectedOwner, expectedRepo, '');
    void loadPRs(expectedOwner, expectedRepo, 'open', routeGeneration);
    void loadBranches(expectedOwner, expectedRepo, routeGeneration);
    void loadForks(expectedOwner, expectedRepo, routeGeneration);
    // `?new=1&base=…&head=…` — the compare page's "Create pull request" lands
    // here with the form to open. Read once per route, not tracked: the query
    // is an instruction, not state this page mirrors.
    untrack(() => applyCreatePrefill(expectedOwner, expectedRepo, routeGeneration));
  });

  function applyCreatePrefill(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    const query = $page.url.searchParams;
    if (query.get('new') !== '1') return;
    const base = query.get('base');
    const head = parseHeadRef(query.get('head') ?? '');
    if (base) newBase = base;
    if (head.owner && head.owner !== expectedOwner) {
      requestedHeadOwner = head.owner;
      headRepoOwner = head.owner;
      void loadHeadBranches(head.owner, expectedOwner, expectedRepo, expectedRoute);
    }
    newHead = head.branch;
    void openCreate();
  }

  async function loadForks(
    expectedOwner = owner,
    expectedRepo = repo,
    expectedRoute = routeGeneration,
  ) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = forkRequests.begin(expectedOwner, expectedRepo);
    const current = () =>
      forkRequests.owns(claim, owner, repo) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute);
    forksError = '';
    try {
      const found: RepositoryFork[] = [];
      for (let pageNumber = 1; pageNumber <= MAX_FORK_PAGES; pageNumber += 1) {
        const response = await repos.forks(expectedOwner, expectedRepo, pageNumber, FORK_PAGE_SIZE);
        if (!current()) return;
        const rows = response?.data ?? [];
        found.push(...rows);
        if (rows.length === 0 || pageNumber >= (response?.pagination?.total_pages ?? 1)) break;
      }
      forks = found.filter((fork) => fork.name === expectedRepo && !fork.deleted_at);
    } catch (e: any) {
      if (current()) {
        forks = [];
        forksError = e?.message || t('pulls.create_form.forks_unavailable');
      }
    }
  }

  async function loadHeadBranches(
    forkOwner: string,
    expectedOwner = owner,
    expectedRepo = repo,
    expectedRoute = routeGeneration,
  ) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = headBranchRequests.begin(expectedOwner, expectedRepo, forkOwner);
    const current = () =>
      headBranchRequests.owns(claim, owner, repo, headRepoOwner) &&
      isCurrentRoute(expectedOwner, expectedRepo, expectedRoute);
    headBranches = [];
    headBranchesLoading = true;
    headBranchesError = '';
    try {
      const next = await repos.branches(forkOwner, expectedRepo);
      if (current()) headBranches = next ?? [];
    } catch (e: any) {
      if (current()) {
        headBranches = [];
        headBranchesError = e?.message || t('pulls.create_form.fork_branches_unavailable');
      }
    } finally {
      if (current()) headBranchesLoading = false;
    }
  }

  function selectHeadRepo(nextOwner: string) {
    if (nextOwner === headRepoOwner) return;
    headRepoOwner = nextOwner;
    newHead = '';
    if (nextOwner && nextOwner !== owner) {
      void loadHeadBranches(nextOwner, owner, repo, routeGeneration);
    } else {
      headBranchRequests.begin(owner, repo, '');
      headBranches = [];
      headBranchesLoading = false;
      headBranchesError = '';
    }
  }

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function selectFilter(nextFilter: string) {
    if (filterState === nextFilter) return;
    filterState = nextFilter;
    void loadPRs(owner, repo, nextFilter, routeGeneration);
  }

  async function loadPRs(
    expectedOwner = owner,
    expectedRepo = repo,
    expectedFilter = filterState,
    expectedRoute = routeGeneration,
  ) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = pullListRequests.begin(expectedOwner, expectedRepo, expectedFilter);
    try {
      loading = true;
      error = '';
      const nextPulls = (await pulls.list(expectedOwner, expectedRepo, expectedFilter)).data;
      if (
        pullListRequests.owns(claim, owner, repo, filterState) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        prList = nextPulls;
      }
    } catch (e: any) {
      if (
        pullListRequests.owns(claim, owner, repo, filterState) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        error = e.message;
      }
    } finally {
      if (
        pullListRequests.owns(claim, owner, repo, filterState) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        loading = false;
      }
    }
  }

  async function loadBranches(
    expectedOwner = owner,
    expectedRepo = repo,
    expectedRoute = routeGeneration,
  ) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = branchRequests.begin(expectedOwner, expectedRepo);
    branchesLoading = true;
    branchesError = '';
    try {
      const nextBranches = await repos.branches(expectedOwner, expectedRepo);
      if (
        branchRequests.owns(claim, owner, repo) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        branches = nextBranches;
        branchesError = '';
        const listed: any[] = nextBranches ?? [];
        if (!listed.some((b) => b.name === newBase)) {
          newBase = listed.find((b) => b.is_default)?.name ?? listed[0]?.name ?? '';
        }
      }
    } catch (e: any) {
      if (
        branchRequests.owns(claim, owner, repo) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        branches = [];
        branchesError = e?.message || t('pulls.create_form.branches_unavailable');
      }
    } finally {
      if (
        branchRequests.owns(claim, owner, repo) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        branchesLoading = false;
      }
    }
  }

  async function handleCreate(e: Event) {
    e.preventDefault();
    if (creating) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    pullListRequests.begin(expectedOwner, expectedRepo, filterState);
    try {
      creating = true;
      error = '';
      await pulls.create(expectedOwner, expectedRepo, {
        title: newTitle,
        body: newBody || undefined,
        head_branch: newHead,
        head_owner: headIsFork ? headRepoOwner : null,
        base_branch: newBase,
        draft: newDraft,
      });
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      showCreate = false;
      newTitle = '';
      newBody = '';
      newDraft = false;
      await loadPRs(expectedOwner, expectedRepo, filterState, expectedRoute);
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) creating = false;
    }
  }

  async function openCreate() {
    if (showCreate) {
      showCreate = false;
      return;
    }
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    try {
      if (!templateLoaded) {
        const template = await pulls.template(expectedOwner, expectedRepo);
        if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
        if (template?.content && !newBody) newBody = template.content;
        templateLoaded = true;
      }
      showCreate = true;
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    }
  }
</script>

<svelte:head>
  <title>{t('pulls.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="pulls" starsCount={0} />

  <div class="gh-toolbar pulls-toolbar">
    <div class="filter-tabs">
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'open'}
        onclick={() => selectFilter('open')}
      >
        {t('pulls.tabs.open')}
      </button>
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'closed'}
        onclick={() => selectFilter('closed')}
      >
        {t('pulls.tabs.closed')}
      </button>
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'merged'}
        onclick={() => selectFilter('merged')}
      >
        {t('pulls.tabs.merged')}
      </button>
    </div>
    <button class="btn-primary" onclick={openCreate}>
      {t('pulls.new')}
    </button>
  </div>

  {#if showCreate}
    <div class="create-form gh-card">
      <h2>{t('pulls.create_form.title')}</h2>
      {#if branchesError}
        <div class="error-banner branch-load-error" role="alert">
          <span>{t('pulls.create_form.branches_unavailable')}</span>
          <button
            type="button"
            class="btn-secondary"
            onclick={() => loadBranches(owner, repo, routeGeneration)}
            disabled={branchesLoading}
          >
            {t('common.retry')}
          </button>
        </div>
      {/if}
      <form onsubmit={handleCreate}>
        <div class="head-repo-row">
          <label>
            {t('pulls.create_form.head_repository')}
            <select
              class="head-repo-select"
              value={headRepoOwner}
              onchange={(event) => selectHeadRepo(event.currentTarget.value)}
              disabled={creating}
            >
              <option value="">{owner}/{repo}</option>
              {#each headRepoOptions as option (option.owner)}
                <option value={option.owner}>
                  {option.owner}/{repo}{option.mine ? ` ${t('pulls.create_form.your_fork')}` : ''}
                </option>
              {/each}
            </select>
          </label>
          {#if forksError}
            <span class="forks-note" role="status">{t('pulls.create_form.forks_unavailable')}</span>
          {/if}
        </div>
        {#if headIsFork && headBranchesError}
          <div class="error-banner branch-load-error head-branch-error" role="alert">
            <span>{t('pulls.create_form.fork_branches_unavailable')}</span>
            <button
              type="button"
              class="btn-secondary"
              onclick={() => loadHeadBranches(headRepoOwner, owner, repo, routeGeneration)}
              disabled={headBranchesLoading}
            >
              {t('common.retry')}
            </button>
          </div>
        {/if}
        <div class="branch-row">
          <label>
            {t('pulls.create_form.from')}
            <select
              bind:value={newHead}
              required
              disabled={creating || headBranchesBusy || (headIsFork ? !!headBranchesError : !!branchesError)}
            >
              <option value="" disabled selected>{t('pulls.create_form.select_branch')}</option>
              {#each headBranchOptions as b}
                <option value={b.name}>{b.name}</option>
              {/each}
            </select>
          </label>
          <span class="arrow">→</span>
          <label>
            {t('pulls.create_form.into')}
            <select bind:value={newBase} required disabled={creating || branchesLoading || !!branchesError}>
              {#each branches as b}
                <option value={b.name}>{b.name} {b.is_default ? t('repo.browser.default_branch') : ''}</option>
              {/each}
            </select>
          </label>
        </div>
        {#if compareLink}
          <a class="compare-link" href={compareLink}>{t('pulls.create_form.compare')}</a>
        {/if}
        <label>
          {t('pulls.create_form.title_label')}
          <input type="text" bind:value={newTitle} required placeholder={t('pulls.create_form.title_placeholder')} disabled={creating} />
        </label>
        <label>
          {t('pulls.create_form.description')} <span class="optional">{t('pulls.create_form.description_hint')}</span>
          <textarea bind:value={newBody} rows="4" placeholder={t('pulls.create_form.description_placeholder')} disabled={creating}></textarea>
        </label>
        <label class="draft-option">
          <input type="checkbox" bind:checked={newDraft} disabled={creating} />
          <span>{t('pulls.create_form.draft')}</span>
        </label>
        <div class="form-actions">
          <button
            type="submit"
            class="btn-primary"
            disabled={creating || branchesLoading || !!branchesError || headBranchesBusy || (headIsFork && !!headBranchesError) || !newHead}
          >{t('pulls.create_form.submit')}</button>
          <button type="button" class="btn-secondary" onclick={() => showCreate = false}>{t('pulls.create_form.cancel')}</button>
        </div>
      </form>
    </div>
  {/if}

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if prList.length === 0}
    <div class="empty"><p>{t('pulls.empty', { state: filterState === 'all' ? '' : filterState })}</p></div>
  {:else}
    <div class="pr-list gh-list">
      {#each prList as pr}
        <a href={`/${owner}/${repo}/pulls/${pr.number}`} class="pr-item gh-list-item">
          <span class="pr-icon">
            {pr.state === 'merged' ? '⊛' : pr.state === 'closed' ? '✓' : '⑂'}
          </span>
          <div class="pr-info">
            <div class="pr-title">
              {pr.title}
              {#if pr.is_draft}<span class="draft-badge">{t('pulls.draft')}</span>{/if}
            </div>
            <div class="pr-meta">
              {t('pulls.meta', { number: pr.number, date: formatDate(pr.created_at), author: pr.author || t('common.unknown') })}<BotBadge owner={pr.author_bot_owner} link={false} />
              <span class="branch-label">{pr.head_branch}</span> → <span class="branch-label">{pr.base_branch}</span>
            </div>
          </div>
        </a>
      {/each}
    </div>
  {/if}
</div>

<style>
  .pulls-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }

  .filter-tabs {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }
  .filter-btn {
    color: var(--text-secondary);
  }
  .filter-btn.active { color: var(--text-primary); background: var(--bg-secondary); font-weight: 600; }
  .filter-btn:hover { background: var(--bg-hover); }

  .btn-primary {
    padding: 6px 16px;
    background: var(--accent);
    color: #fff;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }
  .btn-primary:hover { background: var(--accent-hover); }
  .btn-primary:disabled { opacity: 0.5; }

  .btn-secondary {
    padding: 6px 16px;
    background: none;
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 14px;
    cursor: pointer;
  }

  .create-form {
    padding: 20px;
    margin-bottom: 24px;
  }
  .branch-load-error {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
  }
  h2 { font-size: 18px; margin-bottom: 16px; }
  form { display: flex; flex-direction: column; gap: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; font-weight: 600; }
  .optional { font-weight: 400; color: var(--text-muted); }
  .draft-option { flex-direction: row; align-items: center; font-weight: 500; }
  .draft-option input { width: auto; }
  select { padding: 6px 10px; }
  textarea { font-family: var(--font-mono); font-size: 13px; resize: vertical; }

  .branch-row {
    display: flex;
    align-items: flex-end;
    gap: 12px;
  }
  .arrow { font-size: 20px; color: var(--text-muted); margin-bottom: 8px; }

  .head-repo-row {
    display: flex;
    align-items: flex-end;
    gap: 12px;
    flex-wrap: wrap;
  }
  .forks-note { font-size: 12px; color: var(--text-muted); margin-bottom: 8px; }
  .compare-link { align-self: flex-start; font-size: 13px; }

  .form-actions { display: flex; gap: 8px; margin-top: 8px; }
.empty { text-align: center; padding: 48px; color: var(--text-secondary); }

  .pr-item {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border-light);
    text-decoration: none;
    color: var(--text-primary);
  }
  .pr-item:last-child { border-bottom: none; }
  .pr-item:hover { background: var(--bg-secondary); text-decoration: none; }

  .pr-icon { font-size: 14px; margin-top: 3px; color: var(--green); }

  .pr-title { font-weight: 600; font-size: 15px; }
  .draft-badge { margin-left: 6px; padding: 1px 6px; border: 1px solid var(--border); border-radius: 10px; color: var(--text-secondary); font-size: 11px; }
  .pr-meta { font-size: 12px; color: var(--text-muted); margin-top: 2px; }

  .branch-label {
    display: inline-block;
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-family: var(--font-mono);
    font-size: 11px;
    color: var(--accent);
  }
</style>
