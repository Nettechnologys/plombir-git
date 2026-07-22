<script lang="ts">
  import { page } from '$app/stores';
  import { browser } from '$app/environment';
  import { goto } from '$app/navigation';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import Dropdown from '$lib/components/Dropdown.svelte';
  import { buildSshCloneUrl, withBackendBase } from '$lib/api/_base';
  import { repos } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let ref = $state('');
  let path = $state('');
  let queryRef = $derived($page.url.searchParams.get('ref') || '');
  let queryPath = $derived($page.url.searchParams.get('path') || '');
  let entries = $state<any[]>([]);
  let branches = $state<any[]>([]);
  let commits = $state<any[]>([]);
  let repoInfo = $state<any>(null);
  let readmeContent = $state<string | null>(null);
  let readmeLoading = $state(false);
  let loading = $state(true);
  let error = $state('');
  let currentRefLabel = $derived(ref || repoInfo?.default_branch || branches.find((b: any) => b.is_default)?.name || 'main');

  // Clone URLs for empty-repo setup
  let httpCloneUrl = $derived(withBackendBase(`/git/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}`));
  let sshCloneUrl = $derived(browser ? buildSshCloneUrl(owner, repo, location.hostname) : '');
  let httpCopied = $state(false);
  let sshCopied = $state(false);

  function copyUrl(url: string) {
    return () => {
      navigator.clipboard.writeText(url);
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
    goto(`/${owner}/${repo}${buildRepoQuery(nextRef, normalizedPath)}`, { replaceState: true });
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

  $effect(() => {
    if (ref !== queryRef || path !== queryPath) {
      ref = queryRef;
      path = queryPath;
    }
    loadData();
  });

  async function loadData() {
    loading = true;
    error = '';
    readmeContent = null;
    try {
      const [treeData, branchData, logData, repoData] = await Promise.all([
        repos.tree(owner, repo, ref || undefined, path || undefined),
        repos.branches(owner, repo),
        repos.log(owner, repo, ref || undefined, path || undefined),
        repos.get(owner, repo),
      ]);
      entries = treeData.entries || [];
      branches = branchData || [];
      commits = (logData.commits || []).slice(0, 5);
      repoInfo = repoData;

      // Load README when at root
      if (!path) {
        loadReadme();
      }
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadReadme() {
    readmeLoading = true;
    try {
      // Try common README filenames
      const readmeNames = ['README.md', 'README.markdown', 'README', 'readme.md', 'Readme.md'];
      for (const name of readmeNames) {
        const entry = entries.find((e: any) => e.name === name);
        if (entry) {
          const data = await repos.blob(owner, repo, name, ref || undefined);
          readmeContent = data.content;
          break;
        }
      }
    } catch {
      // No README found — that's OK
    } finally {
      readmeLoading = false;
    }
  }

  function navigateToPath(entryName: string) {
    const nextPath = path ? `${path}/${entryName}` : entryName;
    syncLocation(ref, nextPath);
  }

  function navigateUp() {
    const parts = path.split('/');
    parts.pop();
    syncLocation(ref, parts.join('/'));
  }

  function selectBranch(branchName: string, close: () => void) {
    syncLocation(branchName, path);
    close();
  }

  function formatFileSize(size: number) {
    if (size < 1024) return size + t('repo.file_size.b');
    if (size < 1024 * 1024) return (size / 1024).toFixed(1) + t('repo.file_size.kb');
    return (size / (1024 * 1024)).toFixed(1) + t('repo.file_size.mb');
  }

  function escapeHtml(value: string): string {
    return value
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  function safeMarkdownHref(value: string): string {
    const href = value.trim();
    if (/^(https?:|mailto:|#|\/|\.\/|\.\.\/)/i.test(href)) {
      return href;
    }
    return '#';
  }

  function renderInlineMarkdown(line: string): string {
    return escapeHtml(line)
      .replace(/`([^`]+)`/g, '<code>$1</code>')
      .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
      .replace(/\[([^\]]+)\]\(([^)]+)\)/g, (_match, label, href) => {
        return `<a href='${safeMarkdownHref(href)}' target='_blank' rel='noopener'>${label}</a>`;
      });
  }
</script>

<svelte:head>
  <title>{owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="code" starsCount={repoInfo?.stars_count || 0} defaultBranch={repoInfo?.default_branch} />

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">Loading...</p>
  {:else if commits.length === 0 && entries.length === 0}
    <!-- Empty repository — setup guidance -->
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
    <!-- Branch selector + path breadcrumb -->
    <div class="repo-toolbar">
      <div class="branch-selector">
        <Dropdown ariaLabel={t('repo.select_branch')} triggerClass="btn-outline" placement="left">
          {#snippet trigger()}
            🌿 {currentRefLabel} <span aria-hidden="true">▾</span>
          {/snippet}
          {#snippet menu(close)}
            {#each branches as b}
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

      <div class="toolbar-actions">
        <a href={`/${owner}/${repo}/new`} class="btn-outline btn-sm">
          ➕ {t('repo.new_file') || 'New file'}
        </a>
      </div>

      <div class="breadcrumb">
        <a href={buildTreeHref(ref, '')}>{repo}</a>
        {#if path}
          {#each path.split('/') as part}
            <span class="sep">/</span>
            <span>{part}</span>
          {/each}
        {/if}
      </div>
    </div>

    <div class="content-grid">
      <!-- File tree -->
      <div class="gh-card tree-panel">
        {#if path}
          <!-- svelte-ignore a11y_click_events_have_key_events a11y_no_static_element_interactions -->
          <div class="entry" onclick={navigateUp} role="button" tabindex="0">
            <span class="entry-icon">📁</span>
            <span class="entry-name up">..</span>
          </div>
        {/if}
        {#each entries as entry}
          {#if entry.kind === 'tree' || entry.kind === 'dir'}
            <!-- svelte-ignore a11y_click_events_have_key_events -->
            <div class="entry" onclick={() => navigateToPath(entry.name)} role="button" tabindex="0">
              <span class="entry-icon">📁</span>
              <span class="entry-name dir">{entry.name}</span>
            </div>
          {:else}
            <a href={buildBlobHref(path ? path + '/' + entry.name : entry.name)} class="entry file-entry">
              <span class="entry-icon">📄</span>
              <span class="entry-name">{entry.name}</span>
              {#if entry.size}
                <span class="entry-size">{formatFileSize(entry.size)}</span>
              {/if}
            </a>
          {/if}
        {/each}
      </div>

      <!-- Recent commits -->
      <div class="gh-card commits-panel">
        <h3>{t('repo.browser.recent_commits')}</h3>
        {#each commits as commit}
          <a href={buildCommitHref(commit.sha)} class="commit-item">
            <div class="commit-msg truncate">{commit.message?.split('\n')[0]}</div>
            <div class="commit-meta">
              <span class="commit-author">{commit.author}</span>
              <span class="commit-date">{formatDate(commit.date)}</span>
              <code class="commit-sha">{commit.sha?.slice(0, 7)}</code>
            </div>
          </a>
        {/each}
      </div>
    </div>

    <!-- README rendering (at repo root) -->
    {#if !path && readmeContent}
      <div class="gh-card readme-section">
        <div class="readme-header">
          <span>📄 README.md</span>
        </div>
        <div class="readme-body">
          <div class="markdown-body">
            <!-- Simple markdown rendering -->
            {#each readmeContent.split('\n') as line}
              {#if line.startsWith('# ')}
                <h1 class="md-h1">{line.slice(2)}</h1>
              {:else if line.startsWith('## ')}
                <h2 class="md-h2">{line.slice(3)}</h2>
              {:else if line.startsWith('### ')}
                <h3 class="md-h3">{line.slice(4)}</h3>
              {:else if line.startsWith('```')}
                <hr class="md-hr" />
              {:else if line.startsWith('- ') || line.startsWith('* ')}
                <li class="md-li">{line.slice(2)}</li>
              {:else if line.trim() === ''}
                <br />
              {:else}
                <p class="md-p">
                  {@html renderInlineMarkdown(line)}
                </p>
              {/if}
            {/each}
          </div>
        </div>
      </div>
    {:else if !path && readmeLoading}
      <div class="gh-card readme-section">
        <p class="text-secondary">{t('common.loading')}</p>
      </div>
    {/if}
  {/if}
</div>

<style>
  .repo-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 16px;
  }

  .breadcrumb {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 14px;
  }
  .breadcrumb a { color: var(--accent); font-weight: 600; }
  .sep { color: var(--text-muted); }

  .content-grid {
    display: grid;
    grid-template-columns: 1fr 320px;
    gap: 16px;
  }

  @media (max-width: 900px) {
    .content-grid { grid-template-columns: 1fr; }
  }

  .tree-panel {
    overflow: hidden;
    padding: 0;
  }

  .entry {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--border-light);
    font-size: 14px;
    cursor: pointer;
    text-decoration: none;
    color: var(--text-primary);
  }
  .entry:hover { background: var(--bg-hover); }
  .file-entry { cursor: pointer; }

  .entry-icon { font-size: 14px; }
  .entry-name { flex: 1; }
  .entry-name.dir { color: var(--text-primary); font-weight: 500; }
  .entry-name.up { color: var(--text-muted); }
  .entry-size { font-size: 12px; color: var(--text-muted); font-family: var(--font-mono); }

  .commits-panel {
    padding: 16px;
  }

  h3 { font-size: 14px; margin-bottom: 12px; }

  .commit-item {
    display: block;
    padding: 8px 0;
    border-bottom: 1px solid var(--border-light);
    color: inherit;
    text-decoration: none;
  }
  .commit-item:last-child { border-bottom: none; }

  .commit-msg {
    font-size: 13px;
    font-weight: 500;
    margin-bottom: 4px;
  }

  .commit-meta {
    display: flex;
    gap: 8px;
    font-size: 12px;
    color: var(--text-muted);
    align-items: center;
  }

  .commit-sha {
    font-size: 11px;
    background: var(--bg-tertiary);
    padding: 1px 6px;
    border-radius: 4px;
    color: var(--accent);
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
    padding: 10px 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-bottom: none;
    border-radius: var(--radius) var(--radius) 0 0;
    font-size: 13px;
    font-weight: 600;
  }

  .readme-body {
    background: var(--bg-secondary);
    border: none;
    border-radius: 0;
    padding: 32px;
    max-height: 80vh;
    overflow-y: auto;
  }

  .markdown-body {
    line-height: 1.7;
    color: var(--text-primary);
  }

  .md-h1 {
    font-size: 28px;
    font-weight: 700;
    margin: 0 0 16px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border);
  }

  .md-h2 {
    font-size: 22px;
    font-weight: 600;
    margin: 24px 0 12px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--border-light);
  }

  .md-h3 {
    font-size: 18px;
    font-weight: 600;
    margin: 16px 0 8px;
  }

  .md-p {
    margin: 0 0 8px;
    font-size: 14px;
  }

  .md-li {
    margin: 2px 0 2px 20px;
    font-size: 14px;
  }

  .md-hr {
    border: none;
    border-top: 1px solid var(--border);
    margin: 12px 0;
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
