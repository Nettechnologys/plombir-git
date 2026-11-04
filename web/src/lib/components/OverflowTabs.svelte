<script lang="ts">
  import { onMount, tick } from 'svelte';
  import Dropdown from './Dropdown.svelte';
  import { visibleTabCount } from '$lib/overflowTabs';

  interface Tab {
    id: string;
    href: string;
    label: string;
    icon: string;
    count?: number;
  }

  interface Props {
    tabs: Tab[];
    activeTab: string;
    label: string;
    moreLabel: string;
    moreActiveLabel: string;
  }

  let { tabs, activeTab, label, moreLabel, moreActiveLabel }: Props = $props();
  let nav = $state<HTMLElement | null>(null);
  let measurements = $state<HTMLDivElement | null>(null);
  let visibleCount = $state<number | null>(null);
  let open = $state(false);
  let visible = $derived(tabs.slice(0, visibleCount ?? tabs.length));
  let hidden = $derived(tabs.slice(visibleCount ?? tabs.length));
  let hiddenActive = $derived(hidden.some(tab => tab.id === activeTab));
  let schedule = () => {};
  let pendingFocus: HTMLElement | null = null;

  $effect.pre(() => {
    tabs;
    const focused = document.activeElement as HTMLElement | null;
    if (nav && focused && nav.contains(focused)) pendingFocus = focused;
  });

  async function measure() {
    if (!nav || !measurements) return;
    const available = nav.getBoundingClientRect().width;
    // A detached/hidden container will be measured when it becomes visible.
    if (available <= 0) return;
    const samples = Array.from(measurements.querySelectorAll<HTMLElement>('[data-measure-tab]'));
    const more = measurements.querySelector<HTMLElement>('[data-measure-more]');
    if (!more || samples.length !== tabs.length) return;
    const next = visibleTabCount(samples.map(item => item.getBoundingClientRect().width), available, more.getBoundingClientRect().width);
    const currentFocus = document.activeElement as HTMLElement | null;
    const focused = currentFocus === document.body && pendingFocus ? pendingFocus : currentFocus;
    const ownedFocus = focused && (nav.contains(focused) || focused === pendingFocus);
    const focusedId = ownedFocus ? focused.closest<HTMLElement>('[data-tab-id]')?.dataset.tabId : undefined;
    const focusedMore = ownedFocus && focused?.classList.contains('overflow-trigger');
    pendingFocus = null;
    if (next === tabs.length) open = false;
    if (next === visibleCount && focused?.isConnected) return;
    visibleCount = next;
    await tick();

    // Moving between the row and the menu replaces a link. Preserve keyboard
    // focus only when this navigation owned it before the redistribution.
    if (ownedFocus && (focusedId || focusedMore) && !focused?.isConnected) {
      const links = Array.from(nav.querySelectorAll<HTMLAnchorElement>('a[data-tab-id]'));
      const same = links.find(link => link.dataset.tabId === focusedId);
      const fallback = nav.querySelector<HTMLButtonElement>('.overflow-trigger')
        ?? links.find(link => link.dataset.tabId === activeTab)
        ?? links.at(-1);
      (same ?? fallback)?.focus();
    }
  }

  $effect(() => {
    tabs;
    activeTab;
    moreLabel;
    void tick().then(() => schedule());
  });

  onMount(() => {
    let frame = 0;
    let disposed = false;
    const requestFrame = window.requestAnimationFrame?.bind(window)
      ?? ((callback: FrameRequestCallback) => window.setTimeout(() => callback(performance.now()), 0));
    const cancelFrame = window.cancelAnimationFrame?.bind(window) ?? window.clearTimeout.bind(window);
    schedule = () => {
      if (disposed || frame) return;
      // Observe only geometry independent of the visible prefix, and write
      // after observer delivery to avoid ResizeObserver feedback loops.
      frame = requestFrame(() => {
        frame = 0;
        void measure();
      });
    };
    const observer = typeof ResizeObserver === 'undefined' ? null : new ResizeObserver(schedule);
    if (nav) observer?.observe(nav);
    if (measurements) observer?.observe(measurements);
    window.addEventListener('resize', schedule);
    document.fonts?.addEventListener('loadingdone', schedule);
    void document.fonts?.ready.then(schedule);
    void measure();
    return () => {
      disposed = true;
      cancelFrame(frame);
      observer?.disconnect();
      window.removeEventListener('resize', schedule);
      document.fonts?.removeEventListener('loadingdone', schedule);
      schedule = () => {};
    };
  });
</script>

{#snippet contents(tab: Tab)}
  <span class="tab-icon" aria-hidden="true">{tab.icon}</span>
  <span>{tab.label}</span>
  {#if tab.count !== undefined}<span class="tab-count">{tab.count}</span>{/if}
{/snippet}

<nav bind:this={nav} class="repo-tabs" aria-label={label}>
  {#each visible as tab (tab.id)}
    <a href={tab.href} class="tab" class:active={activeTab === tab.id} aria-current={activeTab === tab.id ? 'page' : undefined} data-tab-id={tab.id}>
      {@render contents(tab)}
    </a>
  {/each}
  {#if hidden.length > 0}
    <Dropdown bind:open fitViewport placement="left" triggerClass="overflow-trigger {hiddenActive ? 'active' : ''}" ariaLabel={hiddenActive ? moreActiveLabel : moreLabel}>
      {#snippet trigger()}
        <span>{moreLabel}</span><span aria-hidden="true">▾</span>
      {/snippet}
      {#snippet menu(close)}
        {#each hidden as tab (tab.id)}
          <a href={tab.href} role="menuitem" class:active={activeTab === tab.id} aria-current={activeTab === tab.id ? 'page' : undefined} data-tab-id={tab.id} onclick={close}>
            {@render contents(tab)}
          </a>
        {/each}
      {/snippet}
    </Dropdown>
  {/if}
  <!-- Intrinsic-size samples are plain spans: no duplicate links, focus stops
       or accessibility-tree entries, even while the overflow menu is closed. -->
  <div class="measurement-clip" aria-hidden="true" inert>
    <div class="measurements" bind:this={measurements}>
      {#each tabs as tab (tab.id)}
        <span class="tab" class:active={activeTab === tab.id} data-measure-tab>{@render contents(tab)}</span>
      {/each}
      <span class="tab more-sample" data-measure-more><span>{moreLabel}</span><span>▾</span></span>
    </div>
  </div>
</nav>

<style>
  .repo-tabs {
    position: relative;
    display: flex;
    min-width: 0;
    width: 100%;
  }

  .tab,
  .repo-tabs :global(.overflow-trigger) {
    display: inline-flex;
    align-items: center;
    flex: 0 0 auto;
    gap: 6px;
    padding: 8px 16px;
    border: 0;
    border-bottom: 2px solid transparent;
    color: var(--text-secondary);
    font: inherit;
    font-size: 14px;
    line-height: 1.5;
    text-decoration: none;
    white-space: nowrap;
  }

  .tab:hover,
  .repo-tabs :global(.overflow-trigger:hover) {
    color: var(--text-primary);
    text-decoration: none;
  }

  .tab.active,
  .repo-tabs :global(.overflow-trigger.active) {
    color: var(--text-primary);
    font-weight: 600;
    border-bottom-color: var(--accent);
  }

  .tab:focus-visible,
  .repo-tabs :global(.overflow-trigger:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }

  .repo-tabs :global(.dropdown-wrapper) { min-width: 0; max-width: 100%; }
  .repo-tabs :global(.overflow-trigger), .more-sample { font-weight: 600; }
  .repo-tabs :global(.overflow-trigger) { white-space: normal; overflow-wrap: anywhere; }
  .repo-tabs :global(.dropdown-menu a) { display: flex; align-items: center; gap: 8px; }
  .repo-tabs :global(.dropdown-menu a > span:nth-child(2)) { min-width: 0; }
  .tab-icon { flex-shrink: 0; font-size: 14px; }
  .tab-count { font-size: 12px; }

  .measurement-clip {
    position: absolute;
    inset: 0 0 auto;
    height: 0;
    overflow: hidden;
    visibility: hidden;
    pointer-events: none;
  }

  .measurements { display: flex; width: max-content; }
</style>
