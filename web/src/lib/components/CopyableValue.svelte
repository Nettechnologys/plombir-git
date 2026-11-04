<script lang="ts">
  import { onDestroy } from 'svelte';
  import { copyToClipboard } from '$lib/clipboard';
  import { createT } from '$lib/i18n';

  let { value, label, copyLabel }: { value: string; label: string; copyLabel: string } = $props();
  const t = createT();
  const feedbackId = $props.id();
  let state = $state<'idle' | 'pending' | 'copied' | 'error'>('idle');
  let generation = 0;
  let disposed = false;
  const actionLabel = $derived(state === 'copied' ? t('common.copied') : state === 'pending' ? t('common.copying') : copyLabel);

  $effect.pre(() => {
    value;
    generation += 1;
    state = 'idle';
  });
  onDestroy(() => {
    disposed = true;
    generation += 1;
  });

  async function copy() {
    if (!value || state === 'pending') return;
    const claim = ++generation;
    const snapshot = value;
    state = 'pending';
    const isCurrent = () => !disposed && claim === generation && snapshot === value;
    const copied = await copyToClipboard(snapshot, isCurrent);
    // An older value or a detached control cannot announce a later result.
    if (!isCurrent()) return;
    state = copied ? 'copied' : 'error';
  }
</script>

<div class="copyable-value">
  <div class="value-field">
    <input class="value" type="text" readonly {value} aria-label={label} aria-describedby={state === 'error' ? feedbackId : undefined} autocomplete="off" spellcheck="false" />
    <button type="button" class="copy-button" class:copied={state === 'copied'} disabled={!value} aria-disabled={state === 'pending'} aria-label={actionLabel} title={actionLabel} onclick={copy}>
      {#if state === 'copied'}
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><path d="m5 12 4 4L19 6" /></svg>
      {:else}
        <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"><rect x="8" y="8" width="12" height="12" rx="2" /><path d="M16 8V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h3" /></svg>
      {/if}
    </button>
  </div>
  <p id={feedbackId} class="feedback" class:visually-hidden={state !== 'error'} role="status" aria-live="polite" aria-atomic="true">
    {state === 'copied' ? t('common.copied') : state === 'pending' ? t('common.copying') : state === 'error' ? t('common.copy_failed') : ''}
  </p>
</div>

<style>
  .copyable-value { min-width: 0; width: 100%; }
  .value-field { display: flex; min-width: 0; border: 1px solid var(--border); border-radius: var(--radius); background: var(--bg-primary); }
  .value { flex: 1; min-width: 0; width: 0; padding: 8px 10px; border: 0; border-radius: var(--radius) 0 0 var(--radius); background: transparent; color: var(--text-primary); font-family: monospace; }
  .copy-button { display: inline-flex; align-items: center; justify-content: center; flex: 0 0 40px; min-height: 40px; padding: 8px; border: 0; border-left: 1px solid var(--border); border-radius: 0 var(--radius) var(--radius) 0; background: var(--bg-secondary); color: var(--text-secondary); cursor: pointer; }
  .copy-button:hover:not(:disabled) { background: var(--bg-hover); color: var(--text-primary); }
  .copy-button.copied { color: var(--green); }
  .copy-button:disabled, .copy-button[aria-disabled='true'] { cursor: default; opacity: 0.65; }
  .value:focus-visible, .copy-button:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .feedback { margin: 8px 0 0; color: var(--red); font-size: 13px; overflow-wrap: anywhere; }
  .visually-hidden { position: absolute; width: 1px; height: 1px; padding: 0; margin: -1px; overflow: hidden; clip-path: inset(50%); white-space: nowrap; }
</style>
