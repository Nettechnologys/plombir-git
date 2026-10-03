<script lang="ts">
  // Marks an author that is a bot account — an AI agent's own identity — and
  // names the person it acts for, so an agent's issue, comment or pull request
  // never reads as its owner's own. Renders nothing for a human author.
  import { t } from '$lib/i18n';

  // `link = false` inside an element that is itself a link: anchors do not nest.
  let { owner = null, link = true }: { owner?: string | null; link?: boolean } = $props();
</script>

{#if owner}
  <span class="bot-mark">
    <span class="bot-badge" title={t('author.bot_title', 'An AI agent account')}>{t('author.bot', 'bot')}</span>
    <span class="on-behalf">{t('author.on_behalf_of', 'on behalf of')} {#if link}<a href={`/${owner}`}>@{owner}</a>{:else}@{owner}{/if}</span>
  </span>
{/if}

<style>
  .bot-mark {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    margin-left: 6px;
  }

  .bot-badge {
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    color: var(--text-secondary);
    background: var(--bg-secondary);
  }

  .on-behalf {
    color: var(--text-secondary);
    font-size: 12px;
  }
</style>
