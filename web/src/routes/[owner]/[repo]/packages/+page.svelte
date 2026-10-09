<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { packages } from '$lib/api/client.svelte';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDate } from '$lib/i18n';
  import { viewerPermission } from '$lib/viewerPermission.svelte';
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
  // Publishing is `RepoWrite` (card_270a0a77fd79).
  const permission = viewerPermission(() => owner, () => repo);
  let formatFilter = $state<string>('');
  let searchQuery = $state<string>('');
  let packageList = $state<any[]>([]);
  let failedRegistryTypes = $state<string[]>([]);
  let loading = $state(true);
  let error = $state('');
  let currentPage = $state(1);
  let totalPages = $state(1);
  const packageListRequests = new LatestRepositoryResourceRequestFence<string>();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    formatFilter = '';
    searchQuery = '';
    packageList = [];
    failedRegistryTypes = [];
    currentPage = 1;
    totalPages = 1;
    loading = true;
    error = '';
    void loadPackages(expectedOwner, expectedRepo, '', '', 1, routeGeneration);
  });

  function packageListIntent(format: string, query: string, pageNumber: number): string {
    return JSON.stringify([format, query, pageNumber]);
  }

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number): boolean {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function ownsPackageList(
    claim: ReturnType<typeof packageListRequests.begin>,
    expectedOwner: string,
    expectedRepo: string,
    expectedRoute: number,
  ): boolean {
    return (
      packageListRequests.owns(
        claim,
        owner,
        repo,
        packageListIntent(formatFilter, searchQuery, currentPage),
      ) && isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
    );
  }

  async function loadPackages(
    expectedOwner = owner,
    expectedRepo = repo,
    expectedFormat = formatFilter,
    expectedQuery = searchQuery,
    expectedPage = currentPage,
    expectedRoute = routeGeneration,
  ) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = packageListRequests.begin(
      expectedOwner,
      expectedRepo,
      packageListIntent(expectedFormat, expectedQuery, expectedPage),
    );
    loading = true;
    error = '';
    try {
      const res = await packages.list(
        expectedOwner,
        expectedRepo,
        expectedFormat || undefined,
        expectedPage,
        20,
        expectedQuery,
      );
      if (ownsPackageList(claim, expectedOwner, expectedRepo, expectedRoute)) {
        packageList = res.data;
        totalPages = res.pagination.total_pages;
        failedRegistryTypes = res.failedRegistryTypes;
      }
    } catch (e: any) {
      if (ownsPackageList(claim, expectedOwner, expectedRepo, expectedRoute)) {
        error = e.message;
        failedRegistryTypes = [];
      }
    } finally {
      if (ownsPackageList(claim, expectedOwner, expectedRepo, expectedRoute)) {
        loading = false;
      }
    }
  }

  function handleFormatChange() {
    currentPage = 1;
    void loadPackages(owner, repo, formatFilter, searchQuery, 1, routeGeneration);
  }

  function handleSearch() {
    currentPage = 1;
    void loadPackages(owner, repo, formatFilter, searchQuery, 1, routeGeneration);
  }

  function retryFailedRegistries() {
    void loadPackages(owner, repo, formatFilter, searchQuery, currentPage, routeGeneration);
  }

  function selectPage(nextPage: number) {
    currentPage = nextPage;
    void loadPackages(owner, repo, formatFilter, searchQuery, nextPage, routeGeneration);
  }

  function encodePackageRouteName(name: string): string {
    return name.split('/').map(encodeURIComponent).join('/');
  }

  function packageHref(pkg: { format: string; name: string }): string {
    return `/${owner}/${repo}/packages/${encodeURIComponent(pkg.format)}/${encodePackageRouteName(pkg.name)}`;
  }
</script>

<svelte:head>
  <title>{t('packages.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader owner={owner!} repo={repo!} activeTab="packages" />

  <div class="page-header">
    <h1>{t('repo.tabs.packages')}</h1>
    {#if permission.canWrite}
      <a href={`/${owner}/${repo}/packages/upload`} class="btn-primary">{t('packages.upload')}</a>
    {/if}
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if failedRegistryTypes.length > 0}
    <div class="partial-banner" role="status">
      <span>
        {t('packages.partial_unavailable', {
          formats: failedRegistryTypes.map(packageFormatLabel).join(', '),
        })}
      </span>
      <button class="btn-secondary partial-retry" onclick={retryFailedRegistries} disabled={loading}>
        {t('common.retry')}
      </button>
    </div>
  {/if}

  <div class="filters">
    <div class="filter-group">
      <label for="format-filter">{t('packages.format')}:</label>
      <select id="format-filter" bind:value={formatFilter} onchange={handleFormatChange}>
        <option value="">{t('common.all', 'All')}</option>
        {#each PACKAGE_FORMATS as f}
          <option value={f}>{packageFormatOptionLabel(f)}</option>
        {/each}
      </select>
    </div>

    <div class="search-group">
      <input
        type="text"
        placeholder={t('common.search', 'Search...')}
        bind:value={searchQuery}
        onkeydown={(e) => e.key === 'Enter' && handleSearch()}
      />
      <button class="btn-secondary" onclick={handleSearch}>{t('common.search', 'Search')}</button>
    </div>
  </div>

  {#if loading}
    <p class="loading-text">{t('common.loading')}</p>
  {:else if packageList.length === 0 && failedRegistryTypes.length === 0}
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
          onclick={() => selectPage(currentPage - 1)}
        >
          {t('common.previous', 'Previous')}
        </button>
        <span class="page-info">{t('packages.page_info', { page: currentPage, total: totalPages })}</span>
        <button
          class="btn-outline"
          disabled={currentPage >= totalPages}
          onclick={() => selectPage(currentPage + 1)}
        >
          {t('common.next', 'Next')}
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

  .partial-banner {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    margin-bottom: 16px;
    padding: 10px 12px;
    border: 1px solid var(--yellow, var(--border));
    border-radius: var(--radius);
    background: var(--bg-secondary);
    font-size: 13px;
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
