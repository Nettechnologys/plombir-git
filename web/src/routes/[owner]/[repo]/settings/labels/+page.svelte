<script lang="ts">
  import { page } from '$app/stores';
  import { labels, buildLabelPayload } from '$lib/api/client.svelte';
  import { viewerPermission } from '$lib/viewerPermission.svelte';
  import { LatestRepositoryRequestFence } from '$lib/asyncStateOwnership';
  import { createT } from '$lib/i18n';
  import Modal from '$lib/components/Modal.svelte';
  import { safeHexColor } from '$lib/utils/color';

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
  let busyRows = $state<Set<string>>(new Set());
  // Creating, editing and deleting labels is behind `RepoWrite`
  // (card_3625a7b89abb); a reader sees the list without the controls.
  const permission = viewerPermission(() => owner, () => repo);
  let canWrite = $derived(permission.canWrite);
  const listRequests = new LatestRepositoryRequestFence();
  let routeGeneration = 0;
  let successTimer: ReturnType<typeof setTimeout> | undefined;

  const presetColors = [
    '#ff0000', '#00ff00', '#0000ff', '#ffff00',
    '#ff00ff', '#00ffff', '#ff8800', '#888888'
  ];
  
  $effect(() => {
    const expectedOwner = owner;
    const expectedRepo = repo;
    clearSuccessTimer();
    routeGeneration += 1;
    labelList = [];
    loading = true;
    error = '';
    success = '';
    saving = false;
    deleting = false;
    busyRows = new Set();
    closeForm();
    deletingLabel = null;
    void loadLabels(expectedOwner, expectedRepo);
    return clearSuccessTimer;
  });
  
  function rowKey(id: number | null, labelName = formData.name.trim()): string {
    return id === null ? `new:${labelName.toLowerCase()}` : `id:${id}`;
  }

  function isCurrentRoute(expectedOwner: string, expectedRepo: string, expectedRoute: number): boolean {
    return routeGeneration === expectedRoute && owner === expectedOwner && repo === expectedRepo;
  }

  function isBusy(key: string): boolean {
    return busyRows.has(key);
  }

  function claimMutation(key: string): boolean {
    if (isBusy(key)) return false;
    busyRows = new Set(busyRows).add(key);
    return true;
  }

  function releaseMutation(key: string): void {
    const next = new Set(busyRows);
    next.delete(key);
    busyRows = next;
  }

  function clearSuccessTimer(): void {
    if (successTimer !== undefined) clearTimeout(successTimer);
    successTimer = undefined;
  }

  function showSuccess(
    message: string,
    expectedOwner: string,
    expectedRepo: string,
    expectedRoute: number,
  ): void {
    clearSuccessTimer();
    success = message;
    successTimer = setTimeout(() => {
      successTimer = undefined;
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) success = '';
    }, 3000);
  }

  async function loadLabels(expectedOwner: string, expectedRepo: string) {
    const claim = listRequests.begin(expectedOwner, expectedRepo);
    try {
      loading = true;
      error = '';
      const result = await labels.list(expectedOwner, expectedRepo);
      if (listRequests.owns(claim, owner, repo)) {
        labelList = result;
        error = '';
      }
    } catch (err: any) {
      if (listRequests.owns(claim, owner, repo)) error = err.message || t('settings.labels_load_failed');
    } finally {
      if (listRequests.owns(claim, owner, repo)) loading = false;
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
      formError = t('settings.label_name_required');
      return;
    }
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const expectedEditingId = editingLabel?.id ?? null;
    const payload = buildLabelPayload(formData);
    const key = rowKey(expectedEditingId);
    if (!claimMutation(key)) return;
    
    try {
      saving = true;
      formError = '';
      if (expectedEditingId !== null) {
        await labels.update(expectedOwner, expectedRepo, expectedEditingId, payload);
      } else {
        await labels.create(expectedOwner, expectedRepo, payload);
      }
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      closeForm();
      showSuccess(
        expectedEditingId === null ? t('settings.create_label') : t('settings.save_label'),
        expectedOwner,
        expectedRepo,
        expectedRoute,
      );
      await loadLabels(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        formError = err.message || t('settings.label_save_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        saving = false;
        releaseMutation(key);
      }
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
    const label = deletingLabel;
    const expectedOwner = owner;
    const expectedRepo = repo;
    const expectedRoute = routeGeneration;
    const key = rowKey(label.id);
    if (!claimMutation(key)) return;
    
    try {
      deleting = true;
      await labels.delete(expectedOwner, expectedRepo, label.id);
      if (!isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) return;
      deletingLabel = null;
      showSuccess(t('settings.delete_label'), expectedOwner, expectedRepo, expectedRoute);
      await loadLabels(expectedOwner, expectedRepo);
    } catch (err: any) {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        error = err.message || t('settings.label_delete_failed');
      }
    } finally {
      if (isCurrentRoute(expectedOwner, expectedRepo, expectedRoute)) {
        deleting = false;
        releaseMutation(key);
      }
    }
  }
  
</script>

<svelte:head>
  <title>{t('settings.labels')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="labels-page">
  <div class="page-header">
    <h1>{t('settings.labels')}</h1>
    {#if canWrite}
      <button class="btn btn-primary" onclick={openCreateForm}>
        + {t('settings.new_label')}
      </button>
    {/if}
  </div>
  
  {#if success}
    <div class="success-box">{success}</div>
  {/if}
  
  {#if error}
    <div class="error-box">{error}</div>
  {/if}
  
  <!-- Create/Edit Form -->
  {#if showForm}
    <Modal onclose={closeForm} labelledby="label-form-title" width="500px" padding="2rem">
      <div class="form-modal">
        <h2 id="label-form-title">{editingLabel ? t('settings.edit_label') : t('settings.new_label')}</h2>
        
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
                  aria-label={t('settings.label_color_swatch', { color })}
                ></button>
              {/each}
            </div>
          </div>
          
          <div class="custom-color">
            <span class="color-section-label">{t('settings.custom_color')}</span>
            <div class="custom-color-input">
              <div class="color-preview" style="background-color: {safeHexColor(formData.color, '#888888')}"></div>
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
            {t('common.cancel')}
          </button>
          <button class="btn btn-primary" onclick={handleSave} disabled={saving || isBusy(rowKey(editingLabel?.id ?? null))} aria-busy={saving}>
            {saving ? t('common.saving') : (editingLabel ? t('settings.save_label') : t('settings.create_label'))}
          </button>
        </div>
      </div>
    </Modal>
  {/if}
  
  <!-- Delete Confirmation -->
  {#if deletingLabel}
    <Modal onclose={cancelDelete} labelledby="label-delete-title" width="500px" padding="2rem">
      <div class="form-modal">
        <h2 id="label-delete-title">{t('settings.confirm_delete_title')}</h2>
        <p>{t('settings.confirm_delete_label')}</p>
        <p><strong>{deletingLabel.name}</strong></p>
        
        <div class="form-actions">
          <button class="btn btn-outline" onclick={cancelDelete} disabled={deleting} data-autofocus>
            {t('common.cancel')}
          </button>
          <button class="btn btn-danger" onclick={handleDelete} disabled={deleting}>
            {deleting ? t('common.deleting') : t('common.delete')}
          </button>
        </div>
      </div>
    </Modal>
  {/if}
  
  <!-- Labels List -->
  {#if loading}
    <div class="loading">{t('common.loading')}</div>
  {:else if labelList.length === 0}
    <div class="empty-state">
      <p>{t('settings.no_labels')}</p>
    </div>
  {:else}
    <div class="labels-grid">
      {#each labelList as label (label.id)}
        <div class="label-card">
          <div class="label-info">
            <div class="label-color" style="background-color: {safeHexColor(label.color, '#888888')}"></div>
            <div class="label-text">
              <span class="label-name">{label.name}</span>
              {#if label.description}
                <span class="label-desc">{label.description}</span>
              {/if}
            </div>
          </div>
          {#if canWrite}
            <div class="label-actions">
              <button class="btn-icon" onclick={() => openEditForm(label)} title={t('common.edit')} disabled={isBusy(rowKey(label.id))}>
                ✏️
              </button>
              <button class="btn-icon" onclick={() => confirmDelete(label)} title={t('common.delete')} disabled={isBusy(rowKey(label.id))} aria-busy={isBusy(rowKey(label.id))}>
                🗑️
              </button>
            </div>
          {/if}
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
  
  /* Modal content (the panel itself is lib/components/Modal.svelte) */
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
