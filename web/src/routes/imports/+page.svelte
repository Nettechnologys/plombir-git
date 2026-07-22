<script lang="ts">
  import { goto } from '$app/navigation';
  import { imports, type ImportTask, type StartImportPayload } from '$lib/api/client.svelte';
  import { isLoggedIn, getUser } from '$lib/stores/auth.svelte';
  import { formatDateTime } from '$lib/i18n';

  let taskList = $state<ImportTask[]>([]);
  let loading = $state(true);
  let submitting = $state(false);
  let error = $state('');
  let success = $state('');

  type ImportPlatform = 'github' | 'gitlab' | 'gitea' | 'git';

  let platform = $state<ImportPlatform>('github');
  let sourceUrl = $state('');
  let targetOwner = $state('');
  let targetName = $state('');
  let authToken = $state('');
  let importRepo = $state(true);
  let importIssues = $state(true);
  let importPullRequests = $state(true);
  let importWiki = $state(false);
  let importReleases = $state(true);
  let importLabels = $state(true);
  let importMilestones = $state(true);

  function supportsMetadataImport(value = platform): boolean {
    return value === 'github' || value === 'gitlab';
  }

  function sourcePlaceholder(): string {
    switch (platform) {
      case 'gitlab':
        return 'https://gitlab.com/example/project';
      case 'gitea':
        return 'https://gitea.example.com/example/project.git';
      case 'git':
        return 'https://git.example.com/example/project.git';
      default:
        return 'https://github.com/example/project';
    }
  }

  $effect(() => {
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }

    if (!targetOwner) {
      targetOwner = getUser()?.username || '';
    }

    loadImports();
  });

  $effect(() => {
    if (!supportsMetadataImport(platform)) {
      importIssues = false;
      importPullRequests = false;
      importWiki = false;
      importReleases = false;
      importLabels = false;
      importMilestones = false;
    }
  });

  async function loadImports() {
    loading = true;
    error = '';
    try {
      taskList = await imports.list();
    } catch (e: any) {
      error = e.message || 'Failed to load imports';
    } finally {
      loading = false;
    }
  }

  async function startImport(e: Event) {
    e.preventDefault();
    error = '';
    success = '';

    const payload: StartImportPayload = {
      platform,
      source_url: sourceUrl.trim(),
      target_owner: targetOwner.trim(),
      import_repo: importRepo,
      import_issues: importIssues,
      import_pull_requests: importPullRequests,
      import_wiki: importWiki,
      import_releases: importReleases,
      import_labels: importLabels,
      import_milestones: importMilestones,
    };

    if (targetName.trim()) payload.target_name = targetName.trim();
    if (authToken.trim()) payload.auth_token = authToken.trim();

    submitting = true;
    try {
      await imports.start(payload);
      sourceUrl = '';
      targetName = '';
      authToken = '';
      success = 'Import queued';
      await loadImports();
    } catch (e: any) {
      error = e.message || 'Failed to start import';
    } finally {
      submitting = false;
    }
  }

  async function deleteImport(id: number) {
    if (!confirm('Cancel and delete this import task?')) return;
    error = '';
    success = '';
    try {
      await imports.remove(id);
      taskList = taskList.filter((task) => task.id !== id);
      success = 'Import deleted';
    } catch (e: any) {
      error = e.message || 'Failed to delete import';
    }
  }

  function taskHref(task: ImportTask): string {
    return `/${encodeURIComponent(task.target_owner)}/${encodeURIComponent(task.target_name)}`;
  }
</script>

<svelte:head>
  <title>Imports · IronForge</title>
</svelte:head>

<div class="page-container">
  <div class="page-header">
    <div>
      <h1>Imports</h1>
      <p class="subtitle">Migrate repositories and project data from GitHub, GitLab, Gitea, or Git remotes.</p>
    </div>
    <button class="btn-secondary" type="button" onclick={loadImports} disabled={loading}>Refresh</button>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}
  {#if success}
    <div class="success-banner">{success}</div>
  {/if}

  <section class="panel">
    <h2>New import</h2>
    <form class="import-form" onsubmit={startImport}>
      <label>
        Platform
        <select bind:value={platform}>
          <option value="github">GitHub</option>
          <option value="gitlab">GitLab</option>
          <option value="gitea">Gitea</option>
          <option value="git">Git</option>
        </select>
      </label>

      <label class="wide">
        Source repository URL
        <input type="url" bind:value={sourceUrl} placeholder={sourcePlaceholder()} required />
      </label>

      <label>
        Target owner
        <input type="text" bind:value={targetOwner} required />
      </label>

      <label>
        Target repository
        <input type="text" bind:value={targetName} placeholder="Derived from source URL" />
      </label>

      <label class="wide">
        Source access token
        <input type="password" bind:value={authToken} autocomplete="off" placeholder="Optional for private repositories" />
      </label>

      <fieldset class="wide options">
        <legend>Content</legend>
        <label><input type="checkbox" bind:checked={importRepo} /> Repository</label>
        <label><input type="checkbox" bind:checked={importIssues} disabled={!supportsMetadataImport()} /> Issues</label>
        <label><input type="checkbox" bind:checked={importPullRequests} disabled={!supportsMetadataImport()} /> Pull requests</label>
        <label><input type="checkbox" bind:checked={importWiki} disabled={!supportsMetadataImport()} /> Wiki</label>
        <label><input type="checkbox" bind:checked={importReleases} disabled={!supportsMetadataImport()} /> Releases</label>
        <label><input type="checkbox" bind:checked={importLabels} disabled={!supportsMetadataImport()} /> Labels</label>
        <label><input type="checkbox" bind:checked={importMilestones} disabled={!supportsMetadataImport()} /> Milestones</label>
      </fieldset>

      <div class="actions wide">
        <button class="btn-primary" type="submit" disabled={submitting || !sourceUrl.trim() || !targetOwner.trim()}>
          {submitting ? 'Starting...' : 'Start import'}
        </button>
      </div>
    </form>
  </section>

  <section class="panel">
    <h2>Import tasks</h2>
    {#if loading}
      <p class="muted">Loading imports...</p>
    {:else if taskList.length === 0}
      <p class="muted">No import tasks yet.</p>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>Source</th>
              <th>Target</th>
              <th>Status</th>
              <th>Progress</th>
              <th>Updated</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each taskList as task}
              <tr>
                <td>
                  <div class="source">
                    <span class="platform">{task.platform}</span>
                    <a href={task.source_url} target="_blank" rel="noreferrer">{task.source_url}</a>
                  </div>
                  {#if task.error}
                    <div class="task-error">{task.error}</div>
                  {/if}
                </td>
                <td><a href={taskHref(task)}>{task.target_owner}/{task.target_name}</a></td>
                <td><span class="status">{task.status}</span></td>
                <td>
                  {task.progress}%
                  {#if task.stage}
                    <div class="muted small">{task.stage}</div>
                  {/if}
                </td>
                <td>{formatDateTime(task.updated_at || task.created_at)}</td>
                <td class="row-actions">
                  <button class="btn-danger" type="button" onclick={() => deleteImport(task.id)}>Delete</button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </section>
</div>

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 16px;
    margin-bottom: 20px;
  }

  h1 {
    margin: 0 0 4px;
    font-size: 24px;
  }

  h2 {
    margin: 0 0 16px;
    font-size: 18px;
  }

  .subtitle,
  .muted {
    color: var(--text-secondary);
  }

  .panel {
    margin-bottom: 20px;
    padding: 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .import-form {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 14px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    color: var(--text-secondary);
    font-size: 13px;
    font-weight: 600;
  }

  input,
  select {
    min-width: 0;
    padding: 8px 10px;
    background: var(--bg-primary);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
  }

  .wide {
    grid-column: 1 / -1;
  }

  .options {
    display: flex;
    flex-wrap: wrap;
    gap: 10px 18px;
    margin: 0;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: 6px;
  }

  .options legend {
    color: var(--text-secondary);
    font-size: 13px;
    font-weight: 600;
    padding: 0 4px;
  }

  .options label {
    flex-direction: row;
    align-items: center;
    font-weight: 500;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
  }

  .btn-primary,
  .btn-secondary,
  .btn-danger {
    border: 0;
    border-radius: 6px;
    padding: 8px 12px;
    cursor: pointer;
    font-weight: 600;
  }

  .btn-primary {
    background: var(--accent);
    color: white;
  }

  .btn-secondary {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    color: var(--text-primary);
  }

  .btn-danger {
    background: rgba(248, 81, 73, 0.12);
    color: #f85149;
  }

  button:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .error-banner,
  .success-banner {
    margin-bottom: 16px;
    padding: 10px 12px;
    border-radius: 6px;
  }

  .error-banner {
    color: #f85149;
    background: rgba(248, 81, 73, 0.1);
  }

  .success-banner {
    color: #3fb950;
    background: rgba(63, 185, 80, 0.1);
  }

  .table-wrap {
    overflow-x: auto;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
  }

  th,
  td {
    padding: 10px 8px;
    border-bottom: 1px solid var(--border);
    text-align: left;
    vertical-align: top;
  }

  th {
    color: var(--text-secondary);
    font-weight: 600;
  }

  .source {
    display: grid;
    gap: 4px;
    min-width: 260px;
  }

  .platform,
  .status {
    width: fit-content;
    padding: 2px 6px;
    border: 1px solid var(--border);
    border-radius: 999px;
    color: var(--text-secondary);
    font-size: 11px;
    text-transform: uppercase;
  }

  .task-error {
    margin-top: 6px;
    color: #f85149;
  }

  .small {
    margin-top: 4px;
    font-size: 12px;
  }

  .row-actions {
    text-align: right;
  }

  @media (max-width: 720px) {
    .page-header,
    .import-form {
      display: block;
    }

    .import-form label,
    .options,
    .actions {
      margin-top: 12px;
    }
  }
</style>
