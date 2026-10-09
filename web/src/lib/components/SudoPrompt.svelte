<!--
  Sudo mode: the password asked again before a credential is minted.

  Mounted once, in the root layout. It registers itself with the API client
  (`setSudoPrompt`), and `request` opens it when a route answers
  `403 { reason: "sudo_required" }` — the SSH key, token, passkey and SSO-link
  routes do, for a session that has not re-proved its password in the last
  ten minutes (security audit finding #7). On a successful `POST /users/me/sudo`
  the re-issued session is stored and the refused request is sent again; the
  page that made it never learns there was a prompt. Cancelling leaves the
  page with the original error.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from '$lib/components/Modal.svelte';
  import { setSudoPrompt } from '$lib/api/_base';
  import { auth, setToken } from '$lib/api/client.svelte';
  import { createT } from '$lib/i18n';
  import { getUser } from '$lib/stores/auth.svelte';

  const t = createT();

  let open = $state(false);
  let password = $state('');
  let code = $state('');
  let useBackupCode = $state(false);
  let error = $state('');
  let busy = $state(false);
  let settle: ((confirmed: boolean) => void) | null = null;

  const needsSecondFactor = $derived(getUser()?.mfa_enabled === true);

  function reset() {
    password = '';
    code = '';
    useBackupCode = false;
    error = '';
    busy = false;
  }

  function prompt(): Promise<boolean> {
    reset();
    open = true;
    return new Promise((resolve) => {
      settle = resolve;
    });
  }

  function finish(confirmed: boolean) {
    open = false;
    const resolve = settle;
    settle = null;
    resolve?.(confirmed);
  }

  function cancel() {
    if (busy) return;
    finish(false);
  }

  async function confirm(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    if (!password && !needsSecondFactor) {
      error = t('account_security.sudo.password_required');
      return;
    }
    const secondFactor = code.trim();
    if (needsSecondFactor && !secondFactor) {
      error = t('account_security.sudo.code_required');
      return;
    }
    busy = true;
    error = '';
    try {
      const session = await auth.sudo({
        password,
        ...(needsSecondFactor
          ? useBackupCode
            ? { backup_code: secondFactor }
            : { totp_code: secondFactor }
          : {}),
      });
      // The cookie came with the answer; the in-memory copy is what the
      // Bearer header of the retried request reads.
      setToken(session.token);
      finish(true);
    } catch (err: any) {
      error = err?.message || t('account_security.sudo.failed');
      busy = false;
    }
  }

  onMount(() => {
    setSudoPrompt(prompt);
    return () => {
      setSudoPrompt(null);
      if (settle) finish(false);
    };
  });
</script>

{#if open}
  <Modal onclose={cancel} labelledby="sudo-prompt-title">
    <form class="sudo-prompt" onsubmit={confirm}>
      <h2 id="sudo-prompt-title">{t('account_security.sudo.title')}</h2>
      <p class="muted">{t('account_security.sudo.description')}</p>

      <label for="sudo-password">{t('account_security.sudo.password')}</label>
      <input
        id="sudo-password"
        type="password"
        autocomplete="current-password"
        bind:value={password}
        disabled={busy}
        data-autofocus
      />

      {#if needsSecondFactor}
        <label for="sudo-code">
          {useBackupCode ? t('account_security.sudo.backup_code') : t('account_security.sudo.code')}
        </label>
        <input
          id="sudo-code"
          type="text"
          inputmode={useBackupCode ? 'text' : 'numeric'}
          autocomplete="one-time-code"
          bind:value={code}
          disabled={busy}
        />
        <button
          type="button"
          class="link"
          disabled={busy}
          onclick={() => {
            useBackupCode = !useBackupCode;
            code = '';
          }}
        >
          {useBackupCode ? t('account_security.sudo.use_code') : t('account_security.sudo.use_backup_code')}
        </button>
      {/if}

      {#if error}
        <p class="error" role="alert">{error}</p>
      {/if}

      <div class="actions">
        <button type="button" class="secondary" disabled={busy} onclick={cancel}>
          {t('common.cancel')}
        </button>
        <button type="submit" class="primary" disabled={busy}>
          {busy ? t('account_security.sudo.confirming') : t('account_security.sudo.confirm')}
        </button>
      </div>
    </form>
  </Modal>
{/if}

<style>
  .sudo-prompt {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  h2 {
    margin: 0;
    font-size: 1.1rem;
  }

  .muted {
    margin: 0 0 0.5rem;
    color: var(--text-secondary);
    font-size: 0.9rem;
  }

  label {
    font-size: 0.85rem;
    color: var(--text-secondary);
  }

  input {
    padding: 0.5rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--bg-secondary);
    color: var(--text-primary);
    font: inherit;
  }

  .link {
    align-self: flex-start;
    padding: 0;
    border: none;
    background: none;
    color: var(--accent, #58a6ff);
    font-size: 0.85rem;
    cursor: pointer;
  }

  .error {
    margin: 0;
    color: var(--red, #f85149);
    font-size: 0.9rem;
  }

  .actions {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }

  .actions button {
    padding: 0.45rem 0.9rem;
    border-radius: 6px;
    border: 1px solid var(--border);
    font: inherit;
    cursor: pointer;
  }

  .primary {
    background: var(--accent, #238636);
    color: #fff;
    border-color: transparent;
  }

  .secondary {
    background: var(--bg-secondary);
    color: var(--text-primary);
  }

  button:disabled {
    cursor: wait;
    opacity: 0.65;
  }
</style>
