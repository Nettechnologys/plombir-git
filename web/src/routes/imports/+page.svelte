<script lang="ts">
  import { goto } from '$app/navigation';
  import { imports, type ImportTask, type StartImportPayload } from '$lib/api/client.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { isLoggedIn, getUser } from '$lib/stores/auth.svelte';
  import { createT, formatDateTime } from '$lib/i18n';

  const t = createT();

  let taskList = $state<ImportTask[]>([]);
  let loading = $state(true);
  let submitting = $state(false);
  let error = $state('');
  let success = $state('');
  let deletingImports = $state<Set<number>>(new Set());
  const listRequests = new LatestRequestFence<string>();
  let accountGeneration = 0;
  let activeAccountIdentity = '';

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
    const user = getUser();
    if (!isLoggedIn() || !user) {
      accountGeneration += 1;
      activeAccountIdentity = '';
      listRequests.begin('logged-out');
      taskList = [];
      loading = false;
      submitting = false;
      deletingImports = new Set();
      error = '';
      success = '';
      goto('/login');
      return;
    }

    const identity = `${user.id}:${user.username}`;
    if (identity === activeAccountIdentity) return;
    activeAccountIdentity = identity;
    accountGeneration += 1;
    taskList = [];
    loading = true;
    submitting = false;
    deletingImports = new Set();
    error = '';
    success = '';
    targetOwner = user.username;
    void loadImports(identity);
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

  function isCurrentAccount(identity: string, generation: number): boolean {
    return activeAccountIdentity === identity && accountGeneration === generation;
  }

  async function loadImports(identity: string) {
    if (!identity) return;
    const claim = listRequests.begin(identity);
    loading = true;
    error = '';
    try {
      const next = await imports.list();
      if (listRequests.owns(claim, activeAccountIdentity)) taskList = next;
    } catch (e: any) {
      if (listRequests.owns(claim, activeAccountIdentity)) {
        error = e.message || t('imports.load_failed');
      }
    } finally {
      if (listRequests.owns(claim, activeAccountIdentity)) loading = false;
    }
  }

  function refreshImports(): void {
    if (activeAccountIdentity) void loadImports(activeAccountIdentity);
  }

  async function startImport(e: Event) {
    e.preventDefault();
    if (submitting || !activeAccountIdentity) return;
    error = '';
    success = '';
    const expectedIdentity = activeAccountIdentity;
    const expectedGeneration = accountGeneration;

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
    listRequests.begin(expectedIdentity);
    try {
      await imports.start(payload);
      if (!isCurrentAccount(expectedIdentity, expectedGeneration)) return;
      sourceUrl = '';
      targetName = '';
      authToken = '';
      success = t('imports.queued');
      await loadImports(expectedIdentity);
    } catch (e: any) {
      if (isCurrentAccount(expectedIdentity, expectedGeneration)) {
        error = e.message || t('imports.start_failed');
      }
    } finally {
      if (isCurrentAccount(expectedIdentity, expectedGeneration)) submitting = false;
    }
  }

  async function deleteImport(id: number) {
    if (!confirm(t('imports.delete_confirm'))) return;
    if (!activeAccountIdentity || deletingImports.has(id)) return;
    const expectedIdentity = activeAccountIdentity;
    const expectedGeneration = accountGeneration;
    deletingImports = new Set(deletingImports).add(id);
    listRequests.begin(expectedIdentity);
    error = '';
    success = '';
    try {
      await imports.remove(id);
      if (!isCurrentAccount(expectedIdentity, expectedGeneration)) return;
      // A manual refresh can start while deletion is pending.  Invalidate that
      // older snapshot before publishing the removal so it cannot resurrect
      // the task after this mutation succeeds.
      listRequests.begin(expectedIdentity);
      loading = false;
      taskList = taskList.filter((task) => task.id !== id);
      success = t('imports.deleted');
    } catch (e: any) {
      if (isCurrentAccount(expectedIdentity, expectedGeneration)) {
        error = e.message || t('imports.delete_failed');
      }
    } finally {
      if (isCurrentAccount(expectedIdentity, expectedGeneration)) {
        const next = new Set(deletingImports);
        next.delete(id);
        deletingImports = next;
      }
    }
  }

  function taskHref(task: ImportTask): string {
    return `/${encodeURIComponent(task.target_owner)}/${encodeURIComponent(task.target_name)}`;
  }
</script>

<svelte:head>
  <title>{t('imports.title')} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <div class="page-header">
    <div>
      <h1>{t('imports.title')}</h1>
      <p class="subtitle">{t('imports.subtitle')}</p>
    </div>
    <button class="btn-secondary" type="button" onclick={refreshImports} disabled={loading}>{t('imports.refresh')}</button>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}
  {#if success}
    <div class="success-banner">{success}</div>
  {/if}

  <section class="panel">
    <h2>{t('imports.new_title')}</h2>
    <form class="import-form" onsubmit={startImport}>
      <label>
        {t('imports.platform')}
        <select bind:value={platform}>
          <option value="github">GitHub</option>
          <option value="gitlab">GitLab</option>
          <option value="gitea">Gitea</option>
          <option value="git">Git</option>
        </select>
      </label>

      <label class="wide">
        {t('imports.source_url')}
        <input type="url" bind:value={sourceUrl} placeholder={sourcePlaceholder()} required />
      </label>

      <label>
        {t('imports.target_owner')}
        <input type="text" bind:value={targetOwner} required />
      </label>

      <label>
        {t('imports.target_name')}
        <input type="text" bind:value={targetName} placeholder={t('imports.target_name_placeholder')} />
      </label>

      <label class="wide">
        {t('imports.auth_token')}
        <input type="password" bind:value={authToken} autocomplete="off" placeholder={t('imports.auth_token_placeholder')} />
      </label>

      <fieldset class="wide options">
        <legend>{t('imports.content.title')}</legend>
        <label><input type="checkbox" bind:checked={importRepo} /> {t('imports.content.repository')}</label>
        <label><input type="checkbox" bind:checked={importIssues} disabled={!supportsMetadataImport()} /> {t('imports.content.issues')}</label>
        <label><input type="checkbox" bind:checked={importPullRequests} disabled={!supportsMetadataImport()} /> {t('imports.content.pull_requests')}</label>
        <label><input type="checkbox" bind:checked={importWiki} disabled={!supportsMetadataImport()} /> {t('imports.content.wiki')}</label>
        <label><input type="checkbox" bind:checked={importReleases} disabled={!supportsMetadataImport()} /> {t('imports.content.releases')}</label>
        <label><input type="checkbox" bind:checked={importLabels} disabled={!supportsMetadataImport()} /> {t('imports.content.labels')}</label>
        <label><input type="checkbox" bind:checked={importMilestones} disabled={!supportsMetadataImport()} /> {t('imports.content.milestones')}</label>
      </fieldset>

      <div class="actions wide">
        <button class="btn-primary" type="submit" disabled={submitting || !sourceUrl.trim() || !targetOwner.trim()}>
          {submitting ? t('imports.starting') : t('imports.start')}
        </button>
      </div>
    </form>
  </section>

  <section class="panel">
    <h2>{t('imports.tasks_title')}</h2>
    {#if loading}
      <p class="muted">{t('imports.loading')}</p>
    {:else if taskList.length === 0}
      <p class="muted">{t('imports.empty')}</p>
    {:else}
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>{t('imports.table.source')}</th>
              <th>{t('imports.table.target')}</th>
              <th>{t('imports.table.status')}</th>
              <th>{t('imports.table.progress')}</th>
              <th>{t('imports.table.updated')}</th>
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
                  <button class="btn-danger" type="button" disabled={deletingImports.has(task.id)} onclick={() => deleteImport(task.id)}>
                    {deletingImports.has(task.id) ? t('imports.deleting') : t('common.delete')}
                  </button>
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
