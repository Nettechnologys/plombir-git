<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { releases, repos } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);

  let tagName = $state('');
  let releaseTitle = $state('');
  let body = $state('');
  let targetCommitish = $state('');
  let isDraft = $state(false);
  let isPrerelease = $state(false);

  let branches = $state<string[]>([]);
  let tags = $state<string[]>([]);
  let loading = $state(true);
  let submitting = $state(false);
  let error = $state('');
  let selectedTargetType = $state<'branch' | 'tag'>('tag');

  $effect(() => {
    loadBranchesAndTags();
  });

  async function loadBranchesAndTags() {
    loading = true;
    try {
      const [branchList, tagList] = await Promise.all([
        repos.branches(owner!, repo!),
        repos.tags(owner!, repo!)
      ]);
      branches = branchList.map(b => b.name);
      tags = tagList.map(t => t.name);
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function handleSubmit(e: Event) {
    e.preventDefault();

    if (!tagName.trim()) {
      error = 'Tag name is required';
      return;
    }

    if (!releaseTitle.trim()) {
      error = 'Release title is required';
      return;
    }

    submitting = true;
    error = '';

    try {
      await releases.create(owner!, repo!, {
        tag_name: tagName.trim(),
        title: releaseTitle.trim(),
        body: body.trim() || undefined,
        target_commitish: targetCommitish || undefined,
        is_draft: isDraft,
        is_prerelease: isPrerelease
      });
      goto(`/${owner}/${repo}/releases`);
    } catch (e: any) {
      error = e.message;
      submitting = false;
    }
  }
</script>

<svelte:head>
  <title>New Release · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader owner={owner!} repo={repo!} activeTab="releases" />

  <div class="page-header">
    <h1>{t('releases.create_title')}</h1>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="loading-text">{t('common.loading')}</p>
  {:else}
    <form class="release-form" onsubmit={handleSubmit}>
      <div class="form-group">
        <label for="tag-name">{t('releases.tag_name')} <span class="required">*</span></label>
        <input
          type="text"
          id="tag-name"
          bind:value={tagName}
          placeholder={t('releases.tag_name_placeholder')}
          required
          class="input"
        />
        {#if tags.length > 0}
          <div class="tag-hints">
            <span class="hint-label">Existing tags:</span>
            {#each tags.slice(0, 10) as tag}
              <button
                type="button"
                class="tag-hint"
                onclick={() => tagName = tag}
              >
                {tag}
              </button>
            {/each}
            {#if tags.length > 10}
              <span class="hint-more">+{tags.length - 10} more</span>
            {/if}
          </div>
        {/if}
      </div>

      <div class="form-group">
        <label for="release-title">{t('releases.release_title')} <span class="required">*</span></label>
        <input
          type="text"
          id="release-title"
          bind:value={releaseTitle}
          placeholder={t('releases.release_title_placeholder')}
          required
          class="input"
        />
      </div>

      <div class="form-group">
        <label for="body">{t('releases.body')}</label>
        <textarea
          id="body"
          bind:value={body}
          placeholder={t('releases.body_placeholder')}
          rows="8"
          class="textarea"
        ></textarea>
      </div>

      <div class="form-group">
        <label for="target-commitish">{t('releases.target_commitish')}</label>
        <div class="target-toggle">
          <button
            type="button"
            class="toggle-btn"
            class:active={selectedTargetType === 'tag'}
            onclick={() => selectedTargetType = 'tag'}
          >
            Tags
          </button>
          <button
            type="button"
            class="toggle-btn"
            class:active={selectedTargetType === 'branch'}
            onclick={() => selectedTargetType = 'branch'}
          >
            Branches
          </button>
        </div>

        <select id="target-commitish" bind:value={targetCommitish} class="select">

        {#if selectedTargetType === 'tag'}
          <option value="">-- Select a tag (optional) --</option>
          {#each tags as tag}
            <option value={tag}>{tag}</option>
          {/each}
        {:else}
          <option value="">-- Select a branch (optional) --</option>
          {#each branches as branch}
            <option value={branch}>{branch}</option>
          {/each}
        {/if}
        </select>
      </div>

      <div class="form-group checkbox-group">
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={isDraft} />
          <span>{t('releases.is_draft')}</span>
        </label>
      </div>

      <div class="form-group checkbox-group">
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={isPrerelease} />
          <span>{t('releases.is_prerelease')}</span>
        </label>
      </div>

      <div class="form-actions">
        <a href={`/${owner}/${repo}/releases`} class="btn-secondary">{t('common.cancel')}</a>
        <button type="submit" class="btn-primary" disabled={submitting}>
          {submitting ? t('releases.submitting') : t('releases.submit')}
        </button>
      </div>
    </form>
  {/if}
</div>

<style>

  .page-header {
    margin-bottom: 24px;
  }

  h1 {
    font-size: 24px;
    font-weight: 600;
  }
.loading-text {
    color: var(--text-secondary);
    text-align: center;
    padding: 48px;
  }

  .release-form {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 24px;
  }

  .form-group {
    margin-bottom: 20px;
  }

  .form-group label {
    display: block;
    font-size: 14px;
    font-weight: 600;
    color: var(--text-primary);
    margin-bottom: 6px;
  }

  .required {
    color: var(--red);
  }

  .input,
  .textarea,
  .select {
    width: 100%;
    padding: 8px 12px;
    font-size: 14px;
    color: var(--text-primary);
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-sizing: border-box;
  }

  .input:focus,
  .textarea:focus,
  .select:focus {
    outline: none;
    border-color: var(--accent);
  }

  .textarea {
    resize: vertical;
    min-height: 120px;
    font-family: inherit;
    line-height: 1.6;
  }

  .select {
    appearance: none;
    background-image: url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12'%3E%3Cpath fill='%23888' d='M6 8L1 3h10z'/%3E%3C/svg%3E");
    background-repeat: no-repeat;
    background-position: right 10px center;
    padding-right: 30px;
  }

  .target-toggle {
    display: flex;
    gap: 0;
    margin-bottom: 8px;
  }

  .toggle-btn {
    padding: 6px 16px;
    font-size: 13px;
    font-weight: 500;
    color: var(--text-secondary);
    background: var(--bg-primary);
    border: 1px solid var(--border);
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .toggle-btn:first-child {
    border-radius: var(--radius) 0 0 var(--radius);
  }

  .toggle-btn:last-child {
    border-radius: 0 var(--radius) var(--radius) 0;
    border-left: none;
  }

  .toggle-btn.active {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }

  .tag-hints {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 6px;
    margin-top: 8px;
  }

  .hint-label {
    font-size: 12px;
    color: var(--text-muted);
  }

  .tag-hint {
    padding: 2px 8px;
    font-size: 12px;
    color: var(--text-secondary);
    background: var(--bg-tertiary);
    border: 1px solid var(--border);
    border-radius: 4px;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .tag-hint:hover {
    background: var(--bg-hover);
    color: var(--text-primary);
  }

  .hint-more {
    font-size: 12px;
    color: var(--text-muted);
  }

  .checkbox-group {
    margin-bottom: 12px;
  }

  .checkbox-label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-weight: 500;
    cursor: pointer;
  }

  .checkbox-label input[type="checkbox"] {
    width: 16px;
    height: 16px;
    cursor: pointer;
  }

  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: 12px;
    margin-top: 24px;
    padding-top: 20px;
    border-top: 1px solid var(--border);
  }

  .btn-primary {
    padding: 8px 20px;
    background: var(--orange);
    color: #fff;
    border: none;
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
    transition: background 0.15s ease;
  }

  .btn-primary:hover:not(:disabled) {
    background: #e09a1e;
  }

  .btn-primary:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn-secondary {
    padding: 8px 20px;
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    font-size: 14px;
    font-weight: 500;
    text-decoration: none;
    cursor: pointer;
    transition: all 0.15s ease;
  }

  .btn-secondary:hover {
    background: var(--bg-hover);
  }

  @media (max-width: 600px) {
    .release-form {
      padding: 16px;
    }

    .form-actions {
      flex-direction: column-reverse;
    }

    .btn-primary,
    .btn-secondary {
      width: 100%;
      text-align: center;
    }
  }
</style>
