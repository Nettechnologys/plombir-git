<script lang="ts">
  import { page } from '$app/stores';
  import { labels } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  interface Label {
    id: number;
    name: string;
    color: string;
    description?: string;
  }

  let { data } = $props();
  const t = createT();
  
  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
  
  let labelList = $state<Label[]>([]);
  let loading = $state(true);
  let error = $state('');
  let success = $state('');
  
  // Form state
  let showForm = $state(false);
  let editingLabel = $state<Label | null>(null);
  let formData = $state({
    name: '',
    color: '#ff0000',
    description: ''
  });
  let saving = $state(false);
  let formError = $state('');
  
  // Delete state
  let deletingLabel = $state<Label | null>(null);
  let deleting = $state(false);

  function closeCreateModalByKey(e: KeyboardEvent) {
    if (e.key === 'Escape' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      closeForm();
    }
  }

  function closeDeleteModalByKey(e: KeyboardEvent) {
    if (e.key === 'Escape' || e.key === 'Enter' || e.key === ' ') {
      e.preventDefault();
      cancelDelete();
    }
  }
  
  const presetColors = [
    '#ff0000', '#00ff00', '#0000ff', '#ffff00',
    '#ff00ff', '#00ffff', '#ff8800', '#888888'
  ];
  
  $effect(() => {
    loadLabels();
  });
  
  async function loadLabels() {
    try {
      loading = true;
      error = '';
      const result = await labels.list(owner!, repo!);
      labelList = result;
    } catch (err: any) {
      error = err.message || 'Failed to load labels';
    } finally {
      loading = false;
    }
  }
  
  function openCreateForm() {
    editingLabel = null;
    formData = { name: '', color: '#ff0000', description: '' };
    formError = '';
    showForm = true;
  }
  
  function openEditForm(label: Label) {
    editingLabel = label;
    formData = {
      name: label.name,
      color: label.color,
      description: label.description || ''
    };
    formError = '';
    showForm = true;
  }
  
  function closeForm() {
    showForm = false;
    editingLabel = null;
    formData = { name: '', color: '#ff0000', description: '' };
    formError = '';
  }
  
  async function handleSave() {
    if (!formData.name.trim()) {
      formError = 'Label name is required';
      return;
    }
    
    try {
      saving = true;
      formError = '';
      
      if (editingLabel) {
        await labels.update(owner!, repo!, editingLabel.id, {
          name: formData.name.trim(),
          color: formData.color,
          description: formData.description.trim() || undefined
        });
        success = t('settings.save_label');
      } else {
        await labels.create(
          owner!, 
          repo!, 
          formData.name.trim(), 
          formData.color, 
          formData.description.trim() || undefined
        );
        success = t('settings.create_label');
      }
      
      closeForm();
      await loadLabels();
      
      setTimeout(() => { success = ''; }, 3000);
    } catch (err: any) {
      formError = err.message || 'Failed to save label';
    } finally {
      saving = false;
    }
  }
  
  function confirmDelete(label: Label) {
    deletingLabel = label;
  }
  
  function cancelDelete() {
    deletingLabel = null;
  }
  
  async function handleDelete() {
    if (!deletingLabel) return;
    
    try {
      deleting = true;
      await labels.delete(owner!, repo!, deletingLabel.id);
      success = t('settings.delete_label');
      deletingLabel = null;
      await loadLabels();
      
      setTimeout(() => { success = ''; }, 3000);
    } catch (err: any) {
      error = err.message || 'Failed to delete label';
    } finally {
      deleting = false;
    }
  }
  
</script>

<div class="labels-page">
  <div class="page-header">
    <h1>{t('settings.labels')}</h1>
    <button class="btn btn-primary" onclick={openCreateForm}>
      + {t('settings.new_label')}
    </button>
  </div>
  
  {#if success}
    <div class="success-box">{success}</div>
  {/if}
  
  {#if error}
    <div class="error-box">{error}</div>
  {/if}
  
  <!-- Create/Edit Form -->
  {#if showForm}
    <div
      class="form-overlay"
      onclick={closeForm}
      role="button"
      tabindex="0"
      onkeydown={closeCreateModalByKey}
    >
      <div class="form-modal" role="dialog" aria-modal="true" tabindex="-1">
        <h2>{editingLabel ? t('settings.edit_label') : t('settings.new_label')}</h2>
        
        {#if formError}
          <div class="error-box">{formError}</div>
        {/if}

        <div class="form-group">
          <label for="label-name">{t('settings.label_name')}</label>
          <input 
            id="label-name"
            type="text" 
            bind:value={formData.name}
            placeholder={t('settings.label_name_placeholder')}
            disabled={saving}
          />
        </div>

        <div class="form-group">
          <label for="label-color-input">{t('settings.label_color')}</label>
          
          <div class="preset-colors">
            <span class="color-section-label">{t('settings.preset_colors')}</span>
            <div class="color-swatches">
              {#each presetColors as color}
                <button 
                  class="color-swatch"
                  class:active={formData.color === color}
                  style="background-color: {color}"
                  onclick={() => formData.color = color}
                  disabled={saving}
                  aria-label="Color {color}"
                ></button>
              {/each}
            </div>
          </div>
          
          <div class="custom-color">
            <span class="color-section-label">{t('settings.custom_color')}</span>
            <div class="custom-color-input">
              <div class="color-preview" style="background-color: {formData.color}"></div>
              <input 
                id="label-color-input"
                type="text" 
                bind:value={formData.color}
                placeholder="#000000"
                disabled={saving}
                maxlength="7"
              />
            </div>
          </div>
        </div>
        
        <div class="form-group">
          <label for="label-desc">{t('settings.label_desc')}</label>
          <input 
            id="label-desc"
            type="text" 
            bind:value={formData.description}
            placeholder={t('settings.label_desc_placeholder')}
            disabled={saving}
          />
        </div>
        
        <div class="form-actions">
          <button class="btn btn-outline" onclick={closeForm} disabled={saving}>
            Cancel
          </button>
          <button class="btn btn-primary" onclick={handleSave} disabled={saving}>
            {saving ? 'Saving...' : (editingLabel ? t('settings.save_label') : t('settings.create_label'))}
          </button>
        </div>
      </div>
    </div>
  {/if}
  
  <!-- Delete Confirmation -->
  {#if deletingLabel}
    <div
      class="form-overlay"
      onclick={cancelDelete}
      role="button"
      tabindex="0"
      onkeydown={closeDeleteModalByKey}
    >
      <div class="form-modal" role="dialog" aria-modal="true" tabindex="-1">
        <h2>Confirm Delete</h2>
        <p>{t('settings.confirm_delete_label')}</p>
        <p><strong>{deletingLabel.name}</strong></p>
        
        <div class="form-actions">
          <button class="btn btn-outline" onclick={cancelDelete} disabled={deleting}>
            Cancel
          </button>
          <button class="btn btn-danger" onclick={handleDelete} disabled={deleting}>
            {deleting ? 'Deleting...' : 'Delete'}
          </button>
        </div>
      </div>
    </div>
  {/if}
  
  <!-- Labels List -->
  {#if loading}
    <div class="loading">Loading...</div>
  {:else if labelList.length === 0}
    <div class="empty-state">
      <p>{t('settings.no_labels')}</p>
    </div>
  {:else}
    <div class="labels-grid">
      {#each labelList as label (label.id)}
        <div class="label-card">
          <div class="label-info">
            <div class="label-color" style="background-color: {label.color}"></div>
            <div class="label-text">
              <span class="label-name">{label.name}</span>
              {#if label.description}
                <span class="label-desc">{label.description}</span>
              {/if}
            </div>
          </div>
          <div class="label-actions">
            <button class="btn-icon" onclick={() => openEditForm(label)} title="Edit">
              ✏️
            </button>
            <button class="btn-icon" onclick={() => confirmDelete(label)} title="Delete">
              🗑️
            </button>
          </div>
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .labels-page {
    max-width: 800px;
  }
  
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 2rem;
  }
  
  h1 {
    font-size: 1.75rem;
    color: var(--text-primary);
    margin: 0;
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
  
  .btn-primary {
    background: var(--orange, #ff8800);
    color: white;
  }
  
  .btn-primary:hover:not(:disabled) {
    background: var(--orange-dark, #cc6600);
  }
  
  .btn-outline {
    background: transparent;
    border: 1px solid var(--border);
    color: var(--text-primary);
  }
  
  .btn-outline:hover:not(:disabled) {
    background: var(--bg-secondary);
  }
  
  .btn-danger {
    background: var(--red, #ff4444);
    color: white;
  }
  
  .btn-danger:hover:not(:disabled) {
    background: var(--red-dark, #cc0000);
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
  
  .error-box {
    padding: 0.75rem;
    background: rgba(255, 0, 0, 0.1);
    border: 1px solid var(--red, #ff4444);
    border-radius: 6px;
    color: var(--red, #ff4444);
    font-size: 0.9rem;
    margin-bottom: 1rem;
  }
  
  /* Form Overlay */
  .form-overlay {
    position: fixed;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    background: rgba(0, 0, 0, 0.7);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  
  .form-modal {
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 8px;
    padding: 2rem;
    max-width: 500px;
    width: 90%;
    max-height: 90vh;
    overflow-y: auto;
  }
  
  .form-modal h2 {
    margin: 0 0 1.5rem 0;
    color: var(--text-primary);
    font-size: 1.25rem;
  }
  
  .form-modal p {
    color: var(--text-secondary);
    margin-bottom: 1rem;
  }
  
  .form-group {
    margin-bottom: 1.25rem;
  }
  
  .form-group label {
    display: block;
    margin-bottom: 0.5rem;
    color: var(--text-primary);
    font-weight: 500;
    font-size: 0.9rem;
  }
  
  .form-group input[type="text"] {
    width: 100%;
    padding: 0.6rem 0.75rem;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 0.9rem;
    box-sizing: border-box;
  }
  
  .form-group input[type="text"]:focus {
    outline: none;
    border-color: var(--accent);
  }
  
  .preset-colors {
    margin-bottom: 1rem;
  }
  
  .color-section-label {
    display: block;
    font-size: 0.85rem;
    color: var(--text-secondary);
    margin-bottom: 0.5rem;
  }
  
  .color-swatches {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  
  .color-swatch {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    border: 2px solid transparent;
    cursor: pointer;
    transition: all 0.2s;
    padding: 0;
  }
  
  .color-swatch:hover {
    transform: scale(1.1);
  }
  
  .color-swatch.active {
    border-color: var(--text-primary);
    box-shadow: 0 0 0 2px var(--bg-primary), 0 0 0 4px var(--text-primary);
  }
  
  .custom-color-input {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  
  .color-preview {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    border: 1px solid var(--border);
    flex-shrink: 0;
  }
  
  .custom-color-input input {
    width: 100px;
    padding: 0.5rem;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-primary);
    font-size: 0.9rem;
    font-family: monospace;
  }
  
  .form-actions {
    display: flex;
    gap: 0.75rem;
    justify-content: flex-end;
    margin-top: 1.5rem;
  }
  
  /* Labels Grid */
  .labels-grid {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
  }
  
  .label-card {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 1rem;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 6px;
    transition: all 0.2s;
  }
  
  .label-card:hover {
    border-color: var(--accent);
  }
  
  .label-info {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  
  .label-color {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    flex-shrink: 0;
  }
  
  .label-text {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  
  .label-name {
    font-weight: 600;
    color: var(--text-primary);
    font-size: 0.95rem;
  }
  
  .label-desc {
    color: var(--text-secondary);
    font-size: 0.85rem;
  }
  
  .label-actions {
    display: flex;
    gap: 0.5rem;
    opacity: 0;
    transition: opacity 0.2s;
  }
  
  .label-card:hover .label-actions {
    opacity: 1;
  }
  
  .btn-icon {
    background: none;
    border: 1px solid var(--border);
    border-radius: 4px;
    padding: 0.25rem 0.5rem;
    cursor: pointer;
    font-size: 0.9rem;
    transition: all 0.2s;
  }
  
  .btn-icon:hover {
    background: var(--bg-primary);
    border-color: var(--accent);
  }
  
  .empty-state {
    padding: 3rem;
    text-align: center;
    color: var(--text-secondary);
    font-size: 0.95rem;
  }
  
  .loading {
    padding: 2rem;
    text-align: center;
    color: var(--text-secondary);
  }
</style>
