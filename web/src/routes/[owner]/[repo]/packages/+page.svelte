<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { packages } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';
  import {
    PACKAGE_FORMATS,
    packageFormatLabel,
    packageFormatOptionLabel,
    packageFormatSupportLabel,
    packageFormatUsesGenericFallback,
  } from '$lib/packageFormats';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let formatFilter = $state<string>('');
  let searchQuery = $state<string>('');
  let packageList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let currentPage = $state(1);
  let totalPages = $state(1);

  $effect(() => {
    loadPackages();
  });

  async function loadPackages() {
    loading = true;
    error = '';
    try {
      const res = await packages.list(
        owner!,
        repo!,
        formatFilter || undefined,
        currentPage,
        20,
        searchQuery,
      );
      packageList = res.data;
      totalPages = res.pagination.total_pages;
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function handleFormatChange() {
    currentPage = 1;
    loadPackages();
  }

  function handleSearch() {
    currentPage = 1;
    loadPackages();
  }

  function encodePackageRouteName(name: string): string {
    return name.split('/').map(encodeURIComponent).join('/');
  }

  function packageHref(pkg: { format: string; name: string }): string {
    return `/${owner}/${repo}/packages/${encodeURIComponent(pkg.format)}/${encodePackageRouteName(pkg.name)}`;
  }
</script>

<svelte:head>
  <title>Packages · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader owner={owner!} repo={repo!} activeTab="packages" />

  <div class="page-header">
    <h1>{t('repo.tabs.packages')}</h1>
    <a href={`/${owner}/${repo}/packages/upload`} class="btn-primary">{t('packages.upload')}</a>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  <div class="filters">
    <div class="filter-group">
      <label for="format-filter">{t('packages.format')}:</label>
      <select id="format-filter" bind:value={formatFilter} onchange={handleFormatChange}>
        <option value="">{t('common.all') || 'All'}</option>
        {#each PACKAGE_FORMATS as f}
          <option value={f}>{packageFormatOptionLabel(f)}</option>
        {/each}
      </select>
    </div>

    <div class="search-group">
      <input
        type="text"
        placeholder={t('common.search') || 'Search...'}
        bind:value={searchQuery}
        onkeydown={(e) => e.key === 'Enter' && handleSearch()}
      />
      <button class="btn-secondary" onclick={handleSearch}>{t('common.search') || 'Search'}</button>
    </div>
  </div>

  {#if loading}
    <p class="loading-text">{t('common.loading')}</p>
  {:else if packageList.length === 0}
    <div class="empty">
      <p>{t('packages.no_packages')}</p>
    </div>
  {:else}
    <div class="package-list">
      {#each packageList as pkg}
        <div class="package-card">
          <div class="package-header">
            <a href={packageHref(pkg)} class="package-name">{pkg.name}</a>
            <span
              class="format-badge"
              class:fallback={packageFormatUsesGenericFallback(pkg.format)}
              title={packageFormatSupportLabel(pkg.format)}
            >
              {packageFormatLabel(pkg.format)}
            </span>
          </div>
          {#if pkg.description}
            <p class="package-desc">{pkg.description}</p>
          {/if}
          <div class="package-meta">
            <span class="version">{t('packages.version')}: {pkg.latest_version}</span>
            {#if pkg.created_at}
              <span class="date">{t('common.created', { date: formatDate(pkg.created_at) })}</span>
            {/if}
          </div>
        </div>
      {/each}
    </div>

    {#if totalPages > 1}
      <div class="pagination">
        <button
          class="btn-outline"
          disabled={currentPage <= 1}
          onclick={() => { currentPage = currentPage - 1; loadPackages(); }}
        >
          {t('common.previous') || 'Previous'}
        </button>
        <span class="page-info">Page {currentPage} of {totalPages}</span>
        <button
          class="btn-outline"
          disabled={currentPage >= totalPages}
          onclick={() => { currentPage = currentPage + 1; loadPackages(); }}
        >
          {t('common.next') || 'Next'}
        </button>
      </div>
    {/if}
  {/if}
</div>

<style>

  .page-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 24px;
  }

  h1 {
    font-size: 24px;
    font-weight: 600;
  }

  .btn-primary {
    padding: 6px 16px;
    background: var(--orange);
    color: #fff;
    border: none;
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
    text-decoration: none;
  }
  .btn-primary:hover {
    background: #e09a1e;
    text-decoration: none;
  }
.filters {
    display: flex;
    gap: 16px;
    margin-bottom: 24px;
    flex-wrap: wrap;
  }

  .filter-group {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .filter-group label {
    font-size: 14px;
    color: var(--text-secondary);
  }

  .filter-group select {
    padding: 6px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-secondary);
    color: var(--text-primary);
    font-size: 14px;
  }

  .search-group {
    display: flex;
    gap: 8px;
    flex: 1;
    max-width: 400px;
  }

  .search-group input {
    flex: 1;
    padding: 6px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-secondary);
    color: var(--text-primary);
    font-size: 14px;
  }

  .btn-secondary {
    padding: 6px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 13px;
    cursor: pointer;
  }
  .btn-secondary:hover { background: var(--bg-hover); }

  .loading-text {
    color: var(--text-secondary);
    text-align: center;
    padding: 48px;
  }

  .empty {
    text-align: center;
    padding: 48px;
    color: var(--text-secondary);
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .package-list {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .package-card {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 20px;
  }

  .package-header {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-bottom: 8px;
  }

  .package-name {
    font-size: 18px;
    font-weight: 600;
    color: var(--accent);
    text-decoration: none;
  }
  .package-name:hover { text-decoration: underline; }

  .format-badge {
    padding: 2px 8px;
    border-radius: 10px;
    font-size: 12px;
    font-weight: 600;
    background: var(--bg-tertiary);
    color: var(--text-secondary);
  }
  .format-badge.fallback {
    border: 1px solid var(--border);
    color: var(--text-muted);
  }

  .package-desc {
    font-size: 14px;
    color: var(--text-secondary);
    line-height: 1.6;
    margin-bottom: 8px;
  }

  .package-meta {
    display: flex;
    gap: 16px;
    font-size: 13px;
    color: var(--text-muted);
  }

  .pagination {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 16px;
    margin-top: 24px;
  }

  .btn-outline {
    padding: 5px 12px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 13px;
    cursor: pointer;
  }
  .btn-outline:hover { background: var(--bg-hover); }
  .btn-outline:disabled { opacity: 0.5; cursor: not-allowed; }

  .page-info {
    font-size: 14px;
    color: var(--text-secondary);
  }

  @media (max-width: 600px) {
    .page-header {
      flex-direction: column;
      align-items: flex-start;
      gap: 12px;
    }

    .filters {
      flex-direction: column;
    }

    .search-group {
      max-width: 100%;
    }
  }
</style>
