<script lang="ts">
  // Read-only rendering of the `PrDiff` shape (`files_changed` + `stats`) that
  // `GET /pulls/{number}/diff` answers with and the compare endpoint reuses.
  // The pull-request page keeps its own copy because it threads inline review
  // comments through every line; this one is for diffs nobody can comment on yet.
  import type { FileDiff, PrDiff } from '$lib/api/pulls';
  import { createT } from '$lib/i18n';

  const t = createT();

  let { files, stats }: { files: FileDiff[]; stats: PrDiff['stats'] } = $props();

  const statusLabel = (status: string) => {
    switch (status) {
      case 'added': return t('pulls.compare.file_status.added');
      case 'deleted': return t('pulls.compare.file_status.deleted');
      case 'modified': return t('pulls.compare.file_status.modified');
      default: return status;
    }
  };

  const isBinary = (file: FileDiff) =>
    file.lines.some((line) => line.kind === 'meta' && /^Binary files .* differ$/.test(line.content));
</script>

<div class="diff-view">
  <div class="diff-summary">
    <strong>{t('pulls.compare.files_changed', { count: stats.files_changed })}</strong>
    <span class="addition-text">+{stats.total_additions}</span>
    <span class="deletion-text">−{stats.total_deletions}</span>
  </div>
  {#each files as file (file.path)}
    <details class="diff-file" open>
      <summary class="diff-file-header">
        <span class="diff-file-name">
          <span class="file-status" data-status={file.status}>{statusLabel(file.status)}</span>
          <code>{file.path}</code>
        </span>
        <span><span class="addition-text">+{file.additions}</span> <span class="deletion-text">−{file.deletions}</span></span>
      </summary>
      <!-- git prints a blob it cannot line-count as one `Binary files … differ`
           line, which on its own reads as an empty diff. -->
      {#if isBinary(file)}
        <p class="no-text-diff">{t('pulls.compare.no_text_diff')}</p>
      {:else}
        <div class="diff-lines">
          {#each file.lines as line, index (`${file.path}:${index}`)}
            <div class="diff-line" class:addition={line.kind === 'addition'} class:deletion={line.kind === 'deletion'} class:meta={line.kind === 'meta'}>
              <span class="line-number">{line.old_line ?? ''}</span>
              <span class="line-number">{line.new_line ?? ''}</span>
              <code>{line.content || ' '}</code>
            </div>
          {/each}
        </div>
      {/if}
    </details>
  {/each}
</div>

<style>
  .diff-view { display: flex; flex-direction: column; gap: 12px; }
  .diff-summary { display: flex; gap: 10px; align-items: center; }
  .addition-text { color: var(--green); }
  .deletion-text { color: var(--red); }
  .diff-file { border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden; }
  .diff-file-header { display: flex; justify-content: space-between; gap: 12px; padding: 10px 12px; background: var(--bg-tertiary); border-bottom: 1px solid var(--border); font-size: 13px; cursor: pointer; list-style: none; }
  .diff-file-header::-webkit-details-marker { display: none; }
  .diff-file:not([open]) .diff-file-header { border-bottom: none; }
  .diff-file-name { display: flex; gap: 8px; align-items: center; min-width: 0; }
  .diff-file-header code { overflow-wrap: anywhere; }
  .file-status { flex: none; padding: 1px 6px; border-radius: 10px; font-size: 11px; border: 1px solid var(--border); color: var(--text-secondary); }
  .file-status[data-status='added'] { color: var(--green); border-color: var(--green); }
  .file-status[data-status='deleted'] { color: var(--red); border-color: var(--red); }
  .no-text-diff { margin: 0; padding: 10px 12px; color: var(--text-secondary); font-size: 13px; }
  .diff-lines { overflow-x: auto; }
  .diff-line {
    display: grid;
    grid-template-columns: 48px 48px 1fr;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 20px;
  }
  .diff-line.addition { background: rgba(63, 185, 80, 0.12); }
  .diff-line.deletion { background: rgba(248, 81, 73, 0.12); }
  .diff-line.meta { background: rgba(88, 166, 255, 0.09); color: var(--text-secondary); }
  .diff-line > code { padding: 0 10px; white-space: pre; border-left: 1px solid var(--border); }
  .line-number { padding: 0 6px; color: var(--text-secondary); text-align: right; user-select: none; border-left: 1px solid var(--border); }
</style>
