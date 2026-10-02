<script lang="ts">
  import { t } from '$lib/i18n';
  import { getSourceLink } from '$lib/stores/instance.svelte';

  // The AGPL §13 offer on every page: the source of the build the visitor is
  // using, at the address the operator configured. Nothing is rendered until
  // the server has said where that is — see `getSourceLink`.
  let link = $derived(getSourceLink());
</script>

{#if link}
  <footer class="source-footer">
    <a
      href={link.url}
      target="_blank"
      rel="noopener noreferrer"
      title={link.commit
        ? t('common.source_code_at_commit', { commit: link.commit })
        : t('common.source_code_commit_unknown')}
    >
      {t('common.source_code')}
    </a>
    {#if link.commit}
      <code>{link.commit.slice(0, 12)}</code>
    {/if}
  </footer>
{/if}

<style>
  .source-footer {
    display: flex;
    justify-content: center;
    align-items: center;
    gap: 0.5rem;
    padding: 0.75rem 1rem;
    font-size: 0.8rem;
    color: var(--text-secondary, #656d76);
    border-top: 1px solid var(--border, #d0d7de);
  }

  .source-footer a {
    color: inherit;
  }

  .source-footer a:hover {
    color: var(--text-primary, #1f2328);
  }

  .source-footer code {
    font-size: 0.75rem;
  }
</style>
