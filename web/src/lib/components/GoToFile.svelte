<script lang="ts">
  import { goto } from '$app/navigation';
  import { repos } from '$lib/api/client.svelte';
  import { findFiles, highlightRuns, type FileMatch } from '$lib/fileFinder';
  import { createT } from '$lib/i18n';

  /**
   * GitHub's "Go to file": type part of a path, pick a file. The paths are
   * read once, on first focus. Pressing `t` anywhere outside a text field
   * focuses it; the last choice searches the code for what was typed.
   */
  interface Props {
    owner: string;
    repo: string;
    /** The ref the page shows, `''` for the default branch. */
    ref: string;
  }

  let { owner, repo, ref }: Props = $props();
  const t = createT();

  let input = $state<HTMLInputElement | null>(null);
  let query = $state('');
  let open = $state(false);
  let paths = $state<string[] | null>(null);
  let complete = $state(true);
  let loading = $state(false);
  let failed = $state(false);
  let active = $state(0);
  let loadedFor = '';

  let matches = $derived<FileMatch[]>(paths && query.trim() ? findFiles(paths, query) : []);
  /** The file matches, then "search the code" when something was typed. */
  let choiceCount = $derived(matches.length + (query.trim() ? 1 : 0));

  const listId = `go-to-file-${Math.random().toString(36).slice(2, 9)}`;

  function encodeRepoPath(path: string): string {
    return path.split('/').map(encodeURIComponent).join('/');
  }

  function blobHref(path: string): string {
    return `/${owner}/${repo}/blob/${encodeRepoPath(path)}${ref ? `?ref=${encodeURIComponent(ref)}` : ''}`;
  }

  function searchHref(): string {
    const terms = [query.trim(), `repo:${owner}/${repo}`].filter(Boolean).join(' ');
    return `/search?q=${encodeURIComponent(terms)}&type=code`;
  }

  async function load() {
    const key = `${owner}/${repo}@${ref}`;
    if (loadedFor === key && (paths || loading)) return;
    loadedFor = key;
    loading = true;
    failed = false;
    try {
      const listing = await Promise.resolve().then(() => repos.files(owner, repo, ref || undefined));
      if (loadedFor !== key) return;
      paths = listing?.paths ?? [];
      complete = listing?.complete ?? true;
    } catch {
      if (loadedFor === key) failed = true;
    } finally {
      if (loadedFor === key) loading = false;
    }
  }

  function choose(index: number) {
    const target = index < matches.length ? blobHref(matches[index].path) : searchHref();
    open = false;
    query = '';
    input?.blur();
    void goto(target);
  }

  function onkeydown(event: KeyboardEvent) {
    if (event.key === 'Escape') {
      open = false;
      input?.blur();
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      if (!choiceCount) return;
      event.preventDefault();
      open = true;
      active = (active + (event.key === 'ArrowDown' ? 1 : -1) + choiceCount) % choiceCount;
    } else if (event.key === 'Enter') {
      if (!choiceCount) return;
      event.preventDefault();
      choose(Math.min(active, choiceCount - 1));
    }
  }

  /** `t` outside a text field, as on GitHub. */
  function onWindowKeydown(event: KeyboardEvent) {
    if (event.key !== 't' || event.metaKey || event.ctrlKey || event.altKey || event.defaultPrevented) return;
    const target = event.target as HTMLElement | null;
    if (target?.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"]')) return;
    event.preventDefault();
    input?.focus();
  }

  $effect(() => {
    void query;
    active = 0;
  });
</script>

<svelte:window onkeydown={onWindowKeydown} />

<div class="go-to-file">
  <input
    bind:this={input}
    type="search"
    class="go-to-file-input"
    bind:value={query}
    placeholder={t('repo.go_to_file.placeholder', 'Go to file')}
    aria-label={t('repo.go_to_file.placeholder', 'Go to file')}
    role="combobox"
    aria-expanded={open && !!query.trim()}
    aria-controls={listId}
    aria-autocomplete="list"
    onfocus={() => { open = true; void load(); }}
    onblur={() => { open = false; }}
    oninput={() => { open = true; }}
    {onkeydown}
  />
  <kbd class="go-to-file-key" aria-hidden="true">t</kbd>
  {#if open && query.trim()}
    <!-- Items are picked with mousedown so the input's blur does not close
         the list before the click lands. -->
    <ul class="go-to-file-results" id={listId} role="listbox">
      {#if loading && !paths}
        <li class="go-to-file-note">{t('common.loading')}</li>
      {:else if failed}
        <li class="go-to-file-note">{t('repo.go_to_file.failed', 'The file list could not be loaded.')}</li>
      {:else if !matches.length}
        <li class="go-to-file-note">{t('repo.go_to_file.none', 'No matching files')}</li>
      {/if}
      {#each matches as match, index (match.path)}
        <li
          role="option"
          aria-selected={index === active}
          class="go-to-file-item"
          class:active={index === active}
          onmousedown={(event) => { event.preventDefault(); choose(index); }}
        >
          {#each highlightRuns(match.path, match.positions) as run}
            {#if run.hit}<mark>{run.text}</mark>{:else}{run.text}{/if}
          {/each}
        </li>
      {/each}
      <li
        role="option"
        aria-selected={active === matches.length}
        class="go-to-file-item go-to-file-search"
        class:active={active === matches.length}
        onmousedown={(event) => { event.preventDefault(); choose(matches.length); }}
      >
        {t('repo.go_to_file.search_code', { query: query.trim() }, 'Search code for “{query}”')}
      </li>
      {#if paths && !complete}
        <li class="go-to-file-note">{t('repo.go_to_file.partial', { count: paths.length }, 'Only the first {count} files are searched.')}</li>
      {/if}
    </ul>
  {/if}
</div>

<style>
  .go-to-file { position: relative; display: flex; align-items: center; flex: 1 1 220px; min-width: 0; }
  .go-to-file-input { width: 100%; min-width: 0; padding-right: 32px; }
  .go-to-file-key {
    position: absolute;
    right: 8px;
    padding: 0 5px;
    border: 1px solid var(--border);
    border-radius: 4px;
    font-size: 11px;
    line-height: 16px;
    color: var(--text-muted);
    pointer-events: none;
  }
  .go-to-file-results {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    right: 0;
    z-index: 20;
    margin: 0;
    padding: 4px 0;
    list-style: none;
    max-height: 360px;
    overflow-y: auto;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.15);
  }
  .go-to-file-item {
    padding: 6px 12px;
    font-size: 13px;
    font-family: var(--font-mono);
    overflow-wrap: anywhere;
    cursor: pointer;
  }
  .go-to-file-item.active { background: var(--bg-hover); }
  .go-to-file-item mark { background: none; color: var(--accent); font-weight: 700; }
  .go-to-file-search { font-family: var(--font-sans); border-top: 1px solid var(--border-light); }
  .go-to-file-note { padding: 6px 12px; font-size: 12px; color: var(--text-muted); }
</style>
