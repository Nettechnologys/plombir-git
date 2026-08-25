// Scenario actions are deliberately browser-only and fixture-agnostic. The
// runtime supplies the personas, live fixture ids and CDP-backed primitives;
// this registry supplies the actual UI paths and controls. Privileged flows
// have two explicit halves: the outsider sends the same live request first,
// while the resource still exists, and the owner then drives the real control.
// That ordering prevents destructive tests from turning a post-delete 404 into
// a vacuous authorization green.

async function dashboardCreateRepository(context) {
  const repository = context.repositoryFor(context.persona);
  await context.navigate('/dashboard');
  await context.click('.dashboard-header button.btn-primary');
  await context.fill('.create-form input[type="text"][required]', repository);
  await context.setChecked('.create-form input[type="checkbox"]', true, 0);
  await context.setChecked('.create-form input[type="checkbox"]', true, 1);
  await context.click('.create-form form button[type="submit"]');
  await context.waitForPath(`/${context.username}/${repository}`);
}

async function privateRepositoryPage(context) {
  await context.navigate(`/${context.ownerUsername}/${context.ownerRepository}`);
}

async function privateRepositoryStar(context) {
  await context.navigate(`/${context.ownerUsername}/${context.ownerRepository}`);
  await context.click('.repo-actions button.action-btn', 0);
}

async function privateRepositoryBlob(context) {
  await context.navigate(`/${context.ownerUsername}/${context.ownerRepository}/blob/README.md`);
}

function privileged(owner, outsider) {
  return Object.freeze({ owner, outsider });
}

async function requestSequence(context, requests) {
  for (const [path, options] of requests) await context.request(path, options);
}

const adminUsersUnlock = privileged(
  async (context) => {
    await context.navigate('/admin/users');
    await context.clickWithin(
      '.users-table tbody tr',
      context.fixture.targetUsername,
      'button',
      'Unlock',
    );
  },
  (context) => requestSequence(context, [
    [`/api/v1/admin/users/${context.fixture.targetUserId}/unlock`, { method: 'POST' }],
    ['/api/v1/admin/users'],
  ]),
);

const adminUsersUpdate = privileged(
  async (context) => {
    await context.navigate('/admin/users');
    await context.clickWithin(
      '.users-table tbody tr',
      context.fixture.targetUsername,
      'button',
      'Edit',
    );
    await context.fill('#admin-user-display-name', 'Browser Sweep Target');
    await context.click('.modal .modal-actions .btn-primary');
  },
  (context) => requestSequence(context, [
    [`/api/v1/admin/users/${context.fixture.targetUserId}`, {
      method: 'PATCH',
      json: { display_name: 'Outsider must not edit this account' },
    }],
    ['/api/v1/admin/users'],
  ]),
);

const adminUsersDelete = privileged(
  async (context) => {
    await context.navigate('/admin/users');
    await context.clickWithin(
      '.users-table tbody tr',
      context.fixture.targetUsername,
      'button',
      'Delete',
    );
    await context.click('.modal .modal-actions .btn-danger');
  },
  (context) => requestSequence(context, [
    [`/api/v1/admin/users/${context.fixture.targetUserId}`, { method: 'DELETE' }],
    ['/api/v1/admin/users'],
  ]),
);

const adminOrgsDelete = privileged(
  async (context) => {
    await context.navigate('/admin/orgs');
    await context.clickWithin('.orgs-table tbody tr', context.fixture.targetOrg, 'button', 'Delete');
    await context.click('.modal .modal-actions .btn-danger');
  },
  (context) => requestSequence(context, [
    [`/api/v1/admin/orgs/${context.fixture.targetOrg}`, { method: 'DELETE' }],
    ['/api/v1/admin/orgs'],
  ]),
);

const adminRunnersRegister = privileged(
  async (context) => {
    await context.navigate('/admin/runners');
    await context.fill('.form-grid input[type="text"]', context.fixture.runnerName, 0);
    await context.fill('.form-grid input[type="text"]', 'browser,sweep', 1);
    await context.click('.form-grid .btn-primary');
    const rows = await context.fetchJson('/api/v1/admin/runners');
    const runner = rows.find((row) => row.name === context.fixture.runnerName);
    if (!runner?.id) throw new Error(`registered runner ${context.fixture.runnerName} is absent from the admin list`);
    context.fixture.runnerId = runner.id;
  },
  (context) => requestSequence(context, [
    ['/api/v1/runners/register', {
      method: 'POST',
      json: { name: 'outsider-denied-runner', labels: ['browser', 'sweep'] },
    }],
    ['/api/v1/admin/runners'],
  ]),
);

const adminRunnersDelete = privileged(
  async (context) => {
    await context.navigate('/admin/runners');
    await context.clickWithin('.runners-table tbody tr', context.fixture.runnerName, 'button', 'Delete');
    await context.click('.modal .modal-actions .btn-danger');
  },
  (context) => requestSequence(context, [
    [`/api/v1/admin/runners/${context.fixture.runnerId}`, { method: 'DELETE' }],
    ['/api/v1/admin/runners'],
  ]),
);

const adminAuditDetail = privileged(
  async (context) => {
    await context.navigate('/admin/audit');
    await context.select('.filters select', 'user.register', 0);
    await context.click('.audit-table tbody .btn-sm');
  },
  async (context) => {
    await context.request('/api/v1/admin/audit/logs?page=1&per_page=20');
    await context.request(`/api/v1/admin/audit/logs/${context.fixture.auditLogId}`);
  },
);

const adminSettingsAndLoginAttempts = privileged(
  async (context) => {
    await context.navigate('/admin/settings');
    await context.fill('#admin-banner-message', 'Browser access sweep');
    await context.clickByText('.actions button', 'Save Settings');
    await context.clickByText('.section-heading button', 'Refresh');
  },
  async (context) => {
    await context.request('/api/v1/admin/settings');
    await context.request('/api/v1/admin/settings', {
      method: 'PATCH',
      json: { banner_message: 'Outsider must not change this banner', banner_type: 'warning' },
    });
    await context.request('/api/v1/admin/login-attempts?page=1&per_page=20');
  },
);

function ldapProviderPayload(context) {
  return {
    name: context.fixture.ssoName,
    slug: context.fixture.ssoSlug,
    provider_type: 'ldap',
    ldap_host: 'ldap://127.0.0.1',
    ldap_port: context.fixture.ldapPort,
    ldap_bind_dn: 'cn=service,dc=example,dc=com',
    ldap_bind_password: 'browser-sweep-bind-secret',
    ldap_base_dn: 'dc=example,dc=com',
    ldap_user_filter: '(uid={username})',
    enabled: true,
    auto_provision: false,
    allowed_email_domains: '',
  };
}

const adminSsoCreate = privileged(
  async (context) => {
    await context.navigate('/admin/settings');
    await context.fill('#sso-name', context.fixture.ssoName);
    await context.fill('#sso-slug', context.fixture.ssoSlug);
    await context.select('#sso-type', 'ldap');
    await context.fill('#sso-ldap-host', 'ldap://127.0.0.1');
    await context.fill('#sso-ldap-port', String(context.fixture.ldapPort));
    await context.fill('#sso-ldap-bind-dn', 'cn=service,dc=example,dc=com');
    await context.fill('#sso-ldap-bind-password', 'browser-sweep-bind-secret');
    await context.fill('#sso-ldap-base-dn', 'dc=example,dc=com');
    await context.fill('#sso-ldap-filter', '(uid={username})');
    await context.clickByText('.sso-form .inline-actions button', 'Create Provider');
    const providers = await context.fetchJson('/api/v1/admin/sso/providers');
    const provider = providers.find((row) => row.slug === context.fixture.ssoSlug);
    if (!provider?.id) throw new Error(`created SSO provider ${context.fixture.ssoSlug} is absent from the admin list`);
    context.fixture.ssoProviderId = provider.id;
  },
  (context) => requestSequence(context, [
    ['/api/v1/admin/sso/providers', {
      method: 'POST',
      json: ldapProviderPayload(context),
    }],
    ['/api/v1/admin/sso/providers'],
  ]),
);

const adminSsoUpdate = privileged(
  async (context) => {
    await context.navigate('/admin/settings');
    await context.clickWithin('.provider-row', context.fixture.ssoName, 'button', 'Disable');
  },
  (context) => requestSequence(context, [
    [`/api/v1/admin/sso/providers/${context.fixture.ssoProviderId}`, {
      method: 'PATCH',
      json: { ...ldapProviderPayload(context), enabled: false },
    }],
    ['/api/v1/admin/sso/providers'],
  ]),
);

const adminSsoTest = privileged(
  async (context) => {
    await context.navigate('/admin/settings');
    await context.clickWithin('.provider-row', context.fixture.ssoName, 'button', 'Test connection');
    await context.waitForSelector('.connection-result');
  },
  (context) => context.request(`/api/v1/admin/sso/providers/${context.fixture.ssoProviderId}/test`, {
    method: 'POST',
  }),
);

const adminSsoDelete = privileged(
  async (context) => {
    await context.navigate('/admin/settings');
    await context.setConfirm(true);
    await context.clickWithin('.provider-row', context.fixture.ssoName, 'button', 'Delete');
  },
  (context) => requestSequence(context, [
    [`/api/v1/admin/sso/providers/${context.fixture.ssoProviderId}`, { method: 'DELETE' }],
    ['/api/v1/admin/sso/providers'],
  ]),
);

export const UI_ACCESS_SWEEP_SCENARIOS = new Map([
  ['dashboard-create-repository', dashboardCreateRepository],
  ['private-repository-page', privateRepositoryPage],
  ['private-repository-star', privateRepositoryStar],
  ['private-repository-blob', privateRepositoryBlob],
  ['admin-users-unlock', adminUsersUnlock],
  ['admin-users-update', adminUsersUpdate],
  ['admin-users-delete', adminUsersDelete],
  ['admin-orgs-delete', adminOrgsDelete],
  ['admin-runners-register', adminRunnersRegister],
  ['admin-runners-delete', adminRunnersDelete],
  ['admin-audit-detail', adminAuditDetail],
  ['admin-settings-and-login-attempts', adminSettingsAndLoginAttempts],
  ['admin-sso-create', adminSsoCreate],
  ['admin-sso-update', adminSsoUpdate],
  ['admin-sso-test', adminSsoTest],
  ['admin-sso-delete', adminSsoDelete],
]);
