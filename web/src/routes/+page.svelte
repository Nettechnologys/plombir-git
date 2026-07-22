<script lang="ts">
  import { isLoggedIn, getUser } from '$lib/stores/auth.svelte';
  import { repos } from '$lib/api/client.svelte';
  import { goto } from '$app/navigation';
  import { createT, formatDate } from '$lib/i18n';

  const t = createT();

  let repoList = $state<any[]>([]);
  let loading = $state(true);
  let error = $state('');
  let showLoginModal = $state(false);

  $effect(() => {
    loadRepos();
  });

  async function loadRepos() {
    try {
      loading = true;
      error = '';
      const r = await repos.explore(1, 6);
      if (r && Array.isArray(r.data)) {
        repoList = r.data;
      }
    } catch (e: any) {
      error = e.message || 'Failed to load repositories';
    } finally {
      loading = false;
    }
  }

  function handleLogin() {
    goto('/login');
  }

  function handleRegister() {
    goto('/register');
  }
</script>

<svelte:head>
  <title>IronForge</title>
</svelte:head>

{#if isLoggedIn()}
  <!-- User Dashboard -->
  <div class="dashboard">
    <div class="dashboard-header">
      <h1>{t('dashboard.title')}</h1>
      <button class="btn-primary" onclick={() => goto('/dashboard')}>
        + {t('dashboard.new_repo')}
      </button>
    </div>

    {#if error}
      <div class="error-banner">{error}</div>
    {/if}

    {#if loading}
      <p class="text-secondary">{t('common.loading')}</p>
    {:else if repoList.length === 0}
      <div class="empty">
        <p>{t('dashboard.empty.no_repos')}</p>
        <p class="text-secondary">{t('dashboard.empty.get_started')}</p>
      </div>
    {:else}
      <div class="repo-grid">
        {#each repoList as repo}
          <a href={`/${repo.owner_name || 'unknown'}/${repo.name}`} class="repo-card">
            <div class="rc-icon">{repo.is_private ? '🔒' : '📂'}</div>
            <div class="rc-body">
              <div class="rc-name">{repo.owner_name || 'unknown'}/{repo.name}</div>
              <div class="rc-desc">{repo.description || t('common.no_description')}</div>
              <div class="rc-meta">
                ⭐ {repo.stars_count || 0} · {t('common.updated', { date: formatDate(repo.updated_at) })}
              </div>
            </div>
          </a>
        {/each}
      </div>

      <div class="dashboard-footer">
        <a href="/explore" class="view-all-btn">{t('home.explore.view_all')} →</a>
      </div>
    {/if}
  </div>
{:else}
  <!-- Product Landing Page -->
  <div class="landing">
    <!-- Hero Section -->
    <section class="hero">
      <div class="hero-content">
        <div class="hero-logo">
          <svg viewBox="0 0 16 16" width="64" height="64" fill="currentColor" aria-hidden="true">
            <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/>
          </svg>
        </div>
        <h1 class="hero-title">IronForge</h1>
        <p class="hero-subtitle">{t('home.tagline')}</p>
        <p class="hero-desc">{t('home.description')}</p>

        <div class="hero-actions">
          <button class="btn-hero-primary" onclick={handleRegister}>
            {t('home.sign_up')}
          </button>
          <button class="btn-hero-secondary" onclick={handleLogin}>
            {t('home.sign_in')}
          </button>
        </div>
      </div>
    </section>

    <!-- Features Section -->
    <section class="features">
      <div class="features-container">
        <h2 class="features-title">{t('home.features.title')}</h2>
        <div class="features-grid">
          <div class="feature-card">
            <div class="feature-icon">🚀</div>
            <h3>{t('home.features.lightweight.title')}</h3>
            <p>{t('home.features.lightweight.desc')}</p>
          </div>
          <div class="feature-card">
            <div class="feature-icon">🔧</div>
            <h3>{t('home.features.all_in_one.title')}</h3>
            <p>{t('home.features.all_in_one.desc')}</p>
          </div>
          <div class="feature-card">
            <div class="feature-icon">🔒</div>
            <h3>{t('home.features.security.title')}</h3>
            <p>{t('home.features.security.desc')}</p>
          </div>
          <div class="feature-card">
            <div class="feature-icon">🌐</div>
            <h3>{t('home.features.self_hosted.title')}</h3>
            <p>{t('home.features.self_hosted.desc')}</p>
          </div>
          <div class="feature-card">
            <div class="feature-icon">⚡</div>
            <h3>{t('home.features.fast.title')}</h3>
            <p>{t('home.features.fast.desc')}</p>
          </div>
          <div class="feature-card">
            <div class="feature-icon">🛠️</div>
            <h3>{t('home.features.extensible.title')}</h3>
            <p>{t('home.features.extensible.desc')}</p>
          </div>
        </div>
      </div>
    </section>

    <!-- Stats Section -->
    <section class="stats">
      <div class="stats-container">
        <div class="stat-item">
          <div class="stat-number">50MB</div>
          <div class="stat-label">{t('home.stats.memory')}</div>
        </div>
        <div class="stat-item">
          <div class="stat-number">1</div>
          <div class="stat-label">{t('home.stats.binary')}</div>
        </div>
        <div class="stat-item">
          <div class="stat-number">100%</div>
          <div class="stat-label">{t('home.stats.compatible')}</div>
        </div>
        <div class="stat-item">
          <div class="stat-number">Rust</div>
          <div class="stat-label">{t('home.stats.language')}</div>
        </div>
      </div>
    </section>

    <!-- Public Repositories Section -->
    <section class="public-repos">
      <div class="repos-container">
        <div class="repos-header">
          <h2>{t('home.public_repos')}</h2>
          <a href="/explore" class="view-all">{t('home.explore.view_all')} →</a>
        </div>

        {#if error}
          <div class="error-banner">
            <p>{error}</p>
            <button onclick={loadRepos}>Retry</button>
          </div>
        {/if}

        {#if loading}
          <p class="text-secondary">{t('common.loading')}</p>
        {:else if repoList.length === 0}
          <div class="empty">
            <p>{t('explore.empty')}</p>
          </div>
        {:else}
          <div class="repo-list">
            {#each repoList as repo}
              <a href={`/${repo.owner_name || 'unknown'}/${repo.name}`} class="repo-item">
                <div class="repo-icon">{repo.is_private ? '🔒' : '📂'}</div>
                <div class="repo-info">
                  <div class="repo-name">
                    {repo.owner_name || 'unknown'}/{repo.name}
                    {#if repo.is_private}
                      <span class="badge-private">{t('repo.private')}</span>
                    {/if}
                  </div>
                  <div class="repo-desc">{repo.description || t('common.no_description')}</div>
                  <div class="repo-meta">
                    ⭐ {repo.stars_count || 0} · {t('common.updated', { date: formatDate(repo.updated_at) })}
                  </div>
                </div>
              </a>
            {/each}
          </div>
        {/if}
      </div>
    </section>

    <!-- Footer -->
    <footer class="footer">
      <div class="footer-container">
        <div class="footer-logo">
          <span class="footer-logo-text">IronForge</span>
        </div>
        <div class="footer-links">
          <a href="https://github.com/lengyuqu/ironforge" target="_blank">{t('home.footer.github')}</a>
          <a href="/explore">{t('home.footer.explore')}</a>
          <a href="https://github.com/lengyuqu/ironforge#readme" target="_blank">{t('home.footer.help')}</a>
        </div>
        <div class="footer-copyright">
          © 2026 IronForge. {t('home.footer.built_with')}
        </div>
      </div>
    </footer>
  </div>
{/if}

<style>
  /* === Landing Page Styles === */
  .landing {
    min-height: 100vh;
  }

  /* Hero Section */
  .hero {
    background: linear-gradient(135deg, #1a1a2e 0%, #16213e 50%, #0f3460 100%);
    color: white;
    padding: 80px 24px;
    text-align: center;
  }

  .hero-content {
    max-width: 800px;
    margin: 0 auto;
  }

  .hero-logo {
    margin-bottom: 24px;
    opacity: 0.9;
  }

  .hero-title {
    font-size: 48px;
    font-weight: 700;
    margin-bottom: 16px;
    letter-spacing: -1px;
  }

  .hero-subtitle {
    font-size: 20px;
    font-weight: 400;
    margin-bottom: 12px;
    opacity: 0.9;
  }

  .hero-desc {
    font-size: 16px;
    opacity: 0.7;
    margin-bottom: 32px;
    max-width: 600px;
    margin-left: auto;
    margin-right: auto;
    line-height: 1.6;
  }

  .hero-actions {
    display: flex;
    gap: 16px;
    justify-content: center;
  }

  .btn-hero-primary {
    padding: 12px 32px;
    background: var(--primary, #2da44e);
    color: white;
    border: none;
    border-radius: 8px;
    font-size: 16px;
    font-weight: 600;
    cursor: pointer;
    transition: background 0.2s;
  }

  .btn-hero-primary:hover {
    background: var(--primary-hover, #218838);
  }

  .btn-hero-secondary {
    padding: 12px 32px;
    background: rgba(255, 255, 255, 0.1);
    color: white;
    border: 1px solid rgba(255, 255, 255, 0.3);
    border-radius: 8px;
    font-size: 16px;
    font-weight: 600;
    cursor: pointer;
    transition: background 0.2s;
  }

  .btn-hero-secondary:hover {
    background: rgba(255, 255, 255, 0.2);
  }

  /* Features Section */
  .features {
    padding: 80px 24px;
    background: var(--bg-primary, #ffffff);
  }

  .features-container {
    max-width: 1200px;
    margin: 0 auto;
  }

  .features-title {
    text-align: center;
    font-size: 32px;
    font-weight: 700;
    margin-bottom: 48px;
    color: var(--text-primary, #1f2328);
  }

  .features-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 32px;
  }

  @media (max-width: 900px) {
    .features-grid {
      grid-template-columns: repeat(2, 1fr);
    }
  }

  @media (max-width: 600px) {
    .features-grid {
      grid-template-columns: 1fr;
    }
  }

  .feature-card {
    padding: 24px;
    border: 1px solid var(--border, #d0d7de);
    border-radius: 12px;
    transition: border-color 0.2s, box-shadow 0.2s;
  }

  .feature-card:hover {
    border-color: var(--primary, #2da44e);
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.1);
  }

  .feature-icon {
    font-size: 40px;
    margin-bottom: 16px;
  }

  .feature-card h3 {
    font-size: 18px;
    font-weight: 600;
    margin-bottom: 8px;
    color: var(--text-primary, #1f2328);
  }

  .feature-card p {
    font-size: 14px;
    color: var(--text-secondary, #656d76);
    line-height: 1.6;
  }

  /* Stats Section */
  .stats {
    padding: 60px 24px;
    background: var(--bg-secondary, #f6f8fa);
    border-top: 1px solid var(--border, #d0d7de);
    border-bottom: 1px solid var(--border, #d0d7de);
  }

  .stats-container {
    max-width: 1200px;
    margin: 0 auto;
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 32px;
    text-align: center;
  }

  @media (max-width: 768px) {
    .stats-container {
      grid-template-columns: repeat(2, 1fr);
    }
  }

  .stat-item {
    padding: 16px;
  }

  .stat-number {
    font-size: 36px;
    font-weight: 700;
    color: var(--primary, #2da44e);
    margin-bottom: 8px;
  }

  .stat-label {
    font-size: 14px;
    color: var(--text-secondary, #656d76);
  }

  /* Public Repositories Section */
  .public-repos {
    padding: 80px 24px;
    background: var(--bg-primary, #ffffff);
  }

  .repos-container {
    max-width: 1200px;
    margin: 0 auto;
  }

  .repos-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 24px;
  }

  .repos-header h2 {
    font-size: 24px;
    font-weight: 700;
    color: var(--text-primary, #1f2328);
  }

  .view-all {
    font-size: 14px;
    color: var(--primary, #2da44e);
    text-decoration: none;
  }

  .view-all:hover {
    text-decoration: underline;
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
    border-bottom: 1px solid var(--border-light, #eaeef2);
    text-decoration: none;
    color: var(--text-primary, #1f2328);
    transition: background 0.2s;
  }

  .repo-item:hover {
    background: var(--bg-secondary, #f6f8fa);
  }

  .repo-item:first-child {
    border-top: 1px solid var(--border-light, #eaeef2);
    border-radius: 8px 8px 0 0;
  }

  .repo-item:last-child {
    border-radius: 0 0 8px 8px;
  }

  .repo-icon {
    font-size: 20px;
    margin-top: 2px;
  }

  .repo-info {
    flex: 1;
  }

  .repo-name {
    font-weight: 600;
    font-size: 15px;
    color: var(--accent, #0969da);
    margin-bottom: 4px;
  }

  .badge-private {
    font-size: 11px;
    font-weight: 500;
    padding: 1px 6px;
    border: 1px solid var(--border, #d0d7de);
    border-radius: 10px;
    color: var(--text-secondary, #656d76);
    margin-left: 8px;
    vertical-align: middle;
  }

  .repo-desc {
    font-size: 13px;
    color: var(--text-secondary, #656d76);
    margin-top: 2px;
  }

  .repo-meta {
    font-size: 12px;
    color: var(--text-muted, #8b949e);
    margin-top: 4px;
  }

  .empty {
    text-align: center;
    padding: 60px 24px;
    color: var(--text-secondary, #656d76);
  }

  /* Footer */
  .footer {
    background: var(--bg-secondary, #f6f8fa);
    border-top: 1px solid var(--border, #d0d7de);
    padding: 40px 24px;
  }

  .footer-container {
    max-width: 1200px;
    margin: 0 auto;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 16px;
  }

  .footer-logo-text {
    font-size: 20px;
    font-weight: 700;
    color: var(--text-primary, #1f2328);
  }

  .footer-links {
    display: flex;
    gap: 24px;
  }

  .footer-links a {
    color: var(--text-secondary, #656d76);
    text-decoration: none;
    font-size: 14px;
  }

  .footer-links a:hover {
    color: var(--primary, #2da44e);
  }

  .footer-copyright {
    font-size: 12px;
    color: var(--text-muted, #8b949e);
  }

  /* Error Banner */
  .error-banner {
    background: var(--error-bg, #fee);
    border: 1px solid var(--error-border, #fcc);
    border-radius: 6px;
    padding: 12px 16px;
    margin-bottom: 16px;
    display: flex;
    justify-content: space-between;
    align-items: center;
  }

  .error-banner button {
    padding: 6px 12px;
    background: var(--primary, #2da44e);
    color: white;
    border: none;
    border-radius: 4px;
    cursor: pointer;
  }

  /* === Dashboard Styles (Logged In) === */
  .dashboard {
    max-width: 1200px;
    margin: 0 auto;
    padding: 24px;
  }

  .dashboard-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 24px;
  }

  .dashboard-header h1 {
    font-size: 24px;
  }

  .btn-primary {
    padding: 6px 16px;
    background: var(--green-dim, #2da44e);
    color: #fff;
    border: none;
    border-radius: var(--radius, 6px);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }

  .btn-primary:hover {
    background: var(--green, #218838);
  }

  .repo-grid {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 12px;
    margin-bottom: 24px;
  }

  @media (max-width: 900px) {
    .repo-grid {
      grid-template-columns: repeat(2, 1fr);
    }
  }

  @media (max-width: 600px) {
    .repo-grid {
      grid-template-columns: 1fr;
    }
  }

  .repo-card {
    display: flex;
    gap: 12px;
    padding: 16px;
    border: 1px solid var(--border, #d0d7de);
    border-radius: 8px;
    text-decoration: none;
    color: inherit;
    transition: border-color 0.15s;
  }

  .repo-card:hover {
    border-color: var(--primary, #2da44e);
  }

  .rc-icon {
    font-size: 32px;
    flex-shrink: 0;
  }

  .rc-body {
    flex: 1;
    min-width: 0;
  }

  .rc-name {
    font-weight: 600;
    font-size: 15px;
    margin-bottom: 4px;
  }

  .rc-desc {
    font-size: 13px;
    color: var(--text-secondary, #656d76);
    margin-bottom: 6px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .rc-meta {
    font-size: 12px;
    color: var(--text-tertiary, #888);
  }

  .dashboard-footer {
    text-align: center;
  }

  .view-all-btn {
    display: inline-block;
    padding: 8px 20px;
    background: var(--primary, #2da44e);
    color: white;
    border-radius: 6px;
    text-decoration: none;
    font-size: 14px;
  }

  .text-secondary {
    color: var(--text-secondary, #656d76);
  }

  .empty {
    text-align: center;
    padding: 60px 24px;
    color: var(--text-secondary, #656d76);
  }
</style>
