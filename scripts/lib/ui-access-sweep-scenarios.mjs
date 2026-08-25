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

function settingsApi(context, suffix = '') {
  return `/api/v1/repos/${context.ownerUsername}/${context.fixture.settingsRepository}${suffix}`;
}

function settingsPage(context, page) {
  return `/${context.ownerUsername}/${context.fixture.settingsRepository}/settings/${page}`;
}

function branchProtectionPayload(context, includeBranch = false) {
  return {
    ...(includeBranch ? { branch_name: context.fixture.branchName } : {}),
    require_pr: true,
    require_status_check: false,
    required_status_checks: [],
    require_approval: true,
    required_approvals: 2,
    allow_force_push: false,
    require_signed_commits: false,
    allowed_push_users: [],
  };
}

const repoBranchProtections = privileged(
  async (context) => {
    await context.navigate(settingsPage(context, 'branches'));
    await context.fill('#protected-branch', 'browser-created');
    await context.click('.rule-form button[type="submit"]');
    await context.waitForText('tbody tr', 'browser-created');
    await context.clickWithin('tbody tr', context.fixture.branchName, 'button', 'Edit');
    await context.fill('#required-approvals', '2');
    await context.click('.rule-form button[type="submit"]');
    await context.waitForTextAbsent('.rule-form', 'Cancel');
    await context.setConfirm(true);
    await context.clickWithin('tbody tr', context.fixture.branchName, 'button', 'Delete');
  },
  (context) => requestSequence(context, [
    [settingsApi(context, '/branches/protection'), {
      method: 'POST',
      json: { ...branchProtectionPayload(context, true), branch_name: 'outsider-created' },
    }],
    [settingsApi(context, `/branches/protection/${context.fixture.branchRuleId}`), {
      method: 'PATCH',
      json: branchProtectionPayload(context),
    }],
    [settingsApi(context, `/branches/protection/${context.fixture.branchRuleId}`), { method: 'DELETE' }],
  ]),
);

const repoCiSecrets = privileged(
  async (context) => {
    await context.navigate(settingsPage(context, 'ci-secrets'));
    await context.fill('#secret-name', 'SWEEP_CREATED');
    await context.fill('#secret-value', 'browser-created-secret');
    await context.click('form button.btn-primary');
    await context.waitForText('article', 'SWEEP_CREATED');
    await context.setConfirm(true);
    await context.clickWithin('article', context.fixture.secretName, 'button', 'Delete');
  },
  (context) => requestSequence(context, [
    [settingsApi(context, '/actions/secrets')],
    [settingsApi(context, '/actions/secrets/OUTSIDER_SECRET'), {
      method: 'PUT',
      json: { value: 'outsider-must-not-write' },
    }],
    [settingsApi(context, `/actions/secrets/${context.fixture.secretName}`), { method: 'DELETE' }],
  ]),
);

const repoCollaborators = privileged(
  async (context) => {
    await context.navigate(settingsPage(context, 'collaborators'));
    await context.select('tbody select', 'write');
    await context.clickWithin('tbody tr', context.fixture.resourceUsername, 'button', 'Save');
    await context.setConfirm(true);
    await context.clickWithin('tbody tr', context.fixture.resourceUsername, 'button', 'Delete');
    await context.waitForTextAbsent('tbody tr', context.fixture.resourceUsername);
    await context.fill('#collaborator-user', context.fixture.resourceUsername);
    await context.select('#collaborator-permission', 'admin');
    await context.click('.add-form button[type="submit"]');
    await context.waitForText('tbody tr', context.fixture.resourceUsername);
  },
  (context) => requestSequence(context, [
    [settingsApi(context, '/collaborators'), {
      method: 'POST',
      json: { username: context.fixture.resourceUsername, permission: 'write' },
    }],
    [settingsApi(context, `/collaborators/${context.fixture.collaboratorId}`), {
      method: 'PATCH',
      json: { permission: 'write' },
    }],
    [settingsApi(context, `/collaborators/${context.fixture.collaboratorUserId}`), { method: 'DELETE' }],
  ]),
);

const repoDeployKeys = privileged(
  async (context) => {
    await context.navigate(settingsPage(context, 'deploy-keys'));
    await context.fill('#deploy-key-title', 'Browser-created key');
    await context.fill('#deploy-public-key', context.fixture.browserDeployKey);
    await context.click('form button[type="submit"]');
    await context.waitForText('article', 'Browser-created key');
    await context.setConfirm(true);
    await context.clickWithin('article', context.fixture.deployKeyTitle, 'button', 'Delete');
  },
  (context) => requestSequence(context, [
    [settingsApi(context, '/keys')],
    [settingsApi(context, '/keys'), {
      method: 'POST',
      json: {
        title: 'Outsider key',
        public_key: context.fixture.browserDeployKey,
        read_only: false,
      },
    }],
    [settingsApi(context, `/keys/${context.fixture.deployKeyId}`), { method: 'DELETE' }],
  ]),
);

function environmentPayload(name) {
  return { name, protected: true, required_approvals: 2, allowed_approvers: [] };
}

const repoEnvironments = privileged(
  async (context) => {
    await context.navigate(settingsPage(context, 'environments'));
    await context.fill('#environment-name', 'browser-environment');
    await context.click('form button.btn-primary');
    await context.waitForText('article', 'browser-environment');
    await context.clickWithin('article', context.fixture.environmentName, 'button', 'Edit');
    await context.fill('#required-approvals', '2');
    await context.clickByText('form button', 'Save changes');
    await context.waitForText('form button', 'Create environment');
    await context.setConfirm(true);
    await context.clickWithin('article', context.fixture.environmentName, 'button', 'Delete');
  },
  (context) => requestSequence(context, [
    [settingsApi(context, '/actions/environments'), {
      method: 'POST',
      json: environmentPayload('outsider-environment'),
    }],
    [settingsApi(context, `/actions/environments/${context.fixture.environmentId}`), {
      method: 'PUT',
      json: environmentPayload(context.fixture.environmentName),
    }],
    [settingsApi(context, `/actions/environments/${context.fixture.environmentId}`), { method: 'DELETE' }],
  ]),
);

const repoRetention = privileged(
  async (context) => {
    await context.navigate(settingsPage(context, 'retention'));
    await context.fill('#artifact-days', '31');
    await context.fill('#cache-days', '8');
    await context.clickByText('form button', 'Save policy');
    await context.clickByText('form button', 'Clean expired storage now');
  },
  (context) => requestSequence(context, [
    [settingsApi(context, '/actions/retention')],
    [settingsApi(context, '/actions/retention'), {
      method: 'PUT',
      json: { artifact_retention_days: 31, cache_retention_days: 8 },
    }],
    [settingsApi(context, '/actions/retention/expired'), { method: 'DELETE' }],
  ]),
);

const repoTagProtections = privileged(
  async (context) => {
    await context.navigate(settingsPage(context, 'tags'));
    await context.fill('#tag-pattern', 'browser-*');
    await context.click('form button.btn-primary');
    await context.waitForText('article', 'browser-*');
    await context.clickWithin('article', context.fixture.tagPattern, 'button', 'Edit');
    await context.fill('#tag-allowed-users', context.ownerUsername);
    await context.clickByText('form button', 'Save changes');
    await context.waitForText('form button', 'Add protection');
    await context.setConfirm(true);
    await context.clickWithin('article', context.fixture.tagPattern, 'button', 'Delete');
  },
  (context) => requestSequence(context, [
    [settingsApi(context, '/tags/protection'), {
      method: 'POST',
      json: { pattern: 'outsider-*', allowed_users: [] },
    }],
    [settingsApi(context, `/tags/protection/${context.fixture.tagRuleId}`), {
      method: 'PATCH',
      json: { allowed_users: [context.ownerUsername] },
    }],
    [settingsApi(context, `/tags/protection/${context.fixture.tagRuleId}`), { method: 'DELETE' }],
  ]),
);

const repoWebhooks = privileged(
  async (context) => {
    await context.navigate(settingsPage(context, 'webhooks'));
    await context.fill('#webhook-url', context.fixture.browserWebhookUrl);
    await context.click('.webhook-form button[type="submit"]');
    await context.waitForText('.hook-item', context.fixture.browserWebhookUrl);
    await context.setCheckedWithin(
      '.hook-item',
      context.fixture.webhookUrl,
      'input[type="checkbox"]',
      false,
    );
    await context.waitForText('.hook-item', 'Disabled');
    await context.clickWithin('.hook-item', context.fixture.webhookUrl, 'button', 'View deliveries');
    await context.waitForSelector('.delivery-item');
    await context.clickByText('.delivery-item button', 'Redeliver');
    await context.waitForEnabled('.delivery-item button');
    await context.setConfirm(true);
    await context.clickWithin('.hook-item', context.fixture.webhookUrl, 'button', 'Delete');
  },
  (context) => requestSequence(context, [
    [settingsApi(context, '/hooks')],
    [settingsApi(context, '/hooks'), {
      method: 'POST',
      json: {
        url: 'https://outsider.example.invalid/hook',
        content_type: 'json',
        active: true,
        events: ['issue.opened'],
      },
    }],
    [settingsApi(context, `/hooks/${context.fixture.webhookId}`)],
    [settingsApi(context, `/hooks/${context.fixture.webhookId}`), {
      method: 'PATCH',
      json: { active: false },
    }],
    [settingsApi(context, `/hooks/${context.fixture.webhookId}/deliveries`)],
    [settingsApi(
      context,
      `/hooks/${context.fixture.webhookId}/deliveries/${context.fixture.webhookDeliveryId}/redeliver`,
    ), { method: 'POST' }],
    [settingsApi(context, `/hooks/${context.fixture.webhookId}`), { method: 'DELETE' }],
  ]),
);

const organizationAdmin = privileged(
  async (context) => {
    await context.navigate(`/orgs/${context.fixture.managedOrg}`);
    await context.clickByText('.header-actions button', 'Edit');
    await context.fill('.edit-organization input[type="text"]', 'Managed Browser Sweep Updated');
    await context.click('.edit-organization button[type="submit"]');
    await context.clickWithin('.managed-item', context.fixture.teamName, 'button', 'View members');
    await context.setConfirm(true);
    await context.clickWithin('.team-members .compact-item', context.fixture.resourceUsername, 'button', 'Delete');
    await context.clickWithin('.managed-item', context.fixture.teamName, 'button', 'Delete');
    await context.clickWithin(
      '.grid .section:last-child .item',
      context.fixture.resourceUsername,
      'button',
      'Delete',
    );
    await context.fill('.grid .section:last-child .member-form input', context.fixture.resourceUsername);
    await context.click('.grid .section:last-child .member-form button[type="submit"]');
    await context.fill('.grid .section:first-child .create-form input', 'browser-team');
    await context.click('.grid .section:first-child .create-form button[type="submit"]');
    await context.clickWithin('.managed-item', 'browser-team', 'button', 'View members');
    await context.fill('.team-members .member-form input', context.fixture.resourceUsername);
    await context.click('.team-members .member-form button[type="submit"]');
    await context.clickByText('.header-actions button', 'Delete organization');
    await context.waitForPath('/orgs');
  },
  (context) => requestSequence(context, [
    [`/api/v1/orgs/${context.fixture.managedOrg}`, {
      method: 'PATCH',
      json: {
        display_name: 'Outsider must not edit this organization',
        description: '',
        visibility: 'private',
      },
    }],
    [`/api/v1/orgs/${context.fixture.managedOrg}/members`, {
      method: 'POST',
      json: { username: context.fixture.resourceUsername, role: 'member' },
    }],
    [`/api/v1/orgs/${context.fixture.managedOrg}/members/${context.fixture.resourceUserId}`, { method: 'DELETE' }],
    [`/api/v1/orgs/${context.fixture.managedOrg}/teams`, {
      method: 'POST',
      json: { name: 'outsider-team', permission: 'read' },
    }],
    [`/api/v1/orgs/${context.fixture.managedOrg}/teams/${context.fixture.teamId}`, { method: 'DELETE' }],
    [`/api/v1/orgs/${context.fixture.managedOrg}/teams/${context.fixture.teamId}/members`, {
      method: 'POST',
      json: { username: context.fixture.resourceUsername, role: 'member' },
    }],
    [`/api/v1/orgs/${context.fixture.managedOrg}/teams/${context.fixture.teamId}/members/${context.fixture.resourceUserId}`, {
      method: 'DELETE',
    }],
    [`/api/v1/orgs/${context.fixture.managedOrg}`, { method: 'DELETE' }],
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
  ['repo-branch-protections', repoBranchProtections],
  ['repo-ci-secrets', repoCiSecrets],
  ['repo-collaborators', repoCollaborators],
  ['repo-deploy-keys', repoDeployKeys],
  ['repo-environments', repoEnvironments],
  ['repo-retention', repoRetention],
  ['repo-tag-protections', repoTagProtections],
  ['repo-webhooks', repoWebhooks],
  ['organization-admin', organizationAdmin],
]);
