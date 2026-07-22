<script lang="ts">
  import { tick, type Snippet } from 'svelte';

  interface Props {
    open?: boolean;
    placement?: 'left' | 'right';
    ariaLabel?: string;
    triggerClass?: string;
    trigger: Snippet<[() => void]>;
    menu: Snippet<[() => void]>;
  }

  let { open = $bindable(false), placement = 'right', ariaLabel, triggerClass = '', trigger, menu }: Props = $props();

  let triggerId = $state(`dropdown-trigger-${Math.random().toString(36).slice(2, 9)}`);
  let menuId = $state(`dropdown-menu-${Math.random().toString(36).slice(2, 9)}`);
  let menuEl = $state<HTMLDivElement | null>(null);
  let triggerEl = $state<HTMLButtonElement | null>(null);

  function toggle() {
    open = !open;
  }

  function close() {
    if (open) {
      open = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      close();
      triggerEl?.focus();
    }
  }

  function handleClickOutside(e: MouseEvent) {
    if (open && menuEl && !menuEl.contains(e.target as Node) && !triggerEl?.contains(e.target as Node)) {
      close();
    }
  }

  $effect(() => {
    if (open) {
      document.addEventListener('click', handleClickOutside);
      document.addEventListener('keydown', handleKeydown);
      // Focus first focusable menu item after opening
      tick().then(() => {
        const first = menuEl?.querySelector<HTMLElement>('a, button, [tabindex]:not([tabindex="-1"])');
        first?.focus();
      });
    } else {
      document.removeEventListener('click', handleClickOutside);
      document.removeEventListener('keydown', handleKeydown);
    }
    return () => {
      document.removeEventListener('click', handleClickOutside);
      document.removeEventListener('keydown', handleKeydown);
    };
  });
</script>

<div class="dropdown-wrapper">
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
  >
    {@render trigger(toggle)}
  </button>

  {#if open}
    <div
      bind:this={menuEl}
      id={menuId}
      class="dropdown-menu {placement}"
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
