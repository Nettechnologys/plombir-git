<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import FileEditor from '$lib/components/FileEditor.svelte';
  import { repos } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let path = $derived($page.url.searchParams.get('path') || '');
  let branch = $derived($page.url.searchParams.get('ref') || 'main');
  let editorKey = $derived(JSON.stringify([owner, repo, path, branch]));
  let routeGeneration = 0;

  $effect(() => {
    editorKey;
    routeGeneration += 1;
  });

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

  async function saveFile(payload: { path: string; content: string; message: string; branch: string }) {
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedPath = path;
    const expectedBranch = branch;
    const expectedRoute = routeGeneration;
    const next = { ...payload };

    await repos.saveContent(expectedOwner, expectedRepo, next.path, {
      branch: next.branch,
      content: next.content,
      message: next.message,
    });
    if (
      routeGeneration !== expectedRoute ||
      owner !== expectedOwner ||
      repo !== expectedRepo ||
      path !== expectedPath ||
      branch !== expectedBranch
    ) return;
    await goto(blobHref(next.path, next.branch, expectedOwner, expectedRepo));
  }
</script>

<svelte:head>
  <title>{t('repo.new_file')} · {owner}/{repo} · ForgeKeep</title>
</svelte:head>

{#key editorKey}
  <FileEditor
    {owner}
    {repo}
    mode="create"
    initialPath={path}
    branch={branch}
    cancelHref={repoHref(branch)}
    onSave={saveFile}
  />
{/key}
