<script lang="ts">
  import { page } from '$app/stores';
  import { createT } from '$lib/i18n';
  import RepoHeader from '$lib/components/RepoHeader.svelte';
  import { viewerPermission } from '$lib/viewerPermission.svelte';

  const t = createT();

  let { children } = $props();

  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
  const currentPath = $derived($page.url.pathname);

  // Each section names the level its server routes require (card_270a0a77fd79):
  // labels, the mirror and LFS locks are `RepoWrite`, everything else here is
  // `RepoAdmin`. A section the viewer may not use is neither listed nor
  // rendered — rendering it only asked the server for a 403 on load.
  type Level = 'write' | 'admin';
  const navItems = $derived([
    { path: `/${owner}/${repo}/settings`, label: t('settings.general'), icon: '⚙️', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/labels`, label: t('settings.labels'), icon: '🏷️', level: 'write' as Level },
    { path: `/${owner}/${repo}/settings/branches`, label: t('settings.branch_protection.title'), icon: '🛡️', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/deploy-keys`, label: t('settings.deploy_keys.title', 'Deploy keys'), icon: '🔑', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/ci-secrets`, label: t('settings.ci_secrets.title', 'CI secrets'), icon: '🔒', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/environments`, label: t('settings.environments.title', 'Environments'), icon: '🚀', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/retention`, label: t('settings.retention.title', 'CI retention'), icon: '🧹', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/tags`, label: t('settings.tag_protection.title', 'Tag protection'), icon: '🏷️', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/mirror`, label: t('settings.mirror.title'), icon: '🔁', level: 'write' as Level },
    { path: `/${owner}/${repo}/settings/lfs-locks`, label: t('settings.lfs_locks.title'), icon: '🔐', level: 'write' as Level },
    { path: `/${owner}/${repo}/settings/lfs-storage`, label: t('settings.lfs_storage.title'), icon: '📦', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/webhooks`, label: t('settings.webhooks.title', 'Webhooks'), icon: '🔔', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/collaborators`, label: t('settings.collaborators.title'), icon: '👥', level: 'admin' as Level },
    { path: `/${owner}/${repo}/settings/runners`, label: t('admin.runners.title'), icon: '🏃', level: 'admin' as Level }
  ]);

  const permission = viewerPermission(() => owner, () => repo);
  const allows = (level: Level) => (level === 'admin' ? permission.isAdmin : permission.canWrite);
  const visibleItems = $derived(navItems.filter((item) => allows(item.level)));
  const currentSection = $derived(navItems.find((item) => item.path === currentPath));
  const sectionAllowed = $derived(!currentSection || allows(currentSection.level));
</script>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="settings" />

  <div class="settings-layout">
    <aside class="sidebar">
      <nav>
        {#each visibleItems as item}
          <a
            href={item.path}
            class="nav-item"
            class:active={currentPath === item.path}
          >
            <span class="nav-icon">{item.icon}</span>
            <span class="nav-label">{item.label}</span>
          </a>
        {/each}
      </nav>
    </aside>

    <div class="content">
      <div class="breadcrumb">
        <a href={`/${owner}/${repo}`}>{owner}/{repo}</a>
        <span class="separator">/</span>
        <span>{t('settings.title')}</span>
        {#if currentSection && currentSection.path !== `/${owner}/${repo}/settings`}
          <span class="separator">/</span>
          <span>{currentSection.label}</span>
        {/if}
      </div>

      {#if !permission.settled}
        <p class="text-secondary">{t('common.loading')}</p>
      {:else if sectionAllowed}
        {@render children()}
      {:else}
        <div class="no-access" role="status">
          {currentSection?.level === 'admin'
            ? t('settings.access.admin_required', 'This page is for the repository’s administrators.')
            : t('settings.access.write_required', 'This page is for people who can write to the repository.')}
        </div>
      {/if}
    </div>
  </div>
</div>

<style>
  .no-access {
    padding: 16px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-secondary);
  }

  .settings-layout {
    display: flex;
    gap: 2rem;
    min-height: calc(100vh - 220px);
  }
  
  .sidebar {
    width: 200px;
    flex-shrink: 0;
  }
  
  .sidebar nav {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  
  .nav-item {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.75rem 1rem;
    border-radius: 6px;
    color: var(--text-primary);
    text-decoration: none;
    transition: all 0.2s;
    border-left: 3px solid transparent;
  }
  
  .nav-item:hover {
    background: var(--bg-secondary);
  }
  
  .nav-item.active {
    color: var(--accent);
    border-left-color: var(--accent);
    background: var(--bg-secondary);
    font-weight: 600;
  }
  
  .nav-icon {
    font-size: 1.1rem;
  }
  
  .nav-label {
    font-size: 0.9rem;
  }
  
  .content {
    flex: 1;
    min-width: 0;
  }
  
  .breadcrumb {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-bottom: 2rem;
    font-size: 0.9rem;
    color: var(--text-secondary);
  }
  
  .breadcrumb a {
    color: var(--accent);
    text-decoration: none;
  }
  
  .breadcrumb a:hover {
    text-decoration: underline;
  }
  
  .separator {
    color: var(--text-muted);
  }

  @media (max-width: 760px) {
    .settings-layout {
      flex-direction: column;
      gap: 1rem;
    }

    .sidebar {
      width: 100%;
    }

    .sidebar nav {
      flex-direction: row;
      flex-wrap: wrap;
    }
  }
</style>
