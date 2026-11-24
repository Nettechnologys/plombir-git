<script lang="ts">
  import Dropdown from '$lib/components/Dropdown.svelte';
  import { createT } from '$lib/i18n';

  /**
   * GitHub's branch selector: one menu for branches and tags, with a filter
   * and links to the full lists. `current` is the ref the page shows, `''`
   * for the default branch.
   */
  interface Props {
    owner: string;
    repo: string;
    branches: { name: string; is_default?: boolean }[];
    tags: { name: string }[];
    current: string;
    label: string;
    onselect: (ref: string) => void;
  }

  let { owner, repo, branches, tags, current, label, onselect }: Props = $props();
  const t = createT();

  let filter = $state('');
  const LIMIT = 50;

  function matching(names: string[]): string[] {
    const needle = filter.trim().toLowerCase();
    return names.filter((name) => name.toLowerCase().includes(needle)).slice(0, LIMIT);
  }
  let branchMatches = $derived(matching(branches.map((b) => b.name)));
  let tagMatches = $derived(matching(tags.map((tag) => tag.name)));
  let defaultBranch = $derived(branches.find((b) => b.is_default)?.name);
  let selected = $derived(current || defaultBranch || '');

  function choose(name: string, close: () => void) {
    filter = '';
    onselect(name);
    close();
  }
</script>

<Dropdown ariaLabel={t('repo.select_branch')} triggerClass="btn btn-outline ref-trigger" placement="left" fitViewport>
  {#snippet trigger()}
    <svg class="ref-icon" viewBox="0 0 16 16" width="16" height="16" aria-hidden="true">
      <circle cx="4.5" cy="3.5" r="1.75" /><circle cx="4.5" cy="12.5" r="1.75" /><circle cx="11.5" cy="5" r="1.75" />
      <path d="M4.5 5.25v5.5M11.5 6.75c0 2.5-2.25 3-7 4" />
    </svg>
    <span class="ref-name" title={label}>{label}</span><span aria-hidden="true">▾</span>
  {/snippet}
  {#snippet menu(close)}
    <div class="ref-menu">
      <div class="ref-menu-title">{t('repo.refs.switch', 'Switch branches/tags')}</div>
      <!-- The filter is the first stop, so typing narrows the list at once;
           the arrow keys go on into the refs. -->
      <input
        type="search"
        class="ref-filter"
        tabindex="0"
        bind:value={filter}
        placeholder={t('repo.refs.find', 'Find a branch or tag...')}
        aria-label={t('repo.refs.find', 'Find a branch or tag...')}
        onkeydown={(event) => {
          const first = branchMatches[0] ?? tagMatches[0];
          if (event.key === 'Enter' && first) {
            event.preventDefault();
            choose(first, close);
          }
        }}
      />
      <div class="ref-list">
        {#if branchMatches.length}
          <div class="ref-group">{t('repo.branches.title')}</div>
          {#each branchMatches as name (name)}
            <button type="button" class="dropdown-item ref-item" class:active={name === selected} role="menuitem" onclick={() => choose(name, close)}>
              <span class="ref-check" aria-hidden="true">{name === selected ? '✓' : ''}</span>
              <span class="ref-item-name">{name}</span>
              {#if name === defaultBranch}
                <span class="ref-default">{t('repo.refs.default', 'default')}</span>
              {/if}
            </button>
          {/each}
        {/if}
        {#if tagMatches.length}
          <div class="ref-group">{t('repo.tags.title')}</div>
          {#each tagMatches as name (name)}
            <button type="button" class="dropdown-item ref-item" class:active={name === selected} role="menuitem" onclick={() => choose(name, close)}>
              <span class="ref-check" aria-hidden="true">{name === selected ? '✓' : ''}</span>
              <span class="ref-item-name">{name}</span>
            </button>
          {/each}
        {/if}
        {#if !branchMatches.length && !tagMatches.length}
          <p class="ref-empty">{t('repo.refs.nothing', 'Nothing to show')}</p>
        {/if}
      </div>
      <div class="ref-all">
        <a href={`/${owner}/${repo}/branches`} onclick={close}>{t('repo.refs.all_branches', 'View all branches')}</a>
        <a href={`/${owner}/${repo}/tags`} onclick={close}>{t('repo.refs.all_tags', 'View all tags')}</a>
      </div>
    </div>
  {/snippet}
</Dropdown>

<style>
  :global(.ref-trigger) { gap: 6px; }
  .ref-icon { flex-shrink: 0; fill: none; stroke: currentColor; stroke-width: 1.3; }
  .ref-name { min-width: 0; max-width: 200px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; text-align: left; }
  .ref-menu { display: flex; flex-direction: column; width: 300px; max-width: 100%; }
  .ref-menu-title { padding: 8px 12px; font-size: 12px; font-weight: 600; border-bottom: 1px solid var(--border-light); }
  .ref-filter { margin: 8px 12px; width: auto; }
  .ref-group { padding: 6px 12px 2px; font-size: 12px; font-weight: 600; color: var(--text-muted); }
  .ref-list { max-height: 280px; overflow-y: auto; }
  .ref-item { display: flex; align-items: center; gap: 8px; width: 100%; text-align: left; }
  .ref-check { width: 14px; flex-shrink: 0; color: var(--accent); }
  .ref-item-name { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .ref-default { font-size: 11px; padding: 0 6px; border: 1px solid var(--border); border-radius: 10px; color: var(--text-secondary); }
  .ref-empty { padding: 8px 12px; margin: 0; color: var(--text-muted); font-size: 13px; }
  .ref-all { display: flex; justify-content: space-between; gap: 8px; padding: 8px 12px; border-top: 1px solid var(--border-light); font-size: 13px; }
  .ref-all a { color: var(--accent); }
</style>
