<script lang="ts">
  // Marks something an import brought and names who wrote it on the platform
  // it came from. The local author shown beside it is the account that ran the
  // import, which is who it belongs to here (card_04088376105b). Renders
  // nothing for what was written on this instance.
  import { t } from '$lib/i18n';
  import { importedPlatform } from '$lib/importedPlatform';

  let { login = null, service = null }: { login?: string | null; service?: string | null } =
    $props();

  const platform = $derived(service ? importedPlatform(service) : '');
</script>

{#if login && service}
  <span
    class="imported-mark"
    title={t('author.imported_title', 'Imported: the account beside it ran the import')}
  >
    <span class="imported-badge">{platform}</span>
    <span class="originally-by">{t('author.originally_by', 'originally by')} @{login}</span>
  </span>
{/if}

<style>
  .imported-mark {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    margin-left: 6px;
  }

  .imported-badge {
    padding: 0 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-size: 11px;
    font-weight: 600;
    color: var(--text-secondary);
    background: var(--bg-secondary);
  }

  .originally-by {
    color: var(--text-secondary);
    font-size: 12px;
  }
</style>
