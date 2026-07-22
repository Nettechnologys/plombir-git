<script lang="ts">
  import { register, getAuthError, getAuthLoading } from '$lib/stores/auth.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();

  let username = $state('');
  let email = $state('');
  let password = $state('');
  let localError = $state('');

  async function handleSubmit(e: Event) {
    e.preventDefault();
    localError = '';
    const ok = await register(username, email, password);
    if (ok) {
      window.location.href = '/dashboard';
    } else {
      localError = getAuthError() || t('auth.register.failed');
    }
  }
</script>

<svelte:head>
  <title>{t('auth.register.title')} · IronForge</title>
</svelte:head>

<div class="login-page">
  <div class="login-card">
    <div class="login-header">
      <svg viewBox="0 0 16 16" width="40" height="40" fill="var(--accent)">
        <path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/>
      </svg>
      <h1>{t('auth.register.title')}</h1>
    </div>

    {#if localError}
      <div class="error-banner">{localError}</div>
    {/if}

    <form onsubmit={handleSubmit}>
      <label>
        {t('auth.register.username')}
        <input type="text" bind:value={username} required autocomplete="username" />
      </label>

      <label>
        {t('auth.register.email')}
        <input type="email" bind:value={email} required autocomplete="email" />
      </label>

      <label>
        {t('auth.register.password')}
        <input type="password" bind:value={password} required autocomplete="new-password" minlength={6} />
      </label>

      <button type="submit" class="btn-primary" disabled={getAuthLoading()}>
        {getAuthLoading() ? t('auth.register.submitting') : t('auth.register.submit')}
      </button>
    </form>

    <p class="footer">
      {t('auth.register.footer', { link: '' })}
      <a href="/login">{t('auth.register.footer_link')}</a>
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

  h1 { font-size: 20px; margin-top: 12px; }

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

  input { padding: 8px 12px; }

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
.footer {
    text-align: center;
    margin-top: 20px;
    font-size: 13px;
    color: var(--text-secondary);
  }
</style>
