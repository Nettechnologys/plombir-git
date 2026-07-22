<script lang="ts">
  import { goto } from '$app/navigation';
  import { getUser, isLoggedIn, isAdmin, logout } from '$lib/stores/auth.svelte';
  import { locale, createT, type Locale } from '$lib/i18n';
  import Dropdown from './Dropdown.svelte';

  const t = createT();

  let search = $state('');

  function handleLogout() {
    logout();
    window.location.href = '/login';
  }

  function setLocale(newLocale: Locale) {
    locale.set(newLocale);
  }

  function performSearch() {
    const q = search.trim();
    if (!q) {
      goto('/search');
      return;
    }
    goto(`/search?q=${encodeURIComponent(q)}`);
  }

  function onSearchKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      performSearch();
    }
    if (e.key === 'Escape') {
      search = '';
    }
  }
</script>

<nav class="navbar">
  <div class="navbar-inner">
    <div class="navbar-left">
      <a href="/" class="logo" aria-label="IronForge Home">
        <svg viewBox="0 0 16 16" width="28" height="28" fill="currentColor" aria-hidden="true">
          <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/>
        </svg>
        <span class="logo-text">IronForge</span>
      </a>

      <a href="/dashboard" class="nav-link">{t('nav.dashboard')}</a>
      <a href="/explore" class="nav-link">{t('nav.explore')}</a>
      <a href="/search" class="nav-link">{t('nav.search')}</a>
    </div>

    <div class="navbar-search">
      <svg viewBox="0 0 16 16" width="16" height="16" fill="currentColor" aria-hidden="true">
        <path d="M11.5 7a4.5 4.5 0 1 1-9 0 4.5 4.5 0 0 1 9 0Zm-.82 4.74a6 6 0 1 1 1.06-1.06l3.04 3.04a.75.75 0 1 1-1.06 1.06l-3.04-3.04Z"/>
      </svg>
      <input
        type="search"
        class="search-input"
        bind:value={search}
        placeholder={t('nav.search_placeholder', 'Search or jump to...')}
        aria-label={t('nav.search')}
        onkeydown={onSearchKeydown}
      />
      <button class="search-btn" type="button" onclick={performSearch} aria-label={t('nav.search')}>
        {t('nav.search_go', 'Go')}
      </button>
    </div>

    <div class="navbar-right">
      {#if isLoggedIn()}
        <a href="/notifications" class="nav-link">{t('nav.notifications')}</a>
        <a href="/orgs" class="nav-link">{t('nav.organizations')}</a>
        <a href="/imports" class="nav-link">{t('nav.imports', 'Imports')}</a>

        <div class="lang-menu-container">
          <Dropdown ariaLabel={t('nav.change_language', 'Change language')} triggerClass="lang-btn">
            {#snippet trigger()}
              {$locale === 'zh-CN' ? t('nav.chinese', '中文') : t('nav.english', 'EN')}
            {/snippet}
            {#snippet menu(close)}
              <button onclick={() => { setLocale('en'); close(); }} class:active={$locale === 'en'} role="menuitem">{t('nav.english', 'English')}</button>
              <button onclick={() => { setLocale('zh-CN'); close(); }} class:active={$locale === 'zh-CN'} role="menuitem">{t('nav.chinese', '中文')}</button>
            {/snippet}
          </Dropdown>
        </div>

        <div class="user-menu-container">
          <Dropdown ariaLabel="User menu" triggerClass="user-btn">
            {#snippet trigger()}
              <div class="avatar" aria-hidden="true">
                {(getUser()?.username || '?')[0].toUpperCase()}
              </div>
              <span>{getUser()?.username}</span>
              <svg viewBox="0 0 16 16" width="12" height="12" fill="currentColor" aria-hidden="true">
                <path d="m4.427 7.427 3.396 3.396a.25.25 0 0 0 .354 0l3.396-3.396A.25.25 0 0 0 11.396 7H4.604a.25.25 0 0 0-.177.427z"/>
              </svg>
            {/snippet}
            {#snippet menu(close)}
              <a href="/dashboard" onclick={close} role="menuitem">{t('nav.dashboard')}</a>
              <a href="/notifications" onclick={close} role="menuitem">{t('nav.notifications')}</a>
              <a href="/orgs" onclick={close} role="menuitem">{t('nav.organizations')}</a>
              <a href="/imports" onclick={close} role="menuitem">{t('nav.imports', 'Imports')}</a>
              <a href="/settings/security" onclick={close} role="menuitem">{t('nav.security', 'Security')}</a>
              <a href="/settings/ssh-keys" onclick={close} role="menuitem">{t('nav.ssh_keys', 'SSH keys')}</a>
              <a href="/settings/tokens" onclick={close} role="menuitem">{t('nav.access_tokens', 'Access tokens')}</a>
              {#if isAdmin()}
                <a href="/admin" class="admin-link" onclick={close} role="menuitem">{t('nav.admin_panel')}</a>
              {/if}
              <button onclick={() => { handleLogout(); close(); }} role="menuitem">{t('nav.sign_out')}</button>
            {/snippet}
          </Dropdown>
        </div>
      {:else}
        <a href="/register" class="btn-outline">{t('nav.sign_up')}</a>
        <a href="/login" class="btn-outline">{t('nav.sign_in')}</a>
      {/if}
    </div>
  </div>
</nav>

<style>
  .navbar {
    position: sticky;
    top: 0;
    z-index: 100;
    background: color-mix(in srgb, var(--bg-secondary) 92%, transparent 8%);
    backdrop-filter: blur(10px);
    border-bottom: 1px solid var(--border);
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.18);
  }

  .navbar-inner {
    max-width: min(1280px, calc(100vw - 32px));
    margin: 0 auto;
    display: grid;
    grid-template-columns: minmax(260px, 1.2fr) minmax(240px, 1fr) minmax(320px, 1.2fr);
    align-items: center;
    gap: 12px;
    padding: 10px 0;
    height: 62px;
  }

  .navbar-left,
  .navbar-right {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .navbar-left {
    min-width: 0;
    justify-self: start;
  }

  .navbar-right {
    justify-self: end;
    min-width: 0;
  }

  .logo {
    display: inline-flex;
    align-items: center;
    gap: 10px;
    color: var(--text-primary);
    text-decoration: none;
    font-weight: 600;
  }

  .logo:hover {
    text-decoration: none;
  }

  .logo-text {
    font-size: 17px;
    letter-spacing: -0.2px;
  }

  .nav-link {
    color: var(--text-secondary);
    text-decoration: none;
    font-size: 14px;
    font-weight: 500;
    padding: 5px 8px;
    border-radius: 6px;
    line-height: 1.2;
  }

  .nav-link:hover {
    color: var(--text-primary);
    background: var(--bg-hover);
    text-decoration: none;
  }

  .navbar-search {
    height: 36px;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    width: min(520px, 44vw);
    padding: 0 10px;
    background: var(--bg-primary);
    border: 1px solid var(--border);
    border-radius: 6px;
    color: var(--text-secondary);
  }

  .search-input {
    width: 100%;
    min-width: 0;
    border: 0;
    background: transparent;
    padding: 4px 0;
    color: var(--text-primary);
  }

  .search-input:focus {
    border: 0;
    outline: none;
    box-shadow: none;
  }

  .search-btn {
    border: 0;
    background: transparent;
    color: var(--text-secondary);
    padding: 0;
    font-size: 12px;
    font-weight: 600;
    cursor: pointer;
  }

  .search-btn:hover {
    color: var(--text-primary);
  }

  :global(.user-btn) {
    display: inline-flex;
    align-items: center;
    gap: 8px;
    background: none;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 4px 10px;
    color: var(--text-primary);
    font-size: 13px;
  }

  :global(.user-btn:hover) {
    background: var(--bg-hover);
  }

  .avatar {
    width: 24px;
    height: 24px;
    border-radius: 50%;
    background: var(--accent);
    color: #fff;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 12px;
    font-weight: 700;
    line-height: 1;
  }

  .lang-menu-container :global(.lang-btn) {
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 4px 10px;
    color: var(--text-primary);
    font-size: 13px;
    font-weight: 500;
    background: transparent;
  }

  .lang-menu-container :global(.lang-btn:hover) {
    background: var(--bg-hover);
  }

  .btn-outline {
    font-size: 13px;
    padding: 6px 12px;
  }

  @media (max-width: 1200px) {
    .navbar-inner {
      grid-template-columns: auto auto;
      grid-template-areas:
        "left search"
        "right right";
      height: auto;
      row-gap: 8px;
      padding: 10px 0;
    }

    .navbar-left { grid-area: left; }
    .navbar-search { grid-area: search; width: 100%; }
    .navbar-right { grid-area: right; justify-self: end; }
  }

  @media (max-width: 900px) {
    .navbar-inner {
      grid-template-columns: 1fr;
      grid-template-areas:
        "left"
        "search"
        "right";
    }

    .navbar-left,
    .navbar-right {
      flex-wrap: wrap;
      gap: 8px;
    }

    .navbar-left {
      gap: 8px;
    }

    .search-input { min-width: 180px; }
  }
</style>
