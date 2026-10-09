<script lang="ts">
  import {
    login,
    loginWithPasskey,
    verifyMfa,
    getAuthError,
    getAuthLoading,
    isLoggedIn,
    isMfaRequired,
    isPasswordChangeRequired,
    completeInitialPassword,
    beginMfa,
  } from '$lib/stores/auth.svelte';
  import { createT } from '$lib/i18n';
  import { auth, isPasskeySupported, type PublicSsoProvider } from '$lib/api/client.svelte';
  import { isUnavailable, optionalSection } from '$lib/optionalSection';
  import { readSsoError, ssoErrorMessage, type SsoError } from '$lib/ssoError';
  import { goto } from '$app/navigation';

  const t = createT();

  let username = $state('');
  let password = $state('');
  let mfaCode = $state('');
  let newPassword = $state('');
  let confirmPassword = $state('');
  let useBackupCode = $state(false);
  let localError = $state('');
  const SSO_UNAVAILABLE = 'unavailable';
  type SsoUnavailable = typeof SSO_UNAVAILABLE;

  let ssoProviders = $state<PublicSsoProvider[] | SsoUnavailable>([]);
  let ssoLoading = $state(true);
  const knownSsoProviders = $derived(
    ssoProviders === SSO_UNAVAILABLE ? [] : ssoProviders,
  );
  const passkeySupported = isPasskeySupported();

  // A refused SSO round trip comes back here as `?sso_error=<code>&provider=`.
  // The sentence is derived, not stored, so it follows a language switch and
  // picks up the provider's display name once the list has loaded.
  let ssoError = $state<SsoError | null>(null);
  const ssoErrorText = $derived.by(() => {
    if (!ssoError) return '';
    const slug = ssoError.provider;
    const name = knownSsoProviders.find((provider) => provider.slug === slug)?.name ?? (slug || 'SSO');
    return ssoErrorMessage(t, ssoError.code, name);
  });

  // Redirect if already logged in (prevents flash of login form for authenticated users)
  $effect(() => {
    if (isLoggedIn()) {
      goto('/dashboard');
    }
  });

  $effect(() => {
    const params = new URLSearchParams(window.location.search);
    const refused = readSsoError(window.location.search);
    if (refused) {
      ssoError = refused;
      window.history.replaceState({}, '', '/login');
    }
    // Two doors hand off here holding a challenge cookie and no session: the
    // SSO callback, and the password reset of an account with a second factor.
    if (params.get('sso_mfa_required') === '1' || params.get('mfa_required') === '1') {
      const usernameParam = params.get('username');
      if (usernameParam) {
        beginMfa(usernameParam);
        window.history.replaceState({}, '', '/login');
      }
    }
    loadSsoProviders();
  });

  async function loadSsoProviders() {
    ssoLoading = true;
    const providers = await optionalSection(
      auth.listSsoProviders(),
      'single sign-on options',
    );
    ssoProviders = isUnavailable(providers)
      ? SSO_UNAVAILABLE
      : providers.filter(
        (provider) => provider.provider_type !== 'ldap',
      );
    ssoLoading = false;
  }

  async function handleSubmit(e: Event) {
    e.preventDefault();
    localError = '';
    const ok = await login(username, password);
    if (ok) {
      window.location.href = '/dashboard';
    } else if (isMfaRequired()) {
      localError = '';
    } else {
      localError = getAuthError() || t('auth.login.failed');
    }
  }

  // An administrator chose the password that just worked: it opens nothing
  // until it is replaced. The server proves it once more with the new one.
  async function handleInitialPassword(e: Event) {
    e.preventDefault();
    localError = '';
    if (newPassword !== confirmPassword) {
      localError = t('auth.login.passwords_differ', 'The two new passwords differ.');
      return;
    }
    const ok = await completeInitialPassword(password, newPassword);
    if (ok) {
      window.location.href = '/dashboard';
    } else if (!isMfaRequired()) {
      localError = getAuthError() || t('auth.login.failed');
    }
  }

  async function handlePasskeyLogin() {
    localError = '';
    if (!username.trim()) {
      localError = t('auth.login.passkey_username_first');
      return;
    }
    const ok = await loginWithPasskey(username);
    if (ok) {
      window.location.href = '/dashboard';
    } else {
      localError = getAuthError() || t('auth.login.passkey_failed');
    }
  }

  async function handleMfaSubmit(e: Event) {
    e.preventDefault();
    localError = '';
    const ok = await verifyMfa(mfaCode.trim(), useBackupCode);
    if (ok) {
      window.location.href = '/dashboard';
    } else {
      localError = getAuthError() || t('auth.login.mfa_failed');
    }
  }
</script>

<svelte:head>
  <title>{t('auth.login.title')} · Plombir Git</title>
</svelte:head>

<div class="login-page">
  <div class="login-card">
    <div class="login-header">
      <svg viewBox="0 0 16 16" width="40" height="40" fill="var(--accent)">
        <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/>
      </svg>
      <h1>{t('auth.login.title')}</h1>
    </div>

    {#if ssoErrorText}
      <div class="error-banner" role="alert">{ssoErrorText}</div>
    {/if}

    {#if localError}
      <div class="error-banner">{localError}</div>
    {/if}

    {#if isPasswordChangeRequired()}
      <form class="initial-password-form" onsubmit={handleInitialPassword}>
        <p class="hint">
          {t(
            'auth.login.password_change_required',
            'An administrator set this password. Choose your own to continue.',
          )}
        </p>
        <label>
          {t('auth.login.new_password', 'New password')}
          <input type="password" bind:value={newPassword} required autocomplete="new-password" />
        </label>
        <label>
          {t('auth.login.confirm_password', 'Repeat the new password')}
          <input type="password" bind:value={confirmPassword} required autocomplete="new-password" />
        </label>
        <button type="submit" class="btn-primary" disabled={getAuthLoading() || !newPassword}>
          {t('auth.login.set_password', 'Set password and sign in')}
        </button>
      </form>
    {:else if isMfaRequired()}
      <form onsubmit={handleMfaSubmit}>
        <label>
          {t('auth.login.mfa_code')}
          <input
            type="text"
            bind:value={mfaCode}
            required
            inputmode={useBackupCode ? 'text' : 'numeric'}
            autocapitalize={useBackupCode ? 'characters' : 'off'}
            autocomplete="one-time-code"
          />
        </label>

        <label class="checkbox-label">
          <input type="checkbox" bind:checked={useBackupCode} />
          {t('auth.login.use_backup_code')}
        </label>

        <button type="submit" class="btn-primary" disabled={getAuthLoading() || !mfaCode.trim()}>
          {getAuthLoading() ? t('auth.login.verifying') : t('auth.login.verify')}
        </button>
      </form>
    {:else}
      <form onsubmit={handleSubmit}>
        <label>
          {t('auth.login.username')}
          <input type="text" bind:value={username} required autocomplete="username" />
        </label>

        <label>
          {t('auth.login.password')}
          <input type="password" bind:value={password} required autocomplete="current-password" />
        </label>

        <button type="submit" class="btn-primary" disabled={getAuthLoading()}>
          {getAuthLoading() ? t('auth.login.submitting') : t('auth.login.submit')}
        </button>

        {#if passkeySupported}
          <button
            type="button"
            class="btn-passkey"
            onclick={handlePasskeyLogin}
            disabled={getAuthLoading()}
          >
            <svg viewBox="0 0 24 24" width="18" height="18" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M2 18v3h6v-3a3 3 0 0 0-3-3 3 3 0 0 0-3 3Z" />
              <circle cx="5" cy="10" r="3" />
              <path d="M12 8h9M18 8v4M15 8v2" />
            </svg>
            {t('auth.login.passkey_sign_in')}
          </button>
        {/if}
      </form>
    {/if}

    {#if ssoLoading}
      <p class="sso-loading">{t('auth.login.sso_loading')}</p>
    {:else if ssoProviders === SSO_UNAVAILABLE && !isMfaRequired()}
      <div class="error-banner sso-unavailable" role="alert">
        <span>{t('auth.login.sso_unavailable')}</span>
        <button type="button" class="btn btn-sm btn-outline" onclick={loadSsoProviders}>
          {t('auth.login.sso_retry')}
        </button>
      </div>
    {:else if knownSsoProviders.length > 0 && !isMfaRequired()}
      <div class="sso-divider"><span>{t('auth.login.sso_or')}</span></div>
      <div class="sso-providers">
        {#each knownSsoProviders as provider (provider.slug)}
          <a class="sso-button" href={auth.ssoAuthorizeUrl(provider.slug)}>
            {#if provider.icon_url}
              <img src={provider.icon_url} alt="" width="20" height="20" />
            {/if}
            {t('auth.login.sso_continue', { provider: provider.name })}
          </a>
        {/each}
      </div>
    {/if}

    <p class="footer">
      {t('auth.login.footer', { link: '' })}
      <a href="/register">{t('auth.login.footer_link')}</a>
      <span class="separator">·</span>
      <a href="/forgot-password">{t('auth.login.forgot_password')}</a>
    </p>
  </div>
</div>

<style>

  .login-card {
    width: 340px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 32px;
  }

  .login-header {
    text-align: center;
    margin-bottom: 24px;
  }

  h1 {
    font-size: 20px;
    margin-top: 12px;
  }

  form {
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  label {
    display: flex;
    flex-direction: column;
    gap: 6px;
    font-size: 14px;
    font-weight: 600;
    color: var(--text-primary);
  }

  input {
    padding: 8px 12px;
  }

  .hint {
    margin: 0;
    font-size: 13px;
    color: var(--text-secondary);
  }

  .checkbox-label {
    flex-direction: row;
    align-items: center;
    font-weight: 500;
  }

  .checkbox-label input {
    width: auto;
  }

  .btn-primary {
    padding: 8px 16px;
    background: var(--green-dim);
    color: #fff;
    border: none;
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }
  .btn-primary:hover { background: var(--green); }
  .btn-primary:disabled { opacity: 0.6; }

  .btn-passkey {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 8px 16px;
    background: var(--bg-primary);
    color: var(--text-primary);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    font-size: 14px;
    font-weight: 600;
    cursor: pointer;
  }
  .btn-passkey:hover { background: var(--bg-tertiary); }
  .btn-passkey:disabled { opacity: 0.6; cursor: not-allowed; }

  .sso-loading {
    margin: 18px 0 0;
    color: var(--text-secondary);
    font-size: 13px;
    text-align: center;
  }

  .sso-unavailable {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    margin: 18px 0 0;
  }

  .sso-unavailable button {
    flex: none;
  }

  .sso-divider {
    display: flex;
    align-items: center;
    gap: 12px;
    margin: 20px 0 14px;
    color: var(--text-secondary);
    font-size: 12px;
  }

  .sso-divider::before,
  .sso-divider::after {
    content: '';
    flex: 1;
    border-top: 1px solid var(--border);
  }

  .sso-providers { display: grid; gap: 10px; }

  .sso-button {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 8px 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--text-primary);
    text-decoration: none;
  }

  .sso-button:hover { background: var(--bg-tertiary); }
  .sso-button img { object-fit: contain; }

  .footer {
    text-align: center;
    margin-top: 20px;
    font-size: 13px;
    color: var(--text-secondary);
  }
</style>
