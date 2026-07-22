<script lang="ts">
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { repos } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';
  import { renderMarkdown } from '$lib/utils/markdown';

  const t = createT();
  const MAX_EDITABLE_SIZE = 1024 * 1024;

  type BlobData = {
    path: string;
    sha: string;
    size: number;
    content: string;
    encoding: string;
    is_binary: boolean;
    name?: string;
  };

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let filePath = $derived($page.params.path!);
  let queryRef = $derived($page.url.searchParams.get('ref') || '');

  let blobData = $state<BlobData | null>(null);
  let ref = $state('');
  let loading = $state(true);
  let error = $state('');
  let loadKey = $state('');
  let viewMode = $state<'rendered' | 'source'>('rendered');
  let copyStatus = $state('');
  let deleteOpen = $state(false);
  let deleteMessage = $state('');
  let deleteError = $state('');
  let deleting = $state(false);

  let isMarkdown = $derived(/\.(md|markdown)$/i.test(filePath));
  let isText = $derived(Boolean(blobData && !blobData.is_binary && blobData.encoding === 'utf-8'));
  let canEdit = $derived(Boolean(isText && blobData && blobData.size <= MAX_EDITABLE_SIZE));
  let renderedMarkdown = $derived(
    isMarkdown && isText && blobData?.content ? renderMarkdown(blobData.content) : '',
  );
  let contentLines = $derived(isText && blobData ? getLineNumbers(blobData.content) : []);

  function buildRepoQuery(nextRef: string, nextPath: string) {
    const params = new URLSearchParams();
    if (nextRef) params.set('ref', nextRef);
    if (nextPath) params.set('path', nextPath);
    const qs = params.toString();
    return qs ? `?${qs}` : '';
  }

  function buildRepoHref(nextRef: string, nextPath: string) {
    return `/${owner}/${repo}${buildRepoQuery(nextRef, nextPath)}`;
  }

  function encodeRepoPath(path: string): string {
    return path.split('/').map(encodeURIComponent).join('/');
  }

  function buildBlobHref(nextRef = ref) {
    const params = new URLSearchParams();
    if (nextRef) params.set('ref', nextRef);
    const qs = params.toString();
    return `/${owner}/${repo}/blob/${encodeRepoPath(filePath)}${qs ? `?${qs}` : ''}`;
  }

  function buildEditHref() {
    const params = new URLSearchParams();
    if (blobData?.sha) params.set('sha', blobData.sha);
    if (ref) params.set('ref', ref);
    const qs = params.toString();
    return `/${owner}/${repo}/edit/${encodeRepoPath(filePath)}${qs ? `?${qs}` : ''}`;
  }

  function getBreadcrumbs(): { name: string; href: string }[] {
    const parts = filePath.split('/');
    const crumbs: { name: string; href: string }[] = [];
    let accumulated = '';
    for (let i = 0; i < parts.length - 1; i += 1) {
      accumulated = accumulated ? `${accumulated}/${parts[i]}` : parts[i];
      crumbs.push({ name: parts[i], href: buildRepoHref(ref, accumulated) });
    }
    return crumbs;
  }

  let breadcrumbs = $derived(getBreadcrumbs());

  $effect(() => {
    const nextRef = queryRef;
    if (ref !== nextRef) ref = nextRef;

    const nextKey = `${owner}/${repo}/${filePath}/${nextRef}`;
    if (loadKey !== nextKey) {
      loadKey = nextKey;
      viewMode = 'rendered';
      deleteOpen = false;
      deleteMessage = `Delete ${filePath}`;
      deleteError = '';
      copyStatus = '';
      loadBlob(nextRef);
    }
  });

  $effect(() => {
    if (blobData && (!isMarkdown || viewMode === 'source')) {
      setTimeout(highlightCode, 0);
    }
  });

  async function loadBlob(activeRef: string) {
    loading = true;
    error = '';
    try {
      blobData = await repos.blob(owner, repo, filePath, activeRef || undefined);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
    }
  }

  function highlightCode() {
    try {
      import('highlight.js').then((hljs) => {
        const blocks = document.querySelectorAll('.code-view code.hljs-code');
        blocks.forEach((block) => {
          hljs.default.highlightElement(block as HTMLElement);
        });
      }).catch(() => {});
    } catch {
      // Optional highlighting should never block file viewing.
    }
  }

  function formatFileSize(size: number) {
    if (size < 1024) return size + t('repo.file_size.b');
    if (size < 1024 * 1024) return (size / 1024).toFixed(1) + t('repo.file_size.kb');
    return (size / (1024 * 1024)).toFixed(1) + t('repo.file_size.mb');
  }

  function getLineNumbers(content: string): { num: number; text: string }[] {
    return content.split('\n').map((line, i) => ({ num: i + 1, text: line }));
  }

  function getLangClass(): string {
    const ext = filePath.split('.').pop()?.toLowerCase();
    const map: Record<string, string> = {
      c: 'c', h: 'c', cpp: 'cpp', cc: 'cpp', cxx: 'cpp', hpp: 'cpp',
      rs: 'rust', go: 'go', py: 'python', js: 'javascript', ts: 'typescript',
      jsx: 'javascript', tsx: 'typescript', java: 'java', rb: 'ruby',
      php: 'php', swift: 'swift', kt: 'kotlin', dart: 'dart',
      html: 'html', css: 'css', scss: 'scss', json: 'json', xml: 'xml',
      yaml: 'yaml', yml: 'yaml', toml: 'ini', md: 'markdown',
      sh: 'bash', bash: 'bash', zsh: 'bash', sql: 'sql',
      dockerfile: 'dockerfile', makefile: 'makefile',
    };
    return map[ext || ''] || '';
  }

  let langClass = $derived(getLangClass());

  async function copyText(value: string, label: string) {
    try {
      await navigator.clipboard.writeText(value);
      copyStatus = label;
      setTimeout(() => {
        copyStatus = '';
      }, 1600);
    } catch (err) {
      copyStatus = err instanceof Error ? err.message : String(err);
    }
  }

  function copyPath() {
    copyText(filePath, t('repo.blob.copied'));
  }

  function copyLink() {
    const href = typeof window === 'undefined'
      ? buildBlobHref()
      : `${window.location.origin}${buildBlobHref()}`;
    copyText(href, t('repo.blob.copied'));
  }

  function isConflictError(message: string): boolean {
    const normalized = message.toLowerCase();
    return normalized.includes('sha mismatch') || normalized.includes('conflict');
  }

  async function deleteFile() {
    if (!blobData?.sha) return;

    deleting = true;
    deleteError = '';
    try {
      await repos.deleteContent(owner, repo, filePath, {
        branch: ref || undefined,
        message: deleteMessage.trim() || `Delete ${filePath}`,
        sha: blobData.sha,
      });
      await goto(buildRepoHref(ref, ''));
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      deleteError = isConflictError(message)
        ? t('repo.blob.delete_conflict')
        : t('repo.blob.delete_failed', { message });
    } finally {
      deleting = false;
    }
  }
</script>

<svelte:head>
  <title>{filePath} · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="code" />

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if blobData}
    <div class="blob-breadcrumb">
      <a href={buildRepoHref(ref, '')} class="crumb-link">{repo}</a>
      {#each breadcrumbs as crumb}
        <span class="crumb-sep">/</span>
        <a href={crumb.href} class="crumb-link">{crumb.name}</a>
      {/each}
    </div>

    <div class="file-header">
      <div class="file-info">
        <span class="file-path">{filePath.split('/').pop()}</span>
      </div>
      <div class="file-meta">
        {#if isText}
          <span class="file-lines">{t('repo.lines_count', { count: contentLines.length })}</span>
        {/if}
        <span class="file-size">({formatFileSize(blobData.size)})</span>
      </div>
      <div class="file-actions">
        {#if isMarkdown && isText}
          <button
            type="button"
            class="btn-outline btn-sm"
            onclick={() => viewMode = viewMode === 'rendered' ? 'source' : 'rendered'}
          >
            {viewMode === 'rendered' ? t('repo.blob.raw') : t('repo.blob.rendered')}
          </button>
        {/if}
        <button type="button" class="btn-outline btn-sm" onclick={copyPath}>
          {t('repo.blob.copy_path')}
        </button>
        <button type="button" class="btn-outline btn-sm" onclick={copyLink}>
          {t('repo.blob.copy_link')}
        </button>
        {#if canEdit}
          <a href={buildEditHref()} class="btn-outline btn-sm">
            {t('repo.edit_file')}
          </a>
        {:else}
          <span class="btn-outline btn-sm disabled" title={t('repo.blob.edit_unavailable')}>
            {t('repo.edit_file')}
          </span>
        {/if}
        <button type="button" class="btn-outline btn-sm danger" onclick={() => deleteOpen = !deleteOpen}>
          {t('repo.blob.delete_file')}
        </button>
      </div>
    </div>

    {#if copyStatus}
      <div class="copy-status">{copyStatus}</div>
    {/if}

    {#if blobData.is_binary}
      <div class="warning-banner">{t('repo.blob.binary_file')}</div>
    {:else if blobData.size > MAX_EDITABLE_SIZE}
      <div class="warning-banner">{t('repo.blob.large_file')}</div>
    {/if}

    {#if deleteOpen}
      <div class="delete-panel">
        <div>
          <strong>{t('repo.blob.delete_title')}</strong>
          <p>{filePath}</p>
        </div>
        {#if deleteError}
          <div class="delete-error">{deleteError}</div>
        {/if}
        <label for="delete-message">{t('repo.blob.delete_message')}</label>
        <input
          id="delete-message"
          bind:value={deleteMessage}
          placeholder={t('repo.blob.delete_placeholder', { path: filePath })}
        />
        <div class="delete-actions">
          <button type="button" class="btn-outline btn-sm" onclick={() => deleteOpen = false}>
            {t('common.cancel')}
          </button>
          <button type="button" class="btn-danger btn-sm" onclick={deleteFile} disabled={deleting}>
            {deleting ? t('repo.blob.deleting') : t('repo.blob.delete_file')}
          </button>
        </div>
      </div>
    {/if}

    <div class="file-content">
      {#if blobData.is_binary}
        <div class="empty-state">{t('repo.blob.binary_file')}</div>
      {:else if isMarkdown && viewMode === 'rendered'}
        <div class="markdown-body">
          {@html renderedMarkdown || `<p>${t('repo.blob.empty')}</p>`}
        </div>
      {:else if blobData.content}
        <div class="code-view">
          <table class="code-table">
            <tbody>
              {#each contentLines as line}
                <tr>
                  <td class="line-number">{line.num}</td>
                  <td class="line-content">
                    <code class="hljs-code {langClass}">{line.text || ' '}</code>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else}
        <div class="empty-state">{t('repo.blob.empty')}</div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .blob-breadcrumb {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 14px;
    margin-bottom: 12px;
  }

  .crumb-link {
    color: var(--accent);
    text-decoration: none;
  }

  .crumb-link:hover {
    text-decoration: underline;
  }

  .crumb-sep {
    color: var(--text-muted);
  }

  .file-header {
    display: grid;
    grid-template-columns: minmax(160px, 1fr) auto minmax(280px, auto);
    align-items: center;
    gap: 12px;
    padding: 10px 16px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-bottom: none;
    border-radius: var(--radius) var(--radius) 0 0;
    font-size: 13px;
  }

  .file-info {
    min-width: 0;
  }

  .file-path {
    display: block;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    font-weight: 600;
  }

  .file-meta {
    display: flex;
    align-items: center;
    gap: 10px;
    color: var(--text-muted);
    font-size: 12px;
    white-space: nowrap;
  }

  .file-lines {
    color: var(--text-secondary);
  }

  .file-actions {
    display: flex;
    justify-content: flex-end;
    flex-wrap: wrap;
    gap: 6px;
  }

  .btn-outline,
  .btn-danger {
    border-radius: 6px;
    min-height: 32px;
    padding: 5px 10px;
    font-size: 13px;
    text-decoration: none;
    cursor: pointer;
  }

  .btn-outline {
    border: 1px solid var(--border);
    background: var(--bg-primary);
    color: var(--text-primary);
  }

  .btn-outline:hover {
    background: var(--bg-tertiary);
  }

  .btn-outline.disabled {
    opacity: 0.55;
    cursor: not-allowed;
    pointer-events: none;
  }

  .btn-outline.danger {
    color: #cf222e;
  }

  .btn-danger {
    border: 1px solid #cf222e;
    background: #cf222e;
    color: white;
  }

  .btn-danger:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .copy-status,
  .warning-banner,
  .delete-panel {
    border: 1px solid var(--border);
    border-bottom: none;
    background: var(--bg-primary);
    padding: 10px 16px;
    font-size: 13px;
  }

  .copy-status {
    color: var(--text-secondary);
  }

  .warning-banner {
    border-color: color-mix(in srgb, #bf8700 35%, transparent);
    background: color-mix(in srgb, #bf8700 11%, transparent);
  }

  .delete-panel {
    display: grid;
    gap: 10px;
    border-color: color-mix(in srgb, #cf222e 30%, transparent);
    background: color-mix(in srgb, #cf222e 7%, var(--bg-primary));
  }

  .delete-panel p {
    margin: 3px 0 0;
    color: var(--text-muted);
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  }

  .delete-panel label {
    font-size: 13px;
    font-weight: 600;
  }

  .delete-panel input {
    min-height: 36px;
    padding: 7px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-primary);
    color: var(--text-primary);
  }

  .delete-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
  }

  .delete-error {
    color: #cf222e;
    font-size: 13px;
  }

  .file-content {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 0 0 var(--radius) var(--radius);
    overflow: auto;
  }

  .code-view {
    overflow-x: auto;
  }

  .code-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 13px;
    line-height: 1.6;
  }

  .code-table tr:hover {
    background: rgba(128, 128, 128, 0.05);
  }

  .line-number {
    padding: 0 12px 0 16px;
    text-align: right;
    color: var(--text-muted);
    font-size: 12px;
    user-select: none;
    border-right: 1px solid var(--border-light);
    white-space: nowrap;
    vertical-align: top;
    width: 1%;
  }

  .line-content {
    padding: 0 16px;
    white-space: pre;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  }

  .hljs-code {
    font-family: inherit;
    font-size: inherit;
    line-height: inherit;
    white-space: pre;
    tab-size: 4;
  }

  .markdown-body {
    padding: 32px;
    line-height: 1.7;
  }

  .markdown-body :global(h1),
  .markdown-body :global(h2),
  .markdown-body :global(h3) {
    margin-top: 24px;
    margin-bottom: 12px;
    border-bottom: 1px solid var(--border);
    padding-bottom: 8px;
  }

  .markdown-body :global(p) {
    margin-bottom: 12px;
  }

  .markdown-body :global(code) {
    background: var(--bg-tertiary);
    padding: 2px 6px;
    border-radius: 3px;
    font-size: 12px;
  }

  .markdown-body :global(pre code) {
    display: block;
    padding: 12px;
    overflow: auto;
  }

  .empty-state {
    padding: 24px;
    color: var(--text-muted);
  }

  @media (max-width: 820px) {
    .file-header {
      grid-template-columns: 1fr;
      align-items: stretch;
    }

    .file-actions {
      justify-content: flex-start;
    }

    .delete-actions {
      justify-content: flex-start;
    }
  }
</style>
