<!--
  What an unknown address or a failed page load shows (card_c30077df5603).
  Without it SvelteKit's bare built-in page answered: a status code and a
  sentence in English, no navigation, no way home.
-->
<script lang="ts">
  import { page } from '$app/stores';
  import { createT } from '$lib/i18n';

  const t = createT();

  const notFound = $derived($page.status === 404);
</script>

<svelte:head>
  <title>{notFound ? t('errors.page.not_found_title') : t('errors.page.failed_title')} · Plombir Git</title>
</svelte:head>

<div class="error-page">
  <p class="status">{$page.status}</p>
  <h1>{notFound ? t('errors.page.not_found_title') : t('errors.page.failed_title')}</h1>
  <p class="explanation">
    {notFound ? t('errors.page.not_found') : t('errors.page.failed')}
  </p>
  <div class="actions">
    <a href="/" class="btn btn-primary">{t('errors.page.home')}</a>
    <a href="/explore" class="btn btn-outline">{t('errors.page.explore')}</a>
  </div>
</div>

<style>
  .error-page {
    max-width: 560px;
    margin: 80px auto;
    padding: 0 var(--layout-gutter);
    text-align: center;
  }

  .status {
    font-size: 64px;
    font-weight: 700;
    color: var(--text-muted);
    line-height: 1;
    margin-bottom: 16px;
  }

  h1 {
    font-size: 24px;
    margin-bottom: 8px;
  }

  .explanation {
    color: var(--text-secondary);
    margin-bottom: 24px;
  }

  .actions {
    display: flex;
    gap: 12px;
    justify-content: center;
    flex-wrap: wrap;
  }
</style>
