<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { packages } from '$lib/api/client.svelte';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDate } from '$lib/i18n';
  import { packageFormatLabel } from '$lib/packageFormats';
  import { packageInstallSnippet, packageInstallText } from '$lib/packageInstall';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let format = $derived($page.params.format!);

  let packageList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let currentPage = $state(1);
  let totalPages = $state(1);
  const packageRequests = new LatestRepositoryResourceRequestFence<string>();

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedFormat = format;
    void loadPackages(expectedOwner, expectedRepo, expectedFormat);
  });

  async function loadPackages(expectedOwner: string, expectedRepo: string, expectedFormat: string) {
    const claim = packageRequests.begin(expectedOwner, expectedRepo, expectedFormat);
    loading = true;
    error = '';
    packageList = [];
    currentPage = 1;
    totalPages = 1;
    try {
      const res = await packages.getFormat(expectedOwner, expectedRepo, expectedFormat);
      if (!packageRequests.owns(claim, owner, repo, format)) return;
      packageList = res.packages || [];
      totalPages = 1;
    } catch (e: any) {
      if (packageRequests.owns(claim, owner, repo, format)) error = e.message;
    } finally {
      if (packageRequests.owns(claim, owner, repo, format)) loading = false;
    }
  }

  function getInstallCommand(pkg: { name: string; latest_version?: string }): string {
    return packageInstallText(
      packageInstallSnippet({
        format: format!,
        owner: owner!,
        repo: repo!,
        name: pkg.name,
        version: pkg.latest_version || undefined,
        origin: $page.url.origin,
      }),
    );
  }

  function encodePackageRouteName(name: string): string {
    return name.split('/').map(encodeURIComponent).join('/');
  }

  function packageHref(pkg: { name: string }): string {
    return `/${owner}/${repo}/packages/${encodeURIComponent(format!)}/${encodePackageRouteName(pkg.name)}`;
  }
</script>

<svelte:head>
  <title>{packageFormatLabel(format!)} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader owner={owner!} repo={repo!} activeTab="packages" />

  <div class="page-header">
    <h1>{t('packages.title')} — {packageFormatLabel(format!)}</h1>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

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
            {#if pkg.latest_version}
              <span class="version-badge">v{pkg.latest_version}</span>
            {/if}
          </div>

          {#if pkg.description}
            <p class="package-desc">{pkg.description}</p>
          {/if}

          <div class="package-meta">
            {#if pkg.created_at}
              <span class="date">{t('common.created', { date: formatDate(pkg.created_at) })}</span>
            {/if}
          </div>

          <div class="install-section">
            <pre><code>{getInstallCommand(pkg)}</code></pre>
            <button class="copy-btn" onclick={() => navigator.clipboard.writeText(getInstallCommand(pkg))}>
              {t('common.copy', 'Copy')}
            </button>
          </div>
        </div>
      {/each}
    </div>
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

  .version-badge {
    padding: 2px 8px;
    border-radius: 10px;
    font-size: 12px;
    font-weight: 600;
    background: var(--green-dim);
    color: #fff;
  }

  .package-desc {
    font-size: 14px;
    color: var(--text-secondary);
    line-height: 1.6;
    margin-bottom: 8px;
  }

  .package-meta {
    font-size: 13px;
    color: var(--text-muted);
    margin-bottom: 12px;
  }

  .install-section {
    background: var(--bg-tertiary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px;
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .install-section pre {
    flex: 1;
    margin: 0;
    overflow-x: auto;
  }

  .install-section code {
    font-size: 13px;
    color: var(--text-primary);
  }

  .copy-btn {
    padding: 4px 10px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12px;
    cursor: pointer;
    color: var(--text-primary);
  }
  .copy-btn:hover { background: var(--bg-hover); }
</style>
