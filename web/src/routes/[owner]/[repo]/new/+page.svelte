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
  let branch = $derived($page.url.searchParams.get('ref') || '');
  // Without `?ref=` the file is read at the server's HEAD and committed to
  // the repository's own default branch. Both pages used to fall back to a
  // literal `main`: on a repository whose default is `master` the edit page
  // could not read the file and "New file" committed to a branch that did
  // not exist (card_2e320f5287d7, sideways).
  let defaultBranch = $state<string | null>(null);
  let targetBranch = $derived(branch || defaultBranch || '');
  let branchResolved = $derived(Boolean(branch) || defaultBranch !== null);

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    if (branch) return;
    defaultBranch = null;
    let current = true;
    void (async () => {
      try {
        const info = await repos.get(expectedOwner, expectedRepo);
        if (current) defaultBranch = info?.default_branch || '';
      } catch {
        // The editor still opens; its branch field is required, so the
        // author names the branch instead of the page guessing one.
        if (current) defaultBranch = '';
      }
    })();
    return () => { current = false; };
  });
  let editorKey = $derived(JSON.stringify([owner, repo, path, branch, targetBranch]));
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
  <title>{t('repo.new_file')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

{#key editorKey}
  {#if branchResolved}
    <FileEditor
      {owner}
      {repo}
      mode="create"
      initialPath={path}
      branch={targetBranch}
      cancelHref={repoHref(branch)}
      onSave={saveFile}
    />
  {:else}
    <div class="loading">{t('common.loading')}</div>
  {/if}
{/key}
