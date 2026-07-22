<script lang="ts">
  import { goto } from '$app/navigation';
  import { orgs } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';
  import { onMount } from 'svelte';

  const t = createT();

  type Organization = {
    id: number;
    name: string;
    display_name?: string | null;
    description?: string | null;
    visibility: string;
    created_at: string;
    updated_at?: string;
  };

  let organizations = $state<Organization[]>([]);
  let name = $state('');
  let displayName = $state('');
  let description = $state('');
  let visibility = $state('public');
  let error = $state('');
  let loading = $state(true);
  let creating = $state(false);
  let showCreate = $state(false);

  async function loadOrgs() {
    loading = true;
    error = '';
    try {
      organizations = await orgs.list();
    } catch (e: any) {
      error = e.message || t('errors.load_failed');
    } finally {
      loading = false;
    }
  }

  function resetForm() {
    name = '';
    displayName = '';
    description = '';
    visibility = 'public';
  }

  function toggleCreate() {
    showCreate = !showCreate;
    error = '';
  }

  async function handleCreate(e?: Event) {
    e?.preventDefault();
    if (!name.trim()) {
      error = t('errors.create_failed');
      return;
    }

    creating = true;
    error = '';
    try {
      const result = await orgs.create(name, displayName || undefined, description || undefined, visibility);
      resetForm();
      showCreate = false;
      await loadOrgs();
      goto(`/orgs/${result.name}`);
    } catch (e: any) {
      error = e.message || t('errors.create_failed');
    } finally {
      creating = false;
    }
  }

  onMount(loadOrgs);
</script>

<div class="container">
  <div class="page-header">
    <div>
      <h1>{t('orgs.title')}</h1>
      <p>{t('orgs.subtitle')}</p>
    </div>
    <button
      type="button"
      class="btn-primary header-action"
      onclick={toggleCreate}
      aria-expanded={showCreate}
    >
      {showCreate ? t('common.cancel') : t('orgs.create_action')}
    </button>
  </div>

  {#if error}
    <div class="error" role="alert">{error}</div>
  {/if}

  {#if showCreate}
    <form onsubmit={handleCreate} class="create-panel">
      <h2>{t('orgs.create_title')}</h2>

      <div class="field">
        <label for="name">{t('orgs.name')} *</label>
        <input id="name" type="text" bind:value={name} placeholder={t('orgs.name_placeholder')} required />
      </div>

      <div class="field">
        <label for="displayName">{t('orgs.display_name')}</label>
        <input id="displayName" type="text" bind:value={displayName} placeholder={t('orgs.display_name_placeholder')} />
      </div>

      <div class="field">
        <label for="description">{t('orgs.description')}</label>
        <textarea id="description" bind:value={description} placeholder={t('orgs.description_placeholder')} rows="3"></textarea>
      </div>

      <div class="field">
        <label for="visibility">{t('orgs.visibility')}</label>
        <select id="visibility" bind:value={visibility}>
          <option value="public">{t('orgs.visibility_public')}</option>
          <option value="private">{t('orgs.visibility_private')}</option>
        </select>
      </div>

      <div class="form-actions">
        <button type="button" class="btn-secondary" onclick={toggleCreate} disabled={creating}>
          {t('common.cancel')}
        </button>
        <button type="submit" class="btn-primary" disabled={creating}>
          {creating ? t('orgs.submitting') : t('orgs.submit')}
        </button>
      </div>
    </form>
  {/if}

  <section class="org-section">
    <div class="section-header">
      <h2>{t('orgs.list_title', { count: String(organizations.length) })}</h2>
    </div>

    {#if loading}
      <p class="loading">{t('common.loading')}</p>
    {:else if organizations.length === 0}
      <div class="empty-state">
        <h3>{t('orgs.no_orgs')}</h3>
        <p>{t('orgs.no_orgs_desc')}</p>
        <button type="button" class="btn-primary" onclick={() => showCreate = true}>
          {t('orgs.create_action')}
        </button>
      </div>
    {:else}
      <div class="org-list">
        {#each organizations as org}
          <a href={`/orgs/${org.name}`} class="org-card">
            <div class="org-avatar" aria-hidden="true">{org.name[0]?.toUpperCase() || '?'}</div>
            <div class="org-body">
              <div class="org-card-header">
                <h3>{org.display_name || org.name}</h3>
                <span class="visibility">{t(`orgs.visibility_${org.visibility}`)}</span>
              </div>
              <p class="org-name">@{org.name}</p>
              <p class="org-desc">{org.description || t('common.no_description')}</p>
              <p class="org-meta">{t('common.created', { date: formatDate(org.created_at) })}</p>
            </div>
          </a>
        {/each}
      </div>
    {/if}
  </section>
</div>

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 1rem;
    margin-bottom: 1.5rem;
  }

  h1 {
    color: var(--text-primary);
    margin: 0 0 0.4rem;
  }

  .page-header p {
    color: var(--text-secondary);
    margin: 0;
  }

  h2 {
    color: var(--text-primary);
    font-size: 1.1rem;
    margin: 0;
  }

  .header-action {
    white-space: nowrap;
  }

  .create-panel,
  .org-section {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 1.25rem;
  }

  .create-panel {
    display: flex;
    flex-direction: column;
    gap: 1rem;
    margin-bottom: 1.5rem;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
  }

  label {
    color: var(--text-secondary);
    font-size: 0.9rem;
    font-weight: 600;
  }

  input,
  textarea,
  select {
    background: var(--bg-primary);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    padding: 0.5rem 0.75rem;
    font-size: 0.95rem;
  }

  input:focus,
  textarea:focus,
  select:focus {
    outline: none;
    border-color: var(--accent);
  }

  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.75rem;
  }

  .btn-primary,
  .btn-secondary {
    border: none;
    border-radius: 6px;
    padding: 0.6rem 1.2rem;
    font-size: 0.95rem;
    cursor: pointer;
  }

  .btn-primary {
    background: var(--accent);
    color: white;
  }

  .btn-secondary {
    background: var(--bg-primary);
    color: var(--text-primary);
    border: 1px solid var(--border);
  }

  .btn-primary:disabled,
  .btn-secondary:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .error {
    color: #f85149;
    background: rgba(248, 81, 73, 0.1);
    border: 1px solid rgba(248, 81, 73, 0.35);
    padding: 0.65rem 0.85rem;
    border-radius: 6px;
    margin-bottom: 1rem;
  }

  .section-header {
    margin-bottom: 1rem;
  }

  .loading,
  .empty-state p,
  .org-desc,
  .org-meta,
  .org-name {
    color: var(--text-secondary);
  }

  .empty-state {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.75rem;
    padding: 1rem 0;
  }

  .empty-state h3 {
    color: var(--text-primary);
    margin: 0;
  }

  .empty-state p {
    margin: 0;
  }

  .org-list {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 0.75rem;
  }

  .org-card {
    display: flex;
    gap: 0.85rem;
    min-width: 0;
    padding: 1rem;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 8px;
    color: var(--text-primary);
    text-decoration: none;
  }

  .org-card:hover {
    border-color: var(--accent);
    text-decoration: none;
  }

  .org-avatar {
    flex: 0 0 42px;
    width: 42px;
    height: 42px;
    border-radius: 50%;
    background: var(--accent);
    color: white;
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 700;
  }

  .org-body {
    min-width: 0;
    flex: 1;
  }

  .org-card-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: 0.75rem;
  }

  .org-card h3 {
    margin: 0;
    font-size: 1rem;
    color: var(--text-primary);
    overflow-wrap: anywhere;
  }

  .visibility {
    flex: 0 0 auto;
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0.1rem 0.5rem;
    color: var(--text-secondary);
    font-size: 0.75rem;
  }

  .org-name,
  .org-desc,
  .org-meta {
    margin: 0.3rem 0 0;
    font-size: 0.85rem;
    overflow-wrap: anywhere;
  }

  @media (max-width: 700px) {
    .page-header {
      flex-direction: column;
    }

    .header-action,
    .btn-primary,
    .btn-secondary {
      width: 100%;
    }

    .form-actions {
      flex-direction: column-reverse;
    }
  }
</style>
