<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { issues, timeTracking } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);

  // Issue selector
  let issueList = $state<any[]>([]);
  let selectedIssue = $state<any | null>(null);
  let issueLoading = $state(true);

  // Time entries for selected issue
  let entries = $state<any[]>([]);
  let totalFormatted = $state('');
  let totalMinutes = $state(0);
  let entriesLoading = $state(false);
  let currentPage = $state(1);
  let totalPages = $state(1);

  // Add entry form
  let durationHours = $state<number>(1);
  let description = $state('');
  let saving = $state(false);

  let error = $state('');

  $effect(() => { loadIssues(); });

  async function loadIssues() {
    issueLoading = true;
    error = '';
    try {
      const res = await issues.list(owner, repo, 'open', 1, 100);
      issueList = res.data || [];
    } catch (e: any) {
      error = e.message;
    } finally {
      issueLoading = false;
    }
  }

  async function selectIssue(issue: any) {
    selectedIssue = issue;
    currentPage = 1;
    await loadEntries();
    await loadTotal();
  }

  async function loadEntries() {
    if (!selectedIssue) return;
    entriesLoading = true;
    try {
      const res = await timeTracking.list(owner, repo, selectedIssue.number, currentPage, 20);
      entries = res.data || [];
      totalPages = res.pagination?.total_pages ?? 1;
    } catch (e: any) {
      error = e.message;
    } finally {
      entriesLoading = false;
    }
  }

  async function loadTotal() {
    if (!selectedIssue) return;
    try {
      const res = await timeTracking.total(owner, repo, selectedIssue.number);
      totalMinutes = res.total_minutes;
      totalFormatted = res.total_formatted;
    } catch {
      totalFormatted = '';
    }
  }

  async function handleAdd() {
    if (!selectedIssue || durationHours <= 0) return;
    saving = true;
    error = '';
    try {
      await timeTracking.add(owner, repo, selectedIssue.number, {
        duration_minutes: Math.round(durationHours * 60),
        description: description || undefined,
      });
      durationHours = 1;
      description = '';
      await loadEntries();
      await loadTotal();
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function handleDelete(id: number) {
    if (!selectedIssue) return;
    if (!confirm('Delete this time entry?')) return;
    try {
      await timeTracking.delete(owner, repo, selectedIssue.number, id);
      await loadEntries();
      await loadTotal();
    } catch (e: any) {
      error = e.message;
    }
  }

  function fmtMinutes(m: number): string {
    if (m < 60) return `${m}m`;
    const h = Math.floor(m / 60);
    const rem = m % 60;
    return rem > 0 ? `${h}h ${rem}m` : `${h}h`;
  }
</script>

<svelte:head>
  <title>Time Tracking · {owner}/{repo} · IronForge</title>
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
        <h3>Issues</h3>
      </div>
      {#if issueLoading}
        <p class="sidebar-empty">Loading…</p>
      {:else if issueList.length === 0}
        <p class="sidebar-empty">No open issues.</p>
      {:else}
        <nav class="issue-nav">
          {#each issueList as issue}
            <button
              class="issue-item"
              class:active={selectedIssue?.id === issue.id}
              onclick={() => selectIssue(issue)}
            >
              <span class="issue-num">#{issue.number}</span>
              <span class="issue-title">{issue.title}</span>
            </button>
          {/each}
        </nav>
      {/if}
    </aside>

    <!-- Main panel -->
    <main class="main-panel">
      {#if !selectedIssue}
        <div class="select-hint">
          <p>← Select an issue to view and log time</p>
        </div>
      {:else}
        <div class="issue-header">
          <h2>
            <a href={`/${owner}/${repo}/issues/${selectedIssue.number}`} class="issue-link">
              #{selectedIssue.number} {selectedIssue.title}
            </a>
          </h2>
          {#if totalFormatted}
            <div class="total-badge">Total: {totalFormatted}</div>
          {/if}
        </div>

        <!-- Add entry form -->
        <div class="form-card">
          <h3>{t('repo.time_tracking.add_entry')}</h3>
          <div class="form-row">
            <div class="form-group">
              <label for="tt-dur">Duration (hours)</label>
              <input id="tt-dur" type="number" min="0.25" step="0.25" bind:value={durationHours} />
            </div>
            <div class="form-group flex-grow">
              <label for="tt-desc">Note</label>
              <input id="tt-desc" type="text" placeholder="(optional)" bind:value={description} />
            </div>
            <div class="form-action">
              <button class="btn-primary" onclick={handleAdd} disabled={saving}>
                {saving ? '…' : 'Add'}
              </button>
            </div>
          </div>
        </div>

        <!-- Entries table -->
        {#if entriesLoading}
          <p class="loading-text">Loading…</p>
        {:else if entries.length === 0}
          <div class="empty">No time entries for this issue yet.</div>
        {:else}
          <table class="entries-table">
            <thead>
              <tr>
                <th>Duration</th>
                <th>Note</th>
                <th>Logged</th>
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
                    <button class="btn-danger btn-sm" onclick={() => handleDelete(entry.id)}>Delete</button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>

          {#if totalPages > 1}
            <div class="pagination">
              <button class="btn-outline" disabled={currentPage <= 1}
                onclick={() => { currentPage--; loadEntries(); }}>Previous</button>
              <span>{currentPage} / {totalPages}</span>
              <button class="btn-outline" disabled={currentPage >= totalPages}
                onclick={() => { currentPage++; loadEntries(); }}>Next</button>
            </div>
          {/if}
        {/if}
      {/if}
    </main>
  </div>
</div>

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
