<!--
  A modal dialog the page mounts while it is open: `{#if open}<Modal …>…</Modal>{/if}`.

  Every modal of the app used to be an overlay `div` with `onclick={close}`
  wrapping the dialog, plus an `onkeydown` that closed it on Escape, Enter
  AND Space. The dialog did not stop propagation, so a click into the Name
  field closed the form, every space typed into an input was swallowed by
  `preventDefault()` and closed it too, and a delete confirmation vanished
  together with its pending state and its error (card_4a99471945dc).

  The rules this component owns, so no page has to repeat them:
  - the backdrop is a SIBLING of the dialog, not its parent: nothing that
    happens inside the dialog can bubble into "close";
  - only Escape closes, and only while focus is inside the dialog;
  - focus moves into the dialog when it opens (a `data-autofocus` element,
    else the first focusable one, else the dialog itself), Tab and Shift+Tab
    cycle inside it, and focus returns to whatever opened it on close.
-->
<script lang="ts">
  import { onMount, type Snippet } from 'svelte';
  import { createT } from '$lib/i18n';

  interface Props {
    /** Called on Escape and on a backdrop click. The page removes the modal. */
    onclose: () => void;
    /** Id of the element (normally the heading) that names the dialog. */
    labelledby?: string;
    /** Accessible name when there is no visible heading to point at. */
    label?: string;
    /** Width of the panel; it never grows past the viewport. */
    width?: string;
    padding?: string;
    children: Snippet;
  }

  let { onclose, labelledby, label, width = '480px', padding = '1.5rem', children }: Props = $props();

  const t = createT();

  const FOCUSABLE = [
    'a[href]',
    'area[href]',
    'button:not([disabled])',
    'input:not([disabled]):not([type="hidden"])',
    'select:not([disabled])',
    'textarea:not([disabled])',
    'iframe',
    '[contenteditable="true"]',
    '[tabindex]:not([tabindex="-1"])',
  ].join(',');

  // Captured while the component is being created — before anything below
  // moves focus — so it is the control that opened the dialog.
  const opener =
    typeof document !== 'undefined' && document.activeElement instanceof HTMLElement && document.activeElement !== document.body
      ? document.activeElement
      : null;

  let panel = $state<HTMLDivElement | null>(null);

  function focusable(): HTMLElement[] {
    if (!panel) return [];
    return Array.from(panel.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
      (candidate) => !candidate.hasAttribute('disabled') && candidate.getAttribute('aria-hidden') !== 'true',
    );
  }

  function focusInitial() {
    if (!panel) return;
    const preferred = panel.querySelector<HTMLElement>('[data-autofocus]');
    const target =
      preferred && !preferred.hasAttribute('disabled') ? preferred : (focusable()[0] ?? panel);
    target.focus();
  }

  function trapTab(e: KeyboardEvent) {
    const items = focusable();
    if (items.length === 0) {
      e.preventDefault();
      panel?.focus();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && (active === first || active === panel || !panel?.contains(active))) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && (active === last || !panel?.contains(active))) {
      e.preventDefault();
      first.focus();
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    } else if (e.key === 'Tab') {
      trapTab(e);
    }
  }

  onMount(() => {
    focusInitial();
    return () => {
      // The opener may have been removed meanwhile (a deleted row's button);
      // focusing a detached element would silently drop focus to <body>.
      if (opener && opener.isConnected) opener.focus();
    };
  });
</script>

<div class="modal-root">
  <button
    type="button"
    class="modal-backdrop"
    tabindex="-1"
    aria-label={t('common.close')}
    onclick={onclose}
  ></button>
  <div
    bind:this={panel}
    class="modal-panel"
    role="dialog"
    aria-modal="true"
    aria-labelledby={labelledby}
    aria-label={labelledby ? undefined : label}
    tabindex="-1"
    style:width="min({width}, 100%)"
    style:padding
    onkeydown={handleKeydown}
  >
    {@render children()}
  </div>
</div>

<style>
  .modal-root {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 16px;
    box-sizing: border-box;
  }

  .modal-backdrop {
    position: absolute;
    inset: 0;
    margin: 0;
    padding: 0;
    border: none;
    background: rgba(0, 0, 0, 0.6);
    cursor: default;
  }

  .modal-panel {
    position: relative;
    box-sizing: border-box;
    max-height: 90vh;
    overflow-y: auto;
    background: var(--bg-primary);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.25);
  }

  .modal-panel:focus {
    outline: none;
  }
</style>
