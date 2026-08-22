<script lang="ts">
  import { page } from '$app/stores';
  import { orgs, repos, type Organization } from '$lib/api/client.svelte';
  import { getUser, isLoggedIn } from '$lib/stores/auth.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let owner = $derived($page.params.owner!);
  let repoList = $state<any[]>([]);
  // Set once the owner turns out to be an organization rather than an account.
  let org = $state<Organization | null>(null);
  // Whether this viewer may create a repository in this namespace.
  let canCreate = $state(false);
  let loading = $state(true);
  let error = $state('');

  $effect(() => {
    loadOwner();
  });

  async function loadOwner() {
    loading = true;
    error = '';
    org = null;
    canCreate = false;
    const name = owner;
    try {
      const result = await repos.list(name);
      repoList = result.data;
    } catch (e: any) {
      error = e.message;
    } finally {
      loading = false;
    }
    await resolveNamespace(name);
  }

  /**
   * Which kind of owner this page is showing, and whether the viewer may add a
   * repository to it. The route serves users and organizations alike, and used
   * to render both as a bare list of repositories: an organization reached from
   * a repository's breadcrumb was a dead end, with no way back to the page that
   * owns it and no way to create anything.
   *
   * Every step is allowed to fail quietly. A name that is not a visible
   * organization is simply a user profile, and a listing this instance refuses
   * leaves the action hidden — a button that promises a 403 is worse than no
   * button.
   */
  async function resolveNamespace(name: string) {
    let found: Organization;
    try {
      found = await orgs.get(name);
    } catch (_) {
      // A user profile: only its own owner gets the create action.
      if (name === owner) canCreate = isLoggedIn() && getUser()?.username === name;
      return;
    }
    // The viewer may have navigated on while the lookup was in flight.
    if (name !== owner) return;
    org = found;
    if (!isLoggedIn()) return;
    try {
      const mine = await orgs.list();
      if (name !== owner) return;
      // Membership is the API's own rule for creating under an organization —
      // `NamespaceCreate` admits any member, not only an admin.
      canCreate = mine.some((candidate) => candidate.name === name);
    } catch (_) {
      // Leave the action hidden.
    }
  }
</script>

<svelte:head>
  <title>{owner} · ForgeKeep</title>
</svelte:head>

<div class="page-container-narrow">
  <div class="profile-header">
    <div class="avatar">{org ? '🏢' : '👤'}</div>
    <div class="info">
      <h1>{org?.display_name || owner}</h1>
      {#if org}
        <p class="handle">@{org.name}</p>
        {#if org.description}<p class="org-desc">{org.description}</p>{/if}
      {/if}
    </div>
    <div class="header-actions">
      {#if org}
        <a href={`/orgs/${org.name}`} class="btn btn-outline">{t('profile.org_page')}</a>
      {/if}
      {#if canCreate}
        <a href={`/dashboard?owner=${encodeURIComponent(owner)}`} class="btn btn-primary">
          + {t('dashboard.new_repo')}
        </a>
      {/if}
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
      {#if canCreate}
        <a href={`/dashboard?owner=${encodeURIComponent(owner)}`} class="btn btn-primary empty-action">
          + {t('dashboard.new_repo')}
        </a>
      {/if}
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

  .info {
    flex: 1;
    min-width: 0;
  }

  .info h1 {
    font-size: 22px;
    margin: 0;
  }

  .handle {
    font-size: 13px;
    color: var(--text-secondary);
    margin: 2px 0 0;
  }

  .org-desc {
    font-size: 13px;
    color: var(--text-secondary);
    margin: 6px 0 0;
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-shrink: 0;
  }

  .header-actions .btn {
    text-decoration: none;
    white-space: nowrap;
  }
  .header-actions .btn:hover { text-decoration: none; }

  .empty {
    text-align: center;
    padding: 60px 24px;
    color: var(--text-secondary);
  }

  .empty-action {
    display: inline-block;
    margin-top: 12px;
    text-decoration: none;
  }
  .empty-action:hover { text-decoration: none; }

  @media (max-width: 600px) {
    .profile-header {
      flex-wrap: wrap;
    }
    .header-actions {
      width: 100%;
    }
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
