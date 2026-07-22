<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { onMount } from 'svelte';
  import FileEditor from '$lib/components/FileEditor.svelte';
  import { repos } from '$lib/api/client.svelte';
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

  let blobData = $state<BlobData | null>(null);
  let loading = $state(true);
  let error = $state('');

  function encodeRepoPath(pathValue: string): string {
    return pathValue.split('/').map(encodeURIComponent).join('/');
  }

  function blobHref(pathValue: string, refValue?: string) {
    const query = refValue ? `?${new URLSearchParams({ ref: refValue }).toString()}` : '';
    return `/${owner}/${repo}/blob/${encodeRepoPath(pathValue)}${query}`;
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

  onMount(async () => {
    try {
      blobData = await repos.blob(owner, repo, path, branch);
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      loading = false;
    }
  });

  async function saveFile(payload: { path: string; content: string; message: string; branch: string; sha?: string }) {
    await repos.saveContent(owner, repo, path, {
      branch: payload.branch,
      content: payload.content,
      message: payload.message,
      sha: payload.sha,
    });
    await goto(blobHref(path, payload.branch));
  }
</script>

<svelte:head>
  <title>{t('repo.edit_file')} · {path} · IronForge</title>
</svelte:head>

{#if loading}
  <div class="loading">{t('common.loading')}</div>
{:else if error}
  <div class="editor-error">{error}</div>
{:else if blobData}
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
