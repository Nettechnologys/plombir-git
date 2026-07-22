<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { releases } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();

  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
  const releaseId = $derived(parseInt($page.params.id!, 10));

  let loading = $state(true);
  let submitting = $state(false);
  let error = $state('');
  let notFound = $state(false);

  let tagName = $state('');
  let releaseTitle = $state('');
  let body = $state('');
  let isDraft = $state(false);
  let isPrerelease = $state(false);

  $effect(() => {
    if (!Number.isFinite(releaseId) || releaseId <= 0) {
      notFound = true;
      loading = false;
      return;
    }
    loadRelease();
  });

  async function loadRelease() {
    loading = true;
    error = '';
    try {
      const release = await releases.get(owner, repo, releaseId);
      tagName = release.tag_name || '';
      releaseTitle = release.title || '';
      body = release.body || '';
      isDraft = !!release.is_draft;
      isPrerelease = !!release.is_prerelease;
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function handleSubmit(e: Event) {
    e.preventDefault();

    if (!releaseTitle.trim()) {
      error = 'Release title is required';
      return;
    }

    submitting = true;
    error = '';

    try {
      await releases.update(owner!, repo!, releaseId, {
        title: releaseTitle.trim(),
        body: body.trim() || undefined,
        is_draft: isDraft,
        is_prerelease: isPrerelease,
      });
      goto(`/${owner}/${repo}/releases`);
    } catch (e: any) {
      error = e.message;
      submitting = false;
    }
  }
</script>

<svelte:head>
  <title>Edit Release · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader owner={owner!} repo={repo!} activeTab="releases" />

  <div class="page-header">
    <h1>{t('releases.edit')} #{releaseId}</h1>
  </div>

  {#if notFound}
    <div class="empty">
      <p>Invalid release id.</p>
      <a href={`/${owner}/${repo}/releases`} class="btn-primary">{t('releases.title')}</a>
    </div>
  {:else if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="loading-text">{t('common.loading')}</p>
  {:else if !notFound}
    <form class="release-form" onsubmit={handleSubmit}>
      <div class="form-group">
        <label for="release-tag">Tag</label>
        <input id="release-tag" type="text" value={tagName} disabled class="input" />
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
          {submitting ? t('common.saving') || 'Saving...' : t('releases.edit')}
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

  .empty {
    text-align: center;
    padding: 48px;
    color: var(--text-secondary);
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
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
  .textarea {
    width: 100%;
    padding: 8px 12px;
    font-size: 14px;
    color: var(--text-primary);
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    box-sizing: border-box;
  }

  .input:disabled {
    color: var(--text-muted);
  }

  .textarea {
    min-height: 180px;
    resize: vertical;
  }

  .checkbox-group {
    margin-bottom: 12px;
  }

  .checkbox-label {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 14px;
  }

  .form-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .btn-primary {
    padding: 6px 16px;
    background: var(--orange);
    color: #fff;
    border: none;
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
    text-decoration: none;
  }

  .btn-primary:hover {
    background: #e09a1e;
    text-decoration: none;
  }

  .btn-secondary {
    padding: 6px 16px;
    border: 1px solid var(--border);
    background: var(--bg-primary);
    color: var(--text-primary);
    border-radius: var(--radius);
    text-decoration: none;
    font-size: 14px;
  }
</style>
