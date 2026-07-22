<script lang="ts">
  import { goto } from '$app/navigation';
  import { isAuthReady, isLoggedIn } from '$lib/stores/auth.svelte';
  import { mfa, type MfaBackupStatus, type MfaSetupResponse } from '$lib/api/client.svelte';

  let loading = $state(true);
  let saving = $state(false);
  let error = $state('');
  let success = $state('');
  let backupStatus = $state<MfaBackupStatus | null>(null);
  let setup = $state<MfaSetupResponse | null>(null);
  let verificationCode = $state('');
  let disablePassword = $state('');
  let newBackupCodes = $state<string[]>([]);

  const mfaEnabled = $derived((backupStatus?.total ?? 0) > 0);

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    loadSecurity();
  });

  async function loadSecurity() {
    try {
      loading = true;
      error = '';
      backupStatus = await mfa.backup();
    } catch (err: any) {
      error = err.message || 'Failed to load security settings';
    } finally {
      loading = false;
    }
  }

  async function startSetup() {
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

  async function copyBackupCodes() {
    if (newBackupCodes.length === 0) return;
    await navigator.clipboard.writeText(newBackupCodes.join('\n'));
    success = 'Backup codes copied';
  }
</script>

<svelte:head>
  <title>Security · IronForge</title>
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
      <span class:enabled={mfaEnabled} class="status">{mfaEnabled ? 'Enabled' : 'Disabled'}</span>
    </div>

    {#if loading}
      <p class="muted">Loading...</p>
    {:else if mfaEnabled}
      <div class="summary-grid">
        <div>
          <strong>{backupStatus?.unused ?? 0}</strong>
          <span>unused backup codes</span>
        </div>
        <div>
          <strong>{backupStatus?.total ?? 0}</strong>
          <span>total backup codes</span>
        </div>
      </div>

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
