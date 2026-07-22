<script lang="ts">
  import { repos } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let repoList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let page = $state(1);
  let totalPages = $state(1);
  let totalCount = $state(0);
  const perPage = 24;

  $effect(() => {
    loadPage(1);
  });

  async function loadPage(p: number) {
    loading = true;
    error = '';
    try {
      const result = await repos.explore(p, perPage);
      repoList = result.data;
      totalPages = result.pagination?.total_pages ?? 1;
      totalCount = result.pagination?.total ?? 0;
      page = p;
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }
</script>

<svelte:head>
  <title>{t('explore.title')} · IronForge</title>
</svelte:head>

<div class="page-container">
  <div class="page-header">
    <h1>{t('explore.title')}</h1>
    <p class="subtitle">{t('explore.subtitle', { count: totalCount })}</p>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if repoList.length === 0}
    <div class="empty">
      <p>{t('explore.empty')}</p>
    </div>
  {:else}
    <div class="repo-grid">
      {#each repoList as repo}
        <a href={`/${repo.owner_name}/${repo.name}`} class="repo-card">
          <div class="rc-icon">📂</div>
          <div class="rc-body">
            <div class="rc-name">{repo.owner_name}/{repo.name}</div>
            <div class="rc-desc">{repo.description || t('common.no_description')}</div>
            <div class="rc-meta">
              {repo.stars_count || 0} ⭐ · {t('common.updated', { date: formatDate(repo.updated_at) })}
            </div>
          </div>
        </a>
      {/each}
    </div>

    {#if totalPages > 1}
      <div class="pagination">
        <button
          class="btn-page"
          disabled={page <= 1}
          onclick={() => loadPage(page - 1)}
        >← {t('explore.prev')}</button>
        <span class="page-info">{page} / {totalPages}</span>
        <button
          class="btn-page"
          disabled={page >= totalPages}
          onclick={() => loadPage(page + 1)}
        >{t('explore.next')} →</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .page-header {
    margin-bottom: 24px;
  }

  h1 { font-size: 24px; margin-bottom: 4px; }

  .subtitle {
    font-size: 14px;
    color: var(--text-secondary);
  }

  .empty {
    text-align: center;
    padding: 60px 24px;
    color: var(--text-secondary);
  }

  .repo-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
  }

  @media (max-width: 900px) {
    .repo-grid { grid-template-columns: repeat(2, 1fr); }
  }
  @media (max-width: 600px) {
    .repo-grid { grid-template-columns: 1fr; }
  }

  .repo-card {
    display: flex;
    gap: 12px;
    padding: 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    text-decoration: none;
    color: var(--text-primary);
    transition: border-color 0.15s;
  }
  .repo-card:hover {
    border-color: var(--accent);
    text-decoration: none;
  }

  .rc-icon { font-size: 20px; flex-shrink: 0; }
  .rc-body { flex: 1; min-width: 0; }

  .rc-name {
    font-size: 14px;
    font-weight: 600;
    color: var(--accent);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rc-desc {
    font-size: 12px;
    color: var(--text-secondary);
    margin-top: 4px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rc-meta {
    font-size: 11px;
    color: var(--text-muted);
    margin-top: 8px;
  }

  .pagination {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 16px;
    margin-top: 32px;
  }

  .btn-page {
    padding: 6px 14px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 13px;
    cursor: pointer;
  }
  .btn-page:hover:not(:disabled) { background: var(--bg-hover); }
  .btn-page:disabled { opacity: 0.4; cursor: default; }

  .page-info {
    font-size: 13px;
    color: var(--text-muted);
  }
</style>
