<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { releases } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);

  let releaseList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let currentPage = $state(1);
  let totalPages = $state(1);
  let deletingId = $state<number | null>(null);
  let confirmDeleteId = $state<number | null>(null);
  let releaseAssets = $state<Record<number, any[]>>({});
  let downloadingAssetId = $state<number | null>(null);

  function buildBrowseLink(tag: string) {
    const params = new URLSearchParams();
    if (tag) params.set('ref', tag);
    const qs = params.toString();
    return `/${owner}/${repo}${qs ? `?${qs}` : ''}`;
  }

  function buildNewReleaseLink() {
    return `/${owner}/${repo}/releases/new`;
  }

  function buildEditReleaseLink(id: number) {
    return `/${owner}/${repo}/releases/edit/${id}`;
  }

  $effect(() => {
    loadReleases();
  });

  async function loadReleases() {
    loading = true;
    error = '';
    try {
      const res = await releases.list(owner!, repo!, currentPage, 20);
      releaseList = res.data;
      totalPages = res.pagination?.total_pages ?? 1;
      await loadReleaseAssets(releaseList);
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadReleaseAssets(items: any[]) {
    const entries = await Promise.all(
      items.map(async (release) => {
        try {
          const assets = await releases.listAssets(owner!, repo!, release.id);
          return [release.id, assets] as const;
        } catch {
          return [release.id, []] as const;
        }
      })
    );

    releaseAssets = Object.fromEntries(entries);
  }

  async function handleDelete(id: number) {
    try {
      deletingId = id;
      await releases.delete(owner!, repo!, id);
      confirmDeleteId = null;
      deletingId = null;
      await loadReleases();
    } catch (e: any) {
      error = e.message;
      deletingId = null;
    }
  }

  async function handleAssetDownload(asset: any) {
    try {
      downloadingAssetId = asset.id;
      await releases.downloadAsset(owner!, repo!, asset.id, asset.filename);
    } catch (e: any) {
      error = e.message;
    } finally {
      downloadingAssetId = null;
    }
  }

  function showConfirm(id: number) {
    confirmDeleteId = id;
  }

  function cancelDelete() {
    confirmDeleteId = null;
  }

  function relativeTime(dateStr: string): string {
    const date = new Date(dateStr);
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffSecs = Math.floor(diffMs / 1000);
    const diffMins = Math.floor(diffSecs / 60);
    const diffHours = Math.floor(diffMins / 60);
    const diffDays = Math.floor(diffHours / 24);

    if (diffDays > 30) return formatDate(dateStr);
    if (diffDays > 0) return `${diffDays} day${diffDays > 1 ? 's' : ''} ago`;
    if (diffHours > 0) return `${diffHours} hour${diffHours > 1 ? 's' : ''} ago`;
    if (diffMins > 0) return `${diffMins} minute${diffMins > 1 ? 's' : ''} ago`;
    return 'just now';
  }

  function formatBytes(size: number): string {
    if (size < 1024) return `${size} B`;
    if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
    return `${(size / (1024 * 1024)).toFixed(1)} MB`;
  }
</script>

<svelte:head>
  <title>Releases · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader owner={owner!} repo={repo!} activeTab="releases" />

  <div class="page-header">
    <h1>{t('releases.title')}</h1>
    <a href={buildNewReleaseLink()} class="btn-primary">{t('releases.new')}</a>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="loading-text">{t('common.loading')}</p>
  {:else if releaseList.length === 0}
    <div class="empty">
      <p>{t('releases.no_releases')}</p>
      <a href={buildNewReleaseLink()} class="btn-primary">{t('releases.new')}</a>
    </div>
  {:else}
    <div class="release-list">
      {#each releaseList as release, index}
        <div class="release-card">
          <div class="release-header">
            <div class="tag-section">
              <span class="tag-badge">🏷 {release.tag_name}</span>
              {#if index === 0}
                <span class="badge latest">{t('releases.latest')}</span>
              {/if}
              {#if release.is_prerelease}
                <span class="badge prerelease">{t('releases.prerelease')}</span>
              {/if}
              {#if release.is_draft}
                <span class="badge draft">{t('releases.draft')}</span>
              {/if}
            </div>
          </div>

          <h2 class="release-title">{release.title}</h2>

          {#if release.body}
            <p class="release-body">{release.body.slice(0, 200)}{release.body.length > 200 ? '...' : ''}</p>
          {/if}

          <div class="release-meta">
            <span class="release-date">{t('releases.created', { date: relativeTime(release.created_at) })}</span>
          </div>

          {#if releaseAssets[release.id]?.length}
            <div class="asset-list" aria-label="Release assets">
              {#each releaseAssets[release.id] as asset}
                <button
                  type="button"
                  class="asset-link"
                  onclick={() => handleAssetDownload(asset)}
                  disabled={downloadingAssetId === asset.id}
                >
                  <span class="asset-name">{asset.filename}</span>
                  <span class="asset-meta">{formatBytes(asset.size)} · {asset.download_count || 0} downloads</span>
                </button>
              {/each}
            </div>
          {/if}

          <div class="release-actions">
            <a href={buildBrowseLink(release.tag_name)} class="action-link">{t('releases.browse_files')}</a>
            <a href={buildEditReleaseLink(release.id)} class="action-link">{t('releases.edit')}</a>

            {#if confirmDeleteId === release.id}
              <div class="delete-confirm">
                <span>Are you sure?</span>
                <button class="btn-danger" onclick={() => handleDelete(release.id)} disabled={deletingId === release.id}>
                  {deletingId === release.id ? '...' : t('common.delete')}
                </button>
                <button class="btn-secondary" onclick={cancelDelete}>{t('common.cancel')}</button>
              </div>
            {:else}
              <button class="action-link danger" onclick={() => showConfirm(release.id)}>{t('releases.delete')}</button>
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
          onclick={() => { currentPage = currentPage - 1; loadReleases(); }}
        >
          Previous
        </button>
        <span class="page-info">Page {currentPage} of {totalPages}</span>
        <button
          class="btn-outline"
          disabled={currentPage >= totalPages}
          onclick={() => { currentPage = currentPage + 1; loadReleases(); }}
        >
          Next
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

  .btn-secondary {
    padding: 5px 12px;
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 13px;
    cursor: pointer;
  }
  .btn-secondary:hover { background: var(--bg-hover); }

  .btn-danger {
    padding: 5px 12px;
    background: var(--red-dim);
    border: 1px solid var(--red);
    border-radius: var(--radius);
    color: #fff;
    font-size: 13px;
    cursor: pointer;
  }
  .btn-danger:hover { background: var(--red); }
  .btn-danger:disabled { opacity: 0.5; cursor: not-allowed; }
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

  .empty p {
    margin-bottom: 16px;
  }

  .release-list {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  .release-card {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 20px;
  }

  .release-header {
    margin-bottom: 12px;
  }

  .tag-section {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }

  .tag-badge {
    font-size: 14px;
    font-weight: 600;
    color: var(--text-primary);
  }

  .badge {
    padding: 2px 8px;
    border-radius: 10px;
    font-size: 12px;
    font-weight: 600;
  }

  .badge.latest {
    background: var(--green-dim);
    color: #fff;
  }

  .badge.prerelease {
    background: var(--yellow-dim);
    color: #fff;
  }

  .badge.draft {
    background: var(--bg-tertiary);
    color: var(--text-muted);
    border: 1px solid var(--border);
  }

  .release-title {
    font-size: 18px;
    font-weight: 600;
    margin-bottom: 8px;
  }

  .release-body {
    font-size: 14px;
    color: var(--text-secondary);
    line-height: 1.6;
    margin-bottom: 12px;
    white-space: pre-wrap;
  }

  .release-meta {
    font-size: 13px;
    color: var(--text-muted);
    margin-bottom: 12px;
  }

  .asset-list {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 12px;
    padding: 10px 12px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .asset-link {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    width: 100%;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text-primary);
    text-decoration: none;
    font-size: 13px;
    cursor: pointer;
    text-align: left;
  }

  .asset-link:hover .asset-name {
    color: var(--accent);
    text-decoration: underline;
  }

  .asset-link:disabled {
    cursor: wait;
    opacity: 0.65;
  }

  .asset-name {
    min-width: 0;
    overflow-wrap: anywhere;
    font-weight: 500;
  }

  .asset-meta {
    flex-shrink: 0;
    color: var(--text-muted);
    font-size: 12px;
  }

  .release-actions {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
  }

  .action-link {
    font-size: 13px;
    color: var(--accent);
    background: none;
    border: none;
    padding: 0;
    cursor: pointer;
  }
  .action-link:hover { text-decoration: underline; }
  .action-link.danger { color: var(--red); }

  .delete-confirm {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 13px;
  }
  .delete-confirm span { color: var(--text-secondary); }

  .pagination {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 16px;
    margin-top: 24px;
  }

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

    .release-actions {
      flex-direction: column;
      align-items: flex-start;
    }

    .delete-confirm {
      flex-direction: column;
      align-items: flex-start;
    }
  }
</style>
