<script lang="ts">
  import { auth } from '$lib/api/client.svelte';
  import { adoptConfirmedSession } from '$lib/stores/auth.svelte';
  import { createT } from '$lib/i18n';

  const t = createT();

  // The token rides in the link; it is spent by the POST below, never by
  // opening the page — so a mail scanner that fetches every link it sees does
  // not use the link up before its owner clicks it.
  const token = new URLSearchParams(window.location.search).get('token') ?? '';

  let working = $state(false);
  let error = $state('');
  let movedTo = $state<string | null>(null);

  async function confirm() {
    working = true;
    error = '';
    try {
      const res = await auth.confirmEmail(token);
      if ('email' in res) {
        movedTo = res.email;
      } else {
        await adoptConfirmedSession(res.token);
        window.location.href = '/dashboard';
      }
    } catch (cause: unknown) {
      error = cause instanceof Error ? cause.message : String(cause);
    } finally {
      working = false;
    }
  }
</script>

<svelte:head>
  <title>{t('auth.verify_email.title', 'Confirm your email address')} · Plombir Git</title>
</svelte:head>

<div class="login-page">
  <div class="login-card">
    <h1>{t('auth.verify_email.title', 'Confirm your email address')}</h1>

    {#if error}
      <div class="error-banner" role="alert">{error}</div>
    {/if}

    {#if !token}
      <p>{t('auth.verify_email.no_token', 'This link carries no confirmation token. Open the link from the mail again.')}</p>
    {:else if movedTo}
      <p class="done" role="status">
        {t('auth.verify_email.moved', 'Your account now uses this address:')}
        <strong>{movedTo}</strong>
      </p>
      <a href="/settings/profile">{t('auth.verify_email.to_settings', 'Back to your profile')}</a>
    {:else}
      <p>{t('auth.verify_email.explain', 'Confirm that this address is yours to finish.')}</p>
      <button type="button" class="btn-primary confirm-email" onclick={confirm} disabled={working}>
        {working ? t('auth.verify_email.working', 'Confirming…') : t('auth.verify_email.confirm', 'Confirm address')}
      </button>
    {/if}
  </div>
</div>

<style>
  .login-card {
    width: 360px;
    background: var(--bg-secondary);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 32px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }

  h1 { font-size: 20px; margin: 0; }

  p { margin: 0; font-size: 14px; line-height: 1.5; }

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
  .btn-primary:disabled { opacity: 0.6; }
</style>
