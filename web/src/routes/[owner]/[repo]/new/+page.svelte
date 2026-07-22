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

  async function saveFile(payload: { path: string; content: string; message: string; branch: string }) {
    await repos.saveContent(owner, repo, payload.path, {
      branch: payload.branch,
      content: payload.content,
      message: payload.message,
    });
    await goto(blobHref(payload.path, payload.branch));
  }
</script>

<svelte:head>
  <title>{t('repo.new_file')} · {owner}/{repo} · IronForge</title>
</svelte:head>

<FileEditor
  {owner}
  {repo}
  mode="create"
  initialPath={path}
  branch={branch}
  cancelHref={repoHref(branch)}
  onSave={saveFile}
/>
