<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { issues } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let issueList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let filterState = $state('open');
  let showCreate = $state(false);
  let showChooser = $state(false);
  let templatesLoaded = $state(false);
  let issueTemplates = $state<any[]>([]);
  let templateConfig = $state<any>({ blank_issues_enabled: true, contact_links: [] });
  let newTitle = $state('');
  let newBody = $state('');
  let newLabels = $state('');

  $effect(() => {
    loadIssues();
  });

  async function loadIssues() {
    try {
      loading = true;
      issueList = (await issues.list(owner, repo, filterState)).data;
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function handleCreate(e: Event) {
    e.preventDefault();
    try {
      const labels = newLabels ? newLabels.split(',').map(l => l.trim()) : undefined;
      await issues.create(owner, repo, newTitle, newBody || undefined, labels);
      showCreate = false;
      newTitle = '';
      newBody = '';
      newLabels = '';
      await loadIssues();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function openCreate() {
    if (showCreate || showChooser) {
      showCreate = false;
      showChooser = false;
      return;
    }
    try {
      if (!templatesLoaded) {
        [issueTemplates, templateConfig] = await Promise.all([
          issues.templates(owner, repo),
          issues.templateConfig(owner, repo),
        ]);
        templatesLoaded = true;
      }
      if (issueTemplates.length > 0 || templateConfig.contact_links.length > 0) {
        showChooser = true;
      } else {
        showCreate = true;
      }
    } catch (e: any) {
      error = e.message;
    }
  }

  function chooseTemplate(template?: any) {
    newTitle = template?.title || '';
    newBody = template?.content || '';
    newLabels = template?.labels?.join(', ') || '';
    showChooser = false;
    showCreate = true;
  }

  function emptyStateLabel(): string {
    if (filterState === 'all') return t('common.all');
    return t(`issues.state_label.${filterState}`, filterState);
  }
</script>

<svelte:head>
  <title>{t('issues.title')} · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="issues" starsCount={0} />

  <div class="gh-toolbar issues-toolbar">
    <div class="filter-tabs">
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'open'}
        onclick={() => { filterState = 'open'; loadIssues(); }}
      >
        {t('issues.tabs.open')}
      </button>
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'closed'}
        onclick={() => { filterState = 'closed'; loadIssues(); }}
      >
        {t('issues.tabs.closed')}
      </button>
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'all'}
        onclick={() => { filterState = 'all'; loadIssues(); }}
      >
        {t('issues.tabs.all')}
      </button>
    </div>
    <button class="btn-primary" onclick={openCreate}>
      {t('issues.new')}
    </button>
  </div>

  {#if showChooser}
    <div class="template-chooser gh-card">
      <div class="chooser-heading">
        <div>
          <h2>{t('issues.templates.title')}</h2>
          <p>{t('issues.templates.description')}</p>
        </div>
        <button class="btn-secondary" onclick={() => showChooser = false}>{t('issues.create_form.cancel')}</button>
      </div>
      <div class="template-list">
        {#each issueTemplates as template}
          <div class="template-option">
            <div>
              <strong>{template.name}</strong>
              <p>{template.about}</p>
            </div>
            <button class="btn-primary" onclick={() => chooseTemplate(template)}>{t('issues.templates.get_started')}</button>
          </div>
        {/each}
        {#if templateConfig.blank_issues_enabled}
          <div class="template-option">
            <div>
              <strong>{t('issues.templates.blank')}</strong>
              <p>{t('issues.templates.blank_about')}</p>
            </div>
            <button class="btn-secondary" onclick={() => chooseTemplate()}>{t('issues.templates.open_blank')}</button>
          </div>
        {/if}
        {#each templateConfig.contact_links as link}
          <div class="template-option">
            <div>
              <strong>{link.name}</strong>
              <p>{link.about}</p>
            </div>
            <a class="btn-secondary external-link" href={link.url} target="_blank" rel="noopener noreferrer">{t('issues.templates.open_link')}</a>
          </div>
        {/each}
      </div>
    </div>
  {/if}

  {#if showCreate}
    <div class="create-form gh-card">
      <form onsubmit={handleCreate}>
        <label>
          {t('issues.create_form.title')}
          <input type="text" bind:value={newTitle} required placeholder={t('issues.create_form.title_placeholder')} />
        </label>
        <label>
          {t('issues.create_form.body')} <span class="optional">{t('issues.create_form.body_hint')}</span>
          <textarea bind:value={newBody} rows="6" placeholder={t('issues.create_form.body_placeholder')}></textarea>
        </label>
        <label>
          {t('issues.create_form.labels')} <span class="optional">{t('issues.create_form.labels_hint')}</span>
          <input type="text" bind:value={newLabels} placeholder={t('issues.create_form.labels_placeholder')} />
        </label>
        <div class="form-actions">
          <button type="submit" class="btn-primary">{t('issues.create_form.submit')}</button>
          <button type="button" class="btn-secondary" onclick={() => { showCreate = false; if (issueTemplates.length > 0 || templateConfig.contact_links.length > 0) showChooser = true; }}>{t('issues.create_form.cancel')}</button>
        </div>
      </form>
    </div>
  {/if}

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if issueList.length === 0}
    <div class="empty">
      <p>{t('issues.empty', { state: emptyStateLabel() })}</p>
    </div>
  {:else}
    <div class="issue-list gh-list">
      {#each issueList as issue}
        <a href={`/${owner}/${repo}/issues/${issue.number}`} class="issue-item gh-list-item">
          <span class="issue-icon">
            {issue.state === 'closed' ? '✓' : '●'}
          </span>
          <div class="issue-info">
            <div class="issue-title">{issue.title}</div>
            <div class="issue-meta">
              {t('issues.meta', { number: issue.number, date: formatDate(issue.created_at), author: issue.author || t('common.unknown') })}
              {#if issue.labels?.length}
                {#each issue.labels as label}
                  <span class="label-badge">{label}</span>
                {/each}
              {/if}
            </div>
          </div>
        </a>
      {/each}
    </div>
  {/if}
</div>

<style>
  .issues-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }

  .filter-tabs {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
  }

  .filter-btn {
    color: var(--text-secondary);
  }
  .filter-btn.active {
    color: var(--text-primary);
    background: var(--bg-secondary);
    font-weight: 600;
  }
  .filter-btn:hover { background: var(--bg-hover); border-color: var(--text-muted); }

  .btn-primary {
    padding: 6px 16px;
    background: var(--accent);
    color: #fff;
    border: 1px solid var(--accent);
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }
  .btn-primary:hover { background: var(--accent-hover); }

  .btn-secondary {
    padding: 6px 16px;
    background: none;
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 14px;
    cursor: pointer;
  }

  .create-form {
    padding: 20px;
    margin-bottom: 24px;
  }

  .template-chooser { padding: 20px; margin-bottom: 24px; }
  .chooser-heading { display: flex; justify-content: space-between; gap: 16px; align-items: flex-start; margin-bottom: 14px; }
  .chooser-heading h2 { margin: 0 0 4px; font-size: 18px; }
  .chooser-heading p, .template-option p { margin: 0; color: var(--text-secondary); font-size: 13px; }
  .template-list { display: flex; flex-direction: column; border: 1px solid var(--border); border-radius: var(--radius); }
  .template-option { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 14px; border-bottom: 1px solid var(--border-light); }
  .template-option:last-child { border-bottom: 0; }
  .external-link { text-decoration: none; white-space: nowrap; }

  form { display: flex; flex-direction: column; gap: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; font-weight: 600; }
  .optional { font-weight: 400; color: var(--text-muted); }
  textarea { font-family: var(--font-mono); font-size: 13px; resize: vertical; }
  .form-actions { display: flex; gap: 8px; margin-top: 8px; }
.empty { text-align: center; padding: 48px; color: var(--text-secondary); }

  .issue-item {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 12px 16px;
    border-bottom: 1px solid var(--border-light);
    text-decoration: none;
    color: var(--text-primary);
  }
  .issue-item:last-child { border-bottom: none; }
  .issue-item:hover { background: var(--bg-secondary); text-decoration: none; }

  .issue-icon {
    font-size: 14px;
    margin-top: 3px;
    color: var(--green);
  }

  .issue-title { font-weight: 600; font-size: 15px; }

  .issue-meta {
    font-size: 12px;
    color: var(--text-muted);
    margin-top: 2px;
  }

  .label-badge {
    display: inline-block;
    padding: 0 6px;
    border: 1px solid var(--purple);
    color: var(--purple);
    border-radius: 10px;
    font-size: 11px;
    margin-left: 4px;
  }
</style>
