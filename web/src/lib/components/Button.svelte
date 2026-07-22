<script lang="ts">
  import type { Snippet } from 'svelte';

  type ButtonVariant = 'primary' | 'outline' | 'ghost' | 'danger';
  type ButtonSize = 'sm' | 'md' | 'lg';

  interface Props {
    variant?: ButtonVariant;
    size?: ButtonSize;
    type?: 'button' | 'submit' | 'reset';
    disabled?: boolean;
    ariaLabel?: string;
    href?: string;
    class?: string;
    children: Snippet;
    onclick?: (e: MouseEvent) => void;
  }

  let {
    variant = 'outline',
    size = 'md',
    type = 'button',
    disabled = false,
    ariaLabel,
    href,
    class: className = '',
    children,
    onclick,
  }: Props = $props();

  let classes = $derived(
    `btn btn-${variant} btn-${size} ${className}`.trim()
  );
</script>

{#if href}
  <a {href} class={classes} aria-label={ariaLabel}>
    {@render children()}
  </a>
{:else}
  <button {type} class={classes} {disabled} aria-label={ariaLabel} onclick={onclick}>
    {@render children()}
  </button>
{/if}

<style>
  .btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    gap: 6px;
    border-radius: var(--radius);
    font-weight: 500;
    cursor: pointer;
    text-decoration: none;
    transition: background-color 0.15s, border-color 0.15s;
  }
  .btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .btn:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn-sm { padding: 3px 8px; font-size: 12px; }
  .btn-md { padding: 5px 12px; font-size: 13px; }
  .btn-lg { padding: 8px 16px; font-size: 14px; }

  .btn-primary {
    background: var(--green-dim);
    border: 1px solid var(--green-dim);
    color: #fff;
  }
  .btn-primary:hover:not(:disabled) { background: var(--green); border-color: var(--green); }

  .btn-outline {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    color: var(--text-primary);
  }
  .btn-outline:hover:not(:disabled) { background: var(--bg-hover); }

  .btn-ghost {
    background: none;
    border: 1px solid transparent;
    color: var(--text-primary);
  }
  .btn-ghost:hover:not(:disabled) { background: var(--bg-hover); }

  .btn-danger {
    background: var(--red-dim);
    border: 1px solid var(--red-dim);
    color: #fff;
  }
  .btn-danger:hover:not(:disabled) { background: var(--red); border-color: var(--red); }
</style>
