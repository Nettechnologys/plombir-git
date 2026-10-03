<script lang="ts">
  import { page } from '$app/stores';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import {
    repos,
    type RepositoryFork,
    type Stargazer,
  } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();
  const PER_PAGE = 20;

  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);

  let stargazers = $state<Stargazer[]>([]);
  let stargazersPage = $state(1);
  let stargazersTotalPages = $state(1);
  let stargazersLoading = $state(true);
  let stargazersError = $state('');
  let stargazersRequest = 0;

  let forks = $state<RepositoryFork[]>([]);
  let forksPage = $state(1);
  let forksTotalPages = $state(1);
  let forksLoading = $state(true);
  let forksError = $state('');
  let forksRequest = 0;

  $effect(() => {
    void loadStargazers(owner, repo, stargazersPage);
  });

  $effect(() => {
    void loadForks(owner, repo, forksPage);
  });

  async function loadStargazers(requestedOwner: string, requestedRepo: string, requestedPage: number) {
    const requestId = ++stargazersRequest;
    stargazersLoading = true;
    stargazersError = '';

    try {
			const response = await repos.stargazers(requestedOwner, requestedRepo, requestedPage, PER_PAGE);
      if (requestId !== stargazersRequest) return;
      stargazers = response.data;
      stargazersTotalPages = Math.max(1, response.pagination?.total_pages ?? 1);
    } catch (error: any) {
      if (requestId !== stargazersRequest) return;
      stargazers = [];
      stargazersTotalPages = 1;
      stargazersError = error?.message || t('repo.social.stargazers_load_failed');
    } finally {
      if (requestId === stargazersRequest) stargazersLoading = false;
    }
  }

  async function loadForks(requestedOwner: string, requestedRepo: string, requestedPage: number) {
    const requestId = ++forksRequest;
    forksLoading = true;
    forksError = '';

    try {
			const response = await repos.forks(requestedOwner, requestedRepo, requestedPage, PER_PAGE);
      if (requestId !== forksRequest) return;
      forks = response.data;
      forksTotalPages = Math.max(1, response.pagination?.total_pages ?? 1);
    } catch (error: any) {
      if (requestId !== forksRequest) return;
      forks = [];
      forksTotalPages = 1;
      forksError = error?.message || t('repo.social.forks_load_failed');
    } finally {
      if (requestId === forksRequest) forksLoading = false;
    }
  }
</script>

<svelte:head>
  <title>{t('repo.social.title')} · {owner}/{repo} · Plombir Git</title>
</svelte:head>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="network" />

  <div class="page-header">
    <h1>{t('repo.social.title')}</h1>
  </div>

  <div class="social-grid">
    <section class="social-panel" aria-labelledby="stargazers-title" aria-busy={stargazersLoading}>
      <h2 id="stargazers-title">⭐ {t('repo.stargazers')}</h2>

      {#if stargazersLoading}
        <p class="state" aria-live="polite">{t('common.loading')}</p>
      {:else if stargazersError}
        <div class="error-banner" role="alert">
          <span>{stargazersError}</span>
          <button type="button" class="retry" onclick={() => loadStargazers(owner, repo, stargazersPage)}>
            {t('common.retry')}
          </button>
        </div>
      {:else if stargazers.length === 0}
        <p class="state empty">{t('repo.social.stargazers_empty')}</p>
      {:else}
        <ul class="social-list">
          {#each stargazers as stargazer (stargazer.user_id)}
            <li>
              <a class="identity" href={`/${stargazer.username}`}>
                {#if stargazer.avatar_url}
                  <img class="avatar" src={stargazer.avatar_url} alt="" />
                {:else}
                  <span class="avatar fallback" aria-hidden="true">
                    {stargazer.username.slice(0, 1).toUpperCase()}
                  </span>
                {/if}
                <span>
                  <strong>{stargazer.display_name || stargazer.username}</strong>
                  <small>@{stargazer.username}</small>
                </span>
              </a>
              <small>{t('repo.social.starred_on', { date: formatDate(stargazer.starred_at) })}</small>
            </li>
          {/each}
        </ul>

        {#if stargazersTotalPages > 1}
          <nav class="pagination" aria-label={t('repo.stargazers')}>
            <button
              type="button"
              disabled={stargazersPage <= 1 || stargazersLoading}
              onclick={() => stargazersPage -= 1}
            >{t('common.previous')}</button>
            <span>{t('repo.social.page', { current: stargazersPage, total: stargazersTotalPages })}</span>
            <button
              type="button"
              disabled={stargazersPage >= stargazersTotalPages || stargazersLoading}
              onclick={() => stargazersPage += 1}
            >{t('common.next')}</button>
          </nav>
        {/if}
      {/if}
    </section>

    <section class="social-panel" aria-labelledby="forks-title" aria-busy={forksLoading}>
      <h2 id="forks-title">⑂ {t('repo.forks')}</h2>

      {#if forksLoading}
        <p class="state" aria-live="polite">{t('common.loading')}</p>
      {:else if forksError}
        <div class="error-banner" role="alert">
          <span>{forksError}</span>
          <button type="button" class="retry" onclick={() => loadForks(owner, repo, forksPage)}>
            {t('common.retry')}
          </button>
        </div>
      {:else if forks.length === 0}
        <p class="state empty">{t('repo.social.forks_empty')}</p>
      {:else}
        <ul class="social-list fork-list">
          {#each forks as fork (fork.id)}
            <li>
              <div class="fork-main">
                <a class="repo-link" href={`/${fork.owner_name}/${fork.name}`}>
                  {fork.owner_name}/{fork.name}
                </a>
                {#if fork.is_private}<span class="badge">{t('repo.private')}</span>{/if}
                <p>{fork.description || t('common.no_description')}</p>
              </div>
              <small>
                ⭐ {fork.stars_count} · ⑂ {fork.forks_count} · {t('common.updated', { date: formatDate(fork.updated_at) })}
              </small>
            </li>
          {/each}
        </ul>

        {#if forksTotalPages > 1}
          <nav class="pagination" aria-label={t('repo.forks')}>
            <button
              type="button"
              disabled={forksPage <= 1 || forksLoading}
              onclick={() => forksPage -= 1}
            >{t('common.previous')}</button>
            <span>{t('repo.social.page', { current: forksPage, total: forksTotalPages })}</span>
            <button
              type="button"
              disabled={forksPage >= forksTotalPages || forksLoading}
              onclick={() => forksPage += 1}
            >{t('common.next')}</button>
          </nav>
        {/if}
      {/if}
    </section>
  </div>
</div>

<style>
  .page-header { margin-bottom: 24px; }
  h1 { font-size: 24px; font-weight: 600; }
  h2 { margin: 0; padding: 16px 20px; border-bottom: 1px solid var(--border); font-size: 18px; }

  .social-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 20px;
    align-items: start;
  }

  .social-panel {
    min-width: 0;
    overflow: hidden;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }

  .state { margin: 0; padding: 40px 20px; text-align: center; color: var(--text-secondary); }
  .empty { color: var(--text-muted); }

  .error-banner {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    margin: 16px;
  }

  .retry,
  .pagination button {
    flex-shrink: 0;
    padding: 5px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
    color: var(--text-primary);
    cursor: pointer;
  }
  .retry:hover,
  .pagination button:hover:not(:disabled) { background: var(--bg-hover); }
  .pagination button:disabled { cursor: not-allowed; opacity: 0.5; }

  .social-list { list-style: none; margin: 0; padding: 0; }
  .social-list > li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 14px 20px;
    border-bottom: 1px solid var(--border);
  }
  .social-list > li:last-child { border-bottom: 0; }
  .social-list small { flex-shrink: 0; color: var(--text-muted); font-size: 12px; }

  .identity { display: flex; align-items: center; min-width: 0; gap: 12px; color: var(--text-primary); }
  .identity:hover { text-decoration: none; }
  .identity strong,
  .identity small { display: block; overflow: hidden; text-overflow: ellipsis; }
  .identity:hover strong { color: var(--accent); text-decoration: underline; }

  .avatar {
    width: 36px;
    height: 36px;
    flex-shrink: 0;
    border-radius: 50%;
    object-fit: cover;
  }
  .avatar.fallback {
    display: grid;
    place-items: center;
    background: var(--accent-weak);
    color: var(--accent);
    font-weight: 700;
  }

  .fork-list > li { align-items: flex-start; }
  .fork-main { min-width: 0; }
  .repo-link { overflow-wrap: anywhere; font-weight: 600; }
  .fork-main p { margin: 5px 0 0; color: var(--text-secondary); font-size: 13px; }
  .badge {
    display: inline-block;
    margin-left: 8px;
    padding: 1px 7px;
    border: 1px solid var(--border);
    border-radius: 10px;
    color: var(--text-muted);
    font-size: 11px;
  }

  .pagination {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 14px 20px;
    border-top: 1px solid var(--border);
    color: var(--text-secondary);
    font-size: 13px;
  }

  @media (max-width: 900px) {
    .social-grid { grid-template-columns: 1fr; }
  }

  @media (max-width: 600px) {
    .social-list > li { align-items: flex-start; flex-direction: column; }
    .social-list small { flex-shrink: 1; }
    .error-banner { align-items: flex-start; flex-direction: column; }
  }
</style>
