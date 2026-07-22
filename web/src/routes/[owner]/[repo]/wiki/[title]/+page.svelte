<script lang="ts">
  import { page } from '$app/stores';
  import { onMount } from 'svelte';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { wiki } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';
  import { renderMarkdown } from '$lib/utils/markdown';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let title = $derived($page.params.title!);
  let wikiPage = $state<any>(null);
  let allPages = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let editing = $state(false);
  let editContent = $state('');
  let renderedHtml = $state('');
  let toc = $state<{ id: string; text: string; level: number }[]>([]);
  // History
  let showHistory = $state(false);
  let revisions = $state<any[]>([]);
  let historyLoading = $state(false);
  let viewingRevision = $state<any | null>(null);

  $effect(() => { loadPage(); });

  async function loadPage() {
    try {
      loading = true;
      const [pageData, pages] = await Promise.all([
        wiki.get(owner, repo, title),
        wiki.list(owner, repo),
      ]);
      wikiPage = pageData;
      allPages = pages;
      renderContent(pageData.content);
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  function renderContent(content: string) {
    if (!content) { renderedHtml = ''; toc = []; return; }

    // Extract headings for TOC before rendering
    const headingRegex = /^(#{1,6})\s+(.+)$/gm;
    const headings: { id: string; text: string; level: number }[] = [];
    let match;
    while ((match = headingRegex.exec(content)) !== null) {
      const text = match[2].trim();
      const id = text.toLowerCase().replace(/[^\w\u4e00-\u9fff]+/g, '-').replace(/^-|-$/g, '');
      headings.push({ id, text, level: match[1].length });
    }
    toc = headings;

    // Add IDs to headings in rendered HTML
    const html = renderMarkdown(content);
    // Inject IDs into heading tags
    let idIndex = 0;
    renderedHtml = html.replace(/<(h[1-6])>(.*?)<\/h[1-6]>/g, (_match, tag, text) => {
      const plainText = text.replace(/<[^>]*>/g, '');
      const id = headings[idIndex]?.id || plainText.toLowerCase().replace(/[^\w]+/g, '-');
      idIndex++;
      return `<${tag} id="${id}">${text}</${tag}>`;
    });
  }

  async function handleSave() {
    try {
      await wiki.update(owner, repo, title, editContent);
      editing = false;
      await loadPage();
    } catch (e: any) {
      error = e.message;
    }
  }

  async function handleDelete() {
    if (!confirm('Delete this wiki page? This cannot be undone.')) return;
    try {
      await wiki.remove(owner, repo, title);
      window.location.href = `/${owner}/${repo}/wiki`;
    } catch (e: any) {
      error = e.message;
    }
  }

  function startEditing() {
    editContent = wikiPage?.content || '';
    editing = true;
  }

  function scrollToHeading(id: string) {
    const el = document.getElementById(id);
    if (el) el.scrollIntoView({ behavior: 'smooth' });
  }

  async function toggleHistory() {
    showHistory = !showHistory;
    viewingRevision = null;
    if (showHistory && revisions.length === 0) {
      historyLoading = true;
      try {
        revisions = await wiki.history(owner, repo, title);
      } catch {
        revisions = [];
      } finally {
        historyLoading = false;
      }
    }
  }

  async function viewRevision(rev: any) {
    if (viewingRevision?.id === rev.id) {
      viewingRevision = null;
      return;
    }
    try {
      const full = await wiki.revision(owner, repo, title, rev.id);
      viewingRevision = full;
    } catch {
      viewingRevision = rev;
    }
  }

  async function restoreRevision(rev: any) {
    if (!confirm(`Restore version ${rev.version}? Current content will become a revision.`)) return;
    try {
      await wiki.update(owner, repo, title, viewingRevision?.content ?? rev.content);
      showHistory = false;
      revisions = [];
      viewingRevision = null;
      await loadPage();
    } catch (e: any) {
      error = e.message;
    }
  }
</script>

<svelte:head>
  <title>{title} · {owner}/{repo} Wiki · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="wiki" />

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if wikiPage}
    <div class="wiki-layout">
      <!-- Sidebar -->
      <aside class="wiki-sidebar">
        <div class="sidebar-section">
          <h3>{t('wiki.pages')}</h3>
          <nav class="page-nav">
            {#each allPages as p}
              <a href={`/${owner}/${repo}/wiki/${encodeURIComponent(p.title)}`} class="page-link" class:active={p.title === title}>
                <span class="page-icon">📄</span>
                <span>{p.title}</span>
              </a>
            {/each}
          </nav>
        </div>

        {#if toc.length > 0}
          <div class="sidebar-section">
            <h3>{t('wiki.toc') || 'Table of Contents'}</h3>
            <nav class="toc-nav">
              {#each toc as heading}
                <button
                  class="toc-link"
                  style="padding-left: {heading.level * 12}px"
                  onclick={() => scrollToHeading(heading.id)}
                >
                  {heading.text}
                </button>
              {/each}
            </nav>
          </div>
        {/if}
      </aside>

      <!-- Main content -->
      <main class="wiki-main">
        <div class="wiki-header">
          <h1>{title}</h1>
          <div class="header-actions">
            {#if wikiPage.updated_at}
              <span class="text-secondary text-sm">Last edited {formatDate(wikiPage.updated_at)}</span>
            {/if}
            <button class="btn-outline" onclick={toggleHistory} class:active={showHistory}>History</button>
            <button class="btn-outline" onclick={startEditing}>{t('wiki.edit')}</button>
            <button class="btn-outline btn-danger" onclick={handleDelete}>{t('wiki.delete') || 'Delete'}</button>
          </div>
        </div>

        {#if showHistory}
          <div class="history-panel">
            <h3>Revision History</h3>
            {#if historyLoading}
              <p class="text-secondary">Loading…</p>
            {:else if revisions.length === 0}
              <p class="text-secondary">No revisions yet. Revisions are saved on every edit.</p>
            {:else}
              <div class="revision-list">
                {#each revisions as rev}
                  <div class="revision-item" class:expanded={viewingRevision?.id === rev.id}>
                    <button class="revision-header" onclick={() => viewRevision(rev)}>
                      <span class="rev-version">v{rev.version}</span>
                      <span class="rev-msg">{rev.message || 'No message'}</span>
                      <span class="rev-date">{formatDate(rev.created_at)}</span>
                      <span class="rev-arrow">{viewingRevision?.id === rev.id ? '▲' : '▼'}</span>
                    </button>
                    {#if viewingRevision?.id === rev.id}
                      <div class="revision-content">
                        <pre class="rev-preview">{viewingRevision.content}</pre>
                        <button class="btn-primary btn-sm" onclick={() => restoreRevision(rev)}>
                          Restore this version
                        </button>
                      </div>
                    {/if}
                  </div>
                {/each}
              </div>
            {/if}
          </div>
        {:else if editing}
          <div class="edit-area">
            <textarea bind:value={editContent} rows="20"></textarea>
            <div class="form-actions">
              <button class="btn-primary" onclick={handleSave}>{t('wiki.save')}</button>
              <button class="btn-secondary" onclick={() => editing = false}>{t('wiki.cancel')}</button>
            </div>
          </div>
        {:else}
          <div class="wiki-content markdown-body">
            {@html renderedHtml}
          </div>
        {/if}

        <div class="wiki-footer">
          <a href={`/${owner}/${repo}/wiki`} class="back-link">← {t('wiki.back') || 'Back to Wiki'}</a>
        </div>
      </main>
    </div>
  {:else}
    <div class="empty"><p>{t('wiki.not_found') || 'Page not found'}</p></div>
  {/if}
</div>

<style>
.empty { text-align: center; padding: 48px; color: var(--text-secondary); }

  /* ── Layout ── */
  .wiki-layout {
    display: grid;
    grid-template-columns: 240px 1fr;
    gap: 24px;
    align-items: start;
  }
  @media (max-width: 900px) { .wiki-layout { grid-template-columns: 1fr; } }

  /* ── Sidebar ── */
  .wiki-sidebar {
    position: sticky;
    top: 24px;
  }
  .sidebar-section {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 12px;
    margin-bottom: 12px;
  }
  .sidebar-section h3 {
    font-size: 12px;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--text-muted);
    margin: 0 0 8px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--border-light);
  }

  .page-nav { display: flex; flex-direction: column; gap: 2px; max-height: 300px; overflow-y: auto; }
  .page-link {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 8px;
    border-radius: 4px;
    font-size: 13px;
    text-decoration: none;
    color: var(--text-primary);
  }
  .page-link:hover { background: var(--bg-hover); }
  .page-link.active { background: var(--bg-tertiary); font-weight: 600; }
  .page-icon { font-size: 12px; }

  .toc-nav { display: flex; flex-direction: column; gap: 2px; max-height: 400px; overflow-y: auto; }
  .toc-link {
    display: block;
    padding: 3px 8px;
    border-radius: 3px;
    font-size: 12px;
    color: var(--text-secondary);
    cursor: pointer;
    text-align: left;
    background: none;
    border: none;
    width: 100%;
  }
  .toc-link:hover { color: var(--accent); background: var(--bg-hover); }

  /* ── Main ── */
  .wiki-main {
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 24px;
    min-height: 400px;
  }

  .wiki-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 12px;
    margin-bottom: 20px;
    border-bottom: 1px solid var(--border);
    padding-bottom: 12px;
  }
  h1 { font-size: 24px; margin: 0; }
  .header-actions { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .text-sm { font-size: 12px; }

  .btn-outline {
    padding: 5px 12px; background: none; border: 1px solid var(--border);
    border-radius: var(--radius); color: var(--text-primary); font-size: 13px; cursor: pointer;
  }
  .btn-outline:hover { background: var(--bg-hover); }
  .btn-danger { border-color: var(--red-dim); color: var(--red); }
  .btn-danger:hover { background: rgba(248, 81, 73, 0.1); }

  .btn-primary {
    padding: 6px 16px; background: var(--accent); color: #fff;
    border: none; border-radius: var(--radius); font-size: 13px; cursor: pointer;
  }
  .btn-secondary {
    padding: 6px 16px; background: var(--bg-tertiary); color: var(--text-primary);
    border: 1px solid var(--border); border-radius: var(--radius); font-size: 13px; cursor: pointer;
  }

  .edit-area { margin-top: 8px; }
  textarea {
    width: 100%; padding: 12px; border: 1px solid var(--border);
    border-radius: var(--radius); font-size: 13px; font-family: var(--font-mono);
    background: var(--bg-primary); color: var(--text-primary);
    resize: vertical; box-sizing: border-box;
  }
  .form-actions { display: flex; gap: 8px; margin-top: 12px; }

  /* ── Markdown Content ── */
  :global(.wiki-main .markdown-body) {
    line-height: 1.7;
    font-size: 15px;
    color: var(--text-primary);
    overflow-x: auto;
  }
  :global(.wiki-main .markdown-body h1), :global(.wiki-main .markdown-body h2), :global(.wiki-main .markdown-body h3),
  :global(.wiki-main .markdown-body h4), :global(.wiki-main .markdown-body h5), :global(.wiki-main .markdown-body h6) {
    margin: 1.2em 0 0.6em;
    color: var(--text-primary);
    scroll-margin-top: 24px;
  }
  :global(.wiki-main .markdown-body h1) { font-size: 1.8em; border-bottom: 1px solid var(--border-light); padding-bottom: 6px; }
  :global(.wiki-main .markdown-body h2) { font-size: 1.5em; border-bottom: 1px solid var(--border-light); padding-bottom: 4px; }
  :global(.wiki-main .markdown-body h3) { font-size: 1.25em; }
  :global(.wiki-main .markdown-body p) { margin: 0 0 12px; }
  :global(.wiki-main .markdown-body a) { color: var(--accent); }
  :global(.wiki-main .markdown-body code) {
    background: var(--bg-tertiary); padding: 2px 6px; border-radius: 3px;
    font-size: 13px; font-family: var(--font-mono);
  }
  :global(.wiki-main .markdown-body pre) {
    background: #1a1a2e; color: #e0e0e0; padding: 14px; border-radius: 6px;
    overflow-x: auto; font-size: 13px; line-height: 1.5;
  }
  :global(.wiki-main .markdown-body pre code) { background: none; padding: 0; }
  :global(.wiki-main .markdown-body blockquote) {
    border-left: 4px solid var(--accent); padding: 4px 16px; margin: 12px 0;
    color: var(--text-secondary); background: var(--bg-primary); border-radius: 0 4px 4px 0;
  }
  :global(.wiki-main .markdown-body table) { border-collapse: collapse; width: 100%; margin: 12px 0; }
  :global(.wiki-main .markdown-body th), :global(.wiki-main .markdown-body td) {
    border: 1px solid var(--border); padding: 8px 12px; text-align: left; font-size: 14px;
  }
  :global(.wiki-main .markdown-body th) { background: var(--bg-tertiary); font-weight: 600; }
  :global(.wiki-main .markdown-body ul), :global(.wiki-main .markdown-body ol) { padding-left: 24px; margin: 8px 0; }
  :global(.wiki-main .markdown-body li) { margin: 4px 0; }
  :global(.wiki-main .markdown-body img) { max-width: 100%; border-radius: 4px; }
  :global(.wiki-main .markdown-body hr) { border: none; border-top: 1px solid var(--border); margin: 24px 0; }

  .wiki-footer { margin-top: 32px; padding-top: 16px; border-top: 1px solid var(--border-light); }
  .back-link { color: var(--accent); text-decoration: none; font-size: 14px; }
  .back-link:hover { text-decoration: underline; }

  .btn-outline.active { background: var(--bg-tertiary); border-color: var(--accent); color: var(--accent); }

  /* ── History panel ── */
  .history-panel { margin-top: 8px; }
  .history-panel h3 { font-size: 16px; font-weight: 600; margin: 0 0 16px; }
  .revision-list { display: flex; flex-direction: column; gap: 6px; }
  .revision-item { border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden; }
  .revision-item.expanded { border-color: var(--accent); }
  .revision-header {
    display: flex; align-items: center; gap: 12px; padding: 10px 14px;
    background: var(--bg-primary); border: none; width: 100%; text-align: left; cursor: pointer;
  }
  .revision-header:hover { background: var(--bg-secondary); }
  .rev-version { font-size: 12px; font-weight: 700; color: var(--accent); min-width: 30px; }
  .rev-msg { flex: 1; font-size: 13px; color: var(--text-primary); }
  .rev-date { font-size: 12px; color: var(--text-muted); white-space: nowrap; }
  .rev-arrow { font-size: 10px; color: var(--text-muted); }
  .revision-content { padding: 12px 14px; border-top: 1px solid var(--border); background: var(--bg-secondary); }
  .rev-preview {
    font-size: 12px; font-family: var(--font-mono); color: var(--text-secondary);
    max-height: 200px; overflow-y: auto; white-space: pre-wrap; word-break: break-all;
    background: var(--bg-primary); border: 1px solid var(--border); border-radius: var(--radius);
    padding: 10px; margin-bottom: 10px;
  }
  .btn-sm { padding: 4px 10px; font-size: 12px; }
</style>
