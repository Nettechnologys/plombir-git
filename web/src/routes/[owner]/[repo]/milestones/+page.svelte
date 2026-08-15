<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import {
    buildMilestoneCreatePayload,
    buildMilestoneUpdatePayload,
    dueDateForInput,
    milestones,
    type Milestone,
    type MilestoneFormState,
  } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let milestoneList = $state<Milestone[]>([]);
  let loading = $state(true);
  let saving = $state(false);
  let error = $state('');
  let filter = $state<'all' | Milestone['state']>('open');
  let editingId = $state<number | null>(null);
  let form = $state<MilestoneFormState>(emptyForm());

  const visibleMilestones = $derived(
    filter === 'all' ? milestoneList : milestoneList.filter((milestone) => milestone.state === filter)
  );

  $effect(() => {
    loadMilestones();
  });

  function emptyForm(): MilestoneFormState {
    return { title: '', description: '', dueDate: '', state: 'open' };
  }

  async function loadMilestones() {
    loading = true;
    error = '';
    try {
      milestoneList = await milestones.list(owner, repo);
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function startCreate() {
    editingId = null;
    form = emptyForm();
  }

  async function startEdit(milestone: Milestone) {
    error = '';
    try {
      milestone = await milestones.get(owner, repo, milestone.id);
    } catch (e: any) {
      error = e.message;
      return;
    }

    editingId = milestone.id;
    form = {
      title: milestone.title,
      description: milestone.description || '',
      dueDate: dueDateForInput(milestone.due_date),
      state: milestone.state,
    };
  }

  async function saveMilestone(event: SubmitEvent) {
    event.preventDefault();
    if (!form.title.trim()) return;

    saving = true;
    error = '';
    try {
      if (editingId === null) {
        await milestones.create(owner, repo, buildMilestoneCreatePayload(form));
      } else {
        await milestones.update(owner, repo, editingId, buildMilestoneUpdatePayload(form));
      }
      startCreate();
      await loadMilestones();
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  async function toggleState(milestone: Milestone) {
    error = '';
    try {
      await milestones.update(owner, repo, milestone.id, {
        state: milestone.state === 'open' ? 'closed' : 'open',
      });
      await loadMilestones();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function deleteMilestone(milestone: Milestone) {
    if (!confirm(t('milestones.delete_confirm', { title: milestone.title }))) return;

    error = '';
    try {
      await milestones.delete(owner, repo, milestone.id);
      if (editingId === milestone.id) startCreate();
      await loadMilestones();
    } catch (e: any) {
      error = e.message;
    }
  }
</script>

<svelte:head>
  <title>{t('milestones.title')} · {owner}/{repo} · ForgeKeep</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="milestones" />

  <div class="page-header">
    <div>
      <h1>{t('milestones.title')}</h1>
      <p>{t('milestones.description')}</p>
    </div>
    <select bind:value={filter} aria-label={t('milestones.filter')}>
      <option value="open">{t('milestones.open')}</option>
      <option value="closed">{t('milestones.closed')}</option>
      <option value="all">{t('milestones.all')}</option>
    </select>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  <section class="editor">
    <h2>{editingId === null ? t('milestones.create') : t('milestones.edit')}</h2>
    <form onsubmit={saveMilestone}>
      <div class="form-grid">
        <label>
          {t('milestones.name')}
          <input bind:value={form.title} required disabled={saving} />
        </label>
        <label>
          {t('milestones.due_date')}
          <input type="date" bind:value={form.dueDate} disabled={saving} />
        </label>
      </div>
      <label>
        {t('milestones.details')}
        <textarea bind:value={form.description} rows="3" disabled={saving}></textarea>
      </label>
      <label class="state-field">
        {t('milestones.state')}
        <select bind:value={form.state} disabled={saving}>
          <option value="open">{t('milestones.open')}</option>
          <option value="closed">{t('milestones.closed')}</option>
        </select>
      </label>
      <div class="form-actions">
        <button class="btn-primary" type="submit" disabled={saving || !form.title.trim()}>
          {saving ? t('common.loading') : t('common.save')}
        </button>
        {#if editingId !== null}
          <button class="btn-outline" type="button" onclick={startCreate} disabled={saving}>
            {t('common.cancel')}
          </button>
        {/if}
      </div>
    </form>
  </section>

  {#if loading}
    <p class="loading">{t('common.loading')}</p>
  {:else if visibleMilestones.length === 0}
    <div class="empty">{t('milestones.empty')}</div>
  {:else}
    <div class="milestone-list">
      {#each visibleMilestones as milestone (milestone.id)}
        <article class="milestone">
          <div class="milestone-main">
            <div class="milestone-title-row">
              <h2>{milestone.title}</h2>
              <span class:closed={milestone.state === 'closed'} class="state-badge">
                {t(`milestones.${milestone.state}`)}
              </span>
            </div>
            {#if milestone.description}
              <p>{milestone.description}</p>
            {/if}
            <div class="milestone-meta">
              {milestone.due_date
                ? t('milestones.due', { date: formatDate(milestone.due_date) })
                : t('milestones.no_due_date')}
            </div>
          </div>
          <div class="milestone-actions">
            <button class="btn-outline" onclick={() => startEdit(milestone)}>{t('common.edit')}</button>
            <button class="btn-outline" onclick={() => toggleState(milestone)}>
              {milestone.state === 'open' ? t('milestones.close') : t('milestones.reopen')}
            </button>
            <button class="btn-danger" onclick={() => deleteMilestone(milestone)}>{t('common.delete')}</button>
          </div>
        </article>
      {/each}
    </div>
  {/if}
</div>

<style>
  .page-header { display: flex; align-items: flex-start; justify-content: space-between; gap: 16px; margin-bottom: 20px; }
  .page-header h1 { margin: 0; font-size: 24px; }
  .page-header p { margin: 6px 0 0; color: var(--text-secondary); }
  select, input, textarea {
    border: 1px solid var(--border); border-radius: var(--radius); background: var(--bg-primary);
    color: var(--text-primary); padding: 7px 10px; box-sizing: border-box;
  }
  .editor { border: 1px solid var(--border); border-radius: var(--radius); padding: 16px; margin-bottom: 20px; }
  .editor h2 { font-size: 16px; margin: 0 0 12px; }
  .editor label { display: flex; flex-direction: column; gap: 5px; font-size: 13px; color: var(--text-secondary); }
  .editor textarea { width: 100%; resize: vertical; margin-top: 12px; }
  .form-grid { display: grid; grid-template-columns: 2fr 1fr; gap: 12px; }
  .state-field { max-width: 180px; margin-top: 12px; }
  .form-actions, .milestone-actions { display: flex; gap: 8px; margin-top: 12px; flex-wrap: wrap; }
  .loading, .empty { color: var(--text-secondary); text-align: center; padding: 40px; }
  .milestone-list { display: flex; flex-direction: column; gap: 10px; }
  .milestone { display: flex; justify-content: space-between; gap: 16px; border: 1px solid var(--border); border-radius: var(--radius); padding: 14px 16px; }
  .milestone-main { min-width: 0; }
  .milestone-title-row { display: flex; align-items: center; gap: 8px; }
  .milestone h2 { margin: 0; font-size: 16px; }
  .milestone p { margin: 8px 0; color: var(--text-secondary); white-space: pre-wrap; }
  .milestone-meta { font-size: 12px; color: var(--text-muted); }
  .state-badge { border-radius: 10px; padding: 2px 8px; font-size: 11px; background: rgba(63, 185, 80, 0.15); color: var(--green); }
  .state-badge.closed { background: rgba(110, 118, 129, 0.18); color: var(--text-secondary); }
  .milestone-actions { align-items: flex-start; margin-top: 0; }
  button { border-radius: var(--radius); padding: 6px 12px; cursor: pointer; }
  .btn-primary { background: var(--accent); border: 1px solid var(--accent); color: #fff; }
  .btn-outline { background: none; border: 1px solid var(--border); color: var(--text-primary); }
  .btn-danger { background: none; border: 1px solid var(--red); color: var(--red); }
  button:disabled { opacity: 0.5; cursor: not-allowed; }
  @media (max-width: 700px) {
    .form-grid { grid-template-columns: 1fr; }
    .milestone { flex-direction: column; }
  }
</style>
