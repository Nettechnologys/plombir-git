<script lang="ts">
  import { getTrialMode } from '$lib/stores/instance.svelte';
  import { t } from '$lib/i18n';
</script>

<!-- No close button: unlike the operator's banner, this one states what the
     instance is, and that stays true on every page. -->
{#if getTrialMode()}
  <div class="trial-banner" role="status" data-testid="trial-banner">
    <strong>{t('trial.title')}</strong>
    <span>{t('trial.body')}</span>
    <span>{t('trial.deploy')}</span>
  </div>
{/if}

<style>
  .trial-banner {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 6px 10px;
    padding: 8px 16px;
    font-size: 13px;
    background: var(--warning-bg);
    color: var(--warning-text);
    border-bottom: 1px solid var(--warning-border);
  }
</style>
