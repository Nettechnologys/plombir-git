<script lang="ts">
  import { page } from '$app/stores';
  import { createT } from '$lib/i18n';
  import RepoHeader from '$lib/components/RepoHeader.svelte';

  const t = createT();

  let { children } = $props();

  const owner = $derived($page.params.owner!);
  const repo = $derived($page.params.repo!);
  const currentPath = $derived($page.url.pathname);

  const navItems = $derived([
    { path: `/${owner}/${repo}/settings`, label: t('settings.general'), icon: '⚙️' },
    { path: `/${owner}/${repo}/settings/labels`, label: t('settings.labels'), icon: '🏷️' },
    { path: `/${owner}/${repo}/settings/branches`, label: t('settings.branch_protection.title'), icon: '🛡️' },
    { path: `/${owner}/${repo}/settings/deploy-keys`, label: t('settings.deploy_keys.title', 'Deploy keys'), icon: '🔑' },
    { path: `/${owner}/${repo}/settings/ci-secrets`, label: t('settings.ci_secrets.title', 'CI secrets'), icon: '🔒' },
    { path: `/${owner}/${repo}/settings/environments`, label: t('settings.environments.title', 'Environments'), icon: '🚀' },
    { path: `/${owner}/${repo}/settings/retention`, label: t('settings.retention.title', 'CI retention'), icon: '🧹' },
    { path: `/${owner}/${repo}/settings/tags`, label: t('settings.tag_protection.title', 'Tag protection'), icon: '🏷️' },
    { path: `/${owner}/${repo}/settings/mirror`, label: t('settings.mirror.title'), icon: '🔁' },
    { path: `/${owner}/${repo}/settings/webhooks`, label: t('settings.webhooks.title', 'Webhooks'), icon: '🔔' },
    { path: `/${owner}/${repo}/settings/collaborators`, label: t('settings.collaborators.title'), icon: '👥' },
    { path: `/${owner}/${repo}/settings/runners`, label: t('admin.runners.title'), icon: '🏃' }
  ]);

  const currentSection = $derived(navItems.find((item) => item.path === currentPath));
</script>

<div class="page-container">
  <RepoHeader {owner} {repo} activeTab="settings" />

  <div class="settings-layout">
    <aside class="sidebar">
      <nav>
        {#each navItems as item}
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

    <main class="content">
      <div class="breadcrumb">
        <a href={`/${owner}/${repo}`}>{owner}/{repo}</a>
        <span class="separator">/</span>
        <span>{t('settings.title')}</span>
        {#if currentSection && currentSection.path !== `/${owner}/${repo}/settings`}
          <span class="separator">/</span>
          <span>{currentSection.label}</span>
        {/if}
      </div>

      {@render children()}
    </main>
  </div>
</div>

<style>
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
