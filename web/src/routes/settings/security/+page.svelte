<script lang="ts">
  import { goto } from '$app/navigation';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';
  import { isUnavailable, optionalSection } from '$lib/optionalSection';
  import {
    auth,
    mfa,
    passkeys,
    isPasskeySupported,
    type MfaBackupStatus,
    type MfaSetupResponse,
    type PasskeyInfo,
    type SsoLink,
  } from '$lib/api/client.svelte';

  let loading = $state(true);
  let saving = $state(false);
  let error = $state('');
  let success = $state('');
  // This page is where a reader decides whether their account is protected, and
  // the MFA slot also decides which action it offers. A read that never answered
  // must therefore not settle into the value a genuine "no" holds: `GET
  // /users/mfa/backup` answering 5xx used to leave `backupStatus` at the same
  // `null` an account without codes has, so the section stated "MFA is not
  // enabled" and offered the one button that overwrites `users.totp_secret`
  // unconditionally — a failed read inviting the reader to destroy a second
  // factor that was live all along (card_59b36db201a2, card_08400088bb40).
  // Each of the three reads resolves to UNKNOWN instead, which the markup shows
  // as its own state and answers with a re-read rather than an action.
  const UNKNOWN = 'unknown';
  type Unknown = typeof UNKNOWN;

  let backupStatus = $state<MfaBackupStatus | Unknown | null>(null);
  let setup = $state<MfaSetupResponse | null>(null);
  let verificationCode = $state('');
  let disablePassword = $state('');
  let regeneratePassword = $state('');
  let newBackupCodes = $state<string[]>([]);

  let passkeyList = $state<PasskeyInfo[] | Unknown>([]);
  let passkeyName = $state('');
  let passkeyBusy = $state(false);
  const passkeySupported = isPasskeySupported();

  let ssoLinks = $state<SsoLink[] | Unknown>([]);
  let ssoBusy = $state(false);
  const securityRequests = new LatestRequestFence<'security-load'>();
  const backupStatusRequests = new LatestRequestFence<'backup-status'>();
  const passkeyRequests = new LatestRequestFence<'passkeys'>();
  const ssoRequests = new LatestRequestFence<'sso-links'>();

  const mfaStateUnknown = $derived(backupStatus === UNKNOWN);
  const backupCodes = $derived(backupStatus === UNKNOWN ? null : backupStatus);
  const mfaEnabled = $derived((backupCodes?.total ?? 0) > 0);
  const passkeyListUnknown = $derived(passkeyList === UNKNOWN);
  const knownPasskeys = $derived(passkeyList === UNKNOWN ? [] : passkeyList);
  const ssoLinksUnknown = $derived(ssoLinks === UNKNOWN);
  const knownSsoLinks = $derived(ssoLinks === UNKNOWN ? [] : ssoLinks);

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    loadSecurity();
  });

  async function loadSecurity() {
    const claim = securityRequests.begin('security-load');
    loading = true;
    error = '';
    // Each slot claims its own state owner, synchronously, inside its loader,
    // so a retry of one section cannot publish over a mutation running in
    // another — and so one refused read leaves the other two intact.
    await Promise.all([loadBackupStatus(), loadPasskeyList(), loadSsoLinks()]);
    if (securityRequests.owns(claim, 'security-load')) loading = false;
  }

  async function loadBackupStatus() {
    const claim = backupStatusRequests.begin('backup-status');
    const result = await optionalSection(
      mfa.backup(),
      'the multi-factor state of this account',
    );

    if (isUnavailable(result)) {
      if (backupStatusRequests.owns(claim, 'backup-status')) backupStatus = UNKNOWN;
    } else if (backupStatusRequests.owns(claim, 'backup-status')) {
      backupStatus = result;
    }
  }

  async function loadPasskeyList() {
    const claim = passkeyRequests.begin('passkeys');
    const result = await optionalSection(
      passkeySupported ? passkeys.list() : Promise.resolve<PasskeyInfo[]>([]),
      'the passkeys registered on this account',
    );

    if (isUnavailable(result)) {
      if (passkeyRequests.owns(claim, 'passkeys')) passkeyList = UNKNOWN;
    } else if (passkeyRequests.owns(claim, 'passkeys')) {
      passkeyList = result;
    }
  }

  async function loadSsoLinks() {
    const claim = ssoRequests.begin('sso-links');
    const result = await optionalSection(
      auth.listSsoLinks(),
      'the external identities linked to this account',
    );

    if (isUnavailable(result)) {
      if (ssoRequests.owns(claim, 'sso-links')) ssoLinks = UNKNOWN;
    } else if (ssoRequests.owns(claim, 'sso-links')) {
      ssoLinks = result;
    }
  }

  async function addPasskey(event: SubmitEvent) {
    event.preventDefault();
    if (passkeyBusy) return;
    const claim = passkeyRequests.begin('passkeys');
    try {
      passkeyBusy = true;
      error = '';
      success = '';
      const next = await passkeys.register(passkeyName.trim());
      if (passkeyRequests.owns(claim, 'passkeys')) passkeyList = next;
      passkeyName = '';
      success = 'Passkey registered.';
    } catch (err: any) {
      error = err.message || 'Failed to register passkey';
    } finally {
      passkeyBusy = false;
    }
  }

  async function removePasskey(id: number) {
    if (!confirm('Remove this passkey? You will no longer be able to sign in with it.')) return;
    if (passkeyBusy) return;
    const claim = passkeyRequests.begin('passkeys');
    try {
      passkeyBusy = true;
      error = '';
      success = '';
      await passkeys.remove(id);
      if (passkeyRequests.owns(claim, 'passkeys') && passkeyList !== UNKNOWN) {
        passkeyList = passkeyList.filter((p) => p.id !== id);
      }
      success = 'Passkey removed.';
    } catch (err: any) {
      error = err.message || 'Failed to remove passkey';
    } finally {
      passkeyBusy = false;
    }
  }

  async function unlinkSsoProvider(link: SsoLink) {
    if (
      !confirm(
        `Unlink ${link.name}? Signing in through that provider will create the link again, ` +
          'so make sure you can still sign in some other way first.',
      )
    )
      return;
    if (ssoBusy) return;
    const claim = ssoRequests.begin('sso-links');
    try {
      ssoBusy = true;
      error = '';
      success = '';
      await auth.unlinkSso(link.slug);
      if (ssoRequests.owns(claim, 'sso-links') && ssoLinks !== UNKNOWN) {
        ssoLinks = ssoLinks.filter((entry) => entry.slug !== link.slug);
      }
      success = `${link.name} unlinked.`;
    } catch (err: any) {
      error = err.message || 'Failed to unlink the provider';
    } finally {
      ssoBusy = false;
    }
  }

  async function startSetup() {
    if (saving) return;
    try {
      saving = true;
      error = '';
      success = '';
      newBackupCodes = [];
      setup = await mfa.setup();
    } catch (err: any) {
      error = err.message || 'Failed to start MFA setup';
    } finally {
      saving = false;
    }
  }

  async function enableMfa(event: SubmitEvent) {
    event.preventDefault();
    if (saving) return;
    if (!verificationCode.trim()) {
      error = 'Authentication code is required';
      return;
    }

    try {
      saving = true;
      error = '';
      success = '';
      const result = await mfa.enable(verificationCode.trim());
      newBackupCodes = result.backup_codes;
      setup = null;
      verificationCode = '';
      success = 'MFA enabled. Save your backup codes before leaving this page.';
      await loadSecurity();
    } catch (err: any) {
      error = err.message || 'Failed to enable MFA';
    } finally {
      saving = false;
    }
  }

  async function disableMfa(event: SubmitEvent) {
    event.preventDefault();
    if (saving) return;
    if (!disablePassword) {
      error = 'Current password is required';
      return;
    }
    if (!confirm('Disable multi-factor authentication for your account?')) return;

    try {
      saving = true;
      error = '';
      success = '';
      await mfa.disable(disablePassword);
      disablePassword = '';
      newBackupCodes = [];
      setup = null;
      success = 'MFA disabled';
      await loadSecurity();
    } catch (err: any) {
      error = err.message || 'Failed to disable MFA';
    } finally {
      saving = false;
    }
  }

  async function regenerateBackupCodes(event: SubmitEvent) {
    event.preventDefault();
    if (saving) return;
    if (!regeneratePassword) {
      error = 'Current password is required';
      return;
    }
    if (!confirm('Replace your backup codes? Every unused code you have now stops working.')) return;

    try {
      saving = true;
      error = '';
      success = '';
      const result = await mfa.regenerateBackup(regeneratePassword);
      regeneratePassword = '';
      newBackupCodes = result.backup_codes;
      success = 'New backup codes issued. Save them before leaving this page — the old ones no longer work.';
      await loadSecurity();
    } catch (err: any) {
      error = err.message || 'Failed to regenerate backup codes';
    } finally {
      saving = false;
    }
  }

  async function copyBackupCodes() {
    if (newBackupCodes.length === 0) return;
    await navigator.clipboard.writeText(newBackupCodes.join('\n'));
    success = 'Backup codes copied';
  }
</script>

<svelte:head>
  <title>Security · ForgeKeep</title>
</svelte:head>

<div class="page-container security-page">
  <header class="page-header">
    <div>
      <h1>Security</h1>
      <p>Manage account protections for web login and Git/API access.</p>
    </div>
  </header>

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  <section class="section">
    <div class="section-heading">
      <div>
        <h2>Multi-Factor Authentication</h2>
        <p>Add a time-based authenticator code after password login.</p>
      </div>
      <span class:enabled={mfaEnabled} class:unknown={mfaStateUnknown} class="status">
        {mfaStateUnknown ? 'Unknown' : mfaEnabled ? 'Enabled' : 'Disabled'}
      </span>
    </div>

    {#if loading}
      <p class="muted">Loading...</p>
    {:else if mfaStateUnknown}
      <p class="muted state-unknown">
        The second factor of this account could not be read, so this page cannot say whether MFA is
        on. Starting a new setup from here would replace an authenticator that may still be live, so
        the read is offered again instead.
      </p>
      <button type="button" class="btn btn-secondary" onclick={loadBackupStatus} disabled={saving}>
        Retry reading MFA state
      </button>
    {:else if mfaEnabled}
      <div class="summary-grid">
        <div>
          <strong>{backupCodes?.unused ?? 0}</strong>
          <span>unused backup codes</span>
        </div>
        <div>
          <strong>{backupCodes?.total ?? 0}</strong>
          <span>total backup codes</span>
        </div>
      </div>

      <form class="disable-form" onsubmit={regenerateBackupCodes}>
        <label>
          Current password
          <input
            type="password"
            bind:value={regeneratePassword}
            autocomplete="current-password"
            disabled={saving}
          />
        </label>
        <button type="submit" class="btn btn-secondary" disabled={saving || !regeneratePassword}>
          {saving ? 'Working...' : 'Regenerate backup codes'}
        </button>
      </form>
      <p class="muted">
        Issues a fresh set of {backupCodes?.total ?? 0} codes and revokes every unused one you have now.
      </p>

      <form class="disable-form" onsubmit={disableMfa}>
        <label>
          Current password
          <input type="password" bind:value={disablePassword} autocomplete="current-password" disabled={saving} />
        </label>
        <button type="submit" class="btn btn-danger" disabled={saving || !disablePassword}>
          {saving ? 'Disabling...' : 'Disable MFA'}
        </button>
      </form>
    {:else}
      <p class="muted">MFA is not enabled for this account.</p>
      <button type="button" class="btn btn-primary" onclick={startSetup} disabled={saving}>
        {saving ? 'Starting...' : 'Set up MFA'}
      </button>
    {/if}
  </section>

  <section class="section">
    <div class="section-heading">
      <div>
        <h2>Passkeys</h2>
        <p>Sign in without a password using Touch ID, Windows Hello, or a security key.</p>
      </div>
      <span class:enabled={knownPasskeys.length > 0} class:unknown={passkeyListUnknown} class="status">
        {#if passkeyListUnknown}
          Unknown
        {:else}
          {knownPasskeys.length > 0 ? `${knownPasskeys.length} active` : 'None'}
        {/if}
      </span>
    </div>

    {#if !passkeySupported}
      <p class="muted">This browser does not support passkeys.</p>
    {:else}
      {#if loading}
        <p class="muted">Loading...</p>
      {:else if passkeyListUnknown}
        <p class="muted state-unknown">
          The passkeys registered on this account could not be read, so this section cannot say
          there are none.
        </p>
        <button type="button" class="btn btn-secondary" onclick={loadPasskeyList} disabled={passkeyBusy}>
          Retry reading passkeys
        </button>
      {:else if knownPasskeys.length > 0}
        <ul class="passkey-list">
          {#each knownPasskeys as key (key.id)}
            <li>
              <div>
                <strong>{key.name}</strong>
                <span class="muted">
                  Added {new Date(key.created_at).toLocaleDateString()}
                  {#if key.last_used_at}· Last used {new Date(key.last_used_at).toLocaleDateString()}{/if}
                </span>
              </div>
              <button
                type="button"
                class="btn btn-danger"
                onclick={() => removePasskey(key.id)}
                disabled={passkeyBusy}
              >
                Remove
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="muted">No passkeys registered yet.</p>
      {/if}

      <form class="passkey-form" onsubmit={addPasskey}>
        <label>
          Passkey name
          <input
            type="text"
            bind:value={passkeyName}
            placeholder="e.g. YubiKey, MacBook"
            disabled={passkeyBusy}
          />
        </label>
        <button type="submit" class="btn btn-primary" disabled={passkeyBusy}>
          {passkeyBusy ? 'Waiting for authenticator...' : 'Add passkey'}
        </button>
      </form>
    {/if}
  </section>

  <section class="section">
    <div class="section-heading">
      <div>
        <h2>Linked accounts</h2>
        <p>External identities that can sign in to this account.</p>
      </div>
      <span class:enabled={knownSsoLinks.length > 0} class:unknown={ssoLinksUnknown} class="status">
        {#if ssoLinksUnknown}
          Unknown
        {:else}
          {knownSsoLinks.length > 0 ? `${knownSsoLinks.length} linked` : 'None'}
        {/if}
      </span>
    </div>

    {#if loading}
      <p class="muted">Loading...</p>
    {:else if ssoLinksUnknown}
      <p class="muted state-unknown">
        The external identities linked to this account could not be read, so this section cannot say
        there are none.
      </p>
      <button type="button" class="btn btn-secondary" onclick={loadSsoLinks} disabled={ssoBusy}>
        Retry reading linked accounts
      </button>
    {:else if knownSsoLinks.length > 0}
      <ul class="passkey-list">
        {#each knownSsoLinks as link (link.slug)}
          <li>
            <div>
              <strong>{link.name}</strong>
              <span class="muted">
                {link.provider_username || link.email}
                · Linked {new Date(link.linked_at).toLocaleDateString()}
                {#if !link.provider_enabled}· Provider is switched off{/if}
              </span>
            </div>
            <button
              type="button"
              class="btn btn-danger"
              onclick={() => unlinkSsoProvider(link)}
              disabled={ssoBusy}
            >
              Unlink
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="muted">No external accounts are linked. Sign in through a provider to link one.</p>
    {/if}
  </section>

  {#if setup}
    <section class="section setup-section">
      <h2>Scan Authenticator QR</h2>
      <div class="setup-grid">
        <div class="qr" aria-label="Authenticator QR code">{@html setup.qr_svg}</div>
        <div>
          <p class="muted">Scan the QR code with an authenticator app, then enter the six-digit code.</p>
          <code>{setup.secret}</code>
          <form class="enable-form" onsubmit={enableMfa}>
            <label>
              Authentication code
              <input inputmode="numeric" autocomplete="one-time-code" bind:value={verificationCode} disabled={saving} />
            </label>
            <button type="submit" class="btn btn-primary" disabled={saving || !verificationCode.trim()}>
              {saving ? 'Verifying...' : 'Enable MFA'}
            </button>
          </form>
        </div>
      </div>
    </section>
  {/if}

  {#if newBackupCodes.length > 0}
    <section class="section backup-section" aria-label="New backup codes">
      <div class="section-heading">
        <div>
          <h2>Backup Codes</h2>
          <p>Each code can be used once if you lose authenticator access.</p>
        </div>
        <button type="button" class="btn btn-secondary" onclick={copyBackupCodes}>Copy Codes</button>
      </div>
      <div class="code-grid">
        {#each newBackupCodes as code}
          <code>{code}</code>
        {/each}
      </div>
    </section>
  {/if}
</div>

<style>
  .security-page {
    max-width: 980px;
  }

  .page-header {
    margin-bottom: 24px;
  }

  h1 {
    margin: 0 0 6px;
    font-size: 28px;
  }

  h2 {
    margin: 0;
    font-size: 18px;
  }

  p {
    margin: 0;
  }

  .page-header p,
  .muted,
  .section-heading p {
    color: var(--text-secondary);
  }

  .section {
    margin-bottom: 20px;
    padding: 20px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-secondary);
  }

  .section-heading {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 16px;
    margin-bottom: 18px;
  }

  .status {
    flex: 0 0 auto;
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 4px 10px;
    color: var(--text-secondary);
    font-size: 12px;
    font-weight: 700;
  }

  .status.enabled {
    border-color: var(--green-dim);
    color: var(--green);
  }

  .status.unknown {
    border-style: dashed;
  }

  .state-unknown {
    margin-bottom: 12px;
  }

  .summary-grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 12px;
    margin-bottom: 18px;
  }

  .summary-grid > div {
    padding: 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
  }

  .summary-grid strong {
    display: block;
    margin-bottom: 4px;
    font-size: 24px;
  }

  .summary-grid span {
    color: var(--text-secondary);
    font-size: 13px;
  }

  form {
    display: grid;
    gap: 12px;
    max-width: 420px;
  }

  label {
    display: grid;
    gap: 6px;
    font-size: 13px;
    font-weight: 600;
  }

  .setup-grid {
    display: grid;
    grid-template-columns: 220px minmax(0, 1fr);
    gap: 20px;
    align-items: start;
  }

  .qr {
    display: grid;
    place-items: center;
    min-height: 220px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: #fff;
    color: #111;
  }

  .qr :global(svg) {
    width: 196px;
    height: 196px;
  }

  .enable-form {
    margin-top: 16px;
  }

  .code-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(150px, 1fr));
    gap: 8px;
  }

  .code-grid code,
  .setup-section code {
    display: block;
    padding: 8px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
  }

  .passkey-list {
    list-style: none;
    margin: 0 0 18px;
    padding: 0;
    display: grid;
    gap: 10px;
  }

  .passkey-list li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px 14px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
  }

  .passkey-list li > div {
    display: grid;
    gap: 2px;
  }

  .passkey-list .muted {
    font-size: 12px;
  }

  .passkey-form {
    margin-top: 4px;
  }

  .error-box,
  .success-box {
    margin-bottom: 16px;
    padding: 12px 14px;
    border-radius: var(--radius);
  }

  .error-box {
    border: 1px solid var(--red-dim);
    background: color-mix(in srgb, var(--red-dim) 14%, transparent);
    color: var(--red);
  }

  .success-box {
    border: 1px solid var(--green-dim);
    background: color-mix(in srgb, var(--green-dim) 14%, transparent);
    color: var(--green);
  }

  .btn {
    width: fit-content;
    padding: 8px 14px;
    border-radius: var(--radius);
    border: 1px solid var(--border);
    cursor: pointer;
    font-weight: 600;
  }

  .btn-primary {
    border-color: var(--green-dim);
    background: var(--green-dim);
    color: #fff;
  }

  .btn-secondary {
    background: var(--bg-primary);
    color: var(--text-primary);
  }

  .btn-danger {
    border-color: var(--red-dim);
    background: var(--red-dim);
    color: #fff;
  }

  .btn:disabled {
    cursor: not-allowed;
    opacity: 0.65;
  }

  @media (max-width: 720px) {
    .section-heading,
    .setup-grid {
      grid-template-columns: 1fr;
    }

    .section-heading {
      display: grid;
    }

    .summary-grid {
      grid-template-columns: 1fr;
    }
  }
</style>
