<script lang="ts">
  import { copyToClipboard } from '$lib/clipboard';
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
    type PublicSsoProvider,
    type SsoLink,
  } from '$lib/api/client.svelte';
  import { createT, formatDate } from '$lib/i18n';
  import { readSsoError, ssoErrorMessage, type SsoError } from '$lib/ssoError';

  const t = createT();

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
  let enablePassword = $state('');
  let disablePassword = $state('');
  let regeneratePassword = $state('');
  let newBackupCodes = $state<string[]>([]);

  let passkeyList = $state<PasskeyInfo[] | Unknown>([]);
  let passkeyName = $state('');
  let passkeyBusy = $state(false);
  const passkeySupported = isPasskeySupported();

  let ssoLinks = $state<SsoLink[] | Unknown>([]);
  let ssoProviders = $state<PublicSsoProvider[] | Unknown>([]);
  let ssoBusy = $state(false);
  // A link the provider's callback refused comes back as `?sso_error=<code>`;
  // kept apart from `error`, which every reload of this page clears.
  let ssoError = $state<SsoError | null>(null);
  const ssoErrorText = $derived.by(() => {
    if (!ssoError) return '';
    const slug = ssoError.provider;
    const known = ssoProviders === UNKNOWN ? [] : ssoProviders;
    const name = known.find((provider) => provider.slug === slug)?.name ?? (slug || 'SSO');
    return ssoErrorMessage(t, ssoError.code, name);
  });
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
  // Providers this account can still link. Offered only while the links are
  // known: with the listing unread, a button could offer a provider that is
  // already linked.
  const linkableProviders = $derived(
    ssoLinks === UNKNOWN || ssoProviders === UNKNOWN
      ? []
      : ssoProviders.filter((provider) => !knownSsoLinks.some((link) => link.slug === provider.slug)),
  );

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    // Where the provider's callback lands after `linkSsoProvider` below.
    if (new URLSearchParams(window.location.search).has('sso_linked')) {
      success = t('account_security.sso.linked_notice');
    }
    ssoError = readSsoError(window.location.search);
    loadSecurity();
  });

  async function loadSecurity() {
    const claim = securityRequests.begin('security-load');
    loading = true;
    error = '';
    // Each slot claims its own state owner, synchronously, inside its loader,
    // so a retry of one section cannot publish over a mutation running in
    // another — and so one refused read leaves the other two intact.
    await Promise.all([
      loadBackupStatus(),
      loadPasskeyList(),
      loadSsoLinks(),
      loadSsoProviders(),
    ]);
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

  async function loadSsoProviders() {
    const result = await optionalSection(
      Promise.resolve().then(() => auth.listSsoProviders()),
      'the sign-in providers this account could link',
    );
    ssoProviders = isUnavailable(result) ? UNKNOWN : Array.isArray(result) ? result : [];
  }

  // The only way a provider joins an account that already exists: a first
  // sign-in through it never attaches to an account by its email address.
  async function linkSsoProvider(provider: PublicSsoProvider) {
    if (ssoBusy) return;
    try {
      ssoBusy = true;
      error = '';
      success = '';
      const { authorize_url } = await auth.linkSso(provider.slug);
      window.location.assign(authorize_url);
    } catch (err: any) {
      error = err.message || t('account_security.sso.link_failed', { provider: provider.name });
      ssoBusy = false;
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
      success = t('account_security.passkeys.registered');
    } catch (err: any) {
      error = err.message || t('account_security.passkeys.register_failed');
    } finally {
      passkeyBusy = false;
    }
  }

  async function removePasskey(id: number) {
    if (!confirm(t('account_security.passkeys.remove_confirm'))) return;
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
      success = t('account_security.passkeys.removed');
    } catch (err: any) {
      error = err.message || t('account_security.passkeys.remove_failed');
    } finally {
      passkeyBusy = false;
    }
  }

  async function unlinkSsoProvider(link: SsoLink) {
    if (!confirm(t('account_security.sso.unlink_confirm', { provider: link.name }))) return;
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
      success = t('account_security.sso.unlinked', { provider: link.name });
    } catch (err: any) {
      error = err.message || t('account_security.sso.unlink_failed');
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
      error = err.message || t('account_security.mfa.setup_failed');
    } finally {
      saving = false;
    }
  }

  async function enableMfa(event: SubmitEvent) {
    event.preventDefault();
    if (saving) return;
    if (!verificationCode.trim()) {
      error = t('account_security.mfa.code_required');
      return;
    }
    if (!enablePassword) {
      error = t('account_security.password_required');
      return;
    }

    try {
      saving = true;
      error = '';
      success = '';
      const result = await mfa.enable(verificationCode.trim(), enablePassword);
      newBackupCodes = result.backup_codes;
      setup = null;
      verificationCode = '';
      enablePassword = '';
      success = t('account_security.mfa.enabled_notice');
      await loadSecurity();
    } catch (err: any) {
      error = err.message || t('account_security.mfa.enable_failed');
    } finally {
      saving = false;
    }
  }

  async function disableMfa(event: SubmitEvent) {
    event.preventDefault();
    if (saving) return;
    if (!disablePassword) {
      error = t('account_security.password_required');
      return;
    }
    if (!confirm(t('account_security.mfa.disable_confirm'))) return;

    try {
      saving = true;
      error = '';
      success = '';
      await mfa.disable(disablePassword);
      disablePassword = '';
      newBackupCodes = [];
      setup = null;
      success = t('account_security.mfa.disabled_notice');
      await loadSecurity();
    } catch (err: any) {
      error = err.message || t('account_security.mfa.disable_failed');
    } finally {
      saving = false;
    }
  }

  async function regenerateBackupCodes(event: SubmitEvent) {
    event.preventDefault();
    if (saving) return;
    if (!regeneratePassword) {
      error = t('account_security.password_required');
      return;
    }
    if (!confirm(t('account_security.mfa.regenerate_confirm'))) return;

    try {
      saving = true;
      error = '';
      success = '';
      const result = await mfa.regenerateBackup(regeneratePassword);
      regeneratePassword = '';
      newBackupCodes = result.backup_codes;
      success = t('account_security.mfa.regenerated_notice');
      await loadSecurity();
    } catch (err: any) {
      error = err.message || t('account_security.mfa.regenerate_failed');
    } finally {
      saving = false;
    }
  }

  async function copyBackupCodes() {
    if (newBackupCodes.length === 0) return;
    if (await copyToClipboard(newBackupCodes.join('\n'))) success = t('account_security.backup.copied');
    else error = t('common.copy_failed', 'Copying failed. Select the text and copy it yourself.');
  }
</script>

<svelte:head>
  <title>{t('account_security.title')} · Plombir Git</title>
</svelte:head>

<div class="page-container security-page">
  <header class="page-header">
    <div>
      <h1>{t('account_security.title')}</h1>
      <p>{t('account_security.description')}</p>
    </div>
  </header>

  {#if ssoErrorText}
    <div class="error-box" role="alert">{ssoErrorText}</div>
  {/if}

  {#if error}
    <div class="error-box">{error}</div>
  {/if}

  {#if success}
    <div class="success-box">{success}</div>
  {/if}

  <section class="section">
    <div class="section-heading">
      <div>
        <h2>{t('account_security.mfa.title')}</h2>
        <p>{t('account_security.mfa.description')}</p>
      </div>
      <span class:enabled={mfaEnabled} class:unknown={mfaStateUnknown} class="status">
        {mfaStateUnknown
          ? t('account_security.status_unknown')
          : mfaEnabled
            ? t('account_security.status_enabled')
            : t('account_security.status_disabled')}
      </span>
    </div>

    {#if loading}
      <p class="muted">{t('common.loading')}</p>
    {:else if mfaStateUnknown}
      <p class="muted state-unknown">{t('account_security.mfa.state_unknown')}</p>
      <button type="button" class="btn btn-secondary" onclick={loadBackupStatus} disabled={saving}>
        {t('account_security.mfa.retry')}
      </button>
    {:else if mfaEnabled}
      <div class="summary-grid">
        <div>
          <strong>{backupCodes?.unused ?? 0}</strong>
          <span>{t('account_security.mfa.unused_codes')}</span>
        </div>
        <div>
          <strong>{backupCodes?.total ?? 0}</strong>
          <span>{t('account_security.mfa.total_codes')}</span>
        </div>
      </div>

      <form class="disable-form" onsubmit={regenerateBackupCodes}>
        <label>
          {t('account_security.mfa.current_password')}
          <input
            type="password"
            bind:value={regeneratePassword}
            autocomplete="current-password"
            disabled={saving}
          />
        </label>
        <button type="submit" class="btn btn-secondary" disabled={saving || !regeneratePassword}>
          {saving ? t('account_security.mfa.working') : t('account_security.mfa.regenerate')}
        </button>
      </form>
      <p class="muted">
        {t('account_security.mfa.regenerate_hint', { count: backupCodes?.total ?? 0 })}
      </p>

      <form class="disable-form" onsubmit={disableMfa}>
        <label>
          {t('account_security.mfa.current_password')}
          <input type="password" bind:value={disablePassword} autocomplete="current-password" disabled={saving} />
        </label>
        <button type="submit" class="btn btn-danger" disabled={saving || !disablePassword}>
          {saving ? t('account_security.mfa.disabling') : t('account_security.mfa.disable')}
        </button>
      </form>
    {:else}
      <p class="muted">{t('account_security.mfa.not_enabled')}</p>
      <button type="button" class="btn btn-primary" onclick={startSetup} disabled={saving}>
        {saving ? t('account_security.mfa.starting') : t('account_security.mfa.set_up')}
      </button>
    {/if}
  </section>

  <section class="section">
    <div class="section-heading">
      <div>
        <h2>{t('account_security.passkeys.title')}</h2>
        <p>{t('account_security.passkeys.description')}</p>
      </div>
      <span class:enabled={knownPasskeys.length > 0} class:unknown={passkeyListUnknown} class="status">
        {#if passkeyListUnknown}
          {t('account_security.status_unknown')}
        {:else}
          {knownPasskeys.length > 0
            ? t('account_security.passkeys.active', { count: knownPasskeys.length })
            : t('account_security.status_none')}
        {/if}
      </span>
    </div>

    {#if !passkeySupported}
      <p class="muted">{t('account_security.passkeys.unsupported')}</p>
    {:else}
      {#if loading}
        <p class="muted">{t('common.loading')}</p>
      {:else if passkeyListUnknown}
        <p class="muted state-unknown">{t('account_security.passkeys.state_unknown')}</p>
        <button type="button" class="btn btn-secondary" onclick={loadPasskeyList} disabled={passkeyBusy}>
          {t('account_security.passkeys.retry')}
        </button>
      {:else if knownPasskeys.length > 0}
        <ul class="passkey-list">
          {#each knownPasskeys as key (key.id)}
            <li>
              <div>
                <strong>{key.name}</strong>
                <span class="muted">
                  {t('account_security.passkeys.added', { date: formatDate(key.created_at) })}
                  {#if key.last_used_at}· {t('account_security.passkeys.last_used', { date: formatDate(key.last_used_at) })}{/if}
                </span>
              </div>
              <button
                type="button"
                class="btn btn-danger"
                onclick={() => removePasskey(key.id)}
                disabled={passkeyBusy}
              >
                {t('account_security.passkeys.remove')}
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="muted">{t('account_security.passkeys.empty')}</p>
      {/if}

      <form class="passkey-form" onsubmit={addPasskey}>
        <label>
          {t('account_security.passkeys.name')}
          <input
            type="text"
            bind:value={passkeyName}
            placeholder={t('account_security.passkeys.name_placeholder')}
            disabled={passkeyBusy}
          />
        </label>
        <button type="submit" class="btn btn-primary" disabled={passkeyBusy}>
          {passkeyBusy ? t('account_security.passkeys.waiting') : t('account_security.passkeys.add')}
        </button>
      </form>
    {/if}
  </section>

  <section class="section">
    <div class="section-heading">
      <div>
        <h2>{t('account_security.sso.title')}</h2>
        <p>{t('account_security.sso.description')}</p>
      </div>
      <span class:enabled={knownSsoLinks.length > 0} class:unknown={ssoLinksUnknown} class="status">
        {#if ssoLinksUnknown}
          {t('account_security.status_unknown')}
        {:else}
          {knownSsoLinks.length > 0
            ? t('account_security.sso.linked_count', { count: knownSsoLinks.length })
            : t('account_security.status_none')}
        {/if}
      </span>
    </div>

    {#if loading}
      <p class="muted">{t('common.loading')}</p>
    {:else if ssoLinksUnknown}
      <p class="muted state-unknown">{t('account_security.sso.state_unknown')}</p>
      <button type="button" class="btn btn-secondary" onclick={loadSsoLinks} disabled={ssoBusy}>
        {t('account_security.sso.retry')}
      </button>
    {:else if knownSsoLinks.length > 0}
      <ul class="passkey-list">
        {#each knownSsoLinks as link (link.slug)}
          <li>
            <div>
              <strong>{link.name}</strong>
              <span class="muted">
                {link.provider_username || link.email}
                · {t('account_security.sso.linked_at', { date: formatDate(link.linked_at) })}
                {#if !link.provider_enabled}· {t('account_security.sso.provider_off')}{/if}
              </span>
            </div>
            <button
              type="button"
              class="btn btn-danger"
              onclick={() => unlinkSsoProvider(link)}
              disabled={ssoBusy}
            >
              {t('account_security.sso.unlink')}
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="muted">{t('account_security.sso.empty')}</p>
    {/if}

    {#if !loading && linkableProviders.length > 0}
      <div class="sso-link-actions">
        {#each linkableProviders as provider (provider.slug)}
          <button
            type="button"
            class="btn btn-secondary"
            onclick={() => linkSsoProvider(provider)}
            disabled={ssoBusy}
          >
            {t('account_security.sso.link', { provider: provider.name })}
          </button>
        {/each}
      </div>
    {:else if !loading && ssoProviders === UNKNOWN}
      <p class="muted state-unknown">{t('account_security.sso.providers_unknown')}</p>
    {/if}
  </section>

  {#if setup}
    <section class="section setup-section">
      <h2>{t('account_security.setup.title')}</h2>
      <div class="setup-grid">
        <div class="qr" aria-label={t('account_security.setup.qr_label')}>{@html setup.qr_svg}</div>
        <div>
          <p class="muted">{t('account_security.setup.hint')}</p>
          <code>{setup.secret}</code>
          <form class="enable-form" onsubmit={enableMfa}>
            <label>
              {t('account_security.setup.code')}
              <input inputmode="numeric" autocomplete="one-time-code" bind:value={verificationCode} disabled={saving} />
            </label>
            <label>
              {t('account_security.setup.password')}
              <input type="password" autocomplete="current-password" bind:value={enablePassword} disabled={saving} />
            </label>
            <button type="submit" class="btn btn-primary" disabled={saving || !verificationCode.trim() || !enablePassword}>
              {saving ? t('account_security.setup.verifying') : t('account_security.setup.enable')}
            </button>
          </form>
        </div>
      </div>
    </section>
  {/if}

  {#if newBackupCodes.length > 0}
    <section class="section backup-section" aria-label={t('account_security.backup.label')}>
      <div class="section-heading">
        <div>
          <h2>{t('account_security.backup.title')}</h2>
          <p>{t('account_security.backup.description')}</p>
        </div>
        <button type="button" class="btn btn-secondary" onclick={copyBackupCodes}>{t('account_security.backup.copy')}</button>
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

  .sso-link-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin-top: 12px;
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
