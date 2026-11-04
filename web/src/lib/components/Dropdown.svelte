<script lang="ts">
  import { tick, type Snippet } from 'svelte';

  interface Props {
    open?: boolean;
    placement?: 'left' | 'right';
    ariaLabel?: string;
    triggerClass?: string;
    fitViewport?: boolean;
    trigger: Snippet<[() => void]>;
    menu: Snippet<[() => void]>;
  }

  let { open = $bindable(false), placement = 'right', ariaLabel, triggerClass = '', fitViewport = false, trigger, menu }: Props = $props();

  let triggerId = $state(`dropdown-trigger-${Math.random().toString(36).slice(2, 9)}`);
  let menuId = $state(`dropdown-menu-${Math.random().toString(36).slice(2, 9)}`);
  let menuEl = $state<HTMLDivElement | null>(null);
  let triggerEl = $state<HTMLButtonElement | null>(null);
  let position = $state<{ left: number; top: number; maxHeight: number; maxWidth: number } | null>(null);
  let focusLast = false;

  function items() {
    return Array.from(menuEl?.querySelectorAll<HTMLElement>('a[href], button:not(:disabled), [tabindex]:not([tabindex="-1"])') ?? []);
  }

  function placeMenu() {
    if (!fitViewport || !menuEl || !triggerEl) return;
    const margin = 8;
    const viewport = window.visualViewport;
    const leftEdge = (viewport?.offsetLeft ?? 0) + margin;
    const topEdge = (viewport?.offsetTop ?? 0) + margin;
    const rightEdge = leftEdge + (viewport?.width ?? window.innerWidth) - 2 * margin;
    const bottomEdge = topEdge + (viewport?.height ?? window.innerHeight) - 2 * margin;
    const triggerBox = triggerEl.getBoundingClientRect();
    if (triggerBox.bottom < topEdge || triggerBox.top > bottomEdge) {
      close();
      return;
    }
    const menuBox = menuEl.getBoundingClientRect();
    const below = Math.max(0, bottomEdge - triggerBox.bottom - 4);
    const above = Math.max(0, triggerBox.top - topEdge - 4);
    const upwards = menuBox.height > below && above > below;
    const maxHeight = upwards ? above : below;
    position = {
      left: Math.max(leftEdge, Math.min(placement === 'left' ? triggerBox.left : triggerBox.right - menuBox.width, rightEdge - menuBox.width)),
      top: upwards ? Math.max(topEdge, triggerBox.top - Math.min(menuBox.height, maxHeight) - 4) : Math.max(topEdge, triggerBox.bottom + 4),
      maxHeight,
      maxWidth: rightEdge - leftEdge,
    };
  }

  function toggle() {
    focusLast = false;
    open = !open;
  }

  function close() {
    if (open) {
      open = false;
      position = null;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    const target = e.target as Node;
    if (!menuEl?.contains(target) && !triggerEl?.contains(target)) return;
    if (e.key === 'Escape') {
      e.preventDefault();
      close();
      triggerEl?.focus();
    } else if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(e.key)) {
      const choices = items();
      if (!choices.length) return;
      e.preventDefault();
      const index = choices.indexOf(document.activeElement as HTMLElement);
      const next = e.key === 'Home' ? 0 : e.key === 'End' ? choices.length - 1
        : (index + (e.key === 'ArrowDown' ? 1 : -1) + choices.length) % choices.length;
      choices[next]?.focus();
    }
  }

  function handleTriggerKeydown(e: KeyboardEvent) {
    if (open || !['ArrowDown', 'ArrowUp'].includes(e.key)) return;
    e.preventDefault();
    e.stopPropagation();
    focusLast = e.key === 'ArrowUp';
    open = true;
  }

  function handleFocusOutside(e: FocusEvent) {
    if (!menuEl?.contains(e.target as Node) && !triggerEl?.contains(e.target as Node)) close();
  }

  function handleClickOutside(e: MouseEvent) {
    if (open && menuEl && !menuEl.contains(e.target as Node) && !triggerEl?.contains(e.target as Node)) {
      close();
    }
  }

  $effect(() => {
    let observer: ResizeObserver | null = null;
    let frame = 0;
    let disposed = false;
    if (open) {
      document.addEventListener('click', handleClickOutside);
      document.addEventListener('keydown', handleKeydown);
      document.addEventListener('focusin', handleFocusOutside);
      if (fitViewport) {
        window.addEventListener('resize', placeMenu);
        window.addEventListener('scroll', placeMenu, true);
        window.visualViewport?.addEventListener('resize', placeMenu);
        window.visualViewport?.addEventListener('scroll', placeMenu);
      }
      // Focus first focusable menu item after opening
      tick().then(() => {
        if (disposed) return;
        placeMenu();
        const choices = items();
        (focusLast ? choices.at(-1) : choices[0])?.focus();
        if (fitViewport && typeof ResizeObserver !== 'undefined') {
          observer = new ResizeObserver(() => {
            if (frame) return;
            frame = requestAnimationFrame(() => { frame = 0; placeMenu(); });
          });
          if (menuEl) observer.observe(menuEl);
        }
      });
    } else {
      document.removeEventListener('click', handleClickOutside);
      document.removeEventListener('keydown', handleKeydown);
      document.removeEventListener('focusin', handleFocusOutside);
    }
    return () => {
      disposed = true;
      observer?.disconnect();
      if (frame) cancelAnimationFrame(frame);
      document.removeEventListener('click', handleClickOutside);
      document.removeEventListener('keydown', handleKeydown);
      document.removeEventListener('focusin', handleFocusOutside);
      window.removeEventListener('resize', placeMenu);
      window.removeEventListener('scroll', placeMenu, true);
      window.visualViewport?.removeEventListener('resize', placeMenu);
      window.visualViewport?.removeEventListener('scroll', placeMenu);
    };
  });
</script>

<div class="dropdown-wrapper" class:button-trigger={triggerClass.split(/\s+/).includes('btn')}>
  <button
    bind:this={triggerEl}
    id={triggerId}
    type="button"
    class="dropdown-trigger {triggerClass}"
    aria-haspopup="true"
    aria-expanded={open}
    aria-controls={menuId}
    aria-label={ariaLabel}
    onclick={toggle}
    onkeydown={handleTriggerKeydown}
  >
    {@render trigger(toggle)}
  </button>

  {#if open}
    <div
      bind:this={menuEl}
      id={menuId}
      class="dropdown-menu {placement}"
      class:fit-viewport={fitViewport}
      style:left={fitViewport && position ? `${position.left}px` : undefined}
      style:top={fitViewport && position ? `${position.top}px` : undefined}
      style:max-height={fitViewport && position ? `${position.maxHeight}px` : undefined}
      style:max-width={fitViewport && position ? `${position.maxWidth}px` : undefined}
      role="menu"
      aria-labelledby={triggerId}
      tabindex="-1"
    >
      {@render menu(close)}
    </div>
  {/if}
</div>

<style>
  .dropdown-wrapper {
    position: relative;
    display: inline-flex;
  }

  .dropdown-trigger {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    cursor: pointer;
  }

  /* A button-styled trigger keeps the shared button surface and spacing. */
  .button-trigger { min-width: 0; max-width: 100%; }
  .dropdown-trigger:global(.btn) { min-width: 0; max-width: 100%; font-family: inherit; }
  .dropdown-trigger:where(:not(:global(.btn))) {
    font: inherit;
    color: inherit;
    background: none;
    border: none;
    padding: 0;
  }

  .dropdown-menu {
    position: absolute;
    top: 100%;
    margin-top: 4px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    min-width: 160px;
    box-shadow: 0 8px 24px rgba(0, 0, 0, 0.4);
    z-index: 200;
    overflow: hidden;
  }
  .dropdown-menu.left { left: 0; }
  .dropdown-menu.right { right: 0; }

  .dropdown-menu.fit-viewport {
    position: fixed;
    right: auto;
    margin-top: 0;
    min-width: min(160px, calc(100vw - 16px));
    max-width: calc(100vw - 16px);
    overflow-y: auto;
    overflow-wrap: anywhere;
    overscroll-behavior: contain;
  }

  .dropdown-menu :global(a),
  .dropdown-menu :global(button) {
    display: block;
    width: 100%;
    padding: 8px 16px;
    background: none;
    border: none;
    color: var(--text-primary);
    font-size: 13px;
    text-align: left;
    cursor: pointer;
    text-decoration: none;
  }
  .dropdown-menu :global(a:hover),
  .dropdown-menu :global(button:hover),
  .dropdown-menu :global(a:focus-visible),
  .dropdown-menu :global(button:focus-visible) {
    background: var(--bg-hover);
    text-decoration: none;
    outline: none;
  }
  .dropdown-menu :global(a.active),
  .dropdown-menu :global(button.active) {
    font-weight: 600;
    color: var(--accent);
  }
</style>
