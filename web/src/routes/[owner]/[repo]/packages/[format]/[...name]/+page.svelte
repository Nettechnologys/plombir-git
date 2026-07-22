<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { packages } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';
  import { packageFormatLabel } from '$lib/packageFormats';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let format = $derived($page.params.format!);
  let name = $derived($page.params.name!);

  type PackageFile = {
    filename: string;
    size?: number;
    sha256?: string | null;
  };

  type PackageVersion = {
    version: string;
    files?: PackageFile[];
  };

  let packageInfo = $state<any>(null);
  let versions = $state<PackageVersion[]>([]);
  let loading = $state(true);
  let error = $state('');
  let deletingVersion = $state<string | null>(null);
  let confirmDelete = $state<string | null>(null);

  $effect(() => {
    loadPackage();
  });

  async function loadPackage() {
    loading = true;
    error = '';
    try {
      const [info, versionRes] = await Promise.all([
        packages.get(owner!, repo!, format!, name!),
        packages.getVersions(owner!, repo!, format!, name!),
      ]);
      packageInfo = info;
      versions = versionRes.versions || [];
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadVersions() {
    try {
      const res = await packages.getVersions(owner!, repo!, format!, name!);
      versions = res.versions || [];
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleDeleteVersion(version: string) {
    deletingVersion = version;
    try {
      await packages.delete(owner!, repo!, format!, name!, version);
      confirmDelete = null;
      await loadPackage();
    } catch (e: any) {
      error = e.message;
    } finally {
      deletingVersion = null;
    }
  }

  function getInstallCommand(ver: string): string {
    const f = format!.toLowerCase();
    if (f === 'cargo') return `cargo add ${name!}`;
    if (f === 'npm') return `npm install ${name!}@${ver}`;
    if (f === 'pypi') return `pip install ${name!}==${ver}`;
    if (f === 'maven') return `<version>${ver}</version>`;
    if (f === 'docker') return `docker pull ${owner}/${repo}:${ver}`;
    if (f === 'nuget') return `dotnet add package ${name!} --version ${ver}`;
    if (f === 'rubygems') return `gem install ${name!} --version ${ver}`;
    if (f === 'go') return `GOPROXY=<IronForge URL>/api/v1/repos/${owner}/${repo}/packages/go go get ${name!}@${ver}`;
    if (f === 'helm') return `helm install my-release ${name!} --version ${ver}`;
    if (f === 'composer') return `composer require ${name!}:${ver}`;
    return `# install ${name!} ${ver}`;
  }

  function copyInstall(ver: string) {
    navigator.clipboard.writeText(getInstallCommand(ver));
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
  <title>{name} · {packageFormatLabel(format!)} · {owner}/{repo} · IronForge</title>
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
        <h2>{t('packages.version') || 'Versions'}</h2>
        {#each versions as version}
          <div class="version-card">
            <div class="version-header">
              <span class="version-name">v{version.version}</span>
              <div class="version-actions">
                <button class="copy-btn" onclick={() => copyInstall(version.version)}>
                  {t('common.copy') || 'Copy'} {t('packages.install') || 'Install'}
                </button>
                <button class="danger-btn" onclick={() => { deletingVersion = version.version; confirmDelete = version.version; }}>
                  {t('common.delete')}
                </button>
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
                <span>{t('packages.delete_confirm', { name: packageInfo.name, version: version.version }) || `Delete ${packageInfo.name} ${version.version}?`}</span>
                <button class="danger-btn" onclick={() => handleDeleteVersion(version.version)}>
                  {t('common.delete')}
                </button>
                <button class="secondary-btn" onclick={() => { confirmDelete = null; deletingVersion = null; }}>
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

  .version-actions {
    display: flex;
    gap: 8px;
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
