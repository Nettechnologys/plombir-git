<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { auth } from '$lib/api/client.svelte';
  import { setToken } from '$lib/api/client.svelte';
  import { fetchUser } from '$lib/stores/auth.svelte';
  import { createT } from '$lib/i18n';
  import { PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH } from '$lib/passwordPolicy';

  const t = createT();

  let token = $state('');
  let password = $state('');
  let confirmPassword = $state('');
  let loading = $state(false);
  let success = $state(false);
  let localError = $state('');

  const specialChars = /[!@#$%^&*()_+\-=[\]{}|;:,.<>?/~`'"\\]/;
  const passwordPattern =
    "(?=.*[a-z])(?=.*[A-Z])(?=.*\\d)(?=.*[!@#$%^&*()_+\\-=\\[\\]{}|;:,.<>?/~`'\\\"\\\\])\\S{8,128}";

  onMount(() => {
    const params = new URLSearchParams(window.location.search);
    const fromLink = params.get('token');
    if (fromLink) token = fromLink;
  });

  async function handleSubmit(e: Event) {
    e.preventDefault();
    localError = '';

    if (password !== confirmPassword) {
      localError = t('auth.reset_password.mismatch');
      return;
    }

    const passwordError = validatePassword(password);
    if (passwordError) {
      localError = passwordError;
      return;
    }

    loading = true;
    try {
      const res = await auth.resetPassword(token, password);
      if (res.mfa_required) {
        // The password changed; the session did not follow. The server has set
        // the same five-minute challenge cookie the login door sets, so the
        // second-factor form on /login finishes the job — the same hand-off SSO
        // makes.
        goto(`/login?mfa_required=1&username=${encodeURIComponent(res.username)}`);
        return;
      }
      setToken(res.token);
      await fetchUser();
      success = true;
    } catch (e: any) {
      localError = e.message || t('auth.reset_password.failed');
    } finally {
      loading = false;
    }
  }

  function validatePassword(value: string): string {
    if (value.length < 8) return t('auth.password_rules.min_length');
    if (value.length > 128) return t('auth.password_rules.max_length');
    if (/\s/.test(value)) return t('auth.password_rules.no_spaces');
    if (!/[A-Z]/.test(value)) return t('auth.password_rules.uppercase');
    if (!/[a-z]/.test(value)) return t('auth.password_rules.lowercase');
    if (!/[0-9]/.test(value)) return t('auth.password_rules.number');
    if (!specialChars.test(value)) return t('auth.password_rules.special');
    return '';
  }
</script>

<svelte:head>
  <title>{t('auth.reset_password.title')} · Plombir Git</title>
</svelte:head>

<div class="reset-page">
  <div class="reset-card">
    <div class="reset-header">
      <h1>{t('auth.reset_password.title')}</h1>
      <p class="subtitle">{t('auth.reset_password.subtitle')}</p>
    </div>

    {#if success}
      <div class="success-banner">
        {t('auth.reset_password.success')}
      </div>
      <a href="/dashboard" class="btn-secondary" style="display:block;text-align:center;margin-top:16px;">
        {t('auth.reset_password.go_dashboard')}
      </a>
    {:else if !token}
      <div class="error-banner">
        {t('auth.reset_password.invalid_link')}
      </div>
      <a href="/forgot-password" class="btn-secondary" style="display:block;text-align:center;margin-top:16px;">
        {t('auth.reset_password.request_reset')}
      </a>
    {:else}
      {#if localError}
        <div class="error-banner">{localError}</div>
      {/if}

      <form onsubmit={handleSubmit}>
        <label>
          {t('auth.reset_password.new_password')}
          <input
            type="password"
            bind:value={password}
            required
            minlength={PASSWORD_MIN_LENGTH}
            maxlength={PASSWORD_MAX_LENGTH}
            pattern={passwordPattern}
            placeholder={t('auth.reset_password.new_password_placeholder')}
            autocomplete="new-password"
          />
        </label>

        <label>
          {t('auth.reset_password.confirm_password')}
          <input
            type="password"
            bind:value={confirmPassword}
            required
            placeholder={t('auth.reset_password.confirm_placeholder')}
            autocomplete="new-password"
          />
        </label>

        <button type="submit" class="btn-primary" disabled={loading}>
          {loading ? t('auth.reset_password.resetting') : t('auth.reset_password.title')}
        </button>
      </form>

      <p class="footer">
        <a href="/login">{t('auth.back_to_login')}</a>
      </p>
    {/if}
  </div>
</div>

<style>
  .reset-card {
    background: var(--card-bg, #fff);
    border: 1px solid var(--border, #e5e7eb);
    border-radius: 8px;
    padding: 32px;
    width: 100%;
    max-width: 400px;
  }
  .reset-header {
    text-align: center;
    margin-bottom: 24px;
  }
  .reset-header h1 {
    margin: 0 0 8px;
    font-size: 24px;
    color: var(--text, #1f2937);
  }
  .subtitle {
    margin: 0;
    color: var(--text-muted, #6b7280);
    font-size: 14px;
  }
  label {
    display: block;
    margin-bottom: 16px;
    font-weight: 500;
    color: var(--text, #1f2937);
  }
  input {
    display: block;
    width: 100%;
    margin-top: 4px;
    padding: 8px 12px;
    border: 1px solid var(--border, #d1d5db);
    border-radius: 6px;
    font-size: 14px;
    box-sizing: border-box;
  }
  .btn-primary {
    width: 100%;
    padding: 10px 16px;
    background: var(--accent, #4f46e5);
    color: #fff;
    border: none;
    border-radius: 6px;
    font-size: 14px;
    cursor: pointer;
  }
  .btn-primary:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
  .btn-secondary {
    padding: 10px 16px;
    background: var(--bg-secondary, #f3f4f6);
    color: var(--text, #1f2937);
    border: 1px solid var(--border, #d1d5db);
    border-radius: 6px;
    font-size: 14px;
    text-decoration: none;
  }
.success-banner {
    background: #f0fdf4;
    border: 1px solid #bbf7d0;
    color: #16a34a;
    padding: 14px 18px;
    border-radius: 6px;
    font-size: 14px;
    line-height: 1.5;
  }
  .footer {
    text-align: center;
    margin-top: 16px;
    font-size: 14px;
  }
  .footer a {
    color: var(--accent, #4f46e5);
    text-decoration: none;
  }
</style>
