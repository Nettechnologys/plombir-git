<script lang="ts">
  import Logo from '$lib/components/Logo.svelte';
  import { register, getAuthError, getAuthLoading } from '$lib/stores/auth.svelte';
  import { createT } from '$lib/i18n';
  import { getRegistrationOpen } from '$lib/stores/instance.svelte';
  import { PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH } from '$lib/passwordPolicy';

  const t = createT();

  let username = $state('');
  let email = $state('');
  let password = $state('');
  let localError = $state('');
  // The instance creates the account only once the mailed link is followed.
  let confirmationSent = $state(false);

  async function handleSubmit(e: Event) {
    e.preventDefault();
    localError = '';
    const ok = await register(username, email, password);
    if (ok === 'confirmation_sent') {
      confirmationSent = true;
    } else if (ok) {
      window.location.href = '/dashboard';
    } else {
      localError = getAuthError() || t('auth.register.failed');
    }
  }
</script>

<svelte:head>
  <title>{t('auth.register.title')} · Plombir Git</title>
</svelte:head>

<div class="login-page">
  <div class="login-card">
    <div class="login-header">
      <span class="brand-mark"><Logo size={40} /></span>
      <h1>{t('auth.register.title')}</h1>
    </div>

    {#if localError}
      <div class="error-banner">{localError}</div>
    {/if}

    {#if getRegistrationOpen() === false}
      <p class="registration-closed" role="status">
        {t(
          'auth.register.closed',
          'Self-service sign-up is closed on this instance. Ask an administrator for an account.',
        )}
      </p>
    {:else if confirmationSent}
      <p class="confirmation-sent" role="status">
        {t(
          'auth.register.confirmation_sent',
          'Check your inbox: we sent a link to confirm the address. The account is created when you follow it.',
        )}
      </p>
    {:else}
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
        <input
          type="password"
          bind:value={password}
          required
          autocomplete="new-password"
          minlength={PASSWORD_MIN_LENGTH}
          maxlength={PASSWORD_MAX_LENGTH}
          aria-describedby="password-policy"
        />
        <small id="password-policy" class="hint">{t('auth.password_policy', { min: PASSWORD_MIN_LENGTH })}</small>
      </label>

      <button type="submit" class="btn-primary" disabled={getAuthLoading()}>
        {getAuthLoading() ? t('auth.register.submitting') : t('auth.register.submit')}
      </button>
    </form>
    {/if}

    <p class="footer">
      {t('auth.register.footer', { link: '' })}
      <a href="/login">{t('auth.register.footer_link')}</a>
    </p>
  </div>
</div>

<style>
  .brand-mark {
    display: inline-flex;
    color: var(--accent);
  }

  .hint {
    display: block;
    margin-top: 4px;
    font-size: 12px;
    color: var(--text-secondary);
  }

  .registration-closed {
    color: var(--text-secondary);
  }

  .confirmation-sent {
    font-size: 14px;
    line-height: 1.5;
    color: var(--text-primary);
  }


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
