<script lang="ts">
  import Logo from '$lib/components/Logo.svelte';
  import { auth } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();

  let email = $state('');
  let loading = $state(false);
  let success = $state('');
  let localError = $state('');

  async function handleSubmit(e: Event) {
    e.preventDefault();
    localError = '';
    success = '';

    const trimmedEmail = email.trim();
    if (!trimmedEmail) {
      localError = t('auth.forgot_password.email_required');
      return;
    }

    loading = true;
    try {
      const res = await auth.forgotPassword(trimmedEmail);
      success = res.message || t('auth.forgot_password.sent');
    } catch (e: any) {
      localError = e.message || t('auth.forgot_password.failed');
    } finally {
      loading = false;
    }
  }
</script>

<svelte:head>
  <title>{t('auth.forgot_password.title')} · Plombir Git</title>
</svelte:head>

<div class="login-page">
  <div class="login-card">
    <div class="login-header">
      <span class="brand-mark"><Logo size={40} /></span>
      <h1>{t('auth.forgot_password.title')}</h1>
      <p>{t('auth.forgot_password.description')}</p>
    </div>

    {#if localError}
      <div class="error-banner">{localError}</div>
    {/if}

    {#if success}
      <div class="success-banner">{success}</div>
    {/if}

    <form onsubmit={handleSubmit}>
      <label>
        {t('auth.forgot_password.email')}
        <input type="email" bind:value={email} required autocomplete="email" disabled={loading} />
      </label>

      <button type="submit" class="btn-primary" disabled={loading || !email.trim()}>
        {loading ? t('auth.forgot_password.sending') : t('auth.forgot_password.submit')}
      </button>
    </form>

    <p class="footer">
      <a href="/login">{t('auth.back_to_login')}</a>
    </p>
  </div>
</div>

<style>
  .brand-mark {
    display: inline-flex;
    color: var(--accent);
  }

  .login-card {
    width: 360px;
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
    margin: 12px 0 6px;
  }

  p {
    margin: 0;
    color: var(--text-secondary);
    font-size: 13px;
    line-height: 1.5;
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

  .btn-primary:hover {
    background: var(--green);
  }

  .btn-primary:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }

  .success-banner {
    background: #f0fdf4;
    border: 1px solid #bbf7d0;
    color: #166534;
    padding: 12px 14px;
    border-radius: var(--radius);
    font-size: 14px;
    line-height: 1.5;
    margin-bottom: 16px;
  }

  .footer {
    text-align: center;
    margin-top: 20px;
    font-size: 13px;
  }

  .footer a {
    color: var(--accent);
    text-decoration: none;
  }
</style>
