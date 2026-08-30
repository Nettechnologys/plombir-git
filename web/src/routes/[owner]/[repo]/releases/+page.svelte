<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { instance, releases, type AttestationReport, type ReleaseAsset } from '$lib/api/client.svelte';
  import {
    LatestRepositoryResourceRequestFence,
    type RepositoryResourceRequestClaim,
  } from '$lib/asyncStateOwnership';
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
  let releaseAssets = $state<Record<number, ReleaseAsset[]>>({});
  let downloadingAssetId = $state<number | null>(null);
  let uploadingReleaseId = $state<number | null>(null);
  let uploadProgress = $state<Record<number, number | null>>({});
  let deletingAssetId = $state<number | null>(null);
  let confirmDeleteAssetId = $state<number | null>(null);

  // Provenance (card_5e52392a0274). `attestationEnabled` starts as `null` —
  // "not asked yet" — and never collapses into `false`: both attestation
  // endpoints answer 404 for a disabled instance and for an unsigned asset, so
  // an unknown capability rendered as "off" would tell every reader on a
  // provenance-enabled forge that the forge has no provenance.
  let attestationEnabled = $state<boolean | null>(null);
  let signedAssetIds = $state<number[]>([]);
  let attestationReports = $state<Record<number, AttestationReport>>({});
  let attestationErrors = $state<Record<number, string>>({});
  let signingAssetId = $state<number | null>(null);
  let verifyingAssetId = $state<number | null>(null);
  let busyReleaseIds = $state<Set<number>>(new Set());
  let busyAssetIds = $state<Set<number>>(new Set());
  const releaseListRequests = new LatestRepositoryResourceRequestFence<number>();
  let routeGeneration = 0;

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
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    routeGeneration += 1;
    currentPage = 1;
    releaseList = [];
    releaseAssets = {};
    signedAssetIds = [];
    attestationReports = {};
    attestationErrors = {};
    attestationEnabled = null;
    totalPages = 1;
    loading = true;
    error = '';
    deletingId = null;
    confirmDeleteId = null;
    uploadingReleaseId = null;
    uploadProgress = {};
    deletingAssetId = null;
    confirmDeleteAssetId = null;
    downloadingAssetId = null;
    signingAssetId = null;
    verifyingAssetId = null;
    busyReleaseIds = new Set();
    busyAssetIds = new Set();
    void loadReleases(expectedOwner, expectedRepo, 1);
  });

  function ownsListClaim(claim: RepositoryResourceRequestClaim<number>): boolean {
    return releaseListRequests.owns(claim, owner!, repo!, currentPage);
  }

  function isCurrentListIntent(
    expectedOwner: string,
    expectedRepo: string,
    expectedPage: number,
    expectedRoute: number,
  ): boolean {
    return routeGeneration === expectedRoute
      && owner === expectedOwner
      && repo === expectedRepo
      && currentPage === expectedPage;
  }

  function claimRelease(id: number): boolean {
    if (isReleaseBusy(id)) return false;
    busyReleaseIds = new Set(busyReleaseIds).add(id);
    return true;
  }

  function releaseRelease(id: number): void {
    const next = new Set(busyReleaseIds);
    next.delete(id);
    busyReleaseIds = next;
  }

  function releaseIdForAsset(id: number): number | null {
    for (const [releaseId, assets] of Object.entries(releaseAssets)) {
      if (assets.some((asset) => asset.id === id)) return Number(releaseId);
    }
    return null;
  }

  function isReleaseBusy(id: number): boolean {
    return busyReleaseIds.has(id)
      || (releaseAssets[id] ?? []).some((asset) => busyAssetIds.has(asset.id));
  }

  function claimAsset(id: number): boolean {
    const releaseId = releaseIdForAsset(id);
    if (busyAssetIds.has(id) || (releaseId !== null && busyReleaseIds.has(releaseId))) return false;
    busyAssetIds = new Set(busyAssetIds).add(id);
    return true;
  }

  function releaseAsset(id: number): void {
    const next = new Set(busyAssetIds);
    next.delete(id);
    busyAssetIds = next;
  }

  async function loadReleases(expectedOwner: string, expectedRepo: string, expectedPage: number) {
    const claim = releaseListRequests.begin(expectedOwner, expectedRepo, expectedPage);
    loading = true;
    error = '';
    try {
      // Awaited before the assets, because whether a 404 from the attestation
      // endpoints means "unsigned" depends on the answer. A failure leaves the
      // capability unknown rather than off — see `attestationEnabled`.
      const [info, res] = await Promise.all([
        instance.get().catch(() => null),
        releases.list(expectedOwner, expectedRepo, expectedPage, 20),
      ]);
      if (!ownsListClaim(claim)) return;
      if (info) attestationEnabled = info.attestation_enabled;
      releaseList = res.data;
      totalPages = res.pagination?.total_pages ?? 1;
      await loadReleaseAssets(expectedOwner, expectedRepo, releaseList, claim);
    } catch (e: any) {
      if (ownsListClaim(claim)) error = e.message;
    } finally {
      if (ownsListClaim(claim)) loading = false;
    }
  }

  async function loadReleaseAssets(
    expectedOwner: string,
    expectedRepo: string,
    items: any[],
    claim: RepositoryResourceRequestClaim<number>,
  ) {
    const entries = await Promise.all(
      items.map(async (release) => {
        try {
          const assets = await releases.listAssets(expectedOwner, expectedRepo, release.id);
          return { release, assets, loadError: '' };
        } catch (e: any) {
          return { release, assets: [] as ReleaseAsset[], loadError: e.message || String(e) };
        }
      })
    );

    if (!ownsListClaim(claim)) return;
    const assets = entries.flatMap((entry) => entry.assets);
    const nextSignedAssetIds = await loadAttestationPresence(
      expectedOwner,
      expectedRepo,
      assets,
      attestationEnabled === true,
    );
    if (!ownsListClaim(claim)) return;
    releaseAssets = Object.fromEntries(entries.map(({ release, assets }) => [release.id, assets]));
    signedAssetIds = nextSignedAssetIds;
    const failures = entries.filter(({ loadError }) => loadError);
    if (failures.length > 0) {
      error = failures
        .map(({ release, loadError }) => t('releases.asset_load_failed', { tag: release.tag_name, error: loadError }))
        .join('\n');
    }
  }

  function changePage(nextPage: number): void {
    // A page is a distinct resource visit even when the user later returns to
    // the same page number.  Bump the generation so a page 1 mutation cannot
    // publish into, or release controls owned by, a later page 1 visit.
    routeGeneration += 1;
    currentPage = nextPage;
    releaseList = [];
    releaseAssets = {};
    signedAssetIds = [];
    attestationReports = {};
    attestationErrors = {};
    confirmDeleteId = null;
    confirmDeleteAssetId = null;
    deletingId = null;
    uploadingReleaseId = null;
    uploadProgress = {};
    deletingAssetId = null;
    downloadingAssetId = null;
    signingAssetId = null;
    verifyingAssetId = null;
    busyReleaseIds = new Set();
    busyAssetIds = new Set();
    void loadReleases(owner!, repo!, nextPage);
  }

  async function handleDelete(id: number) {
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedPage = currentPage;
    const expectedRoute = routeGeneration;
    if (!claimRelease(id)) return;
    releaseListRequests.begin(expectedOwner, expectedRepo, expectedPage);
    try {
      deletingId = id;
      await releases.delete(expectedOwner, expectedRepo, id);
      if (!isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) return;
      confirmDeleteId = null;
      await loadReleases(expectedOwner, expectedRepo, expectedPage);
    } catch (e: any) {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
        deletingId = null;
        releaseRelease(id);
      }
    }
  }

  async function handleAssetUpload(releaseId: number, event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    if (!file || uploadingReleaseId !== null || !claimRelease(releaseId)) return;
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedPage = currentPage;
    const expectedRoute = routeGeneration;
    releaseListRequests.begin(expectedOwner, expectedRepo, expectedPage);

    uploadingReleaseId = releaseId;
    uploadProgress = { ...uploadProgress, [releaseId]: 0 };
    error = '';
    try {
      const asset = await releases.uploadAsset(expectedOwner, expectedRepo, releaseId, file, ({ percent }) => {
        if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
          uploadProgress = { ...uploadProgress, [releaseId]: percent };
        }
      });
      if (!isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) return;
      releaseAssets = {
        ...releaseAssets,
        [releaseId]: [...(releaseAssets[releaseId] ?? []), asset],
      };
    } catch (e: any) {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) error = e.message;
    } finally {
      input.value = '';
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
        uploadingReleaseId = null;
        const nextProgress = { ...uploadProgress };
        delete nextProgress[releaseId];
        uploadProgress = nextProgress;
        releaseRelease(releaseId);
      }
    }
  }

  async function handleAssetDownload(asset: ReleaseAsset) {
    if (downloadingAssetId !== null || !claimAsset(asset.id)) return;
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedPage = currentPage;
    const expectedRoute = routeGeneration;
    try {
      downloadingAssetId = asset.id;
      error = '';
      await releases.downloadAsset(expectedOwner, expectedRepo, asset.id, asset.filename);
    } catch (e: any) {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
        downloadingAssetId = null;
        releaseAsset(asset.id);
      }
    }
  }

  async function handleAssetDelete(releaseId: number, assetId: number) {
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedPage = currentPage;
    const expectedRoute = routeGeneration;
    if (!claimAsset(assetId)) return;
    releaseListRequests.begin(expectedOwner, expectedRepo, expectedPage);
    try {
      deletingAssetId = assetId;
      error = '';
      await releases.deleteAsset(expectedOwner, expectedRepo, assetId);
      if (!isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) return;
      releaseAssets = {
        ...releaseAssets,
        [releaseId]: (releaseAssets[releaseId] ?? []).filter((asset) => asset.id !== assetId),
      };
      confirmDeleteAssetId = null;
    } catch (e: any) {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
        deletingAssetId = null;
        releaseAsset(assetId);
      }
    }
  }

  /// Ask the server which of these assets carry an attestation.
  ///
  /// Only meaningful once the capability is known: on an instance with the
  /// feature off every one of these calls is a 404, and recording that as
  /// "unsigned" would be a lie in the one direction that matters.
  async function loadAttestationPresence(
    expectedOwner: string,
    expectedRepo: string,
    assets: ReleaseAsset[],
    enabled: boolean,
  ): Promise<number[]> {
    if (!enabled) return [];
    const found = await Promise.all(
      assets.map((asset) =>
        releases.attestation.get(expectedOwner, expectedRepo, asset.id).then(() => asset.id).catch(() => null),
      ),
    );
    return found.filter((id): id is number => id !== null);
  }

  async function handleSignAsset(asset: ReleaseAsset) {
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedPage = currentPage;
    const expectedRoute = routeGeneration;
    if (!claimAsset(asset.id)) return;
    releaseListRequests.begin(expectedOwner, expectedRepo, expectedPage);
    try {
      signingAssetId = asset.id;
      attestationErrors = { ...attestationErrors, [asset.id]: '' };
      await releases.attestation.sign(expectedOwner, expectedRepo, asset.id);
      if (!isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) return;
      if (!signedAssetIds.includes(asset.id)) signedAssetIds = [...signedAssetIds, asset.id];
      // A fresh signature says nothing about the bytes on disk until it is
      // checked, so any previous verdict for this asset is dropped rather than
      // left standing next to a signature it does not describe.
      const { [asset.id]: _dropped, ...rest } = attestationReports;
      attestationReports = rest;
    } catch (e: any) {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
        attestationErrors = { ...attestationErrors, [asset.id]: e.message || String(e) };
      }
    } finally {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
        signingAssetId = null;
        releaseAsset(asset.id);
      }
    }
  }

  async function handleVerifyAsset(asset: ReleaseAsset) {
    const expectedOwner = owner!;
    const expectedRepo = repo!;
    const expectedPage = currentPage;
    const expectedRoute = routeGeneration;
    if (!claimAsset(asset.id)) return;
    try {
      verifyingAssetId = asset.id;
      attestationErrors = { ...attestationErrors, [asset.id]: '' };
      const report = await releases.attestation.verify(expectedOwner, expectedRepo, asset.id);
      if (!isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) return;
      attestationReports = { ...attestationReports, [asset.id]: report };
    } catch (e: any) {
      // Distinct from `verified: false`. This is "the check could not run";
      // that is "the check ran and the asset does not match what was signed".
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
        attestationErrors = { ...attestationErrors, [asset.id]: e.message || String(e) };
      }
    } finally {
      if (isCurrentListIntent(expectedOwner, expectedRepo, expectedPage, expectedRoute)) {
        verifyingAssetId = null;
        releaseAsset(asset.id);
      }
    }
  }

  function assetUploadLabel(releaseId: number): string {
    if (uploadingReleaseId !== releaseId) return t('releases.asset_upload');
    const percent = uploadProgress[releaseId];
    return percent === null
      ? t('releases.asset_uploading')
      : t('releases.asset_upload_progress', { progress: percent ?? 0 });
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
  <title>Releases · {owner}/{repo} · ForgeKeep</title>
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

          <div class="asset-section" aria-label={t('releases.assets')}>
            <div class="asset-heading">
              <strong>{t('releases.assets')}</strong>
              <label class="asset-upload" class:disabled={uploadingReleaseId !== null || isReleaseBusy(release.id)}>
                {assetUploadLabel(release.id)}
                <input
                  type="file"
                  onchange={(event) => handleAssetUpload(release.id, event)}
                  disabled={uploadingReleaseId !== null || isReleaseBusy(release.id)}
                />
              </label>
            </div>

            {#if uploadingReleaseId === release.id}
              <div class="asset-upload-progress" aria-live="polite">
                <progress max="100" value={uploadProgress[release.id] ?? undefined}></progress>
                <span>{assetUploadLabel(release.id)}</span>
              </div>
            {/if}

            {#if releaseAssets[release.id]?.length}
              <ul class="asset-list">
                {#each releaseAssets[release.id] as asset (asset.id)}
                  <li class="asset-row">
                    <button
                      type="button"
                      class="asset-link"
                      onclick={() => handleAssetDownload(asset)}
                      disabled={downloadingAssetId !== null || busyAssetIds.has(asset.id) || busyReleaseIds.has(release.id)}
                    >
                      <span class="asset-name">{asset.filename}</span>
                      <span class="asset-meta">
                        {formatBytes(asset.size)} · {t('releases.asset_downloads', { count: asset.download_count || 0 })}
                      </span>
                    </button>

                    {#if attestationEnabled === true}
                      <div class="asset-attestation">
                        {#if attestationReports[asset.id]}
                          {#if attestationReports[asset.id].verified}
                            <span class="attestation-badge verified">{t('releases.attestation.verified')}</span>
                          {:else}
                            <!-- A failed verification is not "no signature". The asset is
                                 signed and its bytes no longer match what was signed, which
                                 is the loudest thing this feature can say. -->
                            <span class="attestation-badge failed">{t('releases.attestation.verify_failed')}</span>
                            <span class="attestation-reason">{attestationReports[asset.id].reason}</span>
                          {/if}
                        {:else if signedAssetIds.includes(asset.id)}
                          <span class="attestation-badge signed">{t('releases.attestation.signed')}</span>
                        {:else}
                          <span class="attestation-badge unsigned">{t('releases.attestation.unsigned')}</span>
                        {/if}

                        {#if signedAssetIds.includes(asset.id)}
                          <button
                            type="button"
                            class="attestation-verify"
                            onclick={() => handleVerifyAsset(asset)}
                            disabled={busyAssetIds.has(asset.id) || busyReleaseIds.has(release.id)}
                          >
                            {verifyingAssetId === asset.id
                              ? t('releases.attestation.verifying')
                              : t('releases.attestation.verify')}
                          </button>
                        {:else}
                          <button
                            type="button"
                            class="attestation-sign"
                            onclick={() => handleSignAsset(asset)}
                            disabled={busyAssetIds.has(asset.id) || busyReleaseIds.has(release.id)}
                          >
                            {signingAssetId === asset.id
                              ? t('releases.attestation.signing')
                              : t('releases.attestation.sign')}
                          </button>
                        {/if}

                        {#if attestationErrors[asset.id]}
                          <!-- Third state: the check could not run at all. Folding this
                               into "verify failed" would accuse an asset of being tampered
                               with because the blob store was unreachable. -->
                          <span class="attestation-error">
                            {t('releases.attestation.error', { error: attestationErrors[asset.id] })}
                          </span>
                        {/if}
                      </div>
                    {/if}

                    {#if confirmDeleteAssetId === asset.id}
                      <div class="asset-delete-confirm">
                        <span>{t('releases.asset_delete_confirm')}</span>
                        <button
                          type="button"
                          class="btn-danger"
                          onclick={() => handleAssetDelete(release.id, asset.id)}
                          disabled={busyAssetIds.has(asset.id) || busyReleaseIds.has(release.id)}
                        >
                          {deletingAssetId === asset.id ? '...' : t('common.delete')}
                        </button>
                        <button type="button" class="btn-secondary" onclick={() => (confirmDeleteAssetId = null)}>
                          {t('common.cancel')}
                        </button>
                      </div>
                    {:else}
                      <button
                        type="button"
                        class="asset-delete"
                        disabled={busyAssetIds.has(asset.id) || busyReleaseIds.has(release.id)}
                        onclick={() => (confirmDeleteAssetId = asset.id)}
                      >
                        {t('releases.asset_delete')}
                      </button>
                    {/if}
                  </li>
                {/each}
              </ul>
            {/if}
          </div>

          <div class="release-actions">
            <a href={buildBrowseLink(release.tag_name)} class="action-link">{t('releases.browse_files')}</a>
            <a href={buildEditReleaseLink(release.id)} class="action-link">{t('releases.edit')}</a>

            {#if confirmDeleteId === release.id}
              <div class="delete-confirm">
                <span>Are you sure?</span>
                <button class="btn-danger" onclick={() => handleDelete(release.id)} disabled={isReleaseBusy(release.id)}>
                  {deletingId === release.id ? '...' : t('common.delete')}
                </button>
                <button class="btn-secondary" onclick={cancelDelete}>{t('common.cancel')}</button>
              </div>
            {:else}
              <button class="action-link danger" disabled={isReleaseBusy(release.id)} onclick={() => showConfirm(release.id)}>{t('releases.delete')}</button>
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
          onclick={() => changePage(currentPage - 1)}
        >
          Previous
        </button>
        <span class="page-info">Page {currentPage} of {totalPages}</span>
        <button
          class="btn-outline"
          disabled={currentPage >= totalPages}
          onclick={() => changePage(currentPage + 1)}
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

  .asset-section {
    margin-bottom: 12px;
    padding: 10px 12px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .asset-heading {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    font-size: 13px;
  }

  .asset-upload {
    color: var(--accent);
    cursor: pointer;
    font-size: 13px;
  }

  .asset-upload:hover { text-decoration: underline; }
  .asset-upload.disabled { cursor: wait; opacity: 0.65; }
  .asset-upload input { display: none; }

  .asset-upload-progress {
    display: grid;
    grid-template-columns: minmax(80px, 180px) auto;
    align-items: center;
    gap: 10px;
    margin-top: 10px;
    color: var(--text-muted);
    font-size: 12px;
  }

  .asset-upload-progress progress { width: 100%; }

  .asset-list {
    display: flex;
    flex-direction: column;
    gap: 8px;
    list-style: none;
    margin: 10px 0 0;
    padding: 10px 0 0;
    border-top: 1px solid var(--border);
  }

  .asset-row {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .asset-link {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    min-width: 0;
    flex: 1;
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

  .asset-attestation {
    display: flex;
    align-items: center;
    flex-shrink: 0;
    flex-wrap: wrap;
    gap: 8px;
    font-size: 12px;
  }

  .attestation-badge {
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
  }

  .attestation-badge.verified { border-color: var(--green, var(--border)); color: var(--green, var(--text-secondary)); }
  .attestation-badge.failed { border-color: var(--red); color: var(--red); }
  .attestation-badge.signed { color: var(--text-primary); }
  .attestation-badge.unsigned { color: var(--text-muted); }

  .attestation-reason,
  .attestation-error {
    overflow-wrap: anywhere;
    color: var(--red);
  }

  .attestation-sign,
  .attestation-verify {
    flex-shrink: 0;
    padding: 0;
    border: 0;
    background: none;
    color: var(--accent);
    cursor: pointer;
    font-size: 12px;
  }

  .attestation-sign:hover,
  .attestation-verify:hover { text-decoration: underline; }

  .attestation-sign:disabled,
  .attestation-verify:disabled { cursor: wait; opacity: 0.65; }

  .asset-delete {
    flex-shrink: 0;
    padding: 0;
    border: 0;
    background: none;
    color: var(--red);
    cursor: pointer;
    font-size: 12px;
  }

  .asset-delete:hover { text-decoration: underline; }

  .asset-delete-confirm {
    display: flex;
    align-items: center;
    gap: 8px;
    color: var(--text-secondary);
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

    .asset-heading,
    .asset-row,
    .asset-delete-confirm {
      align-items: flex-start;
      flex-direction: column;
    }

    .asset-link { width: 100%; }
  }
</style>
