<script lang="ts">
  import { page } from '$app/state';
  import { orgs, repos } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let org = $state<any>(null);
  let teams = $state<any[]>([]);
  let members = $state<any[]>([]);
  let orgRepos = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let newTeamName = $state('');
  let newTeamPermission = $state('read');
  let newRepoName = $state('');
  let newRepoPrivate = $state(false);

  async function load() {
    loading = true;
    error = '';
    try {
      const name = page.params.name!;
      org = await orgs.get(name);
      members = await orgs.listMembers(name);
      teams = await orgs.listTeams(name);
      orgRepos = (await repos.list(name)).data;
    } catch (e: any) {
      error = e.message || t('errors.load_failed');
    } finally {
      loading = false;
    }
  }

  async function createTeam() {
    if (!newTeamName.trim()) return;
    try {
      await orgs.createTeam(page.params.name!, newTeamName, undefined, newTeamPermission);
      newTeamName = '';
      teams = await orgs.listTeams(page.params.name!);
    } catch (e: any) {
      error = e.message;
    }
  }

  async function createOrgRepo() {
    if (!newRepoName.trim()) return;
    try {
      await repos.create({
        name: newRepoName,
        is_private: newRepoPrivate,
        org: page.params.name!,
      });
      newRepoName = '';
      orgRepos = (await repos.list(page.params.name!)).data;
    } catch (e: any) {
      error = e.message;
    }
  }

  load();
</script>

<div class="container">
  {#if loading}
    <p>{t('common.loading')}</p>
  {:else if error && !org}
    <div class="error">{error}</div>
  {:else if org}
    <div class="org-header">
      <div class="org-avatar">{org.name[0]?.toUpperCase() || '?'}</div>
      <div>
        <h1>{org.display_name || org.name}</h1>
        <p class="org-meta">@{org.name} · {t(`orgs.visibility_${org.visibility}`)} · {t('common.created', { date: formatDate(org.created_at) })}</p>
        {#if org.description}<p class="org-desc">{org.description}</p>{/if}
      </div>
    </div>

    {#if error}
      <div class="error" style="margin:1rem 0">{error}</div>
    {/if}

    <!-- Organization Repositories -->
    <div class="section" style="margin-bottom: 1.5rem;">
      <h2>{t('orgs.repositories', { count: String(orgRepos.length) })}</h2>
      <div class="create-form">
        <input type="text" bind:value={newRepoName} placeholder={t('orgs.new_repo')} />
        <label class="checkbox-label">
          <input type="checkbox" bind:checked={newRepoPrivate} />
          {t('orgs.private')}
        </label>
        <button class="btn-sm" onclick={createOrgRepo}>{t('orgs.create_repo')}</button>
      </div>
      {#if orgRepos.length === 0}
        <p class="empty">{t('orgs.no_repos')}</p>
      {:else}
        <div class="repo-list">
          {#each orgRepos as repo}
            <a href={`/${org.name}/${repo.name}`} class="repo-item">
              <span class="repo-icon">{repo.is_private ? '🔒' : '📖'}</span>
              <span class="repo-name">{repo.name}</span>
              {#if repo.description}<span class="repo-desc">{repo.description}</span>{/if}
            </a>
          {/each}
        </div>
      {/if}
    </div>

    <div class="grid">
      <!-- Teams -->
      <div class="section">
        <h2>{t('orgs.teams', { count: String(teams.length) })}</h2>
        <div class="create-form">
          <input type="text" bind:value={newTeamName} placeholder={t('orgs.new_team')} />
          <select bind:value={newTeamPermission}>
            <option value="read">{t('orgs.permission.read')}</option>
            <option value="write">{t('orgs.permission.write')}</option>
            <option value="admin">{t('orgs.permission.admin')}</option>
          </select>
          <button class="btn-sm" onclick={createTeam}>{t('orgs.create_team')}</button>
        </div>
        {#if teams.length === 0}
          <p class="empty">{t('orgs.no_teams')}</p>
        {:else}
          {#each teams as team}
            <div class="item">
              <span class="item-name">{team.name}</span>
              <span class="badge">{t(`orgs.permission.${team.permission}`)}</span>
            </div>
          {/each}
        {/if}
      </div>

      <!-- Members -->
      <div class="section">
        <h2>{t('orgs.members', { count: String(members.length) })}</h2>
        {#if members.length === 0}
          <p class="empty">{t('orgs.no_members')}</p>
        {:else}
          {#each members as member}
            <div class="item">
              <span class="item-name">User #{member.user_id}</span>
              <span class="badge">{member.role}</span>
            </div>
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .org-header { display: flex; align-items: center; gap: 1rem; margin-bottom: 2rem; }
  .org-avatar { width: 64px; height: 64px; border-radius: 50%; background: var(--accent); color: white; display: flex; align-items: center; justify-content: center; font-size: 1.5rem; font-weight: 700; }
  h1 { color: var(--text-primary); margin: 0; }
  .org-meta { color: var(--text-secondary); font-size: 0.9rem; margin: 0.3rem 0 0; }
  .org-desc { color: var(--text-primary); margin: 0.5rem 0 0; }
  .grid { display: grid; grid-template-columns: 1fr 1fr; gap: 1.5rem; }
  .section { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: 8px; padding: 1.25rem; }
  h2 { color: var(--text-primary); font-size: 1.1rem; margin: 0 0 1rem; }
  .create-form { display: flex; gap: 0.5rem; margin-bottom: 1rem; align-items: center; }
  .create-form input, .create-form select { flex: 1; background: var(--bg-primary); color: var(--text-primary); border: 1px solid var(--border); border-radius: 4px; padding: 0.4rem 0.6rem; font-size: 0.85rem; }
  .checkbox-label { display: flex; align-items: center; gap: 0.3rem; font-size: 0.85rem; color: var(--text-secondary); white-space: nowrap; }
  .btn-sm { background: var(--accent); color: white; border: none; border-radius: 4px; padding: 0.4rem 0.8rem; cursor: pointer; font-size: 0.85rem; }
  .item { display: flex; align-items: center; justify-content: space-between; padding: 0.5rem 0; border-bottom: 1px solid var(--border); }
  .item:last-child { border-bottom: none; }
  .item-name { color: var(--text-primary); }
  .badge { background: var(--bg-primary); color: var(--text-secondary); padding: 0.15rem 0.5rem; border-radius: 12px; font-size: 0.75rem; border: 1px solid var(--border); }
  .empty { color: var(--text-secondary); font-style: italic; }
  .error { color: #f85149; background: rgba(248, 81, 73, 0.1); padding: 0.5rem 0.75rem; border-radius: 6px; }
  .repo-list { display: flex; flex-direction: column; gap: 0; }
  .repo-item { display: flex; align-items: center; gap: 0.5rem; padding: 0.5rem 0.75rem; color: var(--text-primary); text-decoration: none; border-bottom: 1px solid var(--border); border-radius: 0; }
  .repo-item:last-child { border-bottom: none; }
  .repo-item:hover { background: var(--bg-hover); }
  .repo-icon { font-size: 1rem; }
  .repo-name { font-weight: 500; }
  .repo-desc { color: var(--text-secondary); font-size: 0.85rem; margin-left: auto; }
  @media (max-width: 900px) { .grid { grid-template-columns: 1fr; } }
</style>
