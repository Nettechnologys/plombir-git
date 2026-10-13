<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import BotBadge from '$lib/components/BotBadge.svelte';
  import { issues, labels } from '$lib/api/client.svelte';
  import {
    LatestRepositoryRequestFence,
    LatestRepositoryResourceRequestFence,
  } from '$lib/asyncStateOwnership';
  import { createT, formatDate, formatTranslationFallback } from '$lib/i18n';
  import { safeHexColor } from '$lib/utils/color';

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
  let labelOptions = $state<Array<{ id: number; name: string; color: string }>>([]);
  let labelsLoading = $state(true);
  let labelsError = $state('');
  let newTitle = $state('');
  let newBody = $state('');
  let newLabels = $state<string[]>([]);
  let creating = $state(false);
  const issueListRequests = new LatestRepositoryResourceRequestFence<string>();
  const labelListRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;

  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    routeGeneration += 1;
    filterState = 'open';
    issueList = [];
    showCreate = false;
    showChooser = false;
    templatesLoaded = false;
    issueTemplates = [];
    templateConfig = { blank_issues_enabled: true, contact_links: [] };
    labelOptions = [];
    labelsLoading = true;
    labelsError = '';
    newTitle = '';
    newBody = '';
    newLabels = [];
    creating = false;
    error = '';
    void loadIssues(expectedOwner, expectedRepo, 'open', routeGeneration);
    void loadLabelOptions(expectedOwner, expectedRepo);
  });

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number) {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  async function loadLabelOptions(expectedOwner: string, expectedRepo: string) {
    const claim = labelListRequests.begin(expectedOwner, expectedRepo);
    try {
      labelsLoading = true;
      labelsError = '';
      const nextLabels = await labels.list(expectedOwner, expectedRepo);
      if (labelListRequests.owns(claim, owner, repo)) {
        labelOptions = nextLabels;
      }
    } catch (e: any) {
      if (labelListRequests.owns(claim, owner, repo)) {
        labelOptions = [];
        labelsError = e.message;
      }
    } finally {
      if (labelListRequests.owns(claim, owner, repo)) labelsLoading = false;
    }
  }

  function toggleLabel(name: string, selected: boolean) {
    if (selected) {
      if (!newLabels.includes(name)) newLabels = [...newLabels, name];
      return;
    }
    newLabels = newLabels.filter((label) => label !== name);
  }

  function selectFilter(nextFilter: string) {
    if (filterState === nextFilter) return;
    filterState = nextFilter;
    void loadIssues(owner, repo, nextFilter, routeGeneration);
  }

  async function loadIssues(
    expectedOwner = owner,
    expectedRepo = repo,
    expectedFilter = filterState,
    expectedRoute = routeGeneration,
  ) {
    if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
    const claim = issueListRequests.begin(expectedOwner, expectedRepo, expectedFilter);
    try {
      loading = true;
      error = '';
      const nextIssues = (await issues.list(expectedOwner, expectedRepo, expectedFilter)).data;
      if (
        issueListRequests.owns(claim, owner, repo, filterState) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        issueList = nextIssues;
      }
    } catch (e: any) {
      if (
        issueListRequests.owns(claim, owner, repo, filterState) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        error = e.message;
      }
    } finally {
      if (
        issueListRequests.owns(claim, owner, repo, filterState) &&
        isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)
      ) {
        loading = false;
      }
    }
  }

  async function handleCreate(e: Event) {
    e.preventDefault();
    if (creating) return;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    issueListRequests.begin(expectedOwner, expectedRepo, filterState);
    try {
      creating = true;
      error = '';
      const selectedLabels = newLabels.length > 0 ? [...newLabels] : undefined;
      await issues.create(expectedOwner, expectedRepo, newTitle, newBody || undefined, selectedLabels);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      showCreate = false;
      newTitle = '';
      newBody = '';
      newLabels = [];
      await loadIssues(expectedOwner, expectedRepo, filterState, expectedRoute);
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) creating = false;
    }
  }

  async function openCreate() {
    if (showCreate || showChooser) {
      showCreate = false;
      showChooser = false;
      return;
    }
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    try {
      if (!templatesLoaded) {
        const [nextTemplates, nextConfig] = await Promise.all([
          issues.templates(expectedOwner, expectedRepo),
          issues.templateConfig(expectedOwner, expectedRepo),
        ]);
        if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
        issueTemplates = nextTemplates;
        templateConfig = nextConfig;
        templatesLoaded = true;
      }
      if (issueTemplates.length > 0 || templateConfig.contact_links.length > 0) {
        showChooser = true;
      } else {
        showCreate = true;
      }
    } catch (e: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) error = e.message;
    }
  }

  function chooseTemplate(template?: any) {
    newTitle = template?.title || '';
    newBody = template?.content || '';
    newLabels = Array.isArray(template?.labels) ? [...template.labels] : [];
    showChooser = false;
    showCreate = true;
  }

  function emptyStateLabel(): string {
    if (filterState === 'all') return t('common.all');
    return t(`issues.state_label.${filterState}`, undefined, formatTranslationFallback(filterState));
  }
</script>

<svelte:head>
  <title>{t('issues.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="issues" starsCount={0} />

  <div class="gh-toolbar issues-toolbar">
    <div class="filter-tabs">
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'open'}
        onclick={() => selectFilter('open')}
      >
        {t('issues.tabs.open')}
      </button>
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'closed'}
        onclick={() => selectFilter('closed')}
      >
        {t('issues.tabs.closed')}
      </button>
      <button
        class="filter-btn btn btn-outline btn-sm"
        class:active={filterState === 'all'}
        onclick={() => selectFilter('all')}
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
          <input type="text" bind:value={newTitle} required placeholder={t('issues.create_form.title_placeholder')} disabled={creating} />
        </label>
        <label>
          {t('issues.create_form.body')} <span class="optional">{t('issues.create_form.body_hint')}</span>
          <textarea bind:value={newBody} rows="6" placeholder={t('issues.create_form.body_placeholder')} disabled={creating}></textarea>
        </label>
        <fieldset class="label-picker" disabled={creating || labelsLoading}>
          <legend>
            {t('issues.create_form.labels')} <span class="optional">{t('issues.create_form.labels_hint')}</span>
          </legend>
          {#if labelsLoading}
            <p class="label-picker-note">{t('issues.create_form.labels_loading')}</p>
          {:else if labelsError}
            <p class="label-picker-note label-picker-error" role="status">
              {t('issues.create_form.labels_unavailable')}
            </p>
          {:else if labelOptions.length === 0}
            <p class="label-picker-note">{t('issues.create_form.labels_empty')}</p>
          {:else}
            <div class="label-options">
              {#each labelOptions as label (label.id)}
                <label class="label-option">
                  <input
                    type="checkbox"
                    value={label.name}
                    checked={newLabels.includes(label.name)}
                    onchange={(event) => toggleLabel(label.name, event.currentTarget.checked)}
                  />
                  <span class="label-swatch" style={`background-color: ${safeHexColor(label.color, '#888888')}`}></span>
                  <span>{label.name}</span>
                </label>
              {/each}
            </div>
          {/if}
        </fieldset>
        <div class="form-actions">
          <button type="submit" class="btn-primary" disabled={creating}>{t('issues.create_form.submit')}</button>
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
              {t('issues.meta', { number: issue.number, date: formatDate(issue.created_at), author: issue.author || t('common.unknown') })}<BotBadge owner={issue.author_bot_owner} link={false} />
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
  .label-picker { margin: 0; padding: 0; border: 0; }
  .label-picker legend { padding: 0; font-size: 13px; font-weight: 600; }
  .label-picker-note { margin: 6px 0 0; color: var(--text-muted); font-size: 13px; }
  .label-picker-error { color: var(--red); }
  .label-options { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 8px; }
  .label-option {
    flex-direction: row;
    align-items: center;
    gap: 6px;
    padding: 6px 9px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    cursor: pointer;
  }
  .label-option:has(input:checked) { background: var(--bg-secondary); border-color: var(--accent); }
  .label-option input { margin: 0; }
  .label-swatch { width: 10px; height: 10px; border-radius: 50%; }
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
