<!--
  Sudo mode: prove it is you again before a credential is minted.

  Mounted once, in the root layout. It registers itself with the API client
  (`setSudoPrompt`), and `request` opens it when a route answers
  `403 { reason: "sudo_required" }`: the key, token, passkey and SSO-link
  routes, and the MFA changes of an account without a password here, do so for
  a session that has not proved itself in the last ten minutes (decision #10).

  What it asks for follows the account, read fresh from `/users/me` each time
  it opens, so a second factor enrolled a minute ago is asked for:
  - a password of its own, or an LDAP account's directory password, plus the
    second factor when one is enrolled;
  - for an account whose password lives with its SSO provider, a sign-in
    through that provider again, which leaves the page and comes back to it;
  - a passkey, for anyone with one.
  On success the re-issued session is stored and the refused request is sent
  again; the page that made it never learns there was a prompt.
-->
<script lang="ts">
  import { onMount } from 'svelte';
  import Modal from '$lib/components/Modal.svelte';
  import { auth, setSudoPrompt, setToken, type Me } from '$lib/api/client.svelte';
  import { isPasskeySupported, passkeys } from '$lib/api/passkeys';
  import { createT } from '$lib/i18n';

  const t = createT();

  let open = $state(false);
  let me = $state<Me | null>(null);
  let password = $state('');
  let code = $state('');
  let useBackupCode = $state(false);
  let error = $state('');
  let busy = $state(false);
  let settle: ((confirmed: boolean) => void) | null = null;

  const asksPassword = $derived(me !== null && (me.has_password !== false || me.auth_provider === 'ldap'));
  const needsSecondFactor = $derived(asksPassword && me?.mfa_enabled === true);
  const provider = $derived(me && !asksPassword ? me.auth_provider : null);
  const canUsePasskey = isPasskeySupported();

  function reset() {
    password = '';
    code = '';
    useBackupCode = false;
    error = '';
    busy = false;
  }

  async function prompt(): Promise<boolean> {
    reset();
    me = null;
    open = true;
    const result = new Promise<boolean>((resolve) => {
      settle = resolve;
    });
    try {
      me = await auth.me();
    } catch (err: any) {
      error = err?.message || t('account_security.sudo.failed');
    }
    return result;
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

  async function confirmPassword(event: SubmitEvent) {
    event.preventDefault();
    if (busy) return;
    if (!password) {
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

  async function confirmWithPasskey() {
    if (busy || !me) return;
    busy = true;
    error = '';
    try {
      // A passkey sign-in is a fresh login, and a fresh login starts in sudo
      // mode.
      const session = await passkeys.login(me.username);
      setToken(session.token);
      finish(true);
    } catch (err: any) {
      error = err?.message || t('account_security.sudo.failed');
      busy = false;
    }
  }

  async function signInAgain() {
    if (busy || !provider) return;
    busy = true;
    error = '';
    try {
      const { authorize_url } = await auth.reauthenticateSso(
        provider,
        `${window.location.pathname}${window.location.search}`,
      );
      // The page is left; whoever asked gets their original refusal, and the
      // person comes back here in sudo mode to try again.
      finish(false);
      window.location.assign(authorize_url);
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
    <div class="sudo-prompt">
      <h2 id="sudo-prompt-title">{t('account_security.sudo.title')}</h2>
      {#if me && asksPassword}
        <p class="muted">{t('account_security.sudo.description')}</p>
        <form onsubmit={confirmPassword}>
          <label for="sudo-password">
            {me.auth_provider === 'ldap' ? t('account_security.sudo.directory_password') : t('account_security.sudo.password')}
          </label>
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
          <button type="submit" class="primary" disabled={busy}>
            {busy ? t('account_security.sudo.confirming') : t('account_security.sudo.confirm')}
          </button>
        </form>
      {:else if me && provider}
        <p class="muted">{t('account_security.sudo.provider_description', { provider })}</p>
        <button type="button" class="primary" disabled={busy} onclick={signInAgain}>
          {t('account_security.sudo.sign_in_again', { provider })}
        </button>
      {/if}

      {#if me && canUsePasskey}
        <button type="button" class="secondary" disabled={busy} onclick={confirmWithPasskey}>
          {t('account_security.sudo.use_passkey')}
        </button>
      {/if}

      {#if error}
        <p class="error" role="alert">{error}</p>
      {/if}

      <div class="actions">
        <button type="button" class="secondary" disabled={busy} onclick={cancel}>
          {t('common.cancel')}
        </button>
      </div>
    </div>
  </Modal>
{/if}

<style>
  .sudo-prompt,
  form {
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
    margin-top: 0.5rem;
  }

  button.primary,
  button.secondary {
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
