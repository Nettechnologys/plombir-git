<script lang="ts">
  // Read-only rendering of the `PrDiff` shape (`files_changed` + `stats`) that
  // `GET /pulls/{number}/diff` answers with and the compare endpoint reuses.
  // The pull-request page keeps its own copy because it threads inline review
  // comments through every line; this one is for diffs nobody can comment on yet.
  import type { FileDiff, PrDiff } from '$lib/api/pulls';
  import { createT } from '$lib/i18n';

  const t = createT();

  let { files, stats }: { files: FileDiff[]; stats: PrDiff['stats'] } = $props();
</script>

<div class="diff-view">
  <div class="diff-summary">
    <strong>{t('pulls.compare.files_changed', { count: stats.files_changed })}</strong>
    <span class="addition-text">+{stats.total_additions}</span>
    <span class="deletion-text">−{stats.total_deletions}</span>
  </div>
  {#each files as file (file.path)}
    <section class="diff-file">
      <header class="diff-file-header">
        <code>{file.path}</code>
        <span><span class="addition-text">+{file.additions}</span> <span class="deletion-text">−{file.deletions}</span></span>
      </header>
      <div class="diff-lines">
        {#each file.lines as line, index (`${file.path}:${index}`)}
          <div class="diff-line" class:addition={line.kind === 'addition'} class:deletion={line.kind === 'deletion'} class:meta={line.kind === 'meta'}>
            <span class="line-number">{line.old_line ?? ''}</span>
            <span class="line-number">{line.new_line ?? ''}</span>
            <code>{line.content || ' '}</code>
          </div>
        {/each}
      </div>
    </section>
  {/each}
</div>

<style>
  .diff-view { display: flex; flex-direction: column; gap: 12px; }
  .diff-summary { display: flex; gap: 10px; align-items: center; }
  .addition-text { color: var(--green); }
  .deletion-text { color: var(--red); }
  .diff-file { border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden; }
  .diff-file-header { display: flex; justify-content: space-between; gap: 12px; padding: 10px 12px; background: var(--bg-tertiary); border-bottom: 1px solid var(--border); font-size: 13px; }
  .diff-file-header code { overflow-wrap: anywhere; }
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
