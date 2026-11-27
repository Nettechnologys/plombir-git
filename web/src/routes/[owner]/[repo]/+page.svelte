<script lang="ts">
  import { copyToClipboard } from '$lib/clipboard';
  import { page } from '$app/state';
  import { browser } from '$app/env';
  import { goto } from '$app/navigation';
  import { tick } from 'svelte';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import GoToFile from '$lib/components/GoToFile.svelte';
  import RefPicker from '$lib/components/RefPicker.svelte';
  import RepoAbout from '$lib/components/RepoAbout.svelte';
  import { ApiError, buildHttpCloneUrl, buildSshCloneUrl } from '$lib/api/_base';
  import { repos, type RepoTreeEntry } from '$lib/api/client.svelte';
  import type { RepoCommitSummary } from '$lib/api/repos';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDate, formatRelativeTime, pluralForm } from '$lib/i18n';
  import { canWriteRepo } from '$lib/repoPermission';
  import { renderMarkdown } from '$lib/utils/markdown';
  import { findReadme, readmeDirectories, type RepositoryLinkTargets } from '$lib/repoReadme';
  import { sortTreeEntries } from '$lib/repoTree';

  const t = createT();
  // Only the newest commit is drawn, in the bar above the file list.
  const LATEST_COMMIT = 1;

  let owner = $derived(page.params.owner!);
  let repo = $derived(page.params.repo!);
  let ref = $state('');
  let path = $state('');
  let queryRef = $derived(page.url.searchParams.get('ref') || '');
  let queryPath = $derived(page.url.searchParams.get('path') || '');
  let entries = $state<RepoTreeEntry[]>([]);
  let branches = $state<any[]>([]);
  let commits = $state<any[]>([]);
  let repoInfo = $state<any>(null);
  let readmeContent = $state<string | null>(null);
  let readmeName = $state<string | null>(null);
  // The directory the README was found in: '' for the root, or `.gitea`,
  // `.github`, `docs` (card_68a6f79c5fe4). Its relative links lead from there.
  let readmeDirectory = $state('');
  let readmePath = $derived(readmeDirectory ? `${readmeDirectory}/${readmeName ?? ''}` : (readmeName ?? ''));
  // The README is Markdown when its name says so, like any other blob; a bare
  // `README` is plain text. Rendered through the shared sanitizer, the same
  // path the file view and issues use.
  let readmeIsMarkdown = $derived(/\.(md|markdown)$/i.test(readmeName ?? ''));
  let renderedReadme = $derived(
    readmeContent && readmeIsMarkdown ? renderMarkdown(readmeContent, readmeLinks(readmeDirectory)) : '',
  );
  let readmeLoading = $state(false);
  let readmeError = $state('');
  let tags = $state<{ name: string }[]>([]);
  // The last commit of each listed entry and the commit count load after the
  // listing and never hold it up; until they arrive the rows show no commit.
  let entryCommits = $state<Record<string, RepoCommitSummary>>({});
  let commitTotal = $state<{ count: number; complete: boolean } | null>(null);
  const overviewRequests = new LatestRepositoryResourceRequestFence<string>();
  let loading = $state(true);
  let error = $state('');
  const dataRequests = new LatestRepositoryResourceRequestFence<string>();
  const readmeRequests = new LatestRepositoryResourceRequestFence<string>();
  // The branch list carries Git's own default marker, so it is the primary
  // source: the label then names the same branch the dropdown highlights. The
  // repository row is the fallback for a repo with no branches yet (unborn
  // HEAD), where the list is empty but the row still knows the branch name.
  let currentRefLabel = $derived(ref || branches.find((b: any) => b.is_default)?.name || repoInfo?.default_branch || 'main');

  // Clone URLs for empty-repo setup
  let httpCloneUrl = $derived(buildHttpCloneUrl(owner, repo));
  let sshCloneUrl = $derived(browser ? buildSshCloneUrl(owner, repo, location.hostname) : '');
  let httpCopied = $state(false);
  let sshCopied = $state(false);

  function copyUrl(url: string) {
    return async () => {
      if (!(await copyToClipboard(url))) return;
      if (url === httpCloneUrl) { httpCopied = true; setTimeout(() => httpCopied = false, 2000); }
      else { sshCopied = true; setTimeout(() => sshCopied = false, 2000); }
    };
  }

  function buildRepoQuery(nextRef: string, nextPath: string) {
    const params = new URLSearchParams();
    if (nextRef) params.set('ref', nextRef);
    if (nextPath) params.set('path', nextPath);
    const qs = params.toString();
    return qs ? `?${qs}` : '';
  }

  function syncLocation(nextRef = ref, nextPath = path) {
    const normalizedPath = nextPath ? nextPath.replace(/\/+/g, '/') : '';
    ref = nextRef;
    path = normalizedPath;
    goto(`/${owner}/${repo}${buildRepoQuery(nextRef, normalizedPath)}`, { replace: true });
  }

  function buildTreeHref(nextRef: string, nextPath: string) {
    return `/${owner}/${repo}${buildRepoQuery(nextRef, nextPath)}`;
  }

  function buildCommitHref(sha: string) {
    return `/${owner}/${repo}/commits/${sha}${buildRepoQuery(ref, '')}`;
  }

  function encodeRepoPath(pathValue: string): string {
    return pathValue.split('/').map(encodeURIComponent).join('/');
  }

  function buildBlobHref(filePath: string) {
    return `/${owner}/${repo}/blob/${encodeRepoPath(filePath)}${buildRepoQuery(ref, '')}`;
  }

  function readmeLinks(directory: string): RepositoryLinkTargets {
    return {
      directory,
      page: (target, isDirectory) => (isDirectory ? buildTreeHref(ref, target) : buildBlobHref(target)),
      image: (target) => repos.rawUrl(owner, repo, target, ref || undefined),
    };
  }

  function repositoryViewIdentity(expectedRef: string, expectedPath: string): string {
    return `${expectedRef}\u0000${expectedPath}`;
  }

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRef = queryRef;
    const expectedPath = queryPath;
    ref = expectedRef;
    path = expectedPath;
    void loadData(expectedOwner, expectedRepo, expectedRef, expectedPath);
  });

  async function loadData(
    expectedOwner: string,
    expectedRepo: string,
    expectedRef: string,
    expectedPath: string,
  ) {
    const identity = repositoryViewIdentity(expectedRef, expectedPath);
    const claim = dataRequests.begin(expectedOwner, expectedRepo, identity);
    // Invalidate any README request owned by the previous repository view.
    // The actual README claim begins only after the tree load succeeds.
    readmeRequests.begin(expectedOwner, expectedRepo, identity);
    loading = true;
    error = '';
    entries = [];
    branches = [];
    commits = [];
    repoInfo = null;
    readmeContent = null;
    readmeName = null;
    readmeDirectory = '';
    readmeLoading = false;
    readmeError = '';
    overviewRequests.begin(expectedOwner, expectedRepo, identity);
    tags = [];
    entryCommits = {};
    commitTotal = null;
    try {
      const [treeData, branchData, logData, repoData] = await Promise.all([
        repos.tree(expectedOwner, expectedRepo, expectedRef || undefined, expectedPath || undefined),
        repos.branches(expectedOwner, expectedRepo),
        // The page shows one commit; asking for the server's default 50
        // walked and serialized fifty times the history it renders.
        repos.log(expectedOwner, expectedRepo, expectedRef || undefined, expectedPath || undefined, LATEST_COMMIT),
        repos.get(expectedOwner, expectedRepo),
      ]);
      if (!dataRequests.owns(claim, owner, repo, repositoryViewIdentity(queryRef, queryPath))) return;

      const nextEntries = treeData.entries || [];
      entries = sortTreeEntries(nextEntries);
      branches = branchData || [];
      commits = (logData.commits || []).slice(0, LATEST_COMMIT);
      repoInfo = repoData;

      // Load README when at root
      if (!expectedPath) {
        void loadReadme(expectedOwner, expectedRepo, expectedRef, expectedPath, nextEntries);
      }
      if (nextEntries.length) void loadOverview(expectedOwner, expectedRepo, expectedRef, expectedPath);
    } catch (e: any) {
      if (dataRequests.owns(claim, owner, repo, repositoryViewIdentity(queryRef, queryPath))) {
        error = e.message;
      }
    } finally {
      if (dataRequests.owns(claim, owner, repo, repositoryViewIdentity(queryRef, queryPath))) {
        loading = false;
      }
    }
  }

  /**
   * What the page draws around the listing: tags for the ref picker, the last
   * commit of every entry, and at the root the commit count. Each part is
   * best effort; a failure leaves that part out.
   */
  async function loadOverview(
    expectedOwner: string,
    expectedRepo: string,
    expectedRef: string,
    expectedPath: string,
  ) {
    const identity = repositoryViewIdentity(expectedRef, expectedPath);
    const claim = overviewRequests.begin(expectedOwner, expectedRepo, identity);
    const owns = () => overviewRequests.owns(claim, owner, repo, repositoryViewIdentity(queryRef, queryPath));
    const ref = expectedRef || undefined;
    // `Promise.resolve().then` turns a call that throws into a rejection, so
    // one failing part can never take the page down with it.
    const attempt = <T,>(read: () => Promise<T>, use: (value: T) => void) =>
      Promise.resolve()
        .then(read)
        .then((value) => { if (owns()) use(value); })
        .catch(() => {});
    await Promise.all([
      attempt(() => repos.tags(expectedOwner, expectedRepo), (list) => { tags = list ?? []; }),
      attempt(
        () => repos.treeCommits(expectedOwner, expectedRepo, ref, expectedPath || undefined),
        (answer) => { entryCommits = answer?.entries ?? {}; },
      ),
      expectedPath
        ? Promise.resolve()
        : attempt(() => repos.commitCount(expectedOwner, expectedRepo, ref), (answer) => { if (answer) commitTotal = answer; }),
    ]);
  }

  async function loadReadme(
    expectedOwner: string,
    expectedRepo: string,
    expectedRef: string,
    expectedPath: string,
    expectedEntries: RepoTreeEntry[],
  ) {
    const identity = repositoryViewIdentity(expectedRef, expectedPath);
    const claim = readmeRequests.begin(expectedOwner, expectedRepo, identity);
    const ownsCurrentView = () => (
      expectedOwner === owner &&
      expectedRepo === repo &&
      identity === repositoryViewIdentity(queryRef, queryPath) &&
      readmeRequests.owns(claim, owner, repo, identity)
    );
    if (!ownsCurrentView()) return;
    readmeLoading = true;
    readmeError = '';
    try {
      // Where Gitea and GitHub look, in Gitea's order: `.gitea/`, `.github/`,
      // the root, `docs/`. A directory's listing is read only when the root
      // has that directory.
      for (const directory of readmeDirectories(expectedEntries)) {
        try {
          const listing = directory
            ? (await repos.tree(expectedOwner, expectedRepo, expectedRef || undefined, directory)).entries || []
            : expectedEntries;
          if (!ownsCurrentView()) return;
          const name = findReadme(listing);
          if (!name) continue;
          const data = await repos.blob(
            expectedOwner,
            expectedRepo,
            directory ? `${directory}/${name}` : name,
            expectedRef || undefined,
          );
          if (ownsCurrentView()) {
            readmeContent = data.content;
            readmeName = name;
            readmeDirectory = directory;
          }
          return;
        } catch (e) {
          // The tree and blob reads are separate snapshots. A 404 means this
          // directory or README disappeared between them, so the next place
          // may still hold one. Every other status is a failed read, not
          // evidence that the repository has no README.
          if (e instanceof ApiError && e.status === 404) continue;
          throw e;
        }
      }
    } catch (e: any) {
      if (ownsCurrentView()) {
        readmeContent = null;
        readmeName = null;
        readmeError = e?.message || t('repo.browser.readme_unavailable');
      }
    } finally {
      if (ownsCurrentView()) readmeLoading = false;
    }
  }

  function parentPath(current: string) {
    const parts = current.split('/');
    parts.pop();
    return parts.join('/');
  }

  let latestCommit = $derived<RepoCommitSummary | null>(commits[0] ?? null);
  /** The history of what is listed: the whole ref at the root, the directory inside it. */
  let historyHref = $derived.by(() => {
    const params = new URLSearchParams();
    if (ref) params.set('ref', ref);
    if (path) params.set('path', path);
    const qs = params.toString();
    return `/${owner}/${repo}/commits${qs ? `?${qs}` : ''}`;
  });

  // README outline: the headings of the rendered README, read from the page
  // once it is drawn. Choosing one scrolls to it; the sanitizer gives
  // headings no ids, so there is nothing to link to.
  let readmeBody = $state<HTMLElement | null>(null);
  let outlineOpen = $state(false);
  let readmeOutline = $state<{ level: number; text: string }[]>([]);
  $effect(() => {
    void renderedReadme;
    const body = readmeBody;
    outlineOpen = false;
    if (!body) {
      readmeOutline = [];
      return;
    }
    void tick().then(() => {
      readmeOutline = Array.from(body.querySelectorAll('h1, h2, h3')).map((heading) => ({
        level: Number(heading.tagName.slice(1)),
        text: heading.textContent?.trim() ?? '',
      }));
    });
  });

  function showHeading(index: number) {
    outlineOpen = false;
    readmeBody?.querySelectorAll('h1, h2, h3')[index]?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }

  /** The current directory as clickable steps, each leading to its own listing. */
  let pathCrumbs = $derived(
    path
      ? path.split('/').map((name, index, parts) => ({ name, path: parts.slice(0, index + 1).join('/') }))
      : [],
  );

</script>

<svelte:head>
  <title>{owner}/{repo} · Plombir Git</title>
</svelte:head>

{#snippet folderIcon()}
  <svg class="entry-icon folder" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path d="M1.5 3.25c0-.69.56-1.25 1.25-1.25h3.1c.4 0 .77.19 1 .5l.9 1.25h5.5c.69 0 1.25.56 1.25 1.25v7.75c0 .69-.56 1.25-1.25 1.25H2.75c-.69 0-1.25-.56-1.25-1.25Z" /></svg>
{/snippet}

{#snippet entryCommit(name: string)}
  {@const commit = entryCommits[name]}
  {#if commit}
    <a class="entry-commit" href={buildCommitHref(commit.sha)} title={commit.message}>{commit.message}</a>
    <span class="entry-date" title={formatDate(commit.date)}>{formatRelativeTime(commit.date)}</span>
  {:else}
    <span class="entry-commit"></span>
    <span class="entry-date"></span>
  {/if}
{/snippet}

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="code" starsCount={repoInfo?.stars_count || 0} defaultBranch={repoInfo?.default_branch} />

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if !error && commits.length === 0 && entries.length === 0}
    <!-- Empty repository — setup guidance.
         Guarded on `error` because a failed load leaves `entries` and
         `commits` empty for a reason that is not emptiness: a repository whose
         HEAD lost its branch answers 409, and drawing "push an existing
         repository" under that banner tells the owner to push a history the
         repository already has (card_9e11f76dddd1). -->
    <div class="empty-repo">
      <div class="empty-icon">📦</div>
      <h2>{t('repo.empty.title')}</h2>
      <p>{t('repo.empty.desc')}</p>

      <div class="setup-steps">
        <div class="step">
          <span class="step-num">1</span>
          <span>{t('repo.empty.step_clone')}</span>
        </div>

        <div class="clone-options">
          <div class="option-box">
            <div class="option-header">
              <strong>HTTPS</strong>
              <button class="mini-copy" onclick={copyUrl(httpCloneUrl)}>
                {httpCopied ? '✓ ' + t('repo.empty.copied') : '📋 ' + t('repo.empty.copy')}
              </button>
            </div>
            <code class="cmd">{httpCloneUrl}</code>
          </div>
          <div class="option-box">
            <div class="option-header">
              <strong>SSH</strong>
              <button class="mini-copy" onclick={copyUrl(sshCloneUrl)}>
                {sshCopied ? '✓ ' + t('repo.empty.copied') : '📋 ' + t('repo.empty.copy')}
              </button>
            </div>
            <code class="cmd">{sshCloneUrl}</code>
          </div>
        </div>

        <div class="step">
          <span class="step-num">2</span>
          <span>{t('repo.empty.step_create')}</span>
        </div>

        <div class="step">
          <span class="step-num">3</span>
          <span>{t('repo.empty.step_push')}</span>
        </div>
      </div>

      <div class="quick-commands">
        <h3>{t('repo.empty.quick')}</h3>
        <pre><code>git init
git add README.md
git commit -m "first commit"
git branch -M {repoInfo?.default_branch || 'main'}
git remote add origin {httpCloneUrl}
git push -u origin {repoInfo?.default_branch || 'main'}</code></pre>
      </div>

      <div class="or-push">
        <h3>{t('repo.empty.existing')}</h3>
        <pre><code>git remote add origin {httpCloneUrl}
git branch -M {repoInfo?.default_branch || 'main'}
git push -u origin {repoInfo?.default_branch || 'main'}</code></pre>
      </div>
    </div>
  {:else}
    <div class="repo-layout" class:with-about={!path}>
      <div class="repo-main">
        <div class="repo-toolbar">
          <div class="branch-selector">
            <RefPicker {owner} {repo} {branches} {tags} current={ref} label={currentRefLabel} onselect={(name) => syncLocation(name, path)} />
          </div>
          <a href={`/${owner}/${repo}/branches`} class="ref-count branches-link">
            <strong>{branches.length}</strong> {pluralForm(branches.length, { one: t('repo.browser.branch_forms.one'), few: t('repo.browser.branch_forms.few'), many: t('repo.browser.branch_forms.many'), other: t('repo.browser.branch_forms.other') })}
          </a>
          <a href={`/${owner}/${repo}/tags`} class="ref-count tags-link">
            <strong>{tags.length}</strong> {pluralForm(tags.length, { one: t('repo.browser.tag_forms.one'), few: t('repo.browser.tag_forms.few'), many: t('repo.browser.tag_forms.many'), other: t('repo.browser.tag_forms.other') })}
          </a>
          <div class="toolbar-actions">
            <GoToFile {owner} {repo} {ref} />
            {#if canWriteRepo(repoInfo?.viewer_permission)}
              <a href={`/${owner}/${repo}/new`} class="btn btn-outline new-file-link">
                <svg class="icon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path d="M8 2.5v11M2.5 8h11" /></svg>
                {t('repo.new_file', 'New file')}
              </a>
            {/if}
          </div>
        </div>

        <!-- Inside a directory, the path is a breadcrumb whose every step is a
             link, as on GitHub. At the root it would only repeat the header. -->
        {#if pathCrumbs.length}
          <nav class="breadcrumb" aria-label={t('repo.browser.path', 'Path')}>
            <a href={buildTreeHref(ref, '')}>{repo}</a>
            {#each pathCrumbs as crumb, index}
              <span class="sep" aria-hidden="true">/</span>
              {#if index === pathCrumbs.length - 1}
                <span class="current" aria-current="page">{crumb.name}</span>
              {:else}
                <a href={buildTreeHref(ref, crumb.path)}>{crumb.name}</a>
              {/if}
            {/each}
          </nav>
        {/if}

        <div class="gh-card tree-panel">
          <!-- The newest commit of what is listed, as GitHub heads the list. -->
          {#if latestCommit}
            <div class="latest-commit">
              <span class="latest-author">{latestCommit.author}</span>
              <a class="latest-message" href={buildCommitHref(latestCommit.sha)} title={latestCommit.message}>{latestCommit.message?.split('\n')[0]}</a>
              <span class="latest-meta">
                <a class="commit-sha" href={buildCommitHref(latestCommit.sha)}>{latestCommit.sha?.slice(0, 7)}</a>
                <span title={formatDate(latestCommit.date)}>{formatRelativeTime(latestCommit.date)}</span>
                <a class="history-link" href={historyHref}>
                  <svg class="icon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path d="M2 8a6 6 0 1 0 1.76-4.24M2 2.5v3h3M8 4.5V8l2.5 1.5" /></svg>
                  {#if commitTotal}
                    <strong>{commitTotal.count}{commitTotal.complete ? '' : '+'}</strong> {pluralForm(commitTotal.count, { one: t('repo.browser.commit_forms.one'), few: t('repo.browser.commit_forms.few'), many: t('repo.browser.commit_forms.many'), other: t('repo.browser.commit_forms.other') })}
                  {:else}
                    {t('repo.browser.history', 'History')}
                  {/if}
                </a>
              </span>
            </div>
          {/if}
          <!-- Directories are links (card_61e77c8abec1): Enter, a middle click and
               the back button work, which a `div role="button"` gave none of. -->
          {#if path}
            <a href={buildTreeHref(ref, parentPath(path))} class="entry">
              {@render folderIcon()}
              <span class="entry-name up">..</span>
            </a>
          {/if}
          {#each entries as entry (entry.name)}
            {#if entry.kind === 'tree'}
              <div class="entry">
                {@render folderIcon()}
                <a href={buildTreeHref(ref, path ? `${path}/${entry.name}` : entry.name)} class="entry-name dir">{entry.name}</a>
                {@render entryCommit(entry.name)}
              </div>
            {:else if entry.kind === 'blob'}
              <div class="entry file-entry">
                <svg class="entry-icon file" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path d="M4 1.75h5.25L12.5 5v8.75a.5.5 0 0 1-.5.5H4a.5.5 0 0 1-.5-.5V2.25a.5.5 0 0 1 .5-.5ZM9 1.75V5.5h3.5" /></svg>
                <a href={buildBlobHref(path ? path + '/' + entry.name : entry.name)} class="entry-name">{entry.name}</a>
                {@render entryCommit(entry.name)}
              </div>
            {:else}
              <div class="entry submodule-entry">
                <svg class="entry-icon submodule" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path d="M1.5 3.25c0-.69.56-1.25 1.25-1.25h3.1c.4 0 .77.19 1 .5l.9 1.25h5.5c.69 0 1.25.56 1.25 1.25v7.75c0 .69-.56 1.25-1.25 1.25H2.75c-.69 0-1.25-.56-1.25-1.25ZM6 9.5h4M8.5 8l1.5 1.5L8.5 11" /></svg>
                <span class="entry-name">{entry.name} <span class="entry-kind">{t('repo.browser.submodule')}</span></span>
                {@render entryCommit(entry.name)}
              </div>
            {/if}
          {/each}
        </div>

        <!-- README rendering (at repo root) -->
        {#if !path && readmeError}
          <div class="gh-card readme-section readme-unavailable" role="alert">
            <span>{t('repo.browser.readme_unavailable')}</span>
            <button
              type="button"
              class="btn btn-outline btn-sm"
              onclick={() => loadReadme(owner, repo, queryRef, queryPath, entries)}
              disabled={readmeLoading}
            >
              {t('common.retry')}
            </button>
          </div>
        {:else if !path && readmeContent}
          <div class="gh-card readme-section">
            <div class="readme-header">
              <span class="readme-title">
                <svg class="entry-icon file" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path d="M4 1.75h5.25L12.5 5v8.75a.5.5 0 0 1-.5.5H4a.5.5 0 0 1-.5-.5V2.25a.5.5 0 0 1 .5-.5ZM9 1.75V5.5h3.5" /></svg>
                {readmePath}
              </span>
              {#if readmeOutline.length > 1}
                <div class="readme-outline">
                  <button
                    type="button"
                    class="btn btn-outline btn-sm outline-toggle"
                    aria-expanded={outlineOpen}
                    aria-label={t('repo.browser.outline', 'Outline')}
                    title={t('repo.browser.outline', 'Outline')}
                    onclick={() => (outlineOpen = !outlineOpen)}
                  >
                    <svg class="icon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path d="M2 4h12M5 8h9M5 12h9" /></svg>
                  </button>
                  {#if outlineOpen}
                    <ul class="outline-list">
                      {#each readmeOutline as heading, index}
                        <li style:padding-left={`${(heading.level - 1) * 12 + 12}px`}>
                          <button type="button" class="outline-item" onclick={() => showHeading(index)}>{heading.text}</button>
                        </li>
                      {/each}
                    </ul>
                  {/if}
                </div>
              {/if}
            </div>
            <div class="readme-body" bind:this={readmeBody}>
              {#if readmeIsMarkdown}
                <div class="markdown-body">{@html renderedReadme}</div>
              {:else}
                <pre class="readme-plain">{readmeContent}</pre>
              {/if}
            </div>
          </div>
        {:else if !path && readmeLoading}
          <div class="gh-card readme-section">
            <p class="text-secondary">{t('common.loading')}</p>
          </div>
        {/if}
      </div>

      {#if !path}
        <RepoAbout {owner} {repo} {ref} info={repoInfo} />
      {/if}
    </div>
  {/if}
</div>

<style>
  /* GitHub's repository home: the file list and README in one column, the
     About column beside them at the root. */
  .repo-layout { display: grid; grid-template-columns: minmax(0, 1fr); gap: 24px; }
  .repo-layout.with-about { grid-template-columns: minmax(0, 1fr) 296px; }
  @media (max-width: 900px) {
    .repo-layout.with-about { grid-template-columns: minmax(0, 1fr); }
  }
  .repo-main { min-width: 0; }

  .repo-toolbar {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px 16px;
    margin-bottom: 16px;
  }

  .branch-selector { min-width: 0; max-width: 100%; }
  .ref-count { display: inline-flex; align-items: center; gap: 4px; color: var(--text-secondary); font-size: 14px; white-space: nowrap; }
  .ref-count strong { color: var(--text-primary); }
  .ref-count:hover, .ref-count:hover strong { color: var(--accent); }
  .toolbar-actions { display: flex; flex: 1 1 320px; flex-wrap: wrap; align-items: center; justify-content: flex-end; gap: 8px; min-width: 0; }
  .new-file-link { display: inline-flex; align-items: center; gap: 6px; }
  .icon { flex-shrink: 0; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }

  .breadcrumb {
    display: flex;
    flex-wrap: wrap;
    min-width: 0;
    max-width: 100%;
    overflow-wrap: anywhere;
    align-items: center;
    gap: 4px;
    margin-bottom: 12px;
    font-size: 16px;
  }
  .breadcrumb a { color: var(--accent); font-weight: 600; }
  .breadcrumb .current { font-weight: 600; }
  .sep { color: var(--text-muted); }

  .tree-panel {
    overflow: hidden;
    padding: 0;
  }

  .latest-commit {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 4px 12px;
    padding: 12px 16px;
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border);
    font-size: 14px;
  }
  .latest-author { font-weight: 600; }
  .latest-message { flex: 1 1 200px; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-secondary); }
  .latest-message:hover { color: var(--accent); }
  .latest-meta { display: inline-flex; align-items: center; gap: 12px; margin-left: auto; color: var(--text-muted); font-size: 13px; white-space: nowrap; }
  .history-link { display: inline-flex; align-items: center; gap: 4px; color: var(--text-secondary); }
  .history-link strong { color: var(--text-primary); }
  .history-link:hover, .history-link:hover strong { color: var(--accent); }

  .entry {
    display: grid;
    /* Every row is its own grid, so the date column has a fixed width: an
       `auto` one sized to each row's date shifted that row's message. */
    grid-template-columns: 16px minmax(120px, 2fr) minmax(0, 3fr) minmax(max-content, 120px);
    align-items: center;
    gap: 12px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--border-light);
    font-size: 14px;
    color: var(--text-primary);
  }
  .entry:last-child { border-bottom: none; }
  .entry:hover { background: var(--bg-hover); }
  a.entry { text-decoration: none; }
  @media (max-width: 600px) {
    .entry { grid-template-columns: 16px minmax(0, 1fr) minmax(max-content, 120px); }
    .entry .entry-commit { display: none; }
  }

  .entry-icon { flex-shrink: 0; }
  .entry-icon.folder { fill: var(--accent); }
  .entry-icon.file,
  .entry-icon.submodule { fill: none; stroke: var(--text-muted); stroke-width: 1.2; stroke-linejoin: round; }
  .entry-name { min-width: 0; overflow-wrap: anywhere; color: var(--text-primary); }
  a.entry-name:hover { color: var(--accent); text-decoration: underline; }
  .entry-name.dir { font-weight: 500; }
  .entry-name.up { color: var(--text-muted); }
  .entry-commit { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-muted); font-size: 13px; }
  a.entry-commit:hover { color: var(--accent); text-decoration: underline; }
  .entry-date { color: var(--text-muted); font-size: 13px; white-space: nowrap; text-align: right; }
  .entry-kind { font-size: 12px; color: var(--text-muted); }

  .commit-sha {
    font-family: var(--font-mono);
    font-size: 12px;
    color: var(--text-secondary);
  }

  /* Empty repo setup guidance */
  .empty-repo {
    text-align: center;
    padding: 48px 24px;
  }

  .empty-icon {
    font-size: 48px;
    margin-bottom: 16px;
  }

  .empty-repo h2 {
    font-size: 22px;
    margin-bottom: 8px;
  }
  .empty-repo > p {
    color: var(--text-secondary);
    margin-bottom: 32px;
  }

  .setup-steps {
    max-width: 640px;
    margin: 0 auto 32px;
    text-align: left;
  }

  .step {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    margin-bottom: 12px;
    font-size: 14px;
  }

  .step-num {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: var(--accent);
    color: #fff;
    font-size: 12px;
    font-weight: 700;
    flex-shrink: 0;
  }

  .clone-options {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 12px;
    margin: 0 0 24px 36px;
  }

  @media (max-width: 600px) {
    .clone-options {
      grid-template-columns: 1fr;
    }
  }

  .option-box {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px;
  }

  .option-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 8px;
    font-size: 12px;
  }

  .mini-copy {
    padding: 2px 8px;
    font-size: 11px;
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    cursor: pointer;
    color: var(--text-secondary);
  }
  .mini-copy:hover { background: var(--bg-hover); }

  .cmd {
    font-size: 12px;
    padding: 8px;
    background: var(--bg-primary);
    border: 1px solid var(--border-light);
    border-radius: 4px;
    display: block;
    word-break: break-all;
    user-select: all;
  }

  .quick-commands, .or-push {
    max-width: 640px;
    margin: 0 auto 24px;
    text-align: left;
  }

  .quick-commands h3, .or-push h3 {
    font-size: 14px;
    margin-bottom: 8px;
  }

  .quick-commands pre, .or-push pre {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px;
    overflow-x: auto;
  }

  .quick-commands code, .or-push code {
    font-size: 13px;
    line-height: 1.6;
    color: var(--text-primary);
  }

  /* README section */
  .readme-section {
    margin-top: 24px;
    padding: 0;
  }

  .readme-header {
    position: relative;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 16px;
    background: var(--bg-secondary);
    border-bottom: 1px solid var(--border);
    font-size: 14px;
    font-weight: 600;
  }
  .readme-title { display: inline-flex; align-items: center; gap: 8px; min-width: 0; overflow-wrap: anywhere; }
  .readme-outline { position: relative; }
  .outline-toggle { display: inline-flex; align-items: center; padding: 4px 8px; }
  .outline-list {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: 20;
    width: min(320px, 80vw);
    max-height: 60vh;
    overflow-y: auto;
    margin: 0;
    padding: 4px 0;
    list-style: none;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.15);
    font-weight: 400;
  }
  .outline-item {
    display: block;
    width: 100%;
    padding: 4px 12px 4px 0;
    border: none;
    background: none;
    color: var(--text-primary);
    font-size: 13px;
    text-align: left;
  }
  .outline-item:hover { color: var(--accent); }

  .readme-body {
    background: var(--bg-secondary);
    border: none;
    border-radius: 0;
    padding: 32px;
  }

  .markdown-body {
    line-height: 1.7;
    color: var(--text-primary);
  }

  .markdown-body :global(h1) {
    font-size: 28px;
    font-weight: 700;
    margin: 0 0 16px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border);
  }

  .markdown-body :global(h2) {
    font-size: 22px;
    font-weight: 600;
    margin: 24px 0 12px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--border-light);
  }

  .markdown-body :global(h3) {
    font-size: 18px;
    font-weight: 600;
    margin: 16px 0 8px;
  }

  .markdown-body :global(p),
  .markdown-body :global(li) {
    font-size: 14px;
  }

  .markdown-body :global(p) {
    margin: 0 0 12px;
  }

  .markdown-body :global(ul),
  .markdown-body :global(ol) {
    margin: 0 0 12px;
    padding-left: 24px;
  }

  .markdown-body :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 12px 0;
  }

  .markdown-body :global(pre) {
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px 16px;
    overflow-x: auto;
    margin: 0 0 12px;
  }

  .markdown-body :global(pre code) {
    background: none;
    padding: 0;
  }

  .markdown-body :global(table) {
    border-collapse: collapse;
    margin: 0 0 12px;
  }

  .markdown-body :global(th),
  .markdown-body :global(td) {
    border: 1px solid var(--border);
    padding: 6px 12px;
    font-size: 14px;
  }

  .readme-plain {
    margin: 0;
    white-space: pre-wrap;
    font-family: var(--font-mono);
    font-size: 13px;
  }

  .markdown-body :global(code) {
    background: var(--bg-tertiary);
    padding: 1px 6px;
    border-radius: 3px;
    font-size: 13px;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  }

  .markdown-body :global(a) {
    color: var(--accent);
    text-decoration: none;
  }
  .markdown-body :global(a:hover) { text-decoration: underline; }

  .markdown-body :global(strong) { font-weight: 700; }
</style>
