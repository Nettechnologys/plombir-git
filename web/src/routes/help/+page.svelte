<script lang="ts">
  import { buildHttpCloneUrl, buildSshCloneUrl } from '$lib/api/_base';
  import { createT } from '$lib/i18n';

  const t = createT();

  // The addresses this instance actually serves, built by the same helpers
  // the repository page's clone box uses — not `localhost:8080`, which was
  // right only on the developer's machine (card_e34af7255aa6). The SSH one
  // is absent when the instance names no SSH host and the page has none.
  const httpExample = buildHttpCloneUrl('OWNER', 'REPO');
  const sshExample = buildSshCloneUrl(
    'OWNER',
    'REPO',
    typeof window === 'undefined' ? undefined : window.location.hostname,
  );
</script>

<svelte:head>
  <title>{t('help.title')} · Plombir Git</title>
</svelte:head>

<div class="help-page">
  <header class="help-header">
    <h1>{t('help.title')}</h1>
    <p>{t('help.subtitle')}</p>
  </header>

  <section class="help-section">
    <h2>{t('help.clone_title')}</h2>
    <div class="command-list">
      <code>git clone {httpExample}</code>
      {#if sshExample}
        <code>git clone {sshExample}</code>
      {/if}
    </div>
  </section>

  <section class="help-section">
    <h2>{t('help.tokens_title')}</h2>
    <p>{t('help.tokens_body')}</p>
    <a href="/settings/tokens" class="link-button">{t('help.tokens_link')}</a>
  </section>

  <section class="help-section">
    <h2>{t('help.agent_title')}</h2>
    <p>{t('help.agent_body')} {t('help.agent_endpoint_before')} <code>/api/v1/mcp</code> {t('help.agent_endpoint_after')}</p>
    <a href="/settings/agents" class="link-button">{t('help.agent_link')}</a>
  </section>

  <section class="help-section">
    <h2>{t('help.find_title')}</h2>
    <div class="quick-links">
      <a href="/explore">{t('help.find_explore')}</a>
      <a href="/search">{t('help.find_search')}</a>
      <a href="/imports">{t('help.find_imports')}</a>
    </div>
  </section>
</div>

<style>
  .help-page {
    max-width: 920px;
    margin: 0 auto;
    padding: 40px 24px 64px;
  }

  .help-header {
    border-bottom: 1px solid var(--border-color);
    padding-bottom: 20px;
    margin-bottom: 28px;
  }

  .help-header h1 {
    margin: 0 0 8px;
    font-size: 32px;
  }

  .help-header p,
  .help-section p {
    color: var(--text-secondary);
    line-height: 1.6;
  }

  .help-section {
    padding: 20px 0;
    border-bottom: 1px solid var(--border-color);
  }

  .help-section h2 {
    margin: 0 0 12px;
    font-size: 20px;
  }

  .command-list {
    display: grid;
    gap: 10px;
  }

  code {
    display: block;
    overflow-x: auto;
    padding: 12px 14px;
    border: 1px solid var(--border-color);
    border-radius: 6px;
    background: var(--bg-secondary);
    color: var(--text-primary);
  }

  .quick-links {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
  }

  .quick-links a,
  .link-button {
    color: var(--color-primary);
    font-weight: 600;
    text-decoration: none;
  }

  .quick-links a:hover,
  .link-button:hover {
    text-decoration: underline;
  }
</style>
