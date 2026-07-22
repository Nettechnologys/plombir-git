<script lang="ts">
  import { page } from '$app/stores';
  import { repos } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repoList = $state<any[]>([]);
  let userInfo = $state<any>(null);
  let loading = $state(true);
  let error = $state('');

  $effect(() => {
    loadOwner();
  });

  async function loadOwner() {
    loading = true;
    error = '';
    try {
      const result = await repos.list(owner);
      repoList = result.data;
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
  }
</script>

<svelte:head>
  <title>{owner} · IronForge</title>
</svelte:head>

<div class="page-container-narrow">
  <div class="profile-header">
    <div class="avatar">👤</div>
    <div class="info">
      <h1>{owner}</h1>
    </div>
  </div>

  {#if error}
    <div class="error-banner">{error}</div>
  {/if}

  {#if loading}
    <p class="text-secondary">{t('common.loading')}</p>
  {:else if repoList.length === 0}
    <div class="empty">
      <p>{t('profile.no_repos')}</p>
    </div>
  {:else}
    <div class="repo-list">
      {#each repoList as repo}
        <a href={`/${owner}/${repo.name}`} class="repo-item">
          <div class="repo-icon">
            {repo.is_private ? '🔒' : '📂'}
          </div>
          <div class="repo-info">
            <div class="repo-name">
              {owner}/{repo.name}
              {#if repo.is_private}
                <span class="badge">{t('profile.private')}</span>
              {/if}
            </div>
            <div class="repo-desc">{repo.description || t('common.no_description')}</div>
            <div class="repo-meta">
              {repo.stars_count || 0} ⭐ · {t('common.updated', { date: formatDate(repo.updated_at) })}
            </div>
          </div>
        </a>
      {/each}
    </div>
  {/if}
</div>

<style>
  .profile-header {
    display: flex;
    align-items: center;
    gap: 16px;
    margin-bottom: 32px;
    padding-bottom: 24px;
    border-bottom: 1px solid var(--border);
  }

  .avatar {
    width: 64px;
    height: 64px;
    border-radius: 50%;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 28px;
  }

  .info h1 {
    font-size: 22px;
    margin: 0;
  }

  .empty {
    text-align: center;
    padding: 60px 24px;
    color: var(--text-secondary);
  }

  .repo-list {
    display: flex;
    flex-direction: column;
    gap: 0;
  }

  .repo-item {
    display: flex;
    align-items: flex-start;
    gap: 12px;
    padding: 16px 20px;
    border-bottom: 1px solid var(--border-light);
    text-decoration: none;
    color: var(--text-primary);
  }
  .repo-item:hover { background: var(--bg-secondary); text-decoration: none; }
  .repo-item:first-child { border-top: 1px solid var(--border-light); }

  .repo-icon { font-size: 20px; margin-top: 2px; }

  .repo-info { flex: 1; }

  .repo-name {
    font-weight: 600;
    font-size: 15px;
    color: var(--accent);
  }

  .badge {
    font-size: 11px;
    font-weight: 500;
    padding: 1px 6px;
    border: 1px solid var(--border);
    border-radius: 10px;
    color: var(--text-secondary);
    margin-left: 8px;
    vertical-align: middle;
  }

  .repo-desc {
    font-size: 13px;
    color: var(--text-secondary);
    margin-top: 2px;
  }

  .repo-meta {
    font-size: 12px;
    color: var(--text-muted);
    margin-top: 6px;
  }
</style>
