<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import FileEditor from '$lib/components/FileEditor.svelte';
  import { repos } from '$lib/api/client.svelte';
  import {
    LatestRepositoryResourceRequestFence,
    type RepositoryResourceRequestClaim,
  } from '$lib/asyncStateOwnership';
  import { createT } from '$lib/i18n';

  const t = createT();
  const MAX_EDITABLE_SIZE = 1024 * 1024;

  type BlobData = {
    content: string;
    size: number;
    name?: string;
    sha: string;
    encoding?: string;
    is_binary?: boolean;
  };

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let path = $derived($page.params.path!);
  let branch = $derived($page.url.searchParams.get('ref') || 'main');
  let editorKey = $derived(JSON.stringify([owner, repo, path, branch]));

  let blobData = $state<BlobData | null>(null);
  let loading = $state(true);
  let error = $state('');
  const blobRequests = new LatestRepositoryResourceRequestFence<string>();
  let routeGeneration = 0;

  function encodeRepoPath(pathValue: string): string {
    return pathValue.split('/').map(encodeURIComponent).join('/');
  }

  function blobHref(
    pathValue: string,
    refValue?: string,
    routeOwner = owner,
    routeRepo = repo,
  ) {
    const query = refValue ? `?${new URLSearchParams({ ref: refValue }).toString()}` : '';
    return `/${routeOwner}/${routeRepo}/blob/${encodeRepoPath(pathValue)}${query}`;
  }

  function repoHref(refValue?: string) {
    const query = refValue ? `?${new URLSearchParams({ ref: refValue }).toString()}` : '';
    return `/${owner}/${repo}${query}`;
  }

  let disabledReason = $derived(
    blobData?.is_binary
      ? t('repo.blob.binary_file')
      : blobData && blobData.size > MAX_EDITABLE_SIZE
        ? t('repo.blob.large_file')
        : '',
  );

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedPath = path;
    const expectedBranch = branch;
    const expectedRoute = ++routeGeneration;

    blobData = null;
    loading = true;
    error = '';
    void loadBlob(expectedOwner, expectedRepo, expectedPath, expectedBranch, expectedRoute);
  });

  function resourceIdentity(expectedPath: string, expectedBranch: string) {
    return `${expectedPath}\u0000${expectedBranch}`;
  }

  function isCurrentRoute(
    expectedOwner: string,
    expectedRepo: string,
    expectedPath: string,
    expectedBranch: string,
    expectedRoute: number,
  ) {
    return (
      routeGeneration === expectedRoute &&
      owner === expectedOwner &&
      repo === expectedRepo &&
      path === expectedPath &&
      branch === expectedBranch
    );
  }

  function ownsBlobClaim(
    claim: RepositoryResourceRequestClaim<string>,
    expectedOwner: string,
    expectedRepo: string,
    expectedPath: string,
    expectedBranch: string,
    expectedRoute: number,
  ) {
    return (
      blobRequests.owns(claim, owner, repo, resourceIdentity(path, branch)) &&
      isCurrentRoute(expectedOwner, expectedRepo, expectedPath, expectedBranch, expectedRoute)
    );
  }

  async function loadBlob(
    expectedOwner: string,
    expectedRepo: string,
    expectedPath: string,
    expectedBranch: string,
    expectedRoute: number,
  ) {
    const claim = blobRequests.begin(
      expectedOwner,
      expectedRepo,
      resourceIdentity(expectedPath, expectedBranch),
    );
    try {
      const nextBlob = await repos.blob(
        expectedOwner,
        expectedRepo,
        expectedPath,
        expectedBranch,
      );
      if (
        ownsBlobClaim(
          claim,
          expectedOwner,
          expectedRepo,
          expectedPath,
          expectedBranch,
          expectedRoute,
        )
      ) blobData = nextBlob;
    } catch (err) {
      if (
        ownsBlobClaim(
          claim,
          expectedOwner,
          expectedRepo,
          expectedPath,
          expectedBranch,
          expectedRoute,
        )
      ) error = err instanceof Error ? err.message : String(err);
    } finally {
      if (
        ownsBlobClaim(
          claim,
          expectedOwner,
          expectedRepo,
          expectedPath,
          expectedBranch,
          expectedRoute,
        )
      ) loading = false;
    }
  }

  async function saveFile(payload: { path: string; content: string; message: string; branch: string; sha?: string }) {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedPath = path;
    const expectedBranch = branch;
    const expectedRoute = routeGeneration;
    const next = { ...payload };

    await repos.saveContent(expectedOwner, expectedRepo, expectedPath, {
      branch: next.branch,
      content: next.content,
      message: next.message,
      sha: next.sha,
    });
    if (
      !isCurrentRoute(
        expectedOwner,
        expectedRepo,
        expectedPath,
        expectedBranch,
        expectedRoute,
      )
    ) return;
    await goto(blobHref(expectedPath, next.branch, expectedOwner, expectedRepo));
  }
</script>

<svelte:head>
  <title>{t('repo.edit_file')} · {path} · ForgeKeep</title>
</svelte:head>

{#if loading}
  <div class="loading">{t('common.loading')}</div>
{:else if error}
  <div class="editor-error">{error}</div>
{:else if blobData}
  {#key editorKey}
    <FileEditor
      {owner}
      {repo}
      mode="edit"
      initialPath={path}
      initialContent={blobData.content}
      initialSha={blobData.sha}
      branch={branch}
      cancelHref={blobHref(path, branch)}
      {disabledReason}
      onSave={saveFile}
    />
  {/key}
{/if}

<style>
  .loading,
  .editor-error {
    max-width: 900px;
    margin: 0 auto;
    padding: 32px 24px;
    color: var(--text-secondary);
  }

  .editor-error {
    color: #cf222e;
  }
</style>
