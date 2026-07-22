<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import { repos } from '$lib/api/client.svelte';
  import { getUser } from '$lib/stores/auth.svelte';
  import { createT } from '$lib/i18n';

  interface Repository {
    id: number;
    name: string;
    description: string | null;
    is_private: boolean;
    default_branch: string;
    created_at: string;
  }

  const t = createT();

  let { data } = $props();

  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
  
  let repository = $state<Repository | null>(null);
  let loading = $state(true);
  let error = $state('');
  
  // Transfer state
  let newOwner = $state('');
  let transferring = $state(false);
  let transferError = $state('');
  let transferSuccess = $state('');
  
  // Delete state
  let deleteConfirm = $state('');
  let deleting = $state(false);
  let deleteError = $state('');
  let repositoryPath = $derived(`${owner}/${repo}`);
  
  $effect(() => {
    loadRepository();
  });
  
  async function loadRepository() {
    try {
      loading = true;
      // Assuming repos.get returns repository info
      const response = await repos.get(owner, repo);
      repository = response;
    } catch (err: any) {
      error = err.message || 'Failed to load repository';
    } finally {
      loading = false;
    }
  }
  
  async   function handleTransfer() {
    if (!newOwner.trim()) return;

    const confirmed = confirm(t('settings.transfer.warning'));
    if (!confirmed) return;
    
    try {
      transferring = true;
      transferError = '';
      transferSuccess = '';
      
      await repos.transfer(owner, repo, newOwner.trim());
      transferSuccess = t('settings.transfer.success');
      // Redirect to new repo URL
      setTimeout(() => {
        goto(`/${newOwner.trim()}/${repo}`);
      }, 1500);
    } catch (err: any) {
      transferError = err.message || 'Transfer failed';
    } finally {
      transferring = false;
    }
  }
  
  async function handleDelete() {
    if (deleteConfirm !== repositoryPath) return;
    
    const confirmed = confirm(t('settings.delete.desc'));
    if (!confirmed) return;
    
    try {
      deleting = true;
      deleteError = '';
      
      await repos.delete(owner, repo);
      
      // Redirect to dashboard
      goto('/dashboard');
    } catch (err: any) {
      deleteError = err.message || 'Delete failed';
      deleting = false;
    }
}
</script>

<svelte:head>
  <title>{t('settings.general')} · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="settings-page">
  <h1>{t('settings.general')}</h1>
  
  {#if loading}
    <div class="loading">{t('common.loading')}</div>
  {:else if error}
    <div class="error">{error}</div>
  {:else if repository}
    <!-- Repository Info -->
    <section class="section">
      <h2>{t('settings.repository_info.title', 'Repository Information')}</h2>
      <div class="info-grid">
        <div class="info-item">
          <span class="info-label">{t('settings.repository_info.name', 'Name')}</span>
          <div class="info-value">{repository.name}</div>
        </div>
        <div class="info-item">
          <span class="info-label">{t('settings.repository_info.description', 'Description')}</span>
          <div class="info-value">{repository.description || '-'}</div>
        </div>
        <div class="info-item">
          <span class="info-label">{t('settings.repository_info.visibility', 'Visibility')}</span>
          <div class="info-value">
            <span class="badge" class:private={repository.is_private}>
              {repository.is_private ? t('settings.repository_info.private', 'Private') : t('settings.repository_info.public', 'Public')}
            </span>
          </div>
        </div>
      </div>
    </section>
    
    <!-- Transfer Ownership -->
    <section class="section transfer-section">
      <h2>{t('settings.transfer.title')}</h2>
      <p class="section-desc">{t('settings.transfer.desc')}</p>
      
      <div class="warning-box">
        <span class="warning-icon">⚠️</span>
        <p>{t('settings.transfer.warning')}</p>
      </div>
      
      {#if transferError}
        <div class="error-box">{transferError}</div>
      {/if}
      
      {#if transferSuccess}
        <div class="success-box">{transferSuccess}</div>
      {/if}
      
      <div class="form-group">
        <label for="new-owner">{t('settings.transfer.new_owner')}</label>
        <div class="input-row">
          <input 
            id="new-owner"
            type="text" 
            bind:value={newOwner}
            placeholder={t('settings.transfer.new_owner_placeholder')}
            disabled={transferring}
          />
          <button 
            class="btn btn-warning"
            onclick={handleTransfer}
            disabled={!newOwner.trim() || transferring}
          >
            {transferring ? t('settings.transfer.confirming') : t('settings.transfer.confirm')}
          </button>
        </div>
      </div>
    </section>
    
    <!-- Danger Zone -->
    <section class="section danger-zone">
      <h2>{t('settings.danger_zone')}</h2>
      
      <div class="danger-box">
        <h3>{t('settings.delete.title')}</h3>
        <p>{t('settings.delete.desc')}</p>
        
        {#if deleteError}
          <div class="error-box">{deleteError}</div>
        {/if}
        
        <div class="form-group">
          <label for="delete-confirm">{@html t('settings.delete.confirm_instruction', { repo: repositoryPath })}</label>
          <input 
            id="delete-confirm"
            type="text" 
            bind:value={deleteConfirm}
            placeholder={t('settings.delete.confirm_placeholder')}
            disabled={deleting}
          />
        </div>
        
        <button 
          class="btn btn-danger"
          onclick={handleDelete}
          disabled={deleteConfirm !== repositoryPath || deleting}
        >
          {deleting ? t('settings.delete.confirming') : t('settings.delete.confirm_button')}
        </button>
      </div>
    </section>
  {/if}
</div>

<style>
  .settings-page {
    max-width: 800px;
  }
  
  h1 {
    font-size: 1.75rem;
    margin-bottom: 2rem;
    color: var(--text-primary);
  }
  
  h2 {
    font-size: 1.25rem;
    margin-bottom: 1rem;
    color: var(--text-primary);
  }
  
  .section {
    margin-bottom: 2.5rem;
    padding-bottom: 2rem;
    border-bottom: 1px solid var(--border);
  }
  
  .section-desc {
    color: var(--text-secondary);
    margin-bottom: 1.5rem;
    font-size: 0.9rem;
  }
  
.info-grid {
    display: flex;
    flex-direction: column;
    gap: 1.25rem;
  }
  
  .info-item {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .info-label {
    font-size: 0.85rem;
    color: var(--text-secondary);
    font-weight: 500;
  }
  
  .info-value {
    color: var(--text-primary);
    font-size: 0.95rem;
  }
  
  .badge {
    display: inline-block;
    padding: 0.25rem 0.75rem;
    border-radius: 12px;
    font-size: 0.8rem;
    font-weight: 600;
    background: var(--green, #28a745);
    color: white;
  }
  
  .badge.private {
    background: var(--orange, #ff8800);
  }
  
  .warning-box {
    display: flex;
    gap: 0.75rem;
    padding: 1rem;
    background: rgba(255, 165, 0, 0.1);
    border: 1px solid var(--orange, #ff8800);
    border-radius: 6px;
    margin-bottom: 1.5rem;
  }
  
  .warning-icon {
    font-size: 1.25rem;
    flex-shrink: 0;
  }
  
  .warning-box p {
    color: var(--text-primary);
    font-size: 0.9rem;
    margin: 0;
  }
  
  .form-group {
    margin-top: 1.5rem;
  }
  
  .form-group label {
    display: block;
    margin-bottom: 0.5rem;
    color: var(--text-primary);
    font-weight: 500;
    font-size: 0.9rem;
  }
  
  .input-row {
    display: flex;
    gap: 0.75rem;
  }
  
  input[type="text"] {
    flex: 1;
    padding: 0.6rem 0.75rem;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 0.9rem;
  }
  
  input[type="text"]:focus {
    outline: none;
    border-color: var(--accent);
  }
  
  .btn {
    padding: 0.6rem 1.25rem;
    border: none;
    border-radius: 6px;
    font-size: 0.9rem;
    font-weight: 500;
    cursor: pointer;
    transition: all 0.2s;
  }
  
  .btn:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }
  
  .btn-warning {
    background: var(--orange, #ff8800);
    color: white;
  }
  
  .btn-warning:hover:not(:disabled) {
    background: var(--orange-dark, #cc6600);
  }
  
  .btn-danger {
    background: var(--red, #ff4444);
    color: white;
  }
  
  .btn-danger:hover:not(:disabled) {
    background: var(--red-dark, #cc0000);
  }
  
  .danger-zone {
    border-bottom: none;
  }
  
  .danger-box {
    border: 1px solid var(--red, #ff4444);
    background: rgba(255, 0, 0, 0.05);
    border-radius: 6px;
    padding: 1.5rem;
  }
  
  .danger-box h3 {
    color: var(--red, #ff4444);
    margin-bottom: 0.5rem;
    font-size: 1.1rem;
  }
  
  .danger-box p {
    color: var(--text-secondary);
    font-size: 0.9rem;
    margin-bottom: 1rem;
  }
  
  .error-box {
    padding: 0.75rem;
    background: rgba(255, 0, 0, 0.1);
    border: 1px solid var(--red, #ff4444);
    border-radius: 6px;
    color: var(--red, #ff4444);
    font-size: 0.9rem;
    margin-bottom: 1rem;
  }
  
  .success-box {
    padding: 0.75rem;
    background: rgba(0, 255, 0, 0.1);
    border: 1px solid var(--green, #28a745);
    border-radius: 6px;
    color: var(--green, #28a745);
    font-size: 0.9rem;
    margin-bottom: 1rem;
  }
  
  .loading, .error {
    padding: 2rem;
    text-align: center;
    color: var(--text-secondary);
  }
  
  .error {
    color: var(--red, #ff4444);
  }
</style>
