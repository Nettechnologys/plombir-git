<script lang="ts">
  import { isLoggedIn, getUser } from '$lib/stores/auth.svelte';
  import { repos } from '$lib/api/client.svelte';
  import { goto } from '$app/navigation';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived(getUser()?.username || '');
  let repoList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let showCreate = $state(false);

  // Create form state
  let newName = $state('');
  let newDesc = $state('');
  let newPrivate = $state(false);
  let autoInit = $state(true);
  let defaultBranch = $state('main');
  let selectedGitignore = $state('');
  let selectedLicense = $state('');
  let selectedReadme = $state('default');
  let selectedLabels = $state('default');

  // Template options (loaded from API)
  let gitignoreOptions = $state<{ key: string; name: string; description: string }[]>([]);
  let licenseOptions = $state<{ key: string; name: string; description: string }[]>([]);
  let readmeOptions = $state<{ key: string; name: string; description: string }[]>([]);
  let labelSetOptions = $state<{ key: string; name: string; description: string }[]>([]);

  $effect(() => {
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    loadRepos();
    loadTemplates();
  });

  async function loadRepos() {
    if (!owner) return;
    try {
      loading = true;
      const result = await repos.list(owner);
      repoList = result.data;
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function loadTemplates() {
    try {
      const [gi, li, re, lb] = await Promise.all([
        repos.templates.gitignores(),
        repos.templates.licenses(),
        repos.templates.readmes(),
        repos.templates.labels(),
      ]);
      gitignoreOptions = gi.data;
      licenseOptions = li.data;
      readmeOptions = re.data;
      labelSetOptions = lb.data;
    } catch (_) {
      // Templates are optional — proceed without them
    }
  }

  async function handleCreate(e: Event) {
    e.preventDefault();
    try {
      await repos.create({
        name: newName,
        description: newDesc || undefined,
        is_private: newPrivate,
        auto_init: autoInit,
        default_branch: defaultBranch || undefined,
        gitignores: selectedGitignore || undefined,
        license: selectedLicense || undefined,
        readme: autoInit && !selectedGitignore && !selectedLicense ? selectedReadme : autoInit ? selectedReadme : undefined,
        issue_labels: autoInit ? selectedLabels : undefined,
      });
      const createdName = newName;
      showCreate = false;
      resetForm();
      await goto(`/${owner}/${createdName}`);
    } catch (e: any) {
      error = e.message;
    }
  }

  function resetForm() {
    newName = '';
    newDesc = '';
    newPrivate = false;
    autoInit = true;
    defaultBranch = 'main';
    selectedGitignore = '';
    selectedLicense = '';
    selectedReadme = 'default';
    selectedLabels = 'default';
  }

  function cancelCreate() {
    showCreate = false;
    resetForm();
  }
</script>

<svelte:head>
  <title>{t('dashboard.title')} · IronForge</title>
</svelte:head>

<div class="dashboard">
  <div class="dashboard-header">
    <h1>{t('dashboard.title')}</h1>
    <button class="btn-primary" onclick={() => showCreate = !showCreate}>
      + {t('dashboard.new_repo')}
    </button>
  </div>

  {#if showCreate}
    <div class="create-form">
      <h2>{t('dashboard.create_form.title')}</h2>
      <form onsubmit={handleCreate}>
        <!-- Repository name -->
        <label>
          {t('dashboard.create_form.name')} <span class="required">*</span>
          <input type="text" bind:value={newName} required placeholder={t('dashboard.create_form.name_placeholder')} />
        </label>

        <!-- Description -->
        <label>
          {t('dashboard.create_form.desc')} <span class="optional">{t('common.optional')}</span>
          <input type="text" bind:value={newDesc} placeholder={t('common.no_description')} />
        </label>

        <!-- Visibility -->
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={newPrivate} />
          <span>
            <strong>{t('dashboard.create_form.private')}</strong>
            <span class="hint">{t('dashboard.create_form.private_hint')}</span>
          </span>
        </label>

        <hr class="divider" />

        <!-- Auto-initialize -->
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={autoInit} />
          <span>
            <strong>{t('dashboard.create_form.auto_init')}</strong>
            <span class="hint">{t('dashboard.create_form.auto_init_hint')}</span>
          </span>
        </label>

        {#if autoInit}
          <div class="template-section">
            <!-- Default branch -->
            <label>
              {t('dashboard.create_form.default_branch')}
              <input type="text" bind:value={defaultBranch} placeholder="main" />
            </label>

            <!-- .gitignore template -->
            <label>
              {t('dashboard.create_form.gitignore_template')}
              <select bind:value={selectedGitignore}>
                <option value="">{t('dashboard.create_form.none')}</option>
                {#each gitignoreOptions as opt}
                  <option value={opt.key}>{opt.name}</option>
                {/each}
              </select>
            </label>

            <!-- LICENSE template -->
            <label>
              {t('dashboard.create_form.license_template')}
              <select bind:value={selectedLicense}>
                <option value="">{t('dashboard.create_form.none')}</option>
                {#each licenseOptions as opt}
                  <option value={opt.key}>{opt.name}</option>
                {/each}
              </select>
            </label>

            <!-- README template -->
            <label>
              {t('dashboard.create_form.readme_template')}
              <select bind:value={selectedReadme}>
                {#each readmeOptions as opt}
                  <option value={opt.key}>{opt.name}</option>
                {/each}
              </select>
            </label>

            <!-- Default issue labels -->
            <label>
              {t('dashboard.create_form.label_set')}
              <select bind:value={selectedLabels}>
                {#each labelSetOptions as opt}
                  <option value={opt.key}>{opt.name}</option>
                {/each}
              </select>
            </label>
          </div>
        {/if}

        <div class="form-actions">
          <button type="submit" class="btn-primary">{t('dashboard.create_form.submit')}</button>
          <button type="button" class="btn-secondary" onclick={cancelCreate}>{t('dashboard.create_form.cancel')}</button>
        </div>
      </form>
    </div>
  {/if}

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if repoList.length === 0}
    <div class="empty">
      <p>{t('dashboard.empty.no_repos')}</p>
      <p class="text-secondary">{t('dashboard.empty.get_started')}</p>
    </div>
  {:else}
    <div class="repo-list">
      {#each repoList as repo}
        <a href={`/${owner}/${repo.name}`} class="repo-item">
          <div class="repo-icon">
            {repo.is_private ? '🔒' : '📂'}
          </div>
          <div class="repo-info">
            <div class="repo-name">
              {owner}/{repo.name}
              {#if repo.is_private}
                <span class="badge-private">{t('dashboard.repo.private')}</span>
              {/if}
            </div>
            <div class="repo-desc">{repo.description || t('common.no_description')}</div>
            <div class="repo-meta">{t('common.created', { date: formatDate(repo.created_at) })}</div>
          </div>
        </a>
      {/each}
    </div>
  {/if}
</div>

<style>

  .dashboard-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 24px;
  }

  h1 { font-size: 24px; }

  .btn-primary {
    padding: 6px 16px;
    background: var(--green-dim);
    color: #fff;
    border: none;
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }
  .btn-primary:hover { background: var(--green); }

  .btn-secondary {
    padding: 6px 16px;
    background: none;
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 14px;
    cursor: pointer;
  }
  .btn-secondary:hover { background: var(--bg-hover); }

  .create-form {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 24px;
    margin-bottom: 24px;
  }

  h2 { font-size: 18px; margin-bottom: 16px; }

  form {
    display: flex;
    flex-direction: column;
    gap: 14px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 13px;
    font-weight: 600;
  }

  .required { color: var(--red); font-weight: 400; }
  .optional { font-weight: 400; color: var(--text-muted); }

  .checkbox-label {
    flex-direction: row;
    align-items: flex-start;
    gap: 8px;
  }
  .checkbox-label input { width: auto; margin-top: 2px; }

  .hint {
    display: block;
    font-weight: 400;
    font-size: 12px;
    color: var(--text-muted);
    margin-top: 2px;
  }

  .divider {
    border: none;
    border-top: 1px solid var(--border);
    margin: 4px 0;
  }

  .template-section {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding-left: 24px;
    border-left: 2px solid var(--border);
  }

  select {
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 14px;
    background: var(--bg-primary);
    color: var(--text-primary);
  }

  .form-actions {
    display: flex;
    gap: 8px;
    margin-top: 8px;
  }
.empty {
    text-align: center;
    padding: 60px 24px;
    color: var(--text-secondary);
  }

  .repo-list {
    display: flex;
    flex-direction: column;
    gap: 0;
  }

  .repo-item {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 16px 20px;
    border-bottom: 1px solid var(--border-light);
    text-decoration: none;
    color: var(--text-primary);
  }
  .repo-item:hover { background: var(--bg-secondary); text-decoration: none; }
  .repo-item:first-child { border-top: 1px solid var(--border-light); }

  .repo-icon { font-size: 20px; margin-top: 2px; }

  .repo-info { flex: 1; }

  .repo-name {
    font-weight: 600;
    font-size: 15px;
    color: var(--accent);
  }

  .badge-private {
    font-size: 11px;
    font-weight: 500;
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-radius: 10px;
    color: var(--text-secondary);
    margin-left: 8px;
    vertical-align: middle;
  }

  .repo-desc {
    font-size: 13px;
    color: var(--text-secondary);
    margin-top: 2px;
  }

  .repo-meta {
    font-size: 12px;
    color: var(--text-muted);
    margin-top: 4px;
  }
</style>
