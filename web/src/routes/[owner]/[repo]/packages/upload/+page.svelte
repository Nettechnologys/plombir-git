<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { packages } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';
  import {
    PACKAGE_FORMATS,
    packageFormatOptionLabel,
    packageFormatSupportLabel,
    packageFormatUsesGenericFallback,
  } from '$lib/packageFormats';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);

  let format = $state('cargo');
  let packageFile = $state<File | null>(null);
  let packageName = $state('');
  let packageVersion = $state('');
  let description = $state('');
  let homepage = $state('');
  let repositoryUrl = $state('');
  let semver = $state('');

  let uploading = $state(false);
  let error = $state('');
  let success = $state('');

  function selectedFileLabel() {
    if (!packageFile) return t('packages.file');
    return packageFile.name;
  }

  function handleFileChange(event: Event) {
    const input = event.currentTarget as HTMLInputElement;
    const files = input.files;
    packageFile = files && files.length > 0 ? files[0] : null;
    error = '';
  }

  async function handleUpload(event: Event) {
    event.preventDefault();
    if (!packageFile) {
      error = 'Package file is required';
      return;
    }

    uploading = true;
    error = '';
    success = '';

    const metadata = {
      name: packageName.trim() || undefined,
      version: packageVersion.trim() || undefined,
      description: description.trim() || undefined,
      homepage: homepage.trim() || undefined,
      repository_url: repositoryUrl.trim() || undefined,
      semver: semver.trim() || undefined,
    };

    try {
      await packages.publish(owner!, repo!, format, packageFile, metadata);
      success = t('packages.upload_success');
      goto(`/${owner}/${repo}/packages`);
    } catch (e: any) {
      error = e.message;
    } finally {
      uploading = false;
    }
  }
</script>

<svelte:head>
  <title>{t('packages.upload')} · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader owner={owner!} repo={repo!} activeTab="packages" />

  <div class="page-header">
    <h1>{t('packages.upload')}</h1>
    <a href={`/${owner}/${repo}/packages`} class="btn-secondary">Back</a>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if success}
    <div class="success-banner">{success}</div>
  {/if}

  <form class="package-form" onsubmit={handleUpload}>
    <div class="form-group">
      <label for="format">{t('packages.format')}</label>
      <select id="format" bind:value={format} class="select">
        {#each PACKAGE_FORMATS as f}
          <option value={f}>{packageFormatOptionLabel(f)}</option>
        {/each}
      </select>
      <p class="format-note" class:fallback={packageFormatUsesGenericFallback(format)}>
        {packageFormatSupportLabel(format)}
      </p>
    </div>

      <div class="form-group">
        <label for="package-file">{t('packages.file')}</label>
        <div class="file-input-wrap">
          <input id="package-file" type="file" onchange={handleFileChange} />
          <span class="file-label">{selectedFileLabel()}</span>
        </div>
      </div>

    <div class="form-group">
      <label for="name">Name</label>
      <input id="name" type="text" bind:value={packageName} class="input" placeholder="Package name" />
    </div>

      <div class="form-group">
        <label for="version">{t('packages.version')}</label>
        <input id="version" type="text" bind:value={packageVersion} class="input" placeholder={t('packages.version')} />
      </div>

      <div class="form-group">
        <label for="description">{t('packages.description')}</label>
        <textarea id="description" bind:value={description} class="textarea" rows="3"></textarea>
      </div>

      <div class="form-group">
        <label for="homepage">Homepage</label>
        <input id="homepage" type="text" bind:value={homepage} class="input" />
      </div>

      <div class="form-group">
        <label for="repository-url">Repository URL</label>
        <input id="repository-url" type="text" bind:value={repositoryUrl} class="input" />
      </div>

      <div class="form-group">
        <label for="semver">Semver</label>
        <input id="semver" type="text" bind:value={semver} class="input" />
      </div>

      <div class="form-actions">
        <button type="submit" class="btn-primary" disabled={uploading || !packageFile}>
          {uploading ? 'Uploading...' : t('packages.upload')}
        </button>
      </div>
  </form>
</div>

<style>
  .page-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 24px;
  }

  h1 {
    font-size: 24px;
    font-weight: 600;
  }

  .package-form {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 24px;
  }

  .form-group {
    margin-bottom: 18px;
  }

  .format-note {
    margin: 6px 0 0;
    font-size: 12px;
    color: var(--text-muted);
  }

  .format-note.fallback {
    color: var(--yellow);
  }

  .form-group label {
    display: block;
    margin-bottom: 6px;
    font-size: 14px;
    font-weight: 600;
    color: var(--text-primary);
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

  .textarea {
    min-height: 80px;
    resize: vertical;
  }

  .file-input-wrap {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .file-label {
    font-size: 13px;
    color: var(--text-secondary);
  }

  .form-actions {
    margin-top: 8px;
    display: flex;
    justify-content: flex-end;
  }

  .btn-primary {
    padding: 8px 20px;
    border: none;
    background: var(--orange);
    color: #fff;
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }

  .btn-primary:hover {
    background: #e09a1e;
  }

  .btn-primary:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .btn-secondary {
    padding: 8px 16px;
    border: 1px solid var(--border);
    background: var(--bg-primary);
    color: var(--text-primary);
    border-radius: var(--radius);
    text-decoration: none;
  }

  .success-banner {
    margin-bottom: 16px;
    color: var(--green);
    padding: 8px 12px;
    border: 1px solid color-mix(in srgb, var(--green) 30%, transparent);
    border-radius: var(--radius);
    background: color-mix(in srgb, var(--green) 12%, transparent);
  }
</style>
