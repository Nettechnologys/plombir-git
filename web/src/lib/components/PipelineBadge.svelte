<script lang="ts">
  import { createT } from '$lib/i18n';

  const t = createT();

  interface Props {
    status: string;
  }

  let { status }: Props = $props();

  const statusConfig: Record<string, { color: string; bg: string }> = {
    pending:  { color: 'var(--yellow)',  bg: 'rgba(210, 153, 34, 0.15)' },
    running:  { color: 'var(--accent)',  bg: 'rgba(88, 166, 255, 0.15)' },
    manual:   { color: 'var(--yellow)',  bg: 'rgba(210, 153, 34, 0.15)' },
    waiting_approval: { color: 'var(--yellow)', bg: 'rgba(210, 153, 34, 0.15)' },
    success:  { color: 'var(--green)',   bg: 'rgba(63, 185, 80, 0.15)' },
    failed:   { color: 'var(--red)',     bg: 'rgba(248, 81, 73, 0.15)' },
    failure:  { color: 'var(--red)',     bg: 'rgba(248, 81, 73, 0.15)' },
    error:    { color: 'var(--red)',     bg: 'rgba(248, 81, 73, 0.15)' },
    canceled: { color: 'var(--text-muted)', bg: 'rgba(110, 118, 129, 0.15)' },
    skipped:  { color: 'var(--text-muted)', bg: 'rgba(110, 118, 129, 0.15)' },
  };

  const cfg = $derived(statusConfig[status] || statusConfig.pending);
</script>

<span class="badge" style="color: {cfg.color}; background: {cfg.bg}">
  {#if status === 'running'}
    <span class="spinner"></span>
  {:else if status === 'success'}
    ✓
  {:else if status === 'failed' || status === 'failure' || status === 'error'}
    ✗
  {:else if status === 'manual'}
    ▶
  {:else if status === 'waiting_approval'}
    ◷
  {:else if status === 'canceled'}
    −
  {:else if status === 'skipped'}
    ○
  {:else}
    ●
  {/if}
  {t(`pipeline.status.${status}`)}
</span>

<style>
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    padding: 2px 8px;
    border-radius: 12px;
    font-size: 12px;
    font-weight: 600;
  }

  .spinner {
    width: 10px;
    height: 10px;
    border: 2px solid currentColor;
    border-top-color: transparent;
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }
</style>
