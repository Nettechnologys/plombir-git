<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { createT } from '$lib/i18n';
  import { search, type SearchResult } from '$lib/api/client.svelte';
  import { highlightText } from '$lib/utils/search';
  import { onMount } from 'svelte';

  const t = createT();

  let query = $state('');
  let activeType = $state('all');
  let loading = $state(false);
  let results = $state<SearchResult[]>([]);
  let total = $state(0);
  let currentPage = $state(1);
  let searchError = $state('');
  let perPage = 20;
  let hasSearched = $state(false);
  let showHelp = $state(false);

  let totalPages = $derived(Math.ceil(total / perPage) || 1);
  let hasNext = $derived(currentPage < totalPages);
  let hasPrev = $derived(currentPage > 1);

  function normalizeSearchType(type: string | null): string {
    if (type === 'repo') return 'repos';
    if (type === 'issue') return 'issues';
    if (type === 'repos' || type === 'issues' || type === 'wiki' || type === 'all') return type;
    return 'all';
  }

  // Sync from URL on mount and on URL changes
  $effect(() => {
    const url = $page.url;
    const q = url.searchParams.get('q') || '';
    const type = normalizeSearchType(url.searchParams.get('type'));
    const pg = parseInt(url.searchParams.get('page') || '1', 10);

    if (q !== query || type !== activeType || pg !== currentPage) {
      query = q;
      activeType = type;
      currentPage = pg;
      if (q) {
        performSearch(q, type, pg);
      }
    }
  });

  // Keyboard shortcut: Ctrl+K or Cmd+K to focus search
  onMount(() => {
    function handleKeyboard(e: KeyboardEvent) {
      if ((e.ctrlKey || e.metaKey) && e.key === 'k') {
        e.preventDefault();
        const input = document.querySelector('.search-input') as HTMLInputElement;
        if (input) input.focus();
      }
    }
    window.addEventListener('keydown', handleKeyboard);
    return () => window.removeEventListener('keydown', handleKeyboard);
  });

  async function performSearch(q: string, type: string, pg: number) {
    try {
      loading = true;
      hasSearched = true;
      searchError = '';
      const response = await search.search(q, type, pg, perPage);
      results = response.results;
      total = response.total;
      currentPage = response.page;
    } catch (err: any) {
      results = [];
      total = 0;
      searchError = err?.message || t('search.load_failed', 'Search failed');
    } finally {
      loading = false;
    }
  }

  function doSearch() {
    if (!query.trim()) return;
    currentPage = 1;
    goto(`/search?q=${encodeURIComponent(query.trim())}&type=${activeType}`);
  }

  function setType(type: string) {
    activeType = type;
    currentPage = 1;
    if (query.trim()) {
      goto(`/search?q=${encodeURIComponent(query.trim())}&type=${type}`);
    }
  }

  function goPage(pg: number) {
    if (pg < 1 || pg > totalPages) return;
    goto(`/search?q=${encodeURIComponent(query.trim())}&type=${activeType}&page=${pg}`);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      doSearch();
    }
  }

  function toggleHelp() {
    showHelp = !showHelp;
  }

  function keyboardHintParts() {
    const hint = t('search.keyboard_hint', 'Tip: Press Ctrl+K to focus search');
    const shortcut = 'Ctrl+K';
    const index = hint.indexOf(shortcut);

    if (index === -1) {
      return { before: hint, shortcut: '', after: '' };
    }

    return {
      before: hint.slice(0, index),
      shortcut,
      after: hint.slice(index + shortcut.length)
    };
  }

  // Get issue state badge color
  function getStateBadgeClass(state: string | null | undefined): string {
    if (!state) return '';
    if (state === 'open') return 'state-badge-open';
    if (state === 'closed') return 'state-badge-closed';
    if (state === 'merged') return 'state-badge-merged';
    return '';
  }

  function resultRepoName(result: SearchResult): string {
    return result.repo_name || result.title || '';
  }

  function repoHref(result: SearchResult): string {
    return `/${result.repo_owner || ''}/${resultRepoName(result)}`;
  }

  function stateLabel(state: string | null | undefined): string {
    return state ? t(`issues.state.${state}`, state) : '';
  }
</script>

<svelte:head>
  <title>{query ? `${query} · ` : ''}{t('search.title')} · IronForge</title>
</svelte:head>

<div class="page-container search-page">
  <div class="search-header">
    <h1>{t('search.title')}</h1>
    <div class="search-box">
      <div class="search-input-wrapper">
        <svg class="search-icon" viewBox="0 0 16 16" width="16" height="16" fill="currentColor">
          <path d="M11.5 7a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0Zm-.82 4.74a6 6 0 1 1 1.06-1.06l3.04 3.04a.75.75 0 1 1-1.06 1.06l-3.04-3.04Z"/>
        </svg>
        <input
          type="text"
          class="search-input"
          bind:value={query}
          onkeydown={handleKeydown}
          placeholder={t('search.placeholder')}
        />
        <button class="search-btn btn btn-primary" onclick={doSearch}>{t('search.search_button')}</button>
      </div>
      <button class="help-btn" onclick={toggleHelp} title={t('search.help_title') || 'Search help'}>
        ?
      </button>
    </div>

    {#if showHelp}
      <div class="search-help">
        <h3>{t('search.help_title') || 'Search Tips'}</h3>
        <p>{t('search.help_desc') || 'Use qualifiers to refine your search:'}</p>
        <div class="help-qualifiers">
          <div class="help-item">
            <code>repo:owner/name</code>
            <span>{t('search.help_repo') || 'Search in a specific repository'}</span>
          </div>
          <div class="help-item">
            <code>author:username</code>
            <span>{t('search.help_author') || 'Search by author'}</span>
          </div>
          <div class="help-item">
            <code>state:open|closed|all</code>
            <span>{t('search.help_state') || 'Filter by issue state'}</span>
          </div>
          <div class="help-item">
            <code>label:name</code>
            <span>{t('search.help_label') || 'Filter by label'}</span>
          </div>
        </div>
        <p class="help-tip">{t('search.help_tip') || 'Example: bug fix repo:owner/name state:open'}</p>
      </div>
    {/if}

    <div class="type-tabs">
      {#each [
        { key: 'all', label: t('search.all') },
        { key: 'repos', label: t('search.repos') },
        { key: 'issues', label: t('search.issues') },
        { key: 'wiki', label: t('search.wiki') }
      ] as tab}
        <button
          class="type-tab"
          class:active={activeType === tab.key}
          onclick={() => setType(tab.key)}
        >
          {tab.label}
        </button>
      {/each}
    </div>
  </div>

  <div class="search-body">
    {#if loading}
      <div class="loading">
        <div class="spinner"></div>
        <span>{t('common.loading')}</span>
      </div>
    {:else if searchError}
      <div class="error-state">
        <p>{searchError}</p>
      </div>
    {:else if !hasSearched}
      <div class="empty-state">
        <svg class="empty-icon" viewBox="0 0 16 16" width="48" height="48" fill="currentColor">
          <path d="M11.5 7a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0Zm-.82 4.74a6 6 0 1 1 1.06-1.06l3.04 3.04a.75.75 0 1 1-1.06 1.06l-3.04-3.04Z"/>
        </svg>
        <p>{t('search.placeholder')}</p>
      </div>
    {:else if results.length === 0}
      <div class="empty-state">
        <p>{t('search.no_results')}</p>
      </div>
    {:else}
      <div class="results-info">
        {t('search.results_count', { total })}
      </div>
      <div class="results-list gh-list">
        {#each results as result (result.result_type + '-' + result.id)}
          {#if result.result_type === 'repo'}
            <a href={repoHref(result)} class="result-card repo-card gh-list-item">
              <div class="result-body">
                <div class="repo-name-row">
                  <svg viewBox="0 0 16 16" width="16" height="16" fill="currentColor" class="type-icon">
                    <path d="M2 2.5A2.5 2.5 0 0 1 4.5 0h8.75a.75.75 0 0 1 .75.75v12.5a.75.75 0 0 1-.75.75h-2.5a.75.75 0 0 1 0-1.5h1.75v-2h-8a1 1 0 0 0-.714 1.7.75.75 0 1 1-1.072 1.05A2.495 2.495 0 0 1 2 11.5Zm10.5-1h-8a1 1 0 0 0-1 1v6.708A2.486 2.486 0 0 1 4.5 9h8ZM5 12.25a.25.25 0 0 1 .25-.25h3.5a.25.25 0 0 1 .25.25v3.25a.25.25 0 0 1-.4.2l-1.45-1.087a.25.25 0 0 0-.3 0L5.4 15.7a.25.25 0 0 1-.4-.2Z"/>
                  </svg>
                  <span class="result-title">{result.repo_owner}/{resultRepoName(result)}</span>
                  <span class="result-kind">{t('search.repo_result')}</span>
                </div>
                {#if result.excerpt}
                  <div class="result-excerpt">{@html highlightText(result.excerpt || '', query)}</div>
                {/if}
              </div>
            </a>
          {:else if result.result_type === 'issue'}
            <a href={`/${result.repo_owner}/${result.repo_name}/issues/${result.number}`} class="result-card issue-card gh-list-item">
              <div class="result-body">
                <div class="issue-header">
                  {#if result.state}
                    <span class="issue-state-badge {getStateBadgeClass(result.state)}">{stateLabel(result.state)}</span>
                  {/if}
                  <span class="issue-badge">#{result.number}</span>
                  <span class="result-title">{@html highlightText(result.title || '', query)}</span>
                </div>
                <div class="result-meta">
                  <span class="repo-path">{result.repo_owner}/{result.repo_name}</span>
                </div>
                {#if result.excerpt}
                  <div class="result-excerpt">{@html highlightText(result.excerpt || '', query)}</div>
                {/if}
              </div>
            </a>
          {:else if result.result_type === 'wiki'}
            <a href={`/${result.repo_owner}/${result.repo_name}/wiki/${encodeURIComponent(result.title)}`} class="result-card wiki-card gh-list-item">
              <div class="result-body">
                <div class="wiki-header">
                  <svg viewBox="0 0 16 16" width="16" height="16" fill="currentColor" class="type-icon">
                    <path d="M0 1.75A.75.75 0 0 1 .75 1h4.253c1.227 0 2.317.59 3 1.501A3.743 3.743 0 0 1 11.006 1h4.245a.75.75 0 0 1 .75.75v10.5a.75.75 0 0 1-.75.75h-4.507a2.25 2.25 0 0 0-1.591.659l-.622.621a.75.75 0 0 1-1.06 0l-.622-.621A2.25 2.25 0 0 0 5.258 13H.75a.75.75 0 0 1-.75-.75Zm7.251 10.324.004-7.073-.002.003A2.25 2.25 0 0 0 5.003 4.5H1.5v7.5h3.757a3.75 3.75 0 0 1 1.994.574Zm.004-8.073-.001.002-.003.002V12.7A3.75 3.75 0 0 1 12.493 12H14.5V4.5h-3.497a2.25 2.25 0 0 0-2.244 2.5Zm-1.504 8.073H1.5v1h3.757a3.75 3.75 0 0 1 1.994.574v-1.574Zm8.254-8.073H14.5v1h-3.497a2.25 2.25 0 0 0-2.244 2.5V4.5Z"/>
                  </svg>
                  <span class="result-title">{@html highlightText(result.title || '', query)}</span>
                </div>
                <div class="result-meta">
                  <span class="repo-path">{result.repo_owner}/{result.repo_name}</span>
                </div>
                {#if result.excerpt}
                  <div class="result-excerpt">{@html highlightText(result.excerpt || '', query)}</div>
                {/if}
              </div>
            </a>
          {/if}
        {/each}
      </div>

      {#if totalPages > 1}
        <div class="pagination">
          <button class="page-btn" disabled={!hasPrev} onclick={() => goPage(currentPage - 1)}>&larr; {t('common.previous')}</button>
          <span class="page-info">{t('search.page_info', { page: currentPage, total: totalPages })}</span>
          <button class="page-btn" disabled={!hasNext} onclick={() => goPage(currentPage + 1)}>{t('common.next')} &rarr;</button>
        </div>
      {/if}
    {/if}
  </div>

<div class="keyboard-hint">
  {keyboardHintParts().before}{#if keyboardHintParts().shortcut}<kbd>{keyboardHintParts().shortcut}</kbd>{/if}{keyboardHintParts().after}
</div>
</div>

<style>

  .search-header {
    margin-bottom: 24px;
  }

  h1 {
    font-size: 24px;
    margin-bottom: 20px;
  }

  .search-box {
    margin-bottom: 16px;
  }

  .search-input-wrapper {
    display: flex;
    align-items: center;
    gap: 0;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    overflow: hidden;
  }

  .search-input-wrapper:focus-within {
    border-color: var(--accent);
  }

  .search-icon {
    flex-shrink: 0;
    margin-left: 12px;
    color: var(--text-muted);
  }

  .search-input {
    flex: 1;
    background: none;
    border: none;
    outline: none;
    padding: 12px;
    font-size: 15px;
    color: var(--text-primary);
  }

  .search-input::placeholder {
    color: var(--text-muted);
  }

  .search-btn {
    flex-shrink: 0;
    border-radius: 0 var(--radius) var(--radius) 0;
  }

  .help-btn {
    width: 36px;
    height: 36px;
    margin-left: 8px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 50%;
    color: var(--text-secondary);
    font-size: 16px;
    font-weight: 600;
    cursor: pointer;
    transition: all 0.15s;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .help-btn:hover {
    background: var(--bg-hover);
    color: var(--text-primary);
    border-color: var(--accent);
  }

  .search-help {
    margin-top: 16px;
    padding: 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 13px;
    color: var(--text-secondary);
  }

  .search-help h3 {
    font-size: 15px;
    font-weight: 600;
    color: var(--text-primary);
    margin-bottom: 8px;
  }

  .search-help p {
    margin-bottom: 12px;
    line-height: 1.5;
  }

  .help-qualifiers {
    display: flex;
    flex-direction: column;
    gap: 8px;
    margin-bottom: 12px;
  }

  .help-item {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .help-item code {
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 2px 6px;
    font-size: 12px;
    color: var(--accent);
    white-space: nowrap;
  }

  .help-item span {
    color: var(--text-secondary);
  }

  .help-tip {
    font-size: 12px;
    color: var(--text-muted);
    padding: 8px;
    background: var(--bg-primary);
    border-radius: 4px;
  }

  .issue-state-badge {
    display: inline-flex;
    align-items: center;
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 12px;
    font-weight: 600;
    flex-shrink: 0;
  }

  .state-badge-open {
    background: #2da44e20;
    color: #2da44e;
    border: 1px solid #2da44e40;
  }

  .state-badge-closed {
    background: #8250df20;
    color: #8250df;
    border: 1px solid #8250df40;
  }

  .state-badge-merged {
    background: #826b0020;
    color: #826b00;
    border: 1px solid #826b0040;
  }

  .keyboard-hint {
    font-size: 12px;
    color: var(--text-muted);
    margin-top: 8px;
    text-align: center;
  }

  .keyboard-hint kbd {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 2px 6px;
    font-size: 11px;
    font-family: monospace;
  }

  .type-tabs {
    display: flex;
    gap: 0;
    border-bottom: 1px solid var(--border);
  }

  .type-tab {
    padding: 8px 16px;
    background: none;
    border: none;
    border-bottom: 2px solid transparent;
    color: var(--text-secondary);
    font-size: 14px;
    cursor: pointer;
    transition: color 0.15s, border-color 0.15s;
  }

  .type-tab:hover {
    color: var(--text-primary);
  }

  .type-tab.active {
    color: var(--accent);
    border-bottom-color: var(--accent);
    font-weight: 600;
  }

  .search-body {
    min-height: 200px;
  }

  .loading {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 40px 0;
    color: var(--text-secondary);
    justify-content: center;
  }

  .spinner {
    width: 20px;
    height: 20px;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .empty-state {
    text-align: center;
    padding: 60px 24px;
    color: var(--text-secondary);
  }

  .empty-icon {
    color: var(--text-muted);
    margin-bottom: 16px;
  }

  .empty-state p {
    font-size: 15px;
  }

  .error-state {
    padding: 16px;
    color: var(--red);
    background: color-mix(in srgb, var(--red) 10%, transparent);
    border: 1px solid color-mix(in srgb, var(--red) 30%, transparent);
    border-radius: var(--radius);
  }

  .error-state p {
    margin: 0;
    font-size: 14px;
  }

  .results-info {
    font-size: 13px;
    color: var(--text-muted);
    margin-bottom: 12px;
  }

  .results-list { margin-bottom: 16px; }

  .result-card {
    padding: 14px 16px;
    color: var(--text-primary);
    text-decoration: none;
  }

  .result-card:hover {
    background: var(--bg-hover);
    text-decoration: none;
  }

  .result-body {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .type-icon {
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .repo-name-row,
  .issue-header,
  .wiki-header {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .result-title {
    font-weight: 600;
    font-size: 15px;
    color: var(--accent);
  }

  .result-kind {
    color: var(--text-muted);
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 11px;
    font-weight: 500;
    line-height: 18px;
    padding: 0 7px;
    margin-left: auto;
  }

  .issue-badge {
    font-size: 13px;
    font-weight: 600;
    color: var(--text-muted);
    flex-shrink: 0;
  }

  .result-meta {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .repo-path {
    font-size: 13px;
    color: var(--text-secondary);
  }

  .result-desc {
    font-size: 13px;
    color: var(--text-secondary);
  }

  .result-excerpt {
    font-size: 13px;
    color: var(--text-muted);
    margin-top: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    line-clamp: 2;
    -webkit-line-clamp: 2;
    -webkit-box-orient: vertical;
  }

  .pagination {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 16px;
    padding: 24px 0;
  }

  .page-btn {
    padding: 6px 14px;
    background: var(--bg-secondary);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 13px;
    cursor: pointer;
  }

  .page-btn:hover:not(:disabled) {
    background: var(--bg-hover);
  }

  .page-btn:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  .page-info {
    font-size: 13px;
    color: var(--text-muted);
  }

  :global(.search-highlight) {
    background: #fef08a;
    color: #1a1a2e;
    padding: 1px 3px;
    border-radius: 2px;
    font-weight: 600;
  }
</style>
