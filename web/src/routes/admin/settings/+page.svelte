<script lang="ts">
  import { goto } from '$app/navigation';
  import { untrack } from 'svelte';
  import { setBanner, clearBanner } from '$lib/stores/instance.svelte';
  import { isAuthReady, isLoggedIn, isAdmin } from '$lib/stores/auth.svelte';
  import { LatestRequestFence } from '$lib/asyncStateOwnership';
  import {
    admin,
    type AdminSettings,
    type AdminSsoProvider,
    type LoginAttemptEntry,
    type SsoProviderPayload,
  } from '$lib/api/client.svelte';

  // Three unrelated surfaces used to share one `loading`/`error` pair and one
  // composite initial `Promise.all`. That made every late response an owner of
  // state it never requested: an initial provider list could resurrect a row a
  // confirmed delete had removed, and a filtered login-attempt page could be
  // replaced by the unfiltered first page the mount had started. Each slot now
  // has its own request owner keyed to the full intent behind the request.
  const SETTINGS_SLOT = 'instance-settings';
  const SSO_SLOT = 'sso-providers';
  const settingsRequests = new LatestRequestFence<string>();
  const ssoRequests = new LatestRequestFence<string>();
  const ssoTestRequests = new LatestRequestFence<number>();
  const loginAttemptRequests = new LatestRequestFence<string>();

  let settingsLoading = $state(true);
  let settingsError = $state('');
  let saving = $state(false);
  let maintenanceMode = $state(false);
  let bannerMessage = $state('');
  let bannerType = $state<'info' | 'warning' | 'error'>('info');
  let ssoProviders = $state<AdminSsoProvider[]>([]);
  let ssoLoading = $state(true);
  let ssoError = $state('');
  let ssoSaving = $state(false);
  let testingSsoId = $state<number | null>(null);
  let ssoTestResult = $state<{ ok: boolean; message: string } | null>(null);
  // One busy claim per provider row. Test / Enable / Edit / Delete all mutate
  // the same row, so they have to share the claim rather than each keeping a
  // private flag that the other three controls cannot see.
  let busySsoIds = $state<Set<number>>(new Set());
  let loginAttempts = $state<LoginAttemptEntry[]>([]);
  let loginAttemptsTotal = $state(0);
  let loginAttemptsPage = $state(1);
  const loginAttemptsPerPage = 20;
  let loginAttemptsPages = $derived(Math.max(1, Math.ceil(loginAttemptsTotal / loginAttemptsPerPage)));
  let loginAttemptsLoading = $state(false);
  let loginAttemptsError = $state('');
  let loginUsernameFilter = $state('');
  let loginProviderFilter = $state('');
  let loginStatusFilter = $state<'all' | 'success' | 'failure'>('all');
  let loginStartTime = $state('');
  let loginEndTime = $state('');
  let editingSsoId = $state<number | null>(null);
  let ssoForm = $state<SsoProviderPayload>(emptySsoProviderForm());

  // The slug decides which endpoints the backend can resolve, and which slugs
  // are usable depends on the type — so the example has to follow the type
  // rather than always suggesting `google`, which plain OAuth2 rejects.
  const ssoSlugPlaceholder = $derived(
    ssoForm.provider_type === 'ldap'
      ? 'corp-ldap'
      : ssoForm.provider_type === 'oidc'
        ? 'keycloak'
        : 'github',
  );

  function emptySsoProviderForm(): SsoProviderPayload {
    return {
      name: '',
      slug: '',
      provider_type: 'oauth2',
      client_id: '',
      client_secret: '',
      discovery_url: '',
      scopes: 'openid profile email',
      ldap_host: '',
      ldap_port: undefined,
      ldap_bind_dn: '',
      ldap_bind_password: '',
      ldap_base_dn: '',
      ldap_user_filter: '',
      enabled: true,
      // A new provider provisions nobody until the operator says otherwise —
      // the same default the API applies, spelled out here so the box the
      // admin sees matches what gets stored.
      auto_provision: false,
      allowed_email_domains: '',
      icon_url: '',
    };
  }

  $effect(() => {
    if (!isAuthReady()) return;
    if (!isLoggedIn()) {
      goto('/login');
      return;
    }
    if (!isAdmin()) {
      goto('/dashboard');
      return;
    }
    // Each surface owns its own reload. This effect must not subscribe to the
    // login-attempt filters merely because the loader reads their snapshot.
    untrack(() => {
      void loadSettings();
      void loadSsoProviders();
      void loadLoginAttempts(1);
    });
  });

  function isSsoBusy(id: number): boolean {
    return busySsoIds.has(id);
  }

  function claimSsoProvider(id: number): boolean {
    if (busySsoIds.has(id)) return false;
    busySsoIds = new Set(busySsoIds).add(id);
    return true;
  }

  function releaseSsoProvider(id: number): void {
    const next = new Set(busySsoIds);
    next.delete(id);
    busySsoIds = next;
  }

  function publishSettings(data: AdminSettings) {
    maintenanceMode = data.maintenance_mode;
    bannerMessage = data.banner_message || '';
    bannerType = data.banner_type || 'info';
    syncBanner(data);
  }

  function syncBanner(data: AdminSettings) {
    if (data.banner_message) {
      setBanner(data.banner_message, data.banner_type);
    } else {
      clearBanner();
    }
  }

  async function loadSettings() {
    const claim = settingsRequests.begin(SETTINGS_SLOT);
    settingsLoading = true;
    settingsError = '';
    try {
      const data = await admin.getSettings();
      if (!settingsRequests.owns(claim, SETTINGS_SLOT)) return;
      publishSettings(data);
    } catch (e: any) {
      if (!settingsRequests.owns(claim, SETTINGS_SLOT)) return;
      settingsError = e.message;
    } finally {
      if (settingsRequests.owns(claim, SETTINGS_SLOT)) settingsLoading = false;
    }
  }

  async function saveSettings() {
    if (saving) return;
    // A submitted save is the newest intent for this slot, so a settings load
    // that is still in flight stops being able to publish over it — and a load
    // started after the save wins in turn. The Save control only exists while
    // `settingsLoading` is false, so taking ownership here can never strand a
    // load that owns the loading flag; a template change that renders Save
    // during a load would have to release that flag with the ownership.
    const claim = settingsRequests.begin(SETTINGS_SLOT);
    saving = true;
    settingsError = '';
    try {
      const payload: Partial<AdminSettings> = {
        maintenance_mode: maintenanceMode,
        banner_message: bannerMessage || null,
        banner_type: bannerType,
      };
      const data = await admin.updateSettings(payload);
      if (!settingsRequests.owns(claim, SETTINGS_SLOT)) return;
      // Sync banner to frontend store
      syncBanner(data);
    } catch (e: any) {
      if (!settingsRequests.owns(claim, SETTINGS_SLOT)) return;
      settingsError = e.message;
    } finally {
      saving = false;
    }
  }

  async function loadSsoProviders() {
    const claim = ssoRequests.begin(SSO_SLOT);
    ssoLoading = true;
    ssoError = '';
    try {
      const providers = await admin.listSsoProviders();
      if (!ssoRequests.owns(claim, SSO_SLOT)) return;
      ssoProviders = providers;
    } catch (e: any) {
      if (!ssoRequests.owns(claim, SSO_SLOT)) return;
      ssoError = e.message;
    } finally {
      if (ssoRequests.owns(claim, SSO_SLOT)) ssoLoading = false;
    }
  }

  function editSsoProvider(provider: AdminSsoProvider) {
    if (isSsoBusy(provider.id)) return;
    editingSsoId = provider.id;
    ssoForm = {
      name: provider.name,
      slug: provider.slug,
      provider_type: provider.provider_type || 'oauth2',
      client_id: provider.client_id || '',
      client_secret: '',
      discovery_url: provider.discovery_url || '',
      scopes: provider.scopes || '',
      ldap_host: provider.ldap_host || '',
      ldap_port: provider.ldap_port ?? undefined,
      ldap_bind_dn: provider.ldap_bind_dn || '',
      ldap_bind_password: '',
      ldap_base_dn: provider.ldap_base_dn || '',
      ldap_user_filter: provider.ldap_user_filter || '',
      enabled: provider.enabled,
      auto_provision: provider.auto_provision,
      allowed_email_domains: provider.allowed_email_domains || '',
      icon_url: provider.icon_url || '',
    };
  }

  function resetSsoForm() {
    editingSsoId = null;
    ssoForm = emptySsoProviderForm();
  }

  function cleanSsoPayload(): SsoProviderPayload {
    return {
      ...ssoForm,
      name: ssoForm.name.trim(),
      slug: ssoForm.slug.trim(),
      provider_type: ssoForm.provider_type || 'oauth2',
      client_id: ssoForm.client_id?.trim() || undefined,
      client_secret: ssoForm.client_secret || undefined,
      discovery_url: ssoForm.discovery_url?.trim() || undefined,
      scopes: ssoForm.scopes?.trim() || undefined,
      ldap_host: ssoForm.ldap_host?.trim() || undefined,
      ldap_port: ssoForm.ldap_port ? Number(ssoForm.ldap_port) : undefined,
      ldap_bind_dn: ssoForm.ldap_bind_dn?.trim() || undefined,
      ldap_bind_password: ssoForm.ldap_bind_password || undefined,
      ldap_base_dn: ssoForm.ldap_base_dn?.trim() || undefined,
      ldap_user_filter: ssoForm.ldap_user_filter?.trim() || undefined,
      auto_provision: ssoForm.auto_provision ?? false,
      // Always sent, empty string included: omitting it means "keep the stored
      // allowlist", so an emptied field would silently keep the old one.
      allowed_email_domains: ssoForm.allowed_email_domains?.trim() ?? '',
      icon_url: ssoForm.icon_url?.trim() || undefined,
    };
  }

  async function saveSsoProvider() {
    if (ssoSaving) return;
    if (!ssoForm.name.trim() || !ssoForm.slug.trim()) {
      ssoError = 'SSO provider name and slug are required';
      return;
    }

    const targetId = editingSsoId;
    if (targetId !== null && !claimSsoProvider(targetId)) return;
    ssoSaving = true;
    ssoError = '';
    try {
      const payload = cleanSsoPayload();
      if (targetId !== null) {
        await admin.updateSsoProvider(targetId, payload);
      } else {
        await admin.createSsoProvider(payload);
      }
      await loadSsoProviders();
      if (editingSsoId === targetId) resetSsoForm();
    } catch (e: any) {
      ssoError = e.message;
    } finally {
      ssoSaving = false;
      if (targetId !== null) releaseSsoProvider(targetId);
    }
  }

  async function toggleSsoProvider(provider: AdminSsoProvider) {
    if (!claimSsoProvider(provider.id)) return;
    ssoError = '';
    try {
      await admin.updateSsoProvider(provider.id, {
        name: provider.name,
        slug: provider.slug,
        provider_type: provider.provider_type,
        client_id: provider.client_id || undefined,
        discovery_url: provider.discovery_url || undefined,
        scopes: provider.scopes || undefined,
        ldap_host: provider.ldap_host || undefined,
        ldap_port: provider.ldap_port || undefined,
        ldap_bind_dn: provider.ldap_bind_dn || undefined,
        ldap_base_dn: provider.ldap_base_dn || undefined,
        ldap_user_filter: provider.ldap_user_filter || undefined,
        icon_url: provider.icon_url || undefined,
        enabled: !provider.enabled,
      });
      await loadSsoProviders();
    } catch (e: any) {
      ssoError = e.message;
    } finally {
      releaseSsoProvider(provider.id);
    }
  }

  async function deleteSsoProvider(provider: AdminSsoProvider) {
    if (isSsoBusy(provider.id)) return;
    if (!confirm(`Delete SSO provider "${provider.name}"?`)) return;
    if (!claimSsoProvider(provider.id)) return;
    ssoError = '';
    try {
      await admin.deleteSsoProvider(provider.id);
      await loadSsoProviders();
      if (editingSsoId === provider.id) resetSsoForm();
    } catch (e: any) {
      ssoError = e.message;
    } finally {
      releaseSsoProvider(provider.id);
    }
  }

  async function testSsoProvider(provider: AdminSsoProvider) {
    if (!claimSsoProvider(provider.id)) return;
    // The result banner is a single slot shared by every row, so a slower test
    // of another provider must not overwrite the newest one.
    const claim = ssoTestRequests.begin(provider.id);
    testingSsoId = provider.id;
    ssoError = '';
    ssoTestResult = null;
    try {
      const result = await admin.testSsoProvider(provider.id);
      if (!ssoTestRequests.owns(claim, provider.id)) return;
      ssoTestResult = result;
    } catch (e: any) {
      if (!ssoTestRequests.owns(claim, provider.id)) return;
      ssoTestResult = { ok: false, message: e.message || 'LDAP connection test failed' };
    } finally {
      if (ssoTestRequests.owns(claim, provider.id)) testingSsoId = null;
      releaseSsoProvider(provider.id);
    }
  }

  function loginAttemptsIdentity(
    page: number,
    username: string | undefined,
    provider: string | undefined,
    success: boolean | undefined,
    startTime: string | undefined,
    endTime: string | undefined,
  ): string {
    return JSON.stringify([
      page,
      loginAttemptsPerPage,
      username ?? null,
      provider ?? null,
      success ?? null,
      startTime ?? null,
      endTime ?? null,
    ]);
  }

  async function loadLoginAttempts(page = 1) {
    // Snapshot the whole filter intent: the claim has to describe the request
    // that was sent, not whatever the filter inputs hold when it comes back.
    const username = loginUsernameFilter.trim() || undefined;
    const authProvider = loginProviderFilter.trim() || undefined;
    const success = loginStatusFilter === 'all' ? undefined : loginStatusFilter === 'success';
    const startTime = loginStartTime ? new Date(loginStartTime).toISOString() : undefined;
    const endTime = loginEndTime ? new Date(loginEndTime).toISOString() : undefined;
    const identity = loginAttemptsIdentity(page, username, authProvider, success, startTime, endTime);
    const claim = loginAttemptRequests.begin(identity);
    loginAttemptsLoading = true;
    loginAttemptsError = '';
    try {
      const result = await admin.listLoginAttempts({
        page,
        per_page: loginAttemptsPerPage,
        username,
        auth_provider: authProvider,
        success,
        start_time: startTime,
        end_time: endTime,
      });
      if (!loginAttemptRequests.owns(claim, identity)) return;
      loginAttempts = result.attempts;
      loginAttemptsTotal = result.total;
      loginAttemptsPage = result.page;
    } catch (e: any) {
      if (!loginAttemptRequests.owns(claim, identity)) return;
      loginAttemptsError = e.message;
    } finally {
      if (loginAttemptRequests.owns(claim, identity)) loginAttemptsLoading = false;
    }
  }

  function formatLoginTime(value: string) {
    return new Date(value).toLocaleString();
  }

</script>

<svelte:head>
  <title>Instance Settings · Admin · Plombir Git</title>
</svelte:head>

<div class="settings-page">
  <h1>Instance Settings</h1>

  {#if settingsError}
    <div class="error-banner">{settingsError}</div>
  {/if}

  {#if settingsLoading}
    <p class="text-secondary">Loading...</p>
  {:else}
    <div class="section">
      <h2>Maintenance Mode</h2>
      <div class="toggle-row">
        <input id="admin-maintenance-mode" type="checkbox" bind:checked={maintenanceMode} />
        <label for="admin-maintenance-mode">Enable maintenance mode (read-only, blocks all mutating requests)</label>
      </div>
    </div>

    <div class="section">
      <h2>Instance Banner</h2>
      <div class="form-group">
        <label for="admin-banner-message">Banner Message (leave empty to hide)</label>
        <input id="admin-banner-message" type="text" bind:value={bannerMessage} placeholder="e.g. Scheduled maintenance tonight at 2am" />
      </div>
      <div class="form-group">
        <label for="admin-banner-type">Banner Type</label>
        <select id="admin-banner-type" bind:value={bannerType}>
          <option value="info">Info (blue)</option>
          <option value="warning">Warning (yellow)</option>
          <option value="error">Error (red)</option>
        </select>
      </div>
    </div>

    <div class="actions">
      <button class="btn-primary" onclick={saveSettings} disabled={saving} aria-busy={saving}>
        {saving ? 'Saving...' : 'Save Settings'}
      </button>
    </div>
  {/if}

  <div class="section">
    <h2>SSO Providers</h2>
    {#if ssoError}
      <div class="error-banner">{ssoError}</div>
    {/if}
    {#if ssoTestResult}
      <div class="connection-result" class:success={ssoTestResult.ok}>
        {ssoTestResult.message}
      </div>
    {/if}

    <!--
      A reload started by a confirmed mutation must not blank the rows it is
      refreshing: the placeholder belongs to the first load, when there is
      genuinely nothing to show yet.
    -->
    {#if ssoLoading && ssoProviders.length === 0}
      <p class="text-secondary">Loading SSO providers...</p>
    {:else if ssoProviders.length === 0}
      <p class="text-secondary">No SSO providers configured.</p>
    {:else}
      <div class="provider-list">
        {#each ssoProviders as provider (provider.id)}
          <div class="provider-row">
            <div>
              <strong>{provider.name}</strong>
              <div class="provider-meta">
                <span>{provider.slug}</span>
                <span>{provider.provider_type}</span>
                <span class:enabled={provider.enabled}>{provider.enabled ? 'Enabled' : 'Disabled'}</span>
                <span>{provider.auto_provision ? 'Creates accounts' : 'No new accounts'}</span>
              </div>
            </div>
            <div class="provider-actions">
              {#if provider.provider_type === 'ldap'}
                <button class="btn-secondary" type="button" disabled={isSsoBusy(provider.id)} aria-busy={isSsoBusy(provider.id)} onclick={() => testSsoProvider(provider)}>
                  {testingSsoId === provider.id ? 'Testing...' : 'Test connection'}
                </button>
              {/if}
              <button class="btn-secondary" type="button" disabled={isSsoBusy(provider.id)} aria-busy={isSsoBusy(provider.id)} onclick={() => toggleSsoProvider(provider)}>
                {provider.enabled ? 'Disable' : 'Enable'}
              </button>
              <button class="btn-secondary" type="button" disabled={isSsoBusy(provider.id)} aria-busy={isSsoBusy(provider.id)} onclick={() => editSsoProvider(provider)}>Edit</button>
              <button class="btn-danger" type="button" disabled={isSsoBusy(provider.id)} aria-busy={isSsoBusy(provider.id)} onclick={() => deleteSsoProvider(provider)}>Delete</button>
            </div>
          </div>
        {/each}
      </div>
    {/if}

    <div class="sso-form">
      <h3>{editingSsoId ? 'Edit SSO Provider' : 'Add SSO Provider'}</h3>
      <div class="form-grid">
        <div class="form-group">
          <label for="sso-name">Name</label>
          <input id="sso-name" type="text" bind:value={ssoForm.name} placeholder="Google Workspace" />
        </div>
        <div class="form-group">
          <label for="sso-slug">Slug</label>
          <input id="sso-slug" type="text" bind:value={ssoForm.slug} placeholder={ssoSlugPlaceholder} />
          {#if ssoForm.provider_type === 'oauth2'}
            <!--
              The slug is not a label here: plain OAuth2 has no discovery
              step, so the endpoints come from the built-in table and nothing
              else. Saying which slugs it holds beats finding out from a 400.
            -->
            <p class="field-hint">Plain OAuth2 recognises <code>github</code> and <code>gitlab</code>. Anything else needs the OIDC type and a discovery URL.</p>
          {/if}
        </div>
        <div class="form-group">
          <label for="sso-type">Type</label>
          <!--
            `oidc` is a distinct type on the backend, not a synonym of
            `oauth2`: only `oidc` reads `discovery_url`, while `oauth2`
            resolves endpoints from the built-in table, which holds github
            and gitlab and nothing else. One combined "OAuth2 / OIDC" option
            meant the Discovery URL below could be filled in but never read,
            and a self-hosted IdP was unreachable from this form entirely.
          -->
          <select id="sso-type" bind:value={ssoForm.provider_type}>
            <option value="oauth2">OAuth2 (GitHub / GitLab)</option>
            <option value="oidc">OIDC (discovery URL)</option>
            <option value="ldap">LDAP</option>
          </select>
        </div>
        {#if ssoForm.provider_type !== 'ldap'}
          <div class="form-group">
            <label for="sso-client-id">Client ID</label>
            <input id="sso-client-id" type="text" bind:value={ssoForm.client_id} />
          </div>
          <div class="form-group">
            <label for="sso-client-secret">Client Secret</label>
            <input id="sso-client-secret" type="password" bind:value={ssoForm.client_secret} placeholder={editingSsoId ? 'Leave blank to keep existing secret' : ''} />
          </div>
        {/if}
        {#if ssoForm.provider_type === 'oidc'}
          <div class="form-group">
            <label for="sso-discovery-url">Discovery URL</label>
            <input id="sso-discovery-url" type="url" bind:value={ssoForm.discovery_url} placeholder="https://idp.example.com/.well-known/openid-configuration" />
          </div>
        {/if}
        {#if ssoForm.provider_type !== 'ldap'}
          <div class="form-group">
            <label for="sso-scopes">Scopes</label>
            <input id="sso-scopes" type="text" bind:value={ssoForm.scopes} />
          </div>
        {/if}
        <div class="form-group">
          <label for="sso-icon-url">Icon URL</label>
          <input id="sso-icon-url" type="url" bind:value={ssoForm.icon_url} />
        </div>
        {#if ssoForm.provider_type === 'ldap'}
          <div class="form-group">
            <label for="sso-ldap-host">LDAP Host</label>
            <input id="sso-ldap-host" type="text" bind:value={ssoForm.ldap_host} placeholder="ldap.example.com (LDAPS by default)" />
          </div>
          <div class="form-group">
            <label for="sso-ldap-port">LDAP Port</label>
            <input id="sso-ldap-port" type="number" min="1" bind:value={ssoForm.ldap_port} />
          </div>
          <div class="form-group">
            <label for="sso-ldap-bind-dn">LDAP Bind DN</label>
            <input id="sso-ldap-bind-dn" type="text" bind:value={ssoForm.ldap_bind_dn} />
          </div>
          <div class="form-group">
            <label for="sso-ldap-bind-password">LDAP Bind Password</label>
            <input id="sso-ldap-bind-password" type="password" bind:value={ssoForm.ldap_bind_password} placeholder={editingSsoId ? 'Leave blank to keep existing password' : ''} />
          </div>
          <div class="form-group">
            <label for="sso-ldap-base-dn">LDAP Base DN</label>
            <input id="sso-ldap-base-dn" type="text" bind:value={ssoForm.ldap_base_dn} />
          </div>
          <div class="form-group">
            <label for="sso-ldap-filter">LDAP User Filter</label>
            <input id="sso-ldap-filter" type="text" bind:value={ssoForm.ldap_user_filter} placeholder={'(uid={username})'} />
          </div>
        {/if}
      </div>
      <div class="toggle-row">
        <input id="sso-enabled" type="checkbox" bind:checked={ssoForm.enabled} />
        <label for="sso-enabled">Enable this provider</label>
      </div>
      <div class="toggle-row">
        <input id="sso-auto-provision" type="checkbox" bind:checked={ssoForm.auto_provision} />
        <label for="sso-auto-provision">Create accounts on first login</label>
      </div>
      <p class="field-hint">
        Off means only people who already have an account here can sign in through this provider.
        On a public identity provider (GitHub, Google) leaving it on hands an account to anyone
        with an account there.
      </p>
      <div class="form-group">
        <label for="sso-allowed-domains">Allowed email domains</label>
        <input id="sso-allowed-domains" type="text" bind:value={ssoForm.allowed_email_domains} placeholder="example.com, partner.org" />
        <p class="field-hint">
          Comma-separated. Empty means no domain restriction. Exact match — <code>example.com</code>
          does not admit <code>mail.example.com</code>. Only limits who gets an account created;
          existing accounts keep signing in.
        </p>
      </div>
      <div class="inline-actions">
        <button class="btn-primary" type="button" onclick={saveSsoProvider} disabled={ssoSaving} aria-busy={ssoSaving}>
          {ssoSaving ? 'Saving...' : editingSsoId ? 'Update Provider' : 'Create Provider'}
        </button>
        {#if editingSsoId}
          <button class="btn-secondary" type="button" onclick={resetSsoForm}>Cancel</button>
        {/if}
      </div>
    </div>
  </div>

  <div class="section">
    <div class="section-heading">
      <div>
        <h2>Login Attempts</h2>
        <span class="text-secondary">{loginAttemptsTotal} matching events</span>
      </div>
      <button class="btn-secondary" type="button" disabled={loginAttemptsLoading} aria-busy={loginAttemptsLoading} onclick={() => loadLoginAttempts(loginAttemptsPage)}>
        {loginAttemptsLoading ? 'Loading...' : 'Refresh'}
      </button>
    </div>
    {#if loginAttemptsError}
      <div class="error-banner">{loginAttemptsError}</div>
    {/if}
    <div class="login-filters">
      <input aria-label="Filter login attempts by username" placeholder="Username" bind:value={loginUsernameFilter} />
      <input aria-label="Filter login attempts by provider" placeholder="Provider (password, ldap...)" bind:value={loginProviderFilter} />
      <select aria-label="Filter login attempts by status" bind:value={loginStatusFilter}>
        <option value="all">All results</option>
        <option value="failure">Failed only</option>
        <option value="success">Successful only</option>
      </select>
      <input aria-label="Login attempts start time" type="datetime-local" bind:value={loginStartTime} />
      <input aria-label="Login attempts end time" type="datetime-local" bind:value={loginEndTime} />
      <button class="btn-secondary" type="button" disabled={loginAttemptsLoading} onclick={() => loadLoginAttempts(1)}>Apply</button>
    </div>
    {#if loginAttempts.length === 0}
      <p class="text-secondary">No matching login attempts.</p>
    {:else}
      <div class="login-attempt-list">
        {#each loginAttempts as attempt (attempt.id)}
          <div class="login-attempt-row">
            <span class="attempt-status" class:success={attempt.success}>{attempt.success ? 'Success' : 'Failed'}</span>
            <div class="attempt-identity">
              <strong>{attempt.username}</strong>
              <span>{attempt.auth_provider}{attempt.failure_reason ? ` · ${attempt.failure_reason}` : ''}</span>
            </div>
            <span title={attempt.user_agent || ''}>{attempt.ip_address || 'Unknown IP'}</span>
            <time datetime={attempt.created_at}>{formatLoginTime(attempt.created_at)}</time>
          </div>
        {/each}
      </div>
      <div class="login-pagination">
        <button class="btn-secondary" type="button" disabled={loginAttemptsLoading || loginAttemptsPage <= 1} onclick={() => loadLoginAttempts(loginAttemptsPage - 1)}>Previous</button>
        <span>Page {loginAttemptsPage} of {loginAttemptsPages}</span>
        <button class="btn-secondary" type="button" disabled={loginAttemptsLoading || loginAttemptsPage >= loginAttemptsPages} onclick={() => loadLoginAttempts(loginAttemptsPage + 1)}>Next</button>
      </div>
    {/if}
  </div>
</div>

<style>
  .settings-page { max-width: 700px; margin: 0 auto; padding: 24px; }
  h1 { font-size: 22px; margin-bottom: 24px; }
  h2 { font-size: 16px; margin: 0 0 12px; }
  h3 { font-size: 14px; margin: 18px 0 12px; }
.section { background: var(--bg-secondary); border: 1px solid var(--border); border-radius: var(--radius); padding: 16px; margin-bottom: 16px; }
  .toggle-row { display: flex; align-items: center; gap: 10px; font-size: 14px; cursor: pointer; }
  .toggle-row input[type="checkbox"] { width: 18px; height: 18px; }
  .form-group { margin-top: 12px; }
  .form-group label { display: block; font-size: 13px; color: var(--text-secondary); margin-bottom: 4px; }
  .form-group input, .form-group select { width: 100%; padding: 8px 12px; border: 1px solid var(--border); border-radius: var(--radius); font-size: 14px; background: var(--bg-primary); color: var(--text-primary); box-sizing: border-box; }
  .field-hint { margin: 4px 0 0; color: var(--text-secondary); font-size: 12px; line-height: 1.5; }
  .field-hint code { font-size: 11px; }
  .actions { margin-top: 16px; }
  .inline-actions { display: flex; gap: 8px; margin-top: 16px; }
  .btn-primary { padding: 8px 20px; background: var(--accent); color: #fff; border: none; border-radius: var(--radius); font-size: 14px; cursor: pointer; }
  .btn-primary:disabled { opacity: 0.6; cursor: not-allowed; }
  .btn-secondary:disabled { opacity: 0.6; cursor: wait; }
  .btn-secondary,
  .btn-danger {
    padding: 6px 10px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
    color: var(--text-primary);
    font-size: 13px;
    cursor: pointer;
  }
  .btn-danger { color: #cf222e; }
  .provider-list { display: flex; flex-direction: column; gap: 8px; margin-bottom: 16px; }
  .connection-result { margin-bottom: 12px; padding: 8px 10px; border: 1px solid #cf222e; border-radius: var(--radius); color: #cf222e; font-size: 13px; }
  .connection-result.success { border-color: #1a7f37; color: #1a7f37; }
  .section-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
  .section-heading h2 { margin-bottom: 2px; }
  .login-filters { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 8px; margin: 14px 0; }
  .login-filters input, .login-filters select { padding: 7px 9px; border: 1px solid var(--border); border-radius: var(--radius); background: var(--bg-primary); color: var(--text-primary); }
  .login-attempt-list { border: 1px solid var(--border); border-radius: var(--radius); overflow: hidden; }
  .login-attempt-row { display: grid; grid-template-columns: 64px minmax(150px, 1fr) minmax(110px, .6fr) auto; align-items: center; gap: 12px; padding: 9px 10px; border-bottom: 1px solid var(--border); font-size: 12px; }
  .login-attempt-row:last-child { border-bottom: 0; }
  .attempt-status { color: #cf222e; font-weight: 600; }
  .attempt-status.success { color: #1a7f37; }
  .attempt-identity { display: flex; flex-direction: column; min-width: 0; }
  .attempt-identity span, .login-attempt-row time { color: var(--text-secondary); }
  .login-pagination { display: flex; align-items: center; justify-content: flex-end; gap: 10px; margin-top: 10px; color: var(--text-secondary); font-size: 12px; }
  .provider-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 12px;
    border: 1px solid var(--border);
    border-radius: var(--radius);
    background: var(--bg-primary);
  }
  .provider-meta { display: flex; flex-wrap: wrap; gap: 8px; color: var(--text-secondary); font-size: 12px; margin-top: 4px; }
  .provider-meta .enabled { color: #1a7f37; }
  .provider-actions { display: flex; flex-wrap: wrap; gap: 6px; justify-content: flex-end; }
  .form-grid { display: grid; grid-template-columns: repeat(2, minmax(0, 1fr)); gap: 0 12px; }
  @media (max-width: 720px) {
    .provider-row { align-items: stretch; flex-direction: column; }
    .provider-actions { justify-content: flex-start; }
    .form-grid { grid-template-columns: 1fr; }
    .login-filters { grid-template-columns: 1fr; }
    .login-attempt-row { grid-template-columns: 64px 1fr; }
  }
</style>
