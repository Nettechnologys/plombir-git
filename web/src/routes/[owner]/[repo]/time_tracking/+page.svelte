<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { issues, timeTracking } from '$lib/api/client.svelte';
  import {
    LatestRepositoryRequestFence,
    LatestRepositoryResourceRequestFence,
  } from '$lib/asyncStateOwnership';
  import { createT } from '$lib/i18n';
  import { viewerPermission } from '$lib/viewerPermission.svelte';
  import ConfirmModal from '$lib/components/ConfirmModal.svelte';
  import { createConfirmer } from '$lib/confirm.svelte';

  const t = createT();
  const confirmer = createConfirmer();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  // Logging and deleting time are `RepoWrite` (card_270a0a77fd79).
  const permission = viewerPermission(() => owner, () => repo);
  const canWrite = $derived(permission.canWrite);

  // Issue selector
  let issueList = $state<any[]>([]);
  let selectedIssue = $state<any | null>(null);
  let issueLoading = $state(true);

  // Time entries for selected issue
  let entries = $state<any[]>([]);
  let totalFormatted = $state('');
  let totalMinutes = $state(0);
  let totalLoading = $state(false);
  let totalError = $state('');
  let entriesLoading = $state(false);
  let currentPage = $state(1);
  let totalPages = $state(1);

  // Add entry form
  let durationHours = $state<number>(1);
  let description = $state('');
  let mutationBusy = $state(false);

  let error = $state('');
  const issueListRequests = new LatestRepositoryRequestFence();
  const entryRequests = new LatestRepositoryResourceRequestFence<string>();
  const totalRequests = new LatestRepositoryResourceRequestFence<number>();
  let routeGeneration = 0;
  let selectionGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    selectionGeneration += 1;
    issueList = [];
    selectedIssue = null;
    entries = [];
    totalFormatted = '';
    totalMinutes = 0;
    totalLoading = false;
    totalError = '';
    currentPage = 1;
    totalPages = 1;
    mutationBusy = false;
    durationHours = 1;
    description = '';
    error = '';
    issueLoading = true;
    entriesLoading = false;
    void loadIssues(expectedOwner, expectedRepo, routeGeneration);
  });

  type TimeRoute = Readonly<{ owner: string; repo: string; generation: number }>;
  type TimeSelection = Readonly<TimeRoute & { issueNumber: number; selection: number }>;

  function currentRoute(): TimeRoute {
    return { owner, repo, generation: routeGeneration };
  }

  function isCurrentRoute(route: TimeRoute) {
    return routeGeneration === route.generation && owner === route.owner && repo === route.repo;
  }

  function isCurrentSelection(selection: TimeSelection) {
    return (
      isCurrentRoute(selection) &&
      selectionGeneration === selection.selection &&
      selectedIssue?.number === selection.issueNumber
    );
  }

  async function loadIssues(
    expectedOwner = owner,
    expectedRepo = repo,
    expectedRoute = routeGeneration,
  ) {
    const route = { owner: expectedOwner, repo: expectedRepo, generation: expectedRoute };
    if (!isCurrentRoute(route)) return;
    const claim = issueListRequests.begin(expectedOwner, expectedRepo);
    issueLoading = true;
    error = '';
    try {
      const res = await issues.list(expectedOwner, expectedRepo, 'open', 1, 100);
      if (issueListRequests.owns(claim, owner, repo) && isCurrentRoute(route)) {
        issueList = res.data || [];
      }
    } catch (e: any) {
      if (issueListRequests.owns(claim, owner, repo) && isCurrentRoute(route)) {
        error = e.message;
      }
    } finally {
      if (issueListRequests.owns(claim, owner, repo) && isCurrentRoute(route)) {
        issueLoading = false;
      }
    }
  }

  async function selectIssue(issue: any) {
    if (mutationBusy) return;
    selectedIssue = issue;
    selectionGeneration += 1;
    mutationBusy = false;
    currentPage = 1;
    entries = [];
    totalFormatted = '';
    totalMinutes = 0;
    totalLoading = false;
    totalError = '';
    totalPages = 1;
    error = '';
    const selection = { ...currentRoute(), issueNumber: issue.number, selection: selectionGeneration };
    await Promise.all([loadEntries(selection, 1), loadTotal(selection)]);
  }

  async function loadEntries(
    selection: TimeSelection = {
      ...currentRoute(),
      issueNumber: selectedIssue?.number,
      selection: selectionGeneration,
    },
    expectedPage = currentPage,
  ) {
    if (!selectedIssue || !isCurrentSelection(selection)) return;
    const requestIdentity = `${selection.issueNumber}:${expectedPage}`;
    const claim = entryRequests.begin(selection.owner, selection.repo, requestIdentity);
    entriesLoading = true;
    try {
      const res = await timeTracking.list(
        selection.owner,
        selection.repo,
        selection.issueNumber,
        expectedPage,
        20,
      );
      if (
        entryRequests.owns(claim, owner, repo, `${selectedIssue?.number}:${currentPage}`) &&
        isCurrentSelection(selection)
      ) {
        entries = res.data || [];
        totalPages = res.pagination?.total_pages ?? 1;
      }
    } catch (e: any) {
      if (
        entryRequests.owns(claim, owner, repo, `${selectedIssue?.number}:${currentPage}`) &&
        isCurrentSelection(selection)
      ) {
        error = e.message;
      }
    } finally {
      if (
        entryRequests.owns(claim, owner, repo, `${selectedIssue?.number}:${currentPage}`) &&
        isCurrentSelection(selection)
      ) {
        entriesLoading = false;
      }
    }
  }

  async function loadTotal(selection: TimeSelection = {
    ...currentRoute(),
    issueNumber: selectedIssue?.number,
    selection: selectionGeneration,
  }) {
    if (!selectedIssue || !isCurrentSelection(selection)) return;
    const claim = totalRequests.begin(selection.owner, selection.repo, selection.issueNumber);
    totalLoading = true;
    totalError = '';
    try {
      const res = await timeTracking.total(selection.owner, selection.repo, selection.issueNumber);
      if (
        totalRequests.owns(claim, owner, repo, selectedIssue?.number ?? -1) &&
        isCurrentSelection(selection)
      ) {
        totalMinutes = res.total_minutes;
        totalFormatted = res.total_formatted;
        totalError = '';
      }
    } catch (e: any) {
      if (
        totalRequests.owns(claim, owner, repo, selectedIssue?.number ?? -1) &&
        isCurrentSelection(selection)
      ) {
        totalMinutes = 0;
        totalFormatted = '';
        totalError = e?.message || t('repo.time_tracking.total_unavailable');
      }
    } finally {
      if (
        totalRequests.owns(claim, owner, repo, selectedIssue?.number ?? -1) &&
        isCurrentSelection(selection)
      ) {
        totalLoading = false;
      }
    }
  }

  async function handleAdd() {
    if (mutationBusy || !selectedIssue || durationHours <= 0) return;
    const selection = {
      ...currentRoute(),
      issueNumber: selectedIssue.number,
      selection: selectionGeneration,
    };
    entryRequests.begin(selection.owner, selection.repo, `${selection.issueNumber}:${currentPage}`);
    totalRequests.begin(selection.owner, selection.repo, selection.issueNumber);
    mutationBusy = true;
    error = '';
    try {
      await timeTracking.add(selection.owner, selection.repo, selection.issueNumber, {
        duration_minutes: Math.round(durationHours * 60),
        description: description || undefined,
      });
      if (!isCurrentSelection(selection)) return;
      durationHours = 1;
      description = '';
      await Promise.all([loadEntries(selection, currentPage), loadTotal(selection)]);
    } catch (e: any) {
      if (isCurrentSelection(selection)) error = e.message;
    } finally {
      if (isCurrentSelection(selection)) mutationBusy = false;
    }
  }

  async function handleDelete(id: number) {
    if (mutationBusy || !selectedIssue) return;
    if (!(await confirmer.ask({
      title: t('repo.time_tracking.delete_confirm_title'),
      message: t('repo.time_tracking.delete_confirm'),
      confirmLabel: t('common.delete'),
    }))) return;
    const selection = {
      ...currentRoute(),
      issueNumber: selectedIssue.number,
      selection: selectionGeneration,
    };
    entryRequests.begin(selection.owner, selection.repo, `${selection.issueNumber}:${currentPage}`);
    totalRequests.begin(selection.owner, selection.repo, selection.issueNumber);
    mutationBusy = true;
    error = '';
    try {
      await timeTracking.delete(selection.owner, selection.repo, selection.issueNumber, id);
      if (!isCurrentSelection(selection)) return;
      await Promise.all([loadEntries(selection, currentPage), loadTotal(selection)]);
    } catch (e: any) {
      if (isCurrentSelection(selection)) error = e.message;
    } finally {
      if (isCurrentSelection(selection)) mutationBusy = false;
    }
  }

  function fmtMinutes(m: number): string {
    if (m < 60) return t('time_tracking.minutes_short', { minutes: m });
    const h = Math.floor(m / 60);
    const rem = m % 60;
    return rem > 0
      ? t('time_tracking.hours_minutes_short', { hours: h, minutes: rem })
      : t('time_tracking.hours_short', { hours: h });
  }
</script>

<svelte:head>
  <title>{t('time_tracking.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="time_tracking" />

  <div class="page-header">
    <h1>{t('time_tracking.title')}</h1>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  <div class="layout">
    <!-- Issue list sidebar -->
    <aside class="issue-sidebar">
      <div class="sidebar-header">
        <h3>{t('time_tracking.issues')}</h3>
      </div>
      {#if issueLoading}
        <p class="sidebar-empty">{t('time_tracking.loading')}</p>
      {:else if issueList.length === 0}
        <p class="sidebar-empty">{t('time_tracking.no_open_issues')}</p>
      {:else}
        <nav class="issue-nav">
          {#each issueList as issue}
            <button
              class="issue-item"
              class:active={selectedIssue?.id === issue.id}
              onclick={() => selectIssue(issue)}
              disabled={mutationBusy}
            >
              <span class="issue-num">#{issue.number}</span>
              <span class="issue-title">{issue.title}</span>
            </button>
          {/each}
        </nav>
      {/if}
    </aside>

    <!-- Main panel -->
    <div class="main-panel">
      {#if !selectedIssue}
        <div class="select-hint">
          <p>{t('time_tracking.select_hint')}</p>
        </div>
      {:else}
        <div class="issue-header">
          <h2>
            <a href={`/${owner}/${repo}/issues/${selectedIssue.number}`} class="issue-link">
              #{selectedIssue.number} {selectedIssue.title}
            </a>
          </h2>
          {#if totalError}
            <div class="total-unavailable" role="alert">
              <span>{t('repo.time_tracking.total_unavailable')}</span>
              <button
                type="button"
                class="btn-secondary btn-sm"
                onclick={() => loadTotal()}
                disabled={totalLoading || mutationBusy}
              >
                {t('common.retry')}
              </button>
            </div>
          {:else if totalLoading}
            <div class="total-badge">{t('common.loading')}</div>
          {:else if totalFormatted}
            <div class="total-badge">{t('time_tracking.total', { total: totalFormatted })}</div>
          {/if}
        </div>

        <!-- Add entry form -->
        {#if canWrite}
        <div class="form-card">
          <h3>{t('repo.time_tracking.add_entry')}</h3>
          <div class="form-row">
            <div class="form-group">
              <label for="tt-dur">{t('repo.time_tracking.duration')}</label>
              <input id="tt-dur" type="number" min="0.25" step="0.25" bind:value={durationHours} disabled={mutationBusy} />
            </div>
            <div class="form-group flex-grow">
              <label for="tt-desc">{t('repo.time_tracking.note')}</label>
              <input id="tt-desc" type="text" placeholder={t('common.optional')} bind:value={description} disabled={mutationBusy} />
            </div>
            <div class="form-action">
              <button class="btn-primary" onclick={handleAdd} disabled={mutationBusy}>
                {mutationBusy ? '…' : t('common.add')}
              </button>
            </div>
          </div>
        </div>
        {/if}

        <!-- Entries table -->
        {#if entriesLoading}
          <p class="loading-text">{t('time_tracking.loading')}</p>
        {:else if entries.length === 0}
          <div class="empty">{t('time_tracking.no_entries_for_issue')}</div>
        {:else}
          <table class="entries-table">
            <thead>
              <tr>
                <th>{t('time_tracking.duration_column')}</th>
                <th>{t('repo.time_tracking.note')}</th>
                <th>{t('time_tracking.logged_column')}</th>
                <th></th>
              </tr>
            </thead>
            <tbody>
              {#each entries as entry (entry.id)}
                <tr>
                  <td class="dur-cell">{fmtMinutes(entry.duration_minutes)}</td>
                  <td class="note-cell">{entry.description || '—'}</td>
                  <td class="date-cell">{entry.created_at?.slice(0, 10) || ''}</td>
                  <td class="act-cell">
                    {#if canWrite}<button class="btn-danger btn-sm" onclick={() => handleDelete(entry.id)} disabled={mutationBusy}>{t('common.delete')}</button>{/if}
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>

          {#if totalPages > 1}
            <div class="pagination">
              <button class="btn-outline" disabled={mutationBusy || currentPage <= 1}
                onclick={() => { currentPage--; loadEntries(); }}>{t('common.previous')}</button>
              <span>{currentPage} / {totalPages}</span>
              <button class="btn-outline" disabled={mutationBusy || currentPage >= totalPages}
                onclick={() => { currentPage++; loadEntries(); }}>{t('common.next')}</button>
            </div>
          {/if}
        {/if}
      {/if}
    </div>
  </div>
</div>

<ConfirmModal {confirmer} />

<style>

  .page-header { margin-bottom: 20px; }
  h1 { font-size: 22px; font-weight: 600; margin: 0; }
/* ── Layout ── */
  .layout { display: grid; grid-template-columns: 240px 1fr; gap: 20px; align-items: start; }
  @media (max-width: 600px) { .layout { grid-template-columns: 1fr; } }

  /* ── Sidebar ── */
  .issue-sidebar {
    background: var(--bg-secondary); border: 1px solid var(--border);
    border-radius: var(--radius); overflow: hidden; position: sticky; top: 24px;
  }
  .sidebar-header {
    padding: 10px 14px; border-bottom: 1px solid var(--border);
    background: var(--bg-tertiary);
  }
  .sidebar-header h3 { font-size: 12px; text-transform: uppercase; letter-spacing: 0.5px; margin: 0; color: var(--text-muted); }
  .sidebar-empty { padding: 16px; font-size: 13px; color: var(--text-muted); }

  .issue-nav { display: flex; flex-direction: column; max-height: 60vh; overflow-y: auto; }
  .issue-item {
    display: flex; flex-direction: column; gap: 2px; padding: 10px 14px;
    border: none; border-bottom: 1px solid var(--border-light);
    background: none; text-align: left; cursor: pointer;
  }
  .issue-item:hover { background: var(--bg-hover); }
  .issue-item.active { background: rgba(var(--accent-rgb, 88,166,255), 0.12); }
  .issue-num { font-size: 11px; color: var(--text-muted); font-weight: 600; }
  .issue-title { font-size: 13px; color: var(--text-primary); line-height: 1.3; }

  /* ── Main panel ── */
  .main-panel { min-width: 0; }

  .select-hint {
    padding: 80px 24px; text-align: center; color: var(--text-secondary); font-size: 14px;
    background: var(--bg-secondary); border: 1px solid var(--border); border-radius: var(--radius);
  }

  .issue-header {
    display: flex; align-items: center; justify-content: space-between;
    flex-wrap: wrap; gap: 10px; margin-bottom: 16px;
  }
  h2 { font-size: 17px; font-weight: 600; margin: 0; }
  .issue-link { color: var(--text-primary); text-decoration: none; }
  .issue-link:hover { color: var(--accent); }

  .total-badge {
    background: var(--bg-tertiary); border: 1px solid var(--border);
    border-radius: var(--radius); padding: 4px 12px; font-size: 13px;
    font-weight: 600; color: var(--text-secondary);
  }

  /* ── Form ── */
  .form-card {
    background: var(--bg-secondary); border: 1px solid var(--border);
    border-radius: var(--radius); padding: 16px; margin-bottom: 20px;
  }
  h3 { font-size: 14px; font-weight: 600; margin: 0 0 12px; }
  .form-row { display: flex; gap: 12px; align-items: flex-end; flex-wrap: wrap; }
  .form-group { display: flex; flex-direction: column; gap: 4px; }
  .form-group label { font-size: 11px; font-weight: 600; color: var(--text-secondary); text-transform: uppercase; }
  .form-group input {
    padding: 6px 10px; border: 1px solid var(--border); border-radius: var(--radius);
    background: var(--bg-primary); color: var(--text-primary); font-size: 14px;
  }
  .form-group input[type="number"] { width: 90px; }
  .flex-grow { flex: 1; }
  .form-action { display: flex; align-items: flex-end; }

  .loading-text { color: var(--text-secondary); text-align: center; padding: 32px; }
  .empty {
    padding: 40px; text-align: center; color: var(--text-secondary); font-size: 14px;
    background: var(--bg-secondary); border: 1px solid var(--border); border-radius: var(--radius);
  }

  /* ── Table ── */
  .entries-table { width: 100%; border-collapse: collapse; font-size: 14px; }
  .entries-table th {
    text-align: left; padding: 6px 12px; border-bottom: 2px solid var(--border);
    color: var(--text-secondary); font-size: 11px; font-weight: 600; text-transform: uppercase;
  }
  .entries-table td { padding: 8px 12px; border-bottom: 1px solid var(--border); }
  .dur-cell { font-weight: 600; white-space: nowrap; }
  .note-cell { max-width: 300px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .date-cell { white-space: nowrap; color: var(--text-muted); font-size: 12px; }
  .act-cell { text-align: right; }

  /* ── Buttons ── */
  .btn-primary {
    padding: 6px 14px; background: var(--accent); color: #fff; border: none;
    border-radius: var(--radius); font-size: 13px; font-weight: 600; cursor: pointer;
  }
  .btn-primary:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-outline {
    padding: 5px 12px; background: var(--bg-secondary); border: 1px solid var(--border);
    border-radius: var(--radius); color: var(--text-primary); font-size: 13px; cursor: pointer;
  }
  .btn-outline:disabled { opacity: 0.5; cursor: not-allowed; }
  .btn-danger {
    padding: 4px 10px; background: var(--red-dim); border: 1px solid var(--red);
    border-radius: var(--radius); color: #fff; font-size: 12px; cursor: pointer;
  }
  .btn-danger:hover { background: var(--red); }
  .btn-sm { padding: 4px 10px; font-size: 12px; }

  .pagination { display: flex; align-items: center; justify-content: center; gap: 16px; margin-top: 20px; }
  .pagination span { font-size: 13px; color: var(--text-secondary); }
</style>
