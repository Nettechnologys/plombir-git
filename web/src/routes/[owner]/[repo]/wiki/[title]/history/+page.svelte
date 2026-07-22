<script lang="ts">
  import { page } from '$app/stores';
  import { onMount } from 'svelte';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { wiki } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let title = $derived($page.params.title!);
  let revisions = $state<any[]>([]);
  let currentRev = $state<any>(null);
  let loading = $state(true);
  let error = $state('');
  let viewingRev = $state(false);

  onMount(() => loadHistory());

  async function loadHistory() {
    try {
      loading = true;
      revisions = await wiki.listRevisions(owner, repo, title);
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function viewRevision(rev: any) {
    try {
      currentRev = await wiki.getRevision(owner, repo, title, rev.id);
      viewingRev = true;
    } catch (e: any) {
      error = e.message;
    }
  }

  function closeRevision() {
    viewingRev = false;
    currentRev = null;
  }
</script>

<RepoHeader {owner} {repo} activeTab="wiki" />

<div class="content">
  <div class="header">
    <div class="breadcrumb">
      <a href={`/${owner}/${repo}/wiki`} class="link">{t('wiki.title')}</a>
      <span class="sep">/</span>
      <a href={`/${owner}/${repo}/wiki/${encodeURIComponent(title)}`} class="link">{title}</a>
      <span class="sep">/</span>
      <span class="current">{t('wiki.history')}</span>
    </div>
  </div>

  {#if loading}
    <div class="loading">{t('common.loading')}...</div>
  {:else if error}
    <div class="error">{error}</div>
  {:else}
    {#if viewingRev && currentRev}
      <div class="revision-detail">
        <div class="rev-meta">
          <h3>Revision #{currentRev.id}</h3>
          <p class="rev-info">
            {formatDate(currentRev.created_at)}
            {#if currentRev.message}
              &mdash; {currentRev.message}
            {/if}
          </p>
          <button class="btn btn-secondary" onclick={closeRevision}>
            {t('common.close')}
          </button>
        </div>
        <div class="rev-content">
          <pre class="wiki-content">{currentRev.content}</pre>
        </div>
      </div>
    {:else}
      <div class="revision-list">
        <table class="rev-table">
          <thead>
            <tr>
              <th>#</th>
              <th>{t('wiki.revisionMessage')}</th>
              <th>{t('common.date')}</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            {#each revisions as rev, i}
              <tr>
                <td class="rev-id">r{revisions.length - i}</td>
                <td class="rev-msg">
                  {rev.message || t('wiki.noMessage')}
                </td>
                <td class="rev-date">{formatDate(rev.created_at)}</td>
                <td class="rev-action">
                  <button class="btn btn-sm" onclick={() => viewRevision(rev)}>
                    {t('common.view')}
                  </button>
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
        {#if revisions.length === 0}
          <p class="empty">{t('wiki.noRevisions')}</p>
        {/if}
      </div>
    {/if}
  {/if}
</div>

<style>
  .content {
    max-width: 900px;
    margin: 0 auto;
    padding: 1.5rem;
  }
  .header {
    margin-bottom: 1.5rem;
  }
  .breadcrumb {
    font-size: 0.9rem;
    color: var(--text-secondary, #666);
  }
  .breadcrumb a {
    color: var(--link-color, #2563eb);
    text-decoration: none;
  }
  .breadcrumb a:hover { text-decoration: underline; }
  .breadcrumb .sep { margin: 0 0.4rem; }
  .breadcrumb .current { color: var(--text-primary, #333); font-weight: 600; }

  .loading, .error, .empty {
    text-align: center;
    padding: 2rem;
    color: var(--text-secondary, #666);
  }
  .error { color: #dc2626; }

  .rev-table {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.9rem;
  }
  .rev-table th {
    text-align: left;
    padding: 0.5rem 0.75rem;
    border-bottom: 2px solid var(--border-color, #e5e7eb);
    color: var(--text-secondary, #666);
    font-weight: 600;
  }
  .rev-table td {
    padding: 0.5rem 0.75rem;
    border-bottom: 1px solid var(--border-color, #e5e7eb);
  }
  .rev-id {
    color: var(--text-secondary, #666);
    font-family: monospace;
    width: 3rem;
  }
  .rev-msg { max-width: 400px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .rev-date { color: var(--text-secondary, #666); width: 10rem; }

  .revision-detail { margin-top: 1rem; }
  .rev-meta {
    background: var(--bg-secondary, #f9fafb);
    padding: 1rem;
    border-radius: 8px;
    margin-bottom: 1rem;
    display: flex;
    align-items: center;
    gap: 1rem;
    flex-wrap: wrap;
  }
  .rev-meta h3 { margin: 0; font-size: 1rem; }
  .rev-info { margin: 0; color: var(--text-secondary, #666); font-size: 0.85rem; }
  .rev-content {
    background: var(--bg-secondary, #f9fafb);
    padding: 1rem;
    border-radius: 8px;
    overflow-x: auto;
  }
  .wiki-content {
    white-space: pre-wrap;
    word-wrap: break-word;
    font-family: inherit;
    font-size: 0.9rem;
    margin: 0;
    line-height: 1.6;
  }

  .btn {
    padding: 0.4rem 1rem;
    border: 1px solid var(--border-color, #d1d5db);
    border-radius: 6px;
    background: var(--bg-primary, #fff);
    cursor: pointer;
    font-size: 0.85rem;
    color: var(--text-primary, #333);
  }
  .btn:hover { background: var(--bg-secondary, #f3f4f6); }
  .btn-sm { padding: 0.2rem 0.6rem; font-size: 0.8rem; }
  .btn-secondary { background: var(--bg-secondary, #f3f4f6); }
</style>
