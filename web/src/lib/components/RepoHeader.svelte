<script lang="ts">
  import { createT } from '$lib/i18n';
  import { getUser, isLoggedIn } from '$lib/stores/auth.svelte';
  import { repos } from '$lib/api/client.svelte';
  import { buildSshCloneUrl, downloadApiFile, withBackendBase } from '$lib/api/_base';
  import { goto } from '$app/navigation';
  import { browser } from '$app/environment';

  const t = createT();

  interface Props {
    owner: string;
    repo: string;
    activeTab?: string;
    starsCount?: number;
    defaultBranch?: string;
  }

  let { owner, repo, activeTab = 'code', starsCount = 0, defaultBranch }: Props = $props();

  // Action button states
  let starred = $state(false);
  let watchState = $state<'not_watching' | 'watching' | 'ignoring'>('not_watching');
  let forking = $state(false);
  let starsLocalCount = $state(0);
  let archiveRef = $state('main');
  let downloadingArchive = $state(false);
  let forkError = $state('');
  let forkSuccess = $state('');
  let currentUsername = $derived(getUser()?.username || '');
  let isOwnRepo = $derived(Boolean(currentUsername) && currentUsername === owner);

  // Sync when prop changes
  $effect(() => {
    starsLocalCount = starsCount;
    if (!isLoggedIn()) {
      starred = false;
    }
  });

  // Clone URLs
  let showClone = $state(false);
  let cloneTab = $state<'http' | 'ssh'>('http');
  let httpCopied = $state(false);
  let sshCopied = $state(false);

  let httpCloneUrl = $derived(withBackendBase(`/git/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}`));
  let sshCloneUrl = $derived(browser ? buildSshCloneUrl(owner, repo, location.hostname) : '');

  function copyUrl(url: string) {
    navigator.clipboard.writeText(url);
    if (cloneTab === 'http') {
      httpCopied = true;
      setTimeout(() => httpCopied = false, 2000);
    } else {
      sshCopied = true;
      setTimeout(() => sshCopied = false, 2000);
    }
  }

  function closeClone() {
    showClone = false;
  }

  function handleCloneClick() {
    showClone = !showClone;
  }

  function handleCloneKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && showClone) {
      e.preventDefault();
      closeClone();
    }
  }

  $effect(() => {
    if (showClone) {
      document.getElementById('clone-dropdown')?.focus();
    }
  });

  // Check auth and load initial states
  $effect(() => {
    if (isLoggedIn()) {
      loadStates();
    }
  });

  $effect(() => {
    archiveRef = defaultBranch || 'main';
    if (!defaultBranch) {
      loadArchiveRef();
    }
  });

  async function loadArchiveRef() {
    const expectedOwner = owner;
    const expectedRepo = repo;
    try {
      const repoInfo = await repos.get(expectedOwner, expectedRepo);
      if (owner === expectedOwner && repo === expectedRepo && repoInfo.default_branch) {
        archiveRef = repoInfo.default_branch;
      }
    } catch {
      archiveRef = defaultBranch || 'main';
    }
  }

  async function loadStates() {
    try {
      const stateRes = await repos.starred(owner, repo);
      starred = stateRes.starred;
    } catch {
      starred = false;
    }
    try {
      const watchRes = await repos.watchStatus(owner, repo);
      watchState = watchRes.watch_state;
    } catch {
      watchState = 'not_watching';
    }
  }

  async function downloadArchive() {
    try {
      downloadingArchive = true;
      await downloadApiFile(
        `/repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/archive/${encodeURIComponent(archiveRef)}.zip`,
        `${repo}-${archiveRef || 'archive'}.zip`
      );
    } finally {
      downloadingArchive = false;
    }
  }

  async function toggleStar() {
    if (!isLoggedIn()) return;

    const prevStarred = starred;
    const prevCount = starsLocalCount;

    // Optimistic update
    starred = !starred;
    starsLocalCount = starred ? starsLocalCount + 1 : starsLocalCount - 1;

    try {
      const res = await repos.star(owner, repo);
      starred = res.starred;
      starsLocalCount = starred ? prevCount + 1 : prevCount - 1;
    } catch {
      // Revert on error
      starred = prevStarred;
      starsLocalCount = prevCount;
    }
  }

  async function cycleWatch() {
    if (!isLoggedIn()) return;

    const states: Array<'not_watching' | 'watching' | 'ignoring'> = ['not_watching', 'watching', 'ignoring'];
    const currentIndex = states.indexOf(watchState);
    const nextState = states[(currentIndex + 1) % states.length];
    const prevState = watchState;

    // Optimistic update
    watchState = nextState;

    try {
      if (nextState === 'not_watching') {
        await repos.unwatch(owner, repo);
      } else {
        await repos.watch(owner, repo, nextState);
      }
    } catch {
      // Revert on error
      watchState = prevState;
    }
  }

  async function handleFork() {
    forkError = '';
    forkSuccess = '';

    if (!isLoggedIn()) {
      forkError = '请先登录后再复刻仓库。';
      return;
    }

    if (isOwnRepo) {
      forkError = '不能复刻自己名下的同名仓库。';
      return;
    }

    forking = true;

    try {
      const result = await repos.fork(owner, repo);
      const forkOwner = result?.owner?.username || result?.owner_name || result?.owner || getUser()?.username;
      const forkName = result?.name || repo;
      forkSuccess = '仓库复刻成功，正在跳转...';
      if (forkOwner) {
        goto(`/${forkOwner}/${forkName}`);
      }
    } catch (e: any) {
      forkError = e?.message || '复刻仓库失败。';
      forking = false;
    }
  }

  function getForkTitle() {
    if (!isLoggedIn()) return 'Login to fork';
    if (isOwnRepo) return '不能复刻自己名下的同名仓库';
    return forking ? t('repo.forking') : t('repo.fork');
  }

  function getWatchLabel() {
    switch (watchState) {
      case 'watching':
        return t('repo.watching');
      case 'ignoring':
        return t('repo.ignoring');
      default:
        return t('repo.watch');
    }
  }

  const tabs = $derived([
    { id: 'code', label: t('repo.tabs.code'), icon: '📁' },
    { id: 'issues', label: t('repo.tabs.issues'), icon: '◉' },
    { id: 'pulls', label: t('repo.tabs.pulls'), icon: '⑂' },
    { id: 'wiki', label: t('repo.tabs.wiki'), icon: '📖' },
    { id: 'pipelines', label: t('repo.tabs.pipelines'), icon: '▶' },
    { id: 'releases', label: t('repo.tabs.releases'), icon: '🏷' },
    { id: 'packages', label: t('repo.tabs.packages'), icon: '📦' },
    { id: 'board', label: t('repo.tabs.board'), icon: '◫', path: 'boards' },
    { id: 'time_tracking', label: t('repo.tabs.time_tracking'), icon: '⏱' },
    { id: 'commits', label: t('repo.tabs.commits'), icon: '📜' },
    { id: 'settings', label: t('repo.tabs.settings'), icon: '⚙' },
  ]);

  function tabHref(tab: { id: string; path?: string }) {
    const suffix = tab.id === 'code' ? '' : `/${tab.path || tab.id}`;
    return `/${owner}/${repo}${suffix}`;
  }
</script>

<svelte:window onkeydown={handleCloneKeydown} />

<div class="repo-header">
  <div class="repo-top">
    <div class="repo-name">
      <a href={`/${owner}`}>{owner}</a>
      <span class="separator">/</span>
      <a href={`/${owner}/${repo}`}>{repo}</a>
    </div>

    <div class="repo-actions">
      <button
        class="action-btn btn btn-outline btn-sm"
        class:starred={starred}
        class:btn-primary={starred}
        class:disabled={!isLoggedIn()}
        onclick={toggleStar}
        title={isLoggedIn() ? (starred ? t('repo.unstar') : t('repo.star')) : 'Login to star'}
        aria-label={isLoggedIn() ? (starred ? t('repo.unstar') : t('repo.star')) : 'Login to star'}
      >
        <span class="star-icon" aria-hidden="true">{starred ? '⭐' : '☆'}</span>
        <span class="count">{starsLocalCount}</span>
      </button>

      <button
        class="action-btn btn btn-outline btn-sm"
        class:watching={watchState !== 'not_watching'}
        class:ignoring={watchState === 'ignoring'}
        class:disabled={!isLoggedIn()}
        onclick={cycleWatch}
        title={isLoggedIn() ? getWatchLabel() : 'Login to watch'}
        aria-label={isLoggedIn() ? getWatchLabel() : 'Login to watch'}
      >
        <span class="watch-icon" aria-hidden="true">👁</span>
        <span class="label">{getWatchLabel()}</span>
      </button>

      <button
        class="action-btn btn btn-outline btn-sm fork-btn"
        class:loading={forking}
        class:disabled={!isLoggedIn() || isOwnRepo}
        onclick={handleFork}
        disabled={forking || !isLoggedIn()}
        title={getForkTitle()}
        aria-label={getForkTitle()}
      >
        <span class="fork-icon">⚡</span>
        <span class="label">{forking ? t('repo.forking') : t('repo.fork')}</span>
      </button>

      <div class="clone-area" style="position:relative">
        <button
          class="btn-code btn btn-primary"
          onclick={handleCloneClick}
          aria-haspopup="true"
          aria-expanded={showClone}
          aria-controls="clone-dropdown"
          aria-label={t('repo.clone_title')}
        >
          <span class="code-icon" aria-hidden="true">⌄</span>
          <span class="label">{t('repo.code')}</span>
          <span class="caret" aria-hidden="true">▾</span>
        </button>
        {#if showClone}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <!-- svelte-ignore a11y_no_static_element_interactions -->
          <div class="clone-backdrop" onclick={closeClone} role="presentation" tabindex="-1"></div>
          <div
            id="clone-dropdown"
            class="clone-dropdown"
            role="dialog"
            aria-label={t('repo.clone_title')}
            tabindex="-1"
          >
            <div class="clone-header">
              <span class="clone-title">{t('repo.clone_title')}</span>
            </div>

            <!-- Tab bar -->
            <div class="clone-tabs" role="tablist">
              <button
                class="clone-tab"
                class:active={cloneTab === 'http'}
                onclick={() => cloneTab = 'http'}
                role="tab"
                aria-selected={cloneTab === 'http'}
                aria-label="HTTPS clone"
              >HTTPS</button>
              <button
                class="clone-tab"
                class:active={cloneTab === 'ssh'}
                onclick={() => cloneTab = 'ssh'}
                role="tab"
                aria-selected={cloneTab === 'ssh'}
                aria-label="SSH clone"
              >SSH</button>
            </div>

            <!-- URL input with copy -->
            <div class="clone-input-row">
              <input
                type="text"
                class="clone-input"
                readonly
                value={cloneTab === 'http' ? httpCloneUrl : sshCloneUrl}
                aria-label={t('repo.clone_url')}
              />
              <button
                class="clone-copy-btn"
                onclick={() => copyUrl(cloneTab === 'http' ? httpCloneUrl : sshCloneUrl)}
                aria-label={t('repo.copy_clone_url')}
                title={t('repo.copy_clone_url')}
              >
                {#if (cloneTab === 'http' && httpCopied) || (cloneTab === 'ssh' && sshCopied)}
                  <span class="copied-check" aria-hidden="true">✓</span>
                {:else}
                  <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
                    <path fill-rule="evenodd" d="M0 6.75C0 5.784.784 5 1.75 5h1.5a.75.75 0 010 1.5h-1.5a.25.25 0 00-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 00.25-.25v-1.5a.75.75 0 011.5 0v1.5A1.75 1.75 0 019.25 16h-7.5A1.75 1.75 0 010 14.25v-7.5z"></path>
                    <path fill-rule="evenodd" d="M5 1.75C5 .784 5.784 0 6.75 0h7.5C15.216 0 16 .784 16 1.75v7.5A1.75 1.75 0 0114.25 11h-7.5A1.75 1.75 0 015 9.25v-7.5zm1.75-.25a.25.25 0 00-.25.25v7.5c0 .138.112.25.25.25h7.5a.25.25 0 00.25-.25v-7.5a.25.25 0 00-.25-.25h-7.5z"></path>
                  </svg>
                {/if}
              </button>
            </div>

            <!-- Download ZIP -->
            <div class="clone-footer">
              <button
                type="button"
                class="clone-footer-link"
                aria-label={t('repo.download_zip')}
                onclick={downloadArchive}
                disabled={downloadingArchive}
              >
                <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
                  <path fill-rule="evenodd" d="M2.75 14A1.75 1.75 0 011 12.25v-2.5a.75.75 0 011.5 0v2.5c0 .138.112.25.25.25h10.5a.25.25 0 00.25-.25v-2.5a.75.75 0 011.5 0v2.5A1.75 1.75 0 0113.25 14H2.75z"></path>
                  <path fill-rule="evenodd" d="M7.25 1.75A.75.75 0 018.75 1v6.69l1.47-1.47a.75.75 0 111.06 1.06l-2.75 2.75a.75.75 0 01-1.06 0L4.72 7.28a.75.75 0 011.06-1.06l1.47 1.47V1.75z"></path>
                </svg>
                {t('repo.download_zip')}
              </button>
            </div>
          </div>
        {/if}
      </div>
    </div>
  </div>

  {#if forkError}
    <div class="repo-action-message error" role="alert">{forkError}</div>
  {:else if forkSuccess}
    <div class="repo-action-message success" role="status">{forkSuccess}</div>
  {/if}

  <nav class="repo-tabs">
    {#each tabs as tab}
      <a
        href={tabHref(tab)}
        class="tab"
        class:active={activeTab === tab.id}
      >
        <span class="tab-icon">{tab.icon}</span>
        {tab.label}
      </a>
    {/each}
  </nav>
</div>

<style>
  .repo-header {
    border-bottom: 1px solid var(--border);
    padding-bottom: 0;
    margin-bottom: 24px;
  }

  .repo-top {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    flex-wrap: wrap;
    gap: 12px;
  }

  .repo-name {
    font-size: 20px;
    margin-bottom: 12px;
  }
  .repo-name a {
    color: var(--accent);
    font-weight: 600;
  }
  .separator {
    margin: 0 4px;
    color: var(--text-muted);
  }

  .repo-actions {
    display: flex;
    gap: 8px;
    align-items: center;
  }

  .action-btn {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    line-height: 1.2;
    white-space: nowrap;
  }

  .action-btn.disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .action-btn.starred {
    background: var(--accent-weak);
    border-color: var(--accent);
    color: var(--accent);
  }

  .action-btn.watching {
    background: var(--accent-weak);
    border-color: var(--accent);
    color: var(--accent);
  }

  .action-btn.ignoring {
    background: var(--bg-secondary);
    border-color: var(--border);
    color: var(--text-muted);
  }

  .fork-btn.loading {
    opacity: 0.8;
    cursor: wait;
  }

  .repo-action-message {
    margin: 4px 0 12px;
    padding: 8px 12px;
    border-radius: var(--radius);
    font-size: 13px;
  }

  .repo-action-message.error {
    background: rgba(248, 81, 73, 0.12);
    border: 1px solid var(--red-dim);
    color: var(--red);
  }

  .repo-action-message.success {
    background: rgba(63, 185, 80, 0.12);
    border: 1px solid rgba(63, 185, 80, 0.25);
    color: var(--green);
  }

  .star-icon,
  .watch-icon,
  .fork-icon {
    font-size: 16px;
  }

  .count {
    min-width: 20px;
    text-align: left;
  }

  .label {
    white-space: nowrap;
  }

  .repo-tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 0;
    overflow: visible;
  }

  .tab {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 8px 16px;
    color: var(--text-secondary);
    font-size: 14px;
    border-bottom: 2px solid transparent;
    text-decoration: none;
    white-space: nowrap;
  }
  .tab:hover {
    color: var(--text-primary);
    text-decoration: none;
  }
  .tab.active {
    color: var(--text-primary);
    font-weight: 600;
    border-bottom-color: var(--accent);
  }
  .tab-icon {
    font-size: 14px;
  }

  /* Clone dropdown — GitHub style */
  .clone-area {
    position: relative;
  }

  .btn-code {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 14px;
    background: var(--green-dim);
    color: #fff;
    border: 1px solid rgba(27,31,36,0.15);
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
    line-height: 20px;
  }
  .btn-code:hover {
    background: #1a7f37;
  }

  .code-icon {
    font-size: 14px;
    line-height: 1;
  }

  .caret {
    font-size: 10px;
    margin-left: 2px;
    opacity: 0.7;
  }

  .clone-backdrop {
    position: fixed;
    inset: 0;
    z-index: 299;
  }

  .clone-dropdown {
    position: absolute;
    top: 100%;
    right: 0;
    margin-top: 4px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    width: 340px;
    box-shadow: 0 8px 24px rgba(0,0,0,0.15);
    z-index: 300;
  }

  .clone-header {
    padding: 8px 12px;
    border-bottom: 1px solid var(--border);
  }

  .clone-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .clone-tabs {
    display: flex;
    border-bottom: 1px solid var(--border);
  }

  .clone-tab {
    flex: 1;
    padding: 8px 12px;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    font-size: 12px;
    font-weight: 500;
    color: var(--text-secondary);
    cursor: pointer;
  }
  .clone-tab:hover {
    color: var(--text-primary);
    background: var(--bg-secondary);
  }
  .clone-tab.active {
    color: var(--text-primary);
    font-weight: 600;
    border-bottom-color: var(--accent);
  }

  .clone-input-row {
    display: flex;
    padding: 8px 12px;
    gap: 0;
  }

  .clone-input {
    flex: 1;
    padding: 5px 8px;
    font-size: 12px;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    line-height: 20px;
    border: 1px solid var(--border);
    border-right: none;
    border-radius: 6px 0 0 6px;
    background: var(--bg-secondary);
    color: var(--text-primary);
    outline: none;
  }

  .clone-copy-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 5px 8px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-left: none;
    border-radius: 0 6px 6px 0;
    cursor: pointer;
    color: var(--text-secondary);
    min-width: 32px;
  }
  .clone-copy-btn:hover {
    background: var(--bg-hover);
    color: var(--text-primary);
  }

  .copied-check {
    color: var(--green);
    font-size: 14px;
    font-weight: 700;
  }

  .clone-footer {
    padding: 8px 12px;
    border-top: 1px solid var(--border);
  }

  .clone-footer-link {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0;
    border: 0;
    background: none;
    font-size: 12px;
    color: var(--accent);
    text-decoration: none;
    cursor: pointer;
  }
  .clone-footer-link:hover {
    text-decoration: underline;
  }
  .clone-footer-link:disabled {
    cursor: wait;
    opacity: 0.65;
  }

  @media (max-width: 600px) {
    .repo-top {
      flex-direction: column;
    }

    .repo-actions {
      width: 100%;
      justify-content: flex-start;
    }

    .action-btn {
      padding: 5px 10px;
      font-size: 13px;
    }
  }
</style>
