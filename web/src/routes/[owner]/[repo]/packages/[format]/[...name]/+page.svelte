<script lang="ts">
  import { copyToClipboard } from '$lib/clipboard';
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { packages } from '$lib/api/client.svelte';
  import { nextYankState } from '$lib/api/packageYank';
  import { LatestRepositoryResourceRequestFence } from '$lib/asyncStateOwnership';
  import { createT, formatDate } from '$lib/i18n';
  import { viewerPermission } from '$lib/viewerPermission.svelte';
  import { packageFormatLabel } from '$lib/packageFormats';
  import { packageInstallSnippet, packageInstallText } from '$lib/packageInstall';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  // Yanking and deleting a version are `RepoWrite` (card_270a0a77fd79).
  const permission = viewerPermission(() => owner, () => repo);
  let format = $derived($page.params.format!);
  let name = $derived($page.params.name!);

  type PackageFile = {
    filename: string;
    size?: number;
    sha256?: string | null;
  };

  type PackageVersion = {
    version: string;
    is_yanked?: boolean;
    files?: PackageFile[];
  };

  let packageInfo = $state<any>(null);
  let versions = $state<PackageVersion[]>([]);
  let loading = $state(true);
  let error = $state('');
  let confirmDelete = $state<string | null>(null);
  let busyVersions = $state<Set<string>>(new Set());
  const packageRequests = new LatestRepositoryResourceRequestFence<string>();
  let routeGeneration = 0;

  $effect(() => {
    routeGeneration += 1;
    packageInfo = null;
    versions = [];
    loading = true;
    error = '';
    confirmDelete = null;
    busyVersions = new Set();
    void loadPackage();
  });

  function packageIdentity(expectedFormat: string, expectedName: string): string {
    return `${expectedFormat}\u0000${expectedName}`;
  }

  function isCurrentRoute(
    expectedOwner: string,
    expectedRepo: string,
    expectedFormat: string,
    expectedName: string,
    expectedRoute: number,
  ): boolean {
    return routeGeneration === expectedRoute
      && owner === expectedOwner
      && repo === expectedRepo
      && format === expectedFormat
      && name === expectedName;
  }

  function isVersionBusy(version: string): boolean {
    return busyVersions.has(version);
  }

  function claimVersion(version: string): boolean {
    if (isVersionBusy(version)) return false;
    busyVersions = new Set(busyVersions).add(version);
    return true;
  }

  function releaseVersion(version: string): void {
    const next = new Set(busyVersions);
    next.delete(version);
    busyVersions = next;
  }

  async function loadPackage() {
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedFormat = format!;
    const expectedName = name!;
    const identity = packageIdentity(expectedFormat, expectedName);
    const claim = packageRequests.begin(expectedOwner, expectedRepo, identity);
    loading = true;
    error = '';
    try {
      const [info, versionRes] = await Promise.all([
        packages.get(expectedOwner, expectedRepo, expectedFormat, expectedName),
        packages.getVersions(expectedOwner, expectedRepo, expectedFormat, expectedName),
      ]);
      if (!packageRequests.owns(claim, owner!, repo!, packageIdentity(format!, name!))) return;
      packageInfo = info;
      versions = versionRes.versions || [];
    } catch (e: any) {
      if (packageRequests.owns(claim, owner!, repo!, packageIdentity(format!, name!))) error = e.message;
    } finally {
      if (packageRequests.owns(claim, owner!, repo!, packageIdentity(format!, name!))) loading = false;
    }
  }

  async function loadVersions(
    expectedOwner: string,
    expectedRepo: string,
    expectedFormat: string,
    expectedName: string,
  ) {
    const identity = packageIdentity(expectedFormat, expectedName);
    const claim = packageRequests.begin(expectedOwner, expectedRepo, identity);
    try {
      const res = await packages.getVersions(expectedOwner, expectedRepo, expectedFormat, expectedName);
      if (!packageRequests.owns(claim, owner!, repo!, packageIdentity(format!, name!))) return;
      versions = res.versions || [];
    } catch (e: any) {
      if (packageRequests.owns(claim, owner!, repo!, packageIdentity(format!, name!))) error = e.message;
    }
  }

  async function handleDeleteVersion(version: string) {
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedFormat = format!;
    const expectedName = name!;
    const expectedRoute = routeGeneration;
    if (!claimVersion(version)) return;
    packageRequests.begin(expectedOwner, expectedRepo, packageIdentity(expectedFormat, expectedName));
    try {
      await packages.delete(expectedOwner, expectedRepo, expectedFormat, expectedName, version);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedFormat, expectedName, expectedRoute)) return;
      confirmDelete = null;
      await loadPackage();
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedFormat, expectedName, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedFormat, expectedName, expectedRoute)) {
        releaseVersion(version);
      }
    }
  }

  /**
   * Withdraw a version without destroying it, or put it back.
   *
   * Deleting is the only other way to pull a bad release, and it takes the
   * publisher attribution with it and cannot be undone. `nextYankState` is
   * what makes this button a toggle rather than a one-way trip.
   */
  async function handleToggleYank(version: PackageVersion) {
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedFormat = format!;
    const expectedName = name!;
    const expectedRoute = routeGeneration;
    const versionName = version.version;
    const yanked = nextYankState(version.is_yanked);
    if (!claimVersion(versionName)) return;
    packageRequests.begin(expectedOwner, expectedRepo, packageIdentity(expectedFormat, expectedName));
    error = '';
    try {
      await packages.yank(expectedOwner, expectedRepo, expectedFormat, expectedName, versionName, yanked);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedFormat, expectedName, expectedRoute)) return;
      await loadVersions(expectedOwner, expectedRepo, expectedFormat, expectedName);
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedFormat, expectedName, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedFormat, expectedName, expectedRoute)) {
        releaseVersion(versionName);
      }
    }
  }

  function installSnippet(ver: string) {
    return packageInstallSnippet({
      format: format!,
      owner: owner!,
      repo: repo!,
      name: name!,
      version: ver,
      origin: $page.url.origin,
    });
  }

  function getInstallCommand(ver: string): string {
    return packageInstallText(installSnippet(ver));
  }

  function copyInstall(ver: string) {
    void copyToClipboard(getInstallCommand(ver));
  }

  function formatSize(size?: number): string {
    if (!Number.isFinite(size)) return '';
    const units = ['B', 'KB', 'MB', 'GB'];
    let value = Number(size);
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return `${value >= 10 || unit === 0 ? value.toFixed(0) : value.toFixed(1)} ${units[unit]}`;
  }

  function packageDownloadUrl(ver: string, filename: string): string {
    return packages.downloadUrl(owner!, repo!, format!, name!, ver, filename);
  }
</script>

<svelte:head>
  <title>{name} · {packageFormatLabel(format!)} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader owner={owner!} repo={repo!} activeTab="packages" />

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="loading-text">{t('common.loading')}</p>
  {:else if !packageInfo}
    <div class="empty">
      <p>{t('packages.no_packages')}</p>
    </div>
  {:else}
    <div class="package-detail">
      <div class="package-header">
        <h1>{packageInfo.name}</h1>
        {#if packageInfo.latest_version}
          <span class="version-badge">v{packageInfo.latest_version}</span>
        {/if}
      </div>

      {#if packageInfo.description}
        <p class="package-desc">{packageInfo.description}</p>
      {/if}

      <div class="package-meta">
        {#if packageInfo.created_at}
          <span>{t('common.created', { date: formatDate(packageInfo.created_at) })}</span>
        {/if}
      </div>

      <!-- Version list -->
      <div class="versions-section">
        <h2>{t('packages.versions')}</h2>
        {#each versions as version}
          <div class="version-card" class:yanked={version.is_yanked}>
            <div class="version-header">
              <span class="version-name">v{version.version}</span>
              {#if version.is_yanked}
                <span class="yanked-badge" title={t('packages.yanked_hint')}>{t('packages.yanked')}</span>
              {/if}
              <div class="version-actions">
                <button class="copy-btn" onclick={() => copyInstall(version.version)}>
                  {t('common.copy')} {t('packages.install')}
                </button>
                {#if permission.canWrite}
                  <button
                    class="secondary-btn"
                    disabled={isVersionBusy(version.version)}
                    title={t('packages.yank_hint')}
                    onclick={() => handleToggleYank(version)}
                  >
                    {version.is_yanked ? t('packages.unyank') : t('packages.yank')}
                  </button>
                  <button class="danger-btn" disabled={isVersionBusy(version.version)} onclick={() => { confirmDelete = version.version; }}>
                    {t('common.delete')}
                  </button>
                {/if}
              </div>
            </div>

            {#if version.files && version.files.length > 0}
              <div class="version-files">
                {#each version.files as file}
                  <a class="file-link" href={packageDownloadUrl(version.version, file.filename)}>
                    <span>{file.filename}</span>
                    {#if file.size !== undefined}
                      <span class="file-size">{formatSize(file.size)}</span>
                    {/if}
                  </a>
                {/each}
              </div>
            {/if}

            {#if confirmDelete === version.version}
              <div class="delete-confirm">
                <span>{t('packages.delete_confirm', { name: packageInfo.name, version: version.version })}</span>
                <button class="danger-btn" disabled={isVersionBusy(version.version)} onclick={() => handleDeleteVersion(version.version)}>
                  {t('common.delete')}
                </button>
                <button class="secondary-btn" disabled={isVersionBusy(version.version)} onclick={() => { confirmDelete = null; }}>
                  {t('common.cancel')}
                </button>
              </div>
            {/if}
          </div>
        {/each}
      </div>
    </div>
  {/if}
</div>

<style>
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

  .package-detail {
    display: flex;
    flex-direction: column;
    gap: 24px;
  }

  .package-header {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  h1 {
    font-size: 24px;
    font-weight: 600;
  }

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
  }

  .package-meta {
    font-size: 13px;
    color: var(--text-muted);
  }

  .versions-section {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  h2 {
    font-size: 18px;
    font-weight: 600;
  }

  .version-card {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 16px;
  }

  .version-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    flex-wrap: wrap;
  }

  .version-name {
    font-size: 16px;
    font-weight: 600;
    color: var(--text-primary);
  }

  /* A yanked version stays on the list — keeping the row and its publisher
     attribution is the whole difference from delete — so it is marked, not
     hidden. */
  .yanked-badge {
    padding: 2px 8px;
    border-radius: 999px;
    border: 1px solid var(--border);
    background: var(--bg-hover);
    color: var(--text-secondary);
    font-size: 12px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }

  .version-card.yanked {
    opacity: 0.72;
  }

  .version-actions {
    display: flex;
    gap: 8px;
    margin-left: auto;
  }

  .version-files {
    margin-top: 12px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .file-link {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 10px;
    color: var(--text-primary);
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    text-decoration: none;
    font-size: 13px;
  }

  .file-link:hover {
    background: var(--bg-hover);
  }

  .file-size {
    flex-shrink: 0;
    color: var(--text-muted);
  }

  .copy-btn {
    padding: 4px 10px;
    background: var(--bg-tertiary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12px;
    cursor: pointer;
    color: var(--text-primary);
  }
  .copy-btn:hover { background: var(--bg-hover); }

  .danger-btn {
    padding: 4px 10px;
    background: var(--red-dim);
    border: 1px solid var(--red);
    border-radius: var(--radius);
    font-size: 12px;
    cursor: pointer;
    color: #fff;
  }
  .danger-btn:hover { background: var(--red); }

  .secondary-btn {
    padding: 4px 10px;
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 12px;
    cursor: pointer;
    color: var(--text-primary);
  }
  .secondary-btn:hover { background: var(--bg-hover); }
  .secondary-btn:disabled { opacity: 0.6; cursor: default; }

  .delete-confirm {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
    margin-top: 12px;
    padding: 8px 12px;
    background: rgba(248, 81, 73, 0.05);
    border: 1px solid var(--red-dim);
    border-radius: var(--radius);
  }
  .delete-confirm span { color: var(--text-secondary); }

  @media (max-width: 600px) {
    .version-header {
      flex-direction: column;
      align-items: flex-start;
    }
    .version-actions {
      flex-direction: column;
      width: 100%;
    }
  }
</style>
