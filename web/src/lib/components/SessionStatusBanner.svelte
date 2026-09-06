<script lang="ts">
  import { createT } from '$lib/i18n';
  import { fetchUser, getSessionCheckError } from '$lib/stores/auth.svelte';

  const t = createT();
  let retrying = $state(false);
  let unavailable = $derived(getSessionCheckError() !== null);

  async function retrySessionCheck() {
    if (retrying) return;
    retrying = true;
    try {
      await fetchUser();
    } finally {
      retrying = false;
    }
  }
</script>

{#if unavailable}
  <div class="session-status" role="alert">
    <span class="session-status-icon" aria-hidden="true">🚫</span>
    <span class="session-status-text">{t('errors.session_check_failed')}</span>
    <button type="button" disabled={retrying} onclick={retrySessionCheck}>
      {retrying ? t('errors.session_check_retrying') : t('errors.session_check_retry')}
    </button>
  </div>
{/if}

<style>
  .session-status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 16px;
    color: var(--red, #f85149);
    background: rgba(248, 81, 73, 0.15);
    border-bottom: 1px solid rgba(248, 81, 73, 0.45);
    font-size: 13px;
    font-weight: 500;
    z-index: 50;
  }

  .session-status-text {
    flex: 1;
  }

  button {
    border: 1px solid currentColor;
    border-radius: 4px;
    padding: 3px 10px;
    color: inherit;
    background: transparent;
    cursor: pointer;
  }

  button:disabled {
    cursor: wait;
    opacity: 0.65;
  }
</style>
