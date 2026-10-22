<script lang="ts">
  // The receipt of a finished import, as the server built it: what the source
  // has beside what is here, by name where something is missing, the CI that
  // will not run here, and what no import can bring (card_a8666ed4f317). The
  // API answered it all along; the import page did not show it, so a person
  // who migrated through the form never learned their CI would not start.
  import type { ImportReceipt, RefKindReceipt } from '$lib/api/imports';
  import { t } from '$lib/i18n';

  let { receipt }: { receipt: ImportReceipt } = $props();

  const refKinds = $derived(
    [
      { key: 'branches', label: t('imports.receipt.branches'), value: receipt.refs.branches },
      { key: 'tags', label: t('imports.receipt.tags'), value: receipt.refs.tags },
    ].filter((row): row is { key: string; label: string; value: RefKindReceipt } => !!row.value),
  );

  const notRun = $derived(receipt.ci?.not_run ?? []);

  const incomplete = $derived(
    receipt.objects.some((row) => row.not_imported > 0) ||
      refKinds.some(({ value }) => (value.missing?.length ?? 0) > 0 || (value.different?.length ?? 0) > 0) ||
      notRun.length > 0,
  );

  function kindLabel(kind: string): string {
    return t(`imports.receipt.kind.${kind}`, undefined, kind);
  }

  function names(list: string[] | undefined, omitted: number | undefined): string {
    const shown = (list ?? []).join(', ');
    return omitted ? `${shown} ${t('imports.receipt.and_more', { count: omitted })}` : shown;
  }

  function shortSha(sha: string | null): string {
    return sha ? sha.slice(0, 12) : '—';
  }
</script>

<details class="import-receipt" open={incomplete}>
  <summary>
    {t('imports.receipt.title')}
    <span class={incomplete ? 'receipt-flag warn' : 'receipt-flag'}>
      {incomplete ? t('imports.receipt.incomplete') : t('imports.receipt.complete')}
    </span>
  </summary>

  {#if notRun.length > 0}
    <div class="receipt-ci">
      <h4>{t('imports.receipt.ci_not_run')}</h4>
      <ul>
        {#each notRun as item (item.path)}
          <li><code>{item.path}</code> — {item.reason}</li>
        {/each}
      </ul>
    </div>
  {/if}
  {#if receipt.ci?.unavailable}
    <p class="muted">{t('imports.receipt.ci_unavailable')}: {receipt.ci.unavailable}</p>
  {/if}

  {#if receipt.objects.length > 0}
    <table class="receipt-objects">
      <thead>
        <tr>
          <th>{t('imports.receipt.object')}</th>
          <th>{t('imports.receipt.source')}</th>
          <th>{t('imports.receipt.here')}</th>
          <th>{t('imports.receipt.not_imported')}</th>
        </tr>
      </thead>
      <tbody>
        {#each receipt.objects as row (row.kind)}
          <tr class={row.not_imported > 0 ? 'missing' : ''}>
            <td>{kindLabel(row.kind)}</td>
            <td>{row.source ?? '—'}</td>
            <td>{row.imported}</td>
            <td>
              {row.not_imported}
              {#if row.missing?.length}
                <div class="receipt-names">{names(row.missing, row.missing_omitted)}</div>
              {/if}
              {#if row.skipped}
                <div class="muted">
                  {t('imports.receipt.skipped', { count: row.skipped })}{row.skipped_reason ? `: ${row.skipped_reason}` : ''}
                </div>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}

  {#if receipt.refs.unavailable}
    <p class="muted">{t('imports.receipt.refs_unavailable')}: {receipt.refs.unavailable}</p>
  {/if}
  {#if refKinds.length > 0 || receipt.refs.default_branch}
    <ul class="receipt-refs">
      {#each refKinds as { key, label, value } (key)}
        <li>
          {label}: {t('imports.receipt.refs_count', { imported: value.imported, source: value.source })}
          {#if value.missing?.length}
            <div class="receipt-names">
              {t('imports.receipt.refs_missing')}: {names(value.missing, value.missing_omitted)}
            </div>
          {/if}
          {#if value.different?.length}
            <div class="receipt-names">
              {t('imports.receipt.refs_different')}: {names(value.different, value.different_omitted)}
            </div>
          {/if}
        </li>
      {/each}
      {#if receipt.refs.default_branch}
        {@const head = receipt.refs.default_branch}
        <li>
          {t('imports.receipt.default_branch')} <code>{head.name}</code>:
          {#if head.source_sha && head.source_sha === head.imported_sha}
            {t('imports.receipt.same_commit', { sha: shortSha(head.imported_sha) })}
          {:else}
            {t('imports.receipt.other_commit', {
              source: shortSha(head.source_sha),
              imported: shortSha(head.imported_sha),
            })}
          {/if}
          {#if head.commits != null}
            · {t('imports.receipt.commits', { count: head.commits })}
          {/if}
        </li>
      {/if}
    </ul>
  {/if}

  {#if receipt.not_transferable.length > 0}
    <div class="receipt-manual">
      <h4>{t('imports.receipt.not_transferable')}</h4>
      <ul>
        {#each receipt.not_transferable as item (item)}
          <li>{item}</li>
        {/each}
      </ul>
    </div>
  {/if}
</details>

<style>
  .import-receipt {
    margin-top: 8px;
    font-size: 12px;
  }

  summary {
    cursor: pointer;
    font-weight: 600;
    color: var(--text-secondary);
  }

  .receipt-flag {
    margin-left: 6px;
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    font-weight: 500;
  }

  .receipt-flag.warn,
  .receipt-ci h4 {
    color: #d29922;
  }

  h4 {
    margin: 10px 0 4px;
    font-size: 12px;
  }

  ul {
    margin: 4px 0;
    padding-left: 18px;
  }

  .receipt-objects {
    margin-top: 8px;
    width: auto;
    border-collapse: collapse;
  }

  .receipt-objects th,
  .receipt-objects td {
    padding: 4px 10px 4px 0;
    border-bottom: 1px solid var(--border);
    text-align: left;
    vertical-align: top;
  }

  .receipt-objects tr.missing td {
    color: #d29922;
  }

  .receipt-names {
    word-break: break-word;
  }

  .muted {
    color: var(--text-secondary);
  }
</style>
