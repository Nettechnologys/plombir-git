<script lang="ts">
  import { isAuthReady, isLoggedIn, isAdmin } from '$lib/stores/auth.svelte';
  import { goto } from '$app/navigation';
  import { createT } from '$lib/i18n';

  const t = createT();

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    if (!isAdmin()) {
      goto('/dashboard');
    }
  });
</script>

<div class="container">
  <div class="header">
    <h1>{t('admin.title')}</h1>
    <p class="subtitle">{t('admin.subtitle')}</p>
  </div>

  <div class="cards">
    <a href="/admin/users" class="card">
      <div class="card-icon">👥</div>
      <div class="card-body">
        <h2>{t('admin.users.title')}</h2>
        <p>{t('admin.users.desc')}</p>
      </div>
      <div class="card-arrow">→</div>
    </a>

    <a href="/admin/orgs" class="card">
      <div class="card-icon">🏢</div>
      <div class="card-body">
        <h2>{t('admin.orgs.title')}</h2>
        <p>{t('admin.orgs.desc')}</p>
      </div>
      <div class="card-arrow">→</div>
    </a>

    <a href="/admin/audit" class="card">
      <div class="card-icon">📋</div>
      <div class="card-body">
        <h2>{t('admin.audit.title')}</h2>
        <p>{t('admin.audit.desc')}</p>
      </div>
      <div class="card-arrow">→</div>
    </a>

    <a href="/admin/runners" class="card">
      <div class="card-icon">▶</div>
      <div class="card-body">
        <h2>{t('admin.runners.title')}</h2>
        <p>{t('admin.runners.desc')}</p>
      </div>
      <div class="card-arrow">→</div>
    </a>

    <a href="/admin/settings" class="card">
      <div class="card-icon">⚙️</div>
      <div class="card-body">
        <h2>Instance Settings</h2>
        <p>Instance maintenance and banner configuration</p>
      </div>
      <div class="card-arrow">→</div>
    </a>
  </div>
</div>

<style>

  .header {
    margin-bottom: 2rem;
  }

  h1 {
    color: var(--text-primary);
    margin: 0 0 0.5rem;
  }

  .subtitle {
    color: var(--text-secondary);
    margin: 0;
  }

  .cards {
    display: flex;
    flex-direction: column;
    gap: 1rem;
  }

  .card {
    display: flex;
    align-items: center;
    gap: 1rem;
    padding: 1.25rem 1.5rem;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: 10px;
    text-decoration: none;
    color: inherit;
    transition: border-color 0.15s, background 0.15s;
  }

  .card:hover {
    border-color: var(--accent);
    background: var(--bg-hover);
    text-decoration: none;
  }

  .card-icon {
    font-size: 2rem;
    line-height: 1;
    flex-shrink: 0;
  }

  .card-body {
    flex: 1;
  }

  .card-body h2 {
    margin: 0 0 0.25rem;
    font-size: 1.05rem;
    color: var(--text-primary);
  }

  .card-body p {
    margin: 0;
    font-size: 0.875rem;
    color: var(--text-secondary);
  }

  .card-arrow {
    color: var(--text-secondary);
    font-size: 1.25rem;
    flex-shrink: 0;
  }
</style>
