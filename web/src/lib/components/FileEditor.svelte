<script lang="ts">
  import { createT } from '$lib/i18n';
  import { buildLineDiff } from '$lib/utils/diff';
  import { renderMarkdown } from '$lib/utils/markdown';

  type EditorMode = 'create' | 'edit';
  type EditorTab = 'edit' | 'preview' | 'diff';
  type SavePayload = {
    path: string;
    content: string;
    message: string;
    branch: string;
    sha?: string;
  };

  interface Props {
    owner: string;
    repo: string;
    mode: EditorMode;
    initialPath?: string;
    initialContent?: string;
    initialSha?: string;
    branch?: string;
    cancelHref: string;
    disabledReason?: string;
    onSave: (payload: SavePayload) => Promise<void>;
  }

  const t = createT();

  let {
    owner,
    repo,
    mode,
    initialPath = '',
    initialContent = '',
    initialSha = '',
    branch = 'main',
    cancelHref,
    disabledReason = '',
    onSave,
  }: Props = $props();

  let filePath = $state('');
  let fileContent = $state('');
  let commitMessage = $state('');
  let targetBranch = $state('');
  let activeTab = $state<EditorTab>('edit');
  let saving = $state(false);
  let error = $state('');
  let conflict = $state(false);
  let initialized = $state(false);
  let editorTextarea = $state<HTMLTextAreaElement | null>(null);
  let highlightBackdrop = $state<HTMLPreElement | null>(null);
  let highlightedContent = $state('');
  let highlightRequestId = 0;

  let isMarkdown = $derived(/\.(md|markdown)$/i.test(filePath));
  let highlightLanguage = $derived(languageFromPath(filePath));
  let renderedPreview = $derived(isMarkdown ? renderMarkdown(fileContent) : '');
  let diffLines = $derived(buildLineDiff(initialContent, fileContent));
  let changed = $derived(mode === 'create' || initialContent !== fileContent);

  $effect(() => {
    if (initialized) return;
    filePath = initialPath;
    fileContent = initialContent;
    commitMessage =
      mode === 'create'
        ? `Create ${initialPath || 'new file'}`
        : `Update ${initialPath || 'file'}`;
    targetBranch = branch;
    initialized = true;
  });

  $effect(() => {
    filePath;
    fileContent;
    void updateHighlightedContent();
  });

  function languageFromPath(path: string): string {
    const ext = path.split('.').pop()?.toLowerCase() || '';
    const map: Record<string, string> = {
      c: 'c',
      h: 'c',
      cpp: 'cpp',
      cc: 'cpp',
      cxx: 'cpp',
      hpp: 'cpp',
      rs: 'rust',
      go: 'go',
      py: 'python',
      js: 'javascript',
      mjs: 'javascript',
      cjs: 'javascript',
      ts: 'typescript',
      jsx: 'javascript',
      tsx: 'typescript',
      java: 'java',
      rb: 'ruby',
      php: 'php',
      swift: 'swift',
      kt: 'kotlin',
      dart: 'dart',
      html: 'xml',
      css: 'css',
      scss: 'scss',
      json: 'json',
      xml: 'xml',
      yaml: 'yaml',
      yml: 'yaml',
      toml: 'ini',
      sh: 'bash',
      bash: 'bash',
      zsh: 'bash',
      sql: 'sql',
      dockerfile: 'dockerfile',
      md: 'markdown',
      markdown: 'markdown',
    };
    return map[ext] || '';
  }

  function escapeHtml(value: string): string {
    return value
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/"/g, '&quot;')
      .replace(/'/g, '&#39;');
  }

  async function updateHighlightedContent() {
    const requestId = ++highlightRequestId;
    const content = fileContent || ' ';
    const language = highlightLanguage;

    try {
      const hljs = await import('highlight.js');
      if (requestId !== highlightRequestId) return;

      const highlighter = hljs.default;
      if (language && highlighter.getLanguage(language)) {
        highlightedContent = highlighter.highlight(content, { language }).value;
      } else {
        highlightedContent = highlighter.highlightAuto(content).value;
      }
    } catch {
      if (requestId === highlightRequestId) {
        highlightedContent = escapeHtml(content);
      }
    } finally {
      syncEditorScroll();
    }
  }

  function syncEditorScroll() {
    if (!editorTextarea || !highlightBackdrop) return;
    highlightBackdrop.scrollTop = editorTextarea.scrollTop;
    highlightBackdrop.scrollLeft = editorTextarea.scrollLeft;
  }

  function validatePath(value: string): string {
    const normalized = value.trim();
    if (!normalized) return t('repo.editor.path_required');
    if (normalized.startsWith('/') || normalized.startsWith('\\')) {
      return t('repo.editor.path_invalid_leading');
    }
    if (normalized.includes('\\')) return t('repo.editor.path_invalid_separator');
    if (normalized.includes('//') || normalized.endsWith('/')) {
      return t('repo.editor.path_invalid_empty_segment');
    }
    if (normalized.split('/').some((segment) => segment === '.' || segment === '..')) {
      return t('repo.editor.path_invalid_segment');
    }
    if (/[\u0000-\u001f]/.test(normalized)) {
      return t('repo.editor.path_invalid_control');
    }
    return '';
  }

  function validate(): boolean {
    error = validatePath(filePath);
    if (error) return false;

    if (!commitMessage.trim()) {
      error = t('repo.editor.message_required');
      return false;
    }

    if (!targetBranch.trim()) {
      error = t('repo.editor.branch_required');
      return false;
    }

    if (disabledReason) {
      error = disabledReason;
      return false;
    }

    return true;
  }

  function isConflictError(message: string): boolean {
    const normalized = message.toLowerCase();
    return normalized.includes('sha mismatch') || normalized.includes('conflict');
  }

  async function saveFile() {
    if (!validate()) return;

    saving = true;
    error = '';
    conflict = false;

    const payload: SavePayload = {
      path: filePath.trim(),
      content: fileContent,
      message: commitMessage.trim(),
      branch: targetBranch.trim(),
      ...(mode === 'edit' && initialSha ? { sha: initialSha } : {}),
    };

    try {
      await onSave(payload);
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      conflict = isConflictError(message);
      error = conflict
        ? t('repo.editor.conflict')
        : t('repo.editor.save_failed', { message });
    } finally {
      saving = false;
    }
  }
</script>

<div class="editor-shell">
  <div class="editor-header">
    <div>
      <div class="repo-context">{owner}/{repo}</div>
      <h1>{mode === 'create' ? t('repo.new_file') : t('repo.edit_file')}</h1>
    </div>
    <a href={cancelHref} class="btn-secondary">{t('common.cancel')}</a>
  </div>

  {#if error}
    <div class:error-message={!conflict} class:conflict-message={conflict}>
      <span>{error}</span>
      {#if conflict}
        <button type="button" class="btn-link" onclick={() => location.reload()}>
          {t('repo.editor.reload_latest')}
        </button>
      {/if}
    </div>
  {/if}

  {#if disabledReason}
    <div class="warning-message">{disabledReason}</div>
  {/if}

  <div class="field-row">
    <label for="repo-file-path">{t('repo.editor.path')}</label>
    <input
      id="repo-file-path"
      class="path-input"
      bind:value={filePath}
      readonly={mode === 'edit'}
      placeholder={t('repo.editor.path_placeholder')}
    />
  </div>

  <div class="editor-panel">
    <div class="tabbar" role="tablist" aria-label={t('repo.editor.tabs_label')}>
      <button type="button" class:active={activeTab === 'edit'} onclick={() => activeTab = 'edit'}>
        {t('repo.editor.edit_tab')}
      </button>
      <button type="button" class:active={activeTab === 'preview'} onclick={() => activeTab = 'preview'}>
        {t('repo.editor.preview_tab')}
      </button>
      <button type="button" class:active={activeTab === 'diff'} onclick={() => activeTab = 'diff'}>
        {t('repo.editor.diff_tab')}
      </button>
    </div>

    {#if activeTab === 'edit'}
      <div class="code-editor-wrap">
        <pre
          class="highlight-backdrop"
          aria-hidden="true"
          bind:this={highlightBackdrop}
        ><code class="hljs language-{highlightLanguage}">{@html highlightedContent}</code></pre>
        <textarea
          id="file-content"
          bind:this={editorTextarea}
          bind:value={fileContent}
          class="file-editor"
          rows="22"
          spellcheck="false"
          autocapitalize="off"
          autocomplete="off"
          placeholder={t('repo.editor.content_placeholder')}
          disabled={Boolean(disabledReason)}
          onscroll={syncEditorScroll}
        ></textarea>
      </div>
    {:else if activeTab === 'preview'}
      <div class="preview-pane">
        {#if isMarkdown}
          <div class="markdown-body">
            {@html renderedPreview || `<p>${t('repo.blob.empty')}</p>`}
          </div>
        {:else if fileContent}
          <pre><code>{fileContent}</code></pre>
        {:else}
          <p class="muted">{t('repo.blob.empty')}</p>
        {/if}
      </div>
    {:else}
      <div class="diff-pane">
        {#if changed}
          <table>
            <tbody>
              {#each diffLines as line}
                <tr class={line.type}>
                  <td class="line-number">{line.oldNumber ?? ''}</td>
                  <td class="line-number">{line.newNumber ?? ''}</td>
                  <td class="marker">{line.type === 'add' ? '+' : line.type === 'del' ? '-' : ' '}</td>
                  <td class="line-text">{line.text || ' '}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {:else}
          <p class="muted">{t('repo.editor.no_changes')}</p>
        {/if}
      </div>
    {/if}
  </div>

  <div class="commit-panel">
    <div class="field-row">
      <label for="commit-message">{t('repo.editor.commit_message')}</label>
      <input
        id="commit-message"
        class="text-input"
        bind:value={commitMessage}
        placeholder={t('repo.editor.commit_placeholder')}
      />
    </div>
    <div class="field-row">
      <label for="target-branch">{t('repo.editor.branch')}</label>
      <input
        id="target-branch"
        class="text-input"
        bind:value={targetBranch}
        placeholder={t('repo.editor.branch_placeholder')}
      />
    </div>
  </div>

  <div class="form-actions">
    <button
      type="button"
      class="btn-primary"
      onclick={saveFile}
      disabled={saving || Boolean(disabledReason) || (mode === 'edit' && !changed)}
    >
      {saving
        ? t('repo.editor.saving')
        : mode === 'create'
          ? t('repo.editor.create_file')
          : t('repo.editor.save_changes')}
    </button>
    <a href={cancelHref} class="btn-secondary">{t('common.cancel')}</a>
  </div>
</div>

<style>
  .editor-shell {
    max-width: 1080px;
    margin: 0 auto;
    padding: 24px;
  }

  .editor-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
  }

  .repo-context {
    margin-bottom: 4px;
    color: var(--text-muted);
    font-size: 13px;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  }

  h1 {
    margin: 0;
    font-size: 22px;
    line-height: 1.25;
  }

  .field-row {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-bottom: 14px;
  }

  label {
    font-weight: 600;
    font-size: 13px;
    color: var(--text-secondary);
  }

  .path-input,
  .text-input {
    width: 100%;
    min-height: 38px;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-primary);
    color: var(--text-primary);
    font-size: 14px;
  }

  .path-input {
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
  }

  .path-input[readonly] {
    background: var(--bg-secondary);
    color: var(--text-secondary);
  }

  .editor-panel {
    border: 1px solid var(--border);
    border-radius: 6px;
    overflow: hidden;
    background: var(--bg-secondary);
  }

  .tabbar {
    display: flex;
    gap: 4px;
    padding: 8px;
    border-bottom: 1px solid var(--border);
    background: var(--bg-tertiary);
  }

  .tabbar button {
    border: 1px solid transparent;
    border-radius: 5px;
    background: transparent;
    color: var(--text-secondary);
    padding: 6px 10px;
    cursor: pointer;
    font-size: 13px;
  }

  .tabbar button.active {
    background: var(--bg-primary);
    border-color: var(--border);
    color: var(--text-primary);
  }

  .code-editor-wrap {
    position: relative;
    min-height: 520px;
    background: var(--bg-primary);
  }

  .highlight-backdrop,
  .file-editor {
    width: 100%;
    min-height: 520px;
    padding: 14px;
    margin: 0;
    border: none;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    font-size: 13px;
    line-height: 1.55;
    tab-size: 4;
    white-space: pre;
  }

  .highlight-backdrop {
    position: absolute;
    inset: 0;
    overflow: hidden;
    color: var(--text-primary);
    pointer-events: none;
    user-select: none;
  }

  .highlight-backdrop code {
    font-family: inherit;
    font-size: inherit;
    line-height: inherit;
    white-space: pre;
  }

  .file-editor {
    position: relative;
    z-index: 1;
    resize: vertical;
    background: transparent;
    color: transparent;
    caret-color: var(--text-primary);
    overflow: auto;
  }

  .file-editor::selection {
    background: color-mix(in srgb, var(--accent) 28%, transparent);
    color: transparent;
  }

  .file-editor::placeholder {
    color: var(--text-muted);
  }

  .file-editor:disabled {
    opacity: 0.72;
  }

  .file-editor:focus,
  .path-input:focus,
  .text-input:focus {
    outline: 2px solid color-mix(in srgb, var(--accent) 35%, transparent);
    outline-offset: 1px;
  }

  .preview-pane,
  .diff-pane {
    min-height: 360px;
    max-height: 680px;
    overflow: auto;
    background: var(--bg-primary);
  }

  .preview-pane pre {
    margin: 0;
    padding: 16px;
    white-space: pre-wrap;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    font-size: 13px;
    line-height: 1.55;
  }

  .markdown-body {
    padding: 24px;
    line-height: 1.7;
  }

  .markdown-body :global(h1),
  .markdown-body :global(h2),
  .markdown-body :global(h3) {
    margin-top: 22px;
    margin-bottom: 10px;
    padding-bottom: 8px;
    border-bottom: 1px solid var(--border);
  }

  .markdown-body :global(p) {
    margin-bottom: 12px;
  }

  .markdown-body :global(code) {
    background: var(--bg-tertiary);
    padding: 2px 5px;
    border-radius: 3px;
    font-size: 12px;
  }

  .markdown-body :global(pre code) {
    display: block;
    padding: 12px;
    overflow: auto;
  }

  .diff-pane table {
    width: 100%;
    border-collapse: collapse;
    font-family: ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    font-size: 13px;
    line-height: 1.55;
  }

  .diff-pane tr.add {
    background: color-mix(in srgb, #2da44e 13%, transparent);
  }

  .diff-pane tr.del {
    background: color-mix(in srgb, #cf222e 12%, transparent);
  }

  .line-number {
    width: 1%;
    min-width: 44px;
    padding: 0 8px;
    text-align: right;
    color: var(--text-muted);
    border-right: 1px solid var(--border-light);
    user-select: none;
    white-space: nowrap;
  }

  .marker {
    width: 1%;
    padding: 0 8px;
    color: var(--text-muted);
    user-select: none;
  }

  .line-text {
    padding: 0 12px 0 4px;
    white-space: pre;
  }

  .commit-panel {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(220px, 1fr);
    gap: 16px;
    margin-top: 18px;
  }

  .form-actions {
    display: flex;
    gap: 10px;
    margin-top: 6px;
  }

  .btn-primary,
  .btn-secondary,
  .btn-link {
    min-height: 36px;
    border-radius: 6px;
    padding: 7px 12px;
    font-size: 14px;
    text-decoration: none;
    cursor: pointer;
  }

  .btn-primary {
    border: 1px solid #1f883d;
    background: #1f883d;
    color: white;
    font-weight: 600;
  }

  .btn-primary:disabled {
    opacity: 0.55;
    cursor: not-allowed;
  }

  .btn-secondary {
    border: 1px solid var(--border);
    background: var(--bg-primary);
    color: var(--text-primary);
  }

  .btn-link {
    border: none;
    background: transparent;
    color: var(--accent);
    padding-inline: 0;
  }

  .error-message,
  .conflict-message,
  .warning-message {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin-bottom: 14px;
    padding: 10px 12px;
    border-radius: 6px;
    font-size: 14px;
  }

  .error-message {
    border: 1px solid color-mix(in srgb, #cf222e 30%, transparent);
    background: color-mix(in srgb, #cf222e 9%, transparent);
    color: #cf222e;
  }

  .conflict-message,
  .warning-message {
    border: 1px solid color-mix(in srgb, #bf8700 35%, transparent);
    background: color-mix(in srgb, #bf8700 11%, transparent);
    color: var(--text-primary);
  }

  .muted {
    margin: 0;
    padding: 16px;
    color: var(--text-muted);
  }

  @media (max-width: 720px) {
    .editor-shell {
      padding: 16px;
    }

    .editor-header,
    .commit-panel {
      display: flex;
      flex-direction: column;
    }

    .form-actions {
      flex-direction: column;
    }
  }
</style>
