<!--
  The confirmation a page shows before an irreversible action, in place of
  `window.confirm()` (card_4c186d530f59). The page creates a `Confirmer`,
  renders this once, and awaits `confirmer.ask(...)`. Cancel takes the
  initial focus, so Enter on a freshly opened dialog never performs the
  action; Escape and the backdrop answer "no" through `Modal`.
-->
<script lang="ts">
  import { onDestroy } from 'svelte';
  import { beforeNavigate } from '$app/navigation';
  import Modal from '$lib/components/Modal.svelte';
  import type { Confirmer } from '$lib/confirm.svelte';
  import { createT } from '$lib/i18n';

  let { confirmer }: { confirmer: Confirmer } = $props();

  const t = createT();

  // A page that goes away mid-question must not leave its caller waiting, and
  // a navigation (back button, a route that reuses this component) declines:
  // the question named an object of the page the user is leaving.
  onDestroy(() => confirmer.answer(false));
  beforeNavigate(() => confirmer.answer(false));
</script>

{#if confirmer.pending}
  {@const request = confirmer.pending}
  <Modal onclose={() => confirmer.answer(false)} labelledby="confirm-modal-title">
    <h2 id="confirm-modal-title" class="confirm-modal-title">{request.title}</h2>
    <p class="confirm-modal-message">{request.message}</p>
    <div class="confirm-modal-actions">
      <button
        type="button"
        class="btn {request.tone === 'primary' ? 'btn-primary' : 'btn-danger'} confirm-modal-accept"
        onclick={() => confirmer.answer(true)}
      >
        {request.confirmLabel}
      </button>
      <button type="button" class="btn confirm-modal-cancel" onclick={() => confirmer.answer(false)} data-autofocus>
        {t('common.cancel')}
      </button>
    </div>
  </Modal>
{/if}

<style>
  .confirm-modal-title {
    margin: 0 0 0.75rem;
    font-size: 1.15rem;
  }

  .confirm-modal-message {
    margin: 0;
    white-space: pre-line;
    overflow-wrap: anywhere;
  }

  .confirm-modal-actions {
    display: flex;
    gap: 0.75rem;
    margin-top: 1.25rem;
  }
</style>
