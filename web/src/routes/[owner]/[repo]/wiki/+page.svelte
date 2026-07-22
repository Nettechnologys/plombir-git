<script lang="ts">
  import { page } from '$app/stores';
  import { goto } from '$app/navigation';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { wiki } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repo = $derived($page.params.repo!);
  let pageList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let showCreate = $state(false);
  let newTitle = $state('');
  let newContent = $state('');

  $effect(() => { loadPages(); });

  async function loadPages() {
    try {
      loading = true;
      pageList = await wiki.list(owner, repo);
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function handleCreate(e: Event) {
    e.preventDefault();
    try {
      const createdTitle = newTitle;
      await wiki.create(owner, repo, createdTitle, newContent);
      showCreate = false;
      newTitle = '';
      newContent = '';
      await goto(`/${owner}/${repo}/wiki/${encodeURIComponent(createdTitle)}`);
    } catch (e: any) {
      error = e.message;
    }
  }
</script>

<svelte:head>
  <title>Wiki · {owner}/{repo} · IronForge</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="wiki" starsCount={0} />

  <div class="toolbar">
    <h2>{t('wiki.pages')}</h2>
    <button class="btn-primary" onclick={() => showCreate = !showCreate}>{t('wiki.new')}</button>
  </div>

  {#if showCreate}
    <div class="create-form">
      <form onsubmit={handleCreate}>
        <label>
          {t('wiki.create_form.title')}
          <input type="text" bind:value={newTitle} required placeholder={t('wiki.create_form.title_placeholder')} />
        </label>
        <label>
          {t('wiki.create_form.content')}
          <textarea bind:value={newContent} rows="8" required placeholder={t('wiki.create_form.content_placeholder')}></textarea>
        </label>
        <div class="form-actions">
          <button type="submit" class="btn-primary">{t('wiki.create_form.submit')}</button>
          <button type="button" class="btn-secondary" onclick={() => showCreate = false}>{t('wiki.create_form.cancel')}</button>
        </div>
      </form>
    </div>
  {/if}

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if pageList.length === 0}
    <div class="empty"><p>{t('wiki.empty')}</p></div>
  {:else}
    <div class="page-list">
      {#each pageList as p}
        <a href={`/${owner}/${repo}/wiki/${encodeURIComponent(p.title)}`} class="wiki-item">
          📄 {p.title}
          <span class="text-secondary text-sm">{formatDate(p.updated_at)}</span>
        </a>
      {/each}
    </div>
  {/if}
</div>

<style>
  .toolbar { display: flex; align-items: center; justify-content: space-between; margin-bottom: 16px; }
  h2 { font-size: 18px; }

  .btn-primary { padding: 6px 16px; background: var(--green-dim); color: #fff; border: none; border-radius: var(--radius); font-size: 14px; font-weight: 600; cursor: pointer; }
  .btn-primary:hover { background: var(--green); }
  .btn-secondary { padding: 6px 16px; background: none; color: var(--text-primary); border: 1px solid var(--border); border-radius: var(--radius); font-size: 14px; cursor: pointer; }

  .create-form { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: var(--radius-lg); padding: 24px; margin-bottom: 24px; }
  form { display: flex; flex-direction: column; gap: 14px; }
  label { display: flex; flex-direction: column; gap: 6px; font-size: 13px; font-weight: 600; }
  textarea { font-family: var(--font-mono); font-size: 13px; resize: vertical; }
  .form-actions { display: flex; gap: 8px; margin-top: 8px; }
.empty { text-align: center; padding: 48px; color: var(--text-secondary); }

  .page-list { border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden; }
  .wiki-item {
    display: flex; align-items: center; justify-content: space-between;
    padding: 12px 16px; border-bottom: 1px solid var(--border-light);
    text-decoration: none; color: var(--text-primary); font-size: 14px;
  }
  .wiki-item:last-child { border-bottom: none; }
  .wiki-item:hover { background: var(--bg-secondary); text-decoration: none; }
</style>
