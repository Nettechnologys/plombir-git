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

async function privateRepositoryBlob(context) {
  await context.navigate(`/${context.ownerUsername}/${context.ownerRepository}/blob/README.md`);
}

function privileged(owner, outsider) {
  return Object.freeze({ owner, outsider });
}

async function requestSequence(context, requests) {
  for (const [path, options] of requests) await context.request(path, options);
}

// `PUT /star` is a server-side toggle, so the header refuses to fire it from a
// state it could not read: an outsider whose `GET /starred` is denied now sees
// the button's "state unavailable" form, and clicking it re-reads instead of
// sending a blind toggle (card_da6f696b88f2). The outsider therefore proves the
// route's own refusal with the same live request, exactly as `repo-watch` does,
// while the owner — whose read succeeds — still drives the real control.
const privateRepositoryStar = privileged(
  async (context) => {
    await context.navigate(`/${context.ownerUsername}/${context.ownerRepository}`);
    await context.click('.repo-actions button.action-btn', 0);
  },
  (context) => requestSequence(context, [
    [`/api/v1/repos/${context.ownerUsername}/${context.ownerRepository}/star`, { method: 'PUT' }],
  ]),
);

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
    await context.fill('.form-grid input[type="text"]', context.fixture.runnerRepository, 1);
    await context.fill('.form-grid input[type="text"]', 'browser,sweep', 2);
    await context.click('.form-grid .btn-primary');
    const rows = await context.fetchJson('/api/v1/admin/runners');
    const runner = rows.find((row) => row.name === context.fixture.runnerName);
    if (!runner?.id) throw new Error(`registered runner ${context.fixture.runnerName} is absent from the admin list`);
    context.fixture.runnerId = runner.id;
  },
  (context) => requestSequence(context, [
    ['/api/v1/runners/register', {
      method: 'POST',
      json: {
        repository: context.fixture.runnerRepository,
        name: 'outsider-denied-runner',
        labels: ['browser', 'sweep'],
      },
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

function surfaceApi(context, suffix = '') {
  return `/api/v1/repos/${context.ownerUsername}/${context.ownerRepository}${suffix}`;
}

function surfacePage(context, suffix = '') {
  return `/${context.ownerUsername}/${context.ownerRepository}${suffix}`;
}

async function requestJson(context, path, options = {}) {
  const response = await context.request(path, options);
  if (response.status < 200 || response.status >= 400) {
    throw new Error(`${options.method || 'GET'} ${path} returned ${response.status}: ${response.text.slice(0, 240)}`);
  }
  try { return response.text ? JSON.parse(response.text) : null; } catch {
    throw new Error(`${options.method || 'GET'} ${path} returned non-JSON`);
  }
}

async function waitForWatchState(context, expected) {
  let last = null;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    last = await context.fetchJson(`${surfaceApi(context)}/watch`);
    if (last.watch_state === expected) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`watch state stayed ${JSON.stringify(last?.watch_state)} instead of ${expected}`);
}

async function waitForBoardCard(context, boardId, columnId, note, expectedIndex = null) {
  let last = null;
  for (let attempt = 0; attempt < 100; attempt += 1) {
    last = await context.fetchJson(`${surfaceApi(context)}/boards/${boardId}`);
    const column = last.columns?.find((entry) => entry.column?.id === columnId);
    const index = column?.cards?.findIndex((card) => card.note === note) ?? -1;
    if (index >= 0 && (expectedIndex === null || index === expectedIndex)) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`board ${boardId} never placed ${JSON.stringify(note)} in column ${columnId} at ${expectedIndex}`);
}

async function waitForBoardCardAbsent(context, boardId, note) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const board = await context.fetchJson(`${surfaceApi(context)}/boards/${boardId}`);
    const present = board.columns?.some((entry) =>
      entry.cards?.some((card) => card.note === note),
    );
    if (!present) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`board ${boardId} kept card ${JSON.stringify(note)} after browser delete`);
}

async function waitForBoardColumnAbsent(context, boardId, columnId) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const board = await context.fetchJson(`${surfaceApi(context)}/boards/${boardId}`);
    if (!board.columns?.some((entry) => entry.column?.id === columnId)) return;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`board ${boardId} kept column ${columnId} after browser delete`);
}

async function waitForNewPipeline(context, previousIds) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const result = await context.fetchJson(`${surfaceApi(context)}/pipelines`);
    const created = result.data?.find((pipeline) => !previousIds.includes(pipeline.id));
    if (created?.id) return created.id;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`manual pipeline never appeared after ${previousIds.join(', ')}`);
}

async function seedRepositorySurface(context) {
  const api = surfaceApi(context);

  // Reviewer requests accept only users who can read the private repository.
  // Grant the dedicated resource account the narrowest permission before the
  // PR detail scenario exercises request/remove through the real UI.
  await context.fetchJson(`${api}/collaborators`, {
    method: 'POST',
    json: { username: context.fixture.resourceUsername, permission: 'read' },
  });

  const issue = await context.fetchJson(`${api}/issues`, {
    method: 'POST',
    json: { title: 'Seeded surface issue', body: 'Browser sweep fixture', labels: [] },
  });
  const milestone = await context.fetchJson(`${api}/milestones`, {
    method: 'POST',
    json: { title: 'Seeded surface milestone', description: 'Browser sweep fixture' },
  });
  const label = await context.fetchJson(`${api}/labels`, {
    method: 'POST',
    json: { name: 'seeded-surface-label', color: '#00ff00', description: 'Browser sweep fixture' },
  });

  const board = await context.fetchJson(`${api}/boards`, {
    method: 'POST',
    json: { name: 'Seeded Surface Board', description: 'Browser sweep fixture' },
  });
  const fullBoard = await context.fetchJson(`${api}/boards/${board.id}`);
  const firstColumn = fullBoard.columns?.[0]?.column;
  const secondColumn = fullBoard.columns?.[1]?.column;
  if (!firstColumn?.id || !secondColumn?.id) {
    throw new Error(`seeded board ${board.id} did not return two columns`);
  }
  const firstCard = await context.fetchJson(`${api}/boards/${board.id}/columns/${firstColumn.id}/cards`, {
    method: 'POST',
    json: { note: 'Seeded board card one' },
  });
  const secondCard = await context.fetchJson(`${api}/boards/${board.id}/columns/${firstColumn.id}/cards`, {
    method: 'POST',
    json: { note: 'Seeded board card two' },
  });

  const wikiTitle = 'Seeded-Surface-Page';
  await context.fetchJson(`${api}/wiki`, {
    method: 'POST',
    json: { title: wikiTitle, content: '# Seeded surface page' },
  });
  await context.fetchJson(`${api}/wiki/${encodeURIComponent(wikiTitle)}`, {
    method: 'PATCH',
    json: { content: '# Seeded surface page\n\nRevised.', message: 'Seed revision' },
  });
  const wikiHistory = await context.fetchJson(`${api}/wiki/${encodeURIComponent(wikiTitle)}/history`);
  const wikiRevisionId = wikiHistory?.[0]?.id;
  if (!wikiRevisionId) throw new Error('seeded wiki page produced no revision');

  const timeEntry = await context.fetchJson(`${api}/issues/${issue.number}/time`, {
    method: 'POST',
    json: { duration_minutes: 30, description: 'Seeded time entry' },
  });

  const release = await context.fetchJson(`${api}/releases`, {
    method: 'POST',
    json: { tag_name: 'v-browser-sweep', title: 'Browser sweep release' },
  });
  const releaseAsset = await requestJson(context, `${api}/releases/${release.id}/assets`, {
    method: 'POST',
    headers: { 'x-asset-filename': 'browser-sweep.txt', 'content-type': 'text/plain' },
    body: 'browser sweep asset',
  });

  await requestJson(
    context,
    `${api}/packages/generic/publish?name=browser-sweep&version=1.0.0`,
    {
      method: 'POST',
      headers: {
        'content-type': 'application/octet-stream',
        'content-disposition': 'attachment; filename="browser-sweep.bin"',
      },
      body: 'browser sweep package',
    },
  );

  await context.fetchJson(`${api}/contents/.forgekeep-ci.yml`, {
    method: 'POST',
    json: {
      branch: 'main',
      message: 'Seed browser sweep pipeline',
      content: [
        'stages:',
        '  - test',
        '',
        'browser-sweep:',
        '  stage: test',
        '  script:',
        '    - echo browser-sweep',
        '',
      ].join('\n'),
    },
  });
  let pipeline = null;
  for (let attempt = 0; attempt < 40 && !pipeline; attempt += 1) {
    const result = await context.fetchJson(`${api}/pipelines`);
    pipeline = result.data?.[0] || null;
    if (!pipeline) await new Promise((resolve) => setTimeout(resolve, 250));
  }
  if (!pipeline?.id) throw new Error('browser sweep workflow produced no pipeline');
  let pipelineJob = null;
  for (let attempt = 0; attempt < 40 && !pipelineJob; attempt += 1) {
    const detail = await context.fetchJson(`${api}/pipelines/${pipeline.id}`);
    pipelineJob = detail.stages?.flatMap((entry) => entry.jobs || [])[0] || null;
    if (!pipelineJob) await new Promise((resolve) => setTimeout(resolve, 250));
  }
  if (!pipelineJob?.id) throw new Error(`browser sweep pipeline ${pipeline.id} produced no job`);

  await context.fetchJson(`${api}/branches/protection`, {
    method: 'POST',
    json: {
      branch_name: 'main',
      require_pr: true,
      require_approval: true,
      required_approvals: 2,
      require_status_check: false,
      required_status_checks: [],
      allow_force_push: false,
      require_signed_commits: false,
      allowed_push_users: [],
    },
  });

  const disposableFile = await context.fetchJson(`${api}/contents/delete-me.txt`, {
    method: 'POST',
    json: { branch: 'main', content: 'delete me', message: 'Seed disposable file' },
  });
  const featureLog = await context.fetchJson(`${api}/log?ref=browser-feature`);
  const commitSha = featureLog?.commits?.[0]?.sha;
  if (!commitSha) throw new Error('browser feature branch produced no commit');

  Object.assign(context.fixture, {
    surfaceIssueId: issue.id,
    surfaceIssueNumber: issue.number,
    surfaceMilestoneId: milestone.id,
    surfaceLabelId: label.id,
    surfaceBoardId: board.id,
    surfaceBoardFirstColumnId: firstColumn.id,
    surfaceBoardSecondColumnId: secondColumn.id,
    surfaceBoardFirstCardId: firstCard.id,
    surfaceBoardSecondCardId: secondCard.id,
    surfaceWikiTitle: wikiTitle,
    surfaceWikiRevisionId: wikiRevisionId,
    surfaceTimeEntryId: timeEntry.id,
    surfaceReleaseId: release.id,
    surfaceReleaseAssetId: releaseAsset.id,
    surfaceDisposableSha: disposableFile.commit_sha,
    surfaceCommitSha: commitSha,
    surfacePipelineId: pipeline.id,
    surfacePipelineJobId: pipelineJob.id,
  });
}

const repoContentCreateAndSeed = privileged(
  async (context) => {
    await context.navigate(`${surfacePage(context, '/new')}?path=feature.txt&ref=browser-feature`);
    await context.fill('#file-content', 'browser feature branch\n');
    await context.fill('#commit-message', 'Create browser feature');
    await context.click('.form-actions button.btn-primary');
    await context.waitForPath(surfacePage(context, '/blob/feature.txt'));
    await seedRepositorySurface(context);
  },
  (context) => context.request(`${surfaceApi(context)}/contents/outsider.txt`, {
    method: 'POST',
    json: { branch: 'main', content: 'must not land', message: 'Outsider write' },
  }),
);

const repoContentDelete = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/blob/delete-me.txt'));
    await context.clickByText('.file-actions button', 'Delete');
    await context.fill('#delete-message', 'Delete disposable browser fixture');
    await context.click('.delete-actions button.btn-danger');
    await context.waitForPath(surfacePage(context));
  },
  (context) => context.request(`${surfaceApi(context)}/contents/delete-me.txt?branch=main&message=denied&sha=${context.fixture.surfaceDisposableSha}`, {
    method: 'DELETE',
  }),
);

const repoWatch = privileged(
  async (context) => {
    await context.navigate(surfacePage(context));
    await context.click('.repo-actions button.action-btn', 1);
    await waitForWatchState(context, 'watching');
    await context.navigate(surfacePage(context));
    await context.waitForSelector('.repo-actions button.action-btn[aria-label="Watching"]');
    await context.click('.repo-actions button.action-btn', 1);
    await waitForWatchState(context, 'ignoring');
    await context.navigate(surfacePage(context));
    await context.waitForSelector('.repo-actions button.action-btn[aria-label="Ignoring"]');
    await context.click('.repo-actions button.action-btn', 1);
    await waitForWatchState(context, 'not_watching');
    await context.navigate(surfacePage(context));
    await context.waitForSelector('.repo-actions button.action-btn[aria-label="Watch"]');
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/watch`, { method: 'PUT', json: { mode: 'watching' } }],
    [`${surfaceApi(context)}/watch`, { method: 'DELETE' }],
  ]),
);

const repoFork = privileged(
  async (context) => {
    await context.navigate(`/${context.fixture.resourceUsername}/${context.fixture.forkSourceRepository}`);
    await context.click('.repo-actions button.fork-btn');
    await context.waitForPath(`/${context.username}/${context.fixture.forkSourceRepository}`);
  },
  (context) => context.request(
    `/api/v1/repos/${context.fixture.resourceUsername}/${context.fixture.forkSourceRepository}/fork`,
    { method: 'POST' },
  ),
);

const repoIssues = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/issues'));
    await context.clickByText('.issues-toolbar button', 'New Issue');
    await context.fill('.create-form input[type="text"]', 'Browser-created issue');
    await context.fill('.create-form textarea', 'Created through the live issue form');
    await context.click('.create-form button[type="submit"]');
    await context.waitForText('.issue-list', 'Browser-created issue');

    await context.navigate(surfacePage(context, `/issues/${context.fixture.surfaceIssueNumber}`));
    await context.fill('.comment-form textarea', 'Browser-created comment');
    await context.click('.comment-form button[type="submit"]');
    await context.waitForText('.comment', 'Browser-created comment');
    await context.click('.comment-form button.btn-close');
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/issues`],
    [`${surfaceApi(context)}/issue_templates`],
    [`${surfaceApi(context)}/issue_config`],
    [`${surfaceApi(context)}/issues`, { method: 'POST', json: { title: 'Denied issue' } }],
    [`${surfaceApi(context)}/issues/${context.fixture.surfaceIssueNumber}`],
    [`${surfaceApi(context)}/issues/${context.fixture.surfaceIssueNumber}/comments`],
    [`${surfaceApi(context)}/milestones`],
    [`${surfaceApi(context)}/collaborators`],
    [`${surfaceApi(context)}/issues/${context.fixture.surfaceIssueNumber}`, { method: 'PATCH', json: { state: 'closed' } }],
    [`${surfaceApi(context)}/issues/${context.fixture.surfaceIssueNumber}/comments`, { method: 'POST', json: { body: 'Denied comment' } }],
  ]),
);

const repoMilestones = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/milestones'));
    await context.fill('.editor input', 'Browser-created milestone');
    await context.click('.editor button[type="submit"]');
    await context.waitForText('.milestone-list', 'Browser-created milestone');

    await context.clickWithin('.milestone', 'Seeded surface milestone', 'button', 'Edit');
    await context.waitForText('.editor h2', 'Edit milestone');
    await context.fill('.editor input', 'Seeded surface milestone updated');
    await context.click('.editor button[type="submit"]');
    await context.waitForText('.milestone-list', 'Seeded surface milestone updated');

    await context.setConfirm(true);
    await context.clickWithin('.milestone', 'Browser-created milestone', 'button', 'Delete');
    await context.waitForTextAbsent('.milestone-list', 'Browser-created milestone');
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/milestones`],
    [`${surfaceApi(context)}/milestones`, { method: 'POST', json: { title: 'Denied milestone' } }],
    [`${surfaceApi(context)}/milestones/${context.fixture.surfaceMilestoneId}`],
    [`${surfaceApi(context)}/milestones/${context.fixture.surfaceMilestoneId}`, { method: 'PATCH', json: { title: 'Denied update' } }],
    [`${surfaceApi(context)}/milestones/${context.fixture.surfaceMilestoneId}`, { method: 'DELETE' }],
  ]),
);

const repoBoards = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/boards'));
    await context.clickByText('.page-header button', 'Create Board');
    await context.fill('.modal input', 'Browser Surface Board');
    await context.clickByText('.modal-actions button', 'Create');
    await context.waitForText('.board-header', 'Browser Surface Board');
    const browserBoards = await context.fetchJson(`${surfaceApi(context)}/boards`);
    const browserBoard = browserBoards.find((board) => board.name === 'Browser Surface Board');
    if (!browserBoard?.id) throw new Error('browser-created board is absent from the board list');
    const browserFullBoard = await context.fetchJson(`${surfaceApi(context)}/boards/${browserBoard.id}`);
    const browserTargetColumnId = browserFullBoard.columns?.[1]?.column?.id;
    if (!browserTargetColumnId) throw new Error('browser-created board did not return a target column');

    await context.clickByText('.board-header-actions button', 'Edit');
    await context.fill('.board-edit-form input', 'Browser Surface Board Updated');
    await context.clickByText('.board-edit-form button', 'Save');
    await context.waitForText('.board-header', 'Browser Surface Board Updated');

    await context.click('.kanban-column .add-card-btn', 0);
    await context.fill('.kanban-column .inline-form input', 'Browser board card one', 0);
    await context.click('.kanban-column .inline-form button.btn-primary', 0);
    await context.waitForText('.kanban-column', 'Browser board card one');
    await context.click('.kanban-column .add-card-btn', 0);
    await context.fill('.kanban-column .inline-form input', 'Browser board card two', 0);
    await context.click('.kanban-column .inline-form button.btn-primary', 0);
    await context.waitForText('.kanban-column', 'Browser board card two');

    await context.clickWithin('.card', 'Browser board card one', 'button', '✎');
    await context.fill('.modal textarea', 'Browser board card one updated');
    await context.clickByText('.modal-actions button', 'Save');
    await context.waitForText('.kanban-column', 'Browser board card one updated');
    await context.clickWithin('.card', 'Browser board card one updated', 'button', '↓');
    const browserSourceColumnId = browserFullBoard.columns[0].column.id;
    await waitForBoardCard(
      context,
      browserBoard.id,
      browserSourceColumnId,
      'Browser board card one updated',
      1,
    );
    await context.selectWithin('.card', 'Browser board card one updated', 'select.card-move', browserTargetColumnId);
    await waitForBoardCard(
      context,
      browserBoard.id,
      browserTargetColumnId,
      'Browser board card one updated',
    );
    await context.waitForEnabled('.kanban-column:nth-child(2) .card button[title="Delete"]');
    await context.clickWithin('.card', 'Browser board card one updated', 'button', '×');
    await waitForBoardCardAbsent(context, browserBoard.id, 'Browser board card one updated');
    await context.waitForTextAbsent('.kanban-column', 'Browser board card one updated');
    await context.waitForEnabled('.card button[title="Delete"]');
    await context.click('.kanban-column:nth-child(2) .col-header button[title="Delete"]');
    await waitForBoardColumnAbsent(context, browserBoard.id, browserTargetColumnId);
    await context.waitForTextAbsent('.kanban-board', 'In Progress');
    await context.waitForEnabled('.card button[title="Delete"]');

    await context.setConfirm(true);
    await context.clickWithin('.tab', 'Seeded Surface Board', 'button', '×');
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/boards`],
    [`${surfaceApi(context)}/issues`],
    [`${surfaceApi(context)}/boards`, { method: 'POST', json: { name: 'Denied board' } }],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}`],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}`, { method: 'PATCH', json: { name: 'Denied update' } }],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}`, { method: 'DELETE' }],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}/columns/${context.fixture.surfaceBoardSecondColumnId}`, { method: 'DELETE' }],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}/columns/${context.fixture.surfaceBoardFirstColumnId}/cards`, { method: 'POST', json: { note: 'Denied card' } }],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}/cards/${context.fixture.surfaceBoardFirstCardId}`, { method: 'PATCH', json: { note: 'Denied update' } }],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}/cards/reorder`, { method: 'POST', json: { positions: [{ card_id: context.fixture.surfaceBoardFirstCardId, position: 1 }] } }],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}/cards/${context.fixture.surfaceBoardFirstCardId}/move`, { method: 'POST', json: { column_id: context.fixture.surfaceBoardSecondColumnId, position: 0 } }],
    [`${surfaceApi(context)}/boards/${context.fixture.surfaceBoardId}/cards/${context.fixture.surfaceBoardSecondCardId}`, { method: 'DELETE' }],
  ]),
);

const repoTimeTracking = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/time_tracking'));
    await context.clickByText('.issue-item', 'Browser-created issue');
    await context.fill('#tt-dur', '0.5');
    await context.fill('#tt-desc', 'Browser-created time entry');
    await context.clickByText('.form-action button', 'Add');
    await context.waitForText('tbody', 'Browser-created time entry');
    await context.setConfirm(true);
    await context.clickWithin('tbody tr', 'Browser-created time entry', 'button', 'Delete');
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/issues/${context.fixture.surfaceIssueNumber}/time`],
    [`${surfaceApi(context)}/issues/${context.fixture.surfaceIssueNumber}/time/total`],
    [`${surfaceApi(context)}/issues/${context.fixture.surfaceIssueNumber}/time`, { method: 'POST', json: { duration_minutes: 15 } }],
    [`${surfaceApi(context)}/issues/${context.fixture.surfaceIssueNumber}/time/${context.fixture.surfaceTimeEntryId}`, { method: 'DELETE' }],
  ]),
);

const repoWiki = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/wiki'));
    await context.clickByText('.toolbar button', 'New Page');
    await context.fill('.create-form input', 'Browser-Surface-Page');
    await context.fill('.create-form textarea', '# Browser surface page');
    await context.click('.create-form button[type="submit"]');
    await context.waitForPath(surfacePage(context, '/wiki/Browser-Surface-Page'));

    await context.clickByText('.header-actions button', 'Edit');
    await context.fill('.edit-area textarea', '# Browser surface page\n\nUpdated.');
    await context.clickByText('.edit-area button', 'Save');
    await context.waitForText('.wiki-content', 'Updated.');
    await context.clickByText('.header-actions button', 'History');
    await context.click('.revision-header');

    await context.navigate(surfacePage(context, `/wiki/${context.fixture.surfaceWikiTitle}`));
    await context.setConfirm(true);
    await context.clickByText('.header-actions button', 'Delete');
    await context.waitForPath(surfacePage(context, '/wiki'));
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/wiki`],
    [`${surfaceApi(context)}/wiki`, { method: 'POST', json: { title: 'Denied', content: 'Denied' } }],
    [`${surfaceApi(context)}/wiki/${context.fixture.surfaceWikiTitle}`],
    [`${surfaceApi(context)}/wiki/${context.fixture.surfaceWikiTitle}`, { method: 'PATCH', json: { content: 'Denied' } }],
    [`${surfaceApi(context)}/wiki/${context.fixture.surfaceWikiTitle}/history`],
    [`${surfaceApi(context)}/wiki/${context.fixture.surfaceWikiTitle}/revisions/${context.fixture.surfaceWikiRevisionId}`],
    [`${surfaceApi(context)}/wiki/${context.fixture.surfaceWikiTitle}`, { method: 'DELETE' }],
  ]),
);

const repoLabels = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/settings/labels'));
    await context.clickByText('.page-header button', 'New label');
    await context.fill('#label-name', 'browser-surface-label');
    await context.fill('#label-desc', 'Browser-created label');
    await context.clickByText('.form-actions button', 'Create label');
    await context.waitForText('.labels-grid', 'browser-surface-label');

    await context.clickWithin('.label-card', 'browser-surface-label', 'button', '✏️');
    await context.fill('#label-name', 'browser-surface-label-updated');
    await context.clickByText('.form-actions button', 'Save label');
    await context.waitForText('.labels-grid', 'browser-surface-label-updated');

    await context.clickWithin('.label-card', 'browser-surface-label-updated', 'button', '🗑️');
    await context.clickByText('.form-actions button', 'Delete');
    await context.waitForTextAbsent('.labels-grid', 'browser-surface-label-updated');
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/labels`],
    [`${surfaceApi(context)}/labels`, { method: 'POST', json: { name: 'denied', color: '#ff0000' } }],
    [`${surfaceApi(context)}/labels/${context.fixture.surfaceLabelId}`, { method: 'PATCH', json: { name: 'denied' } }],
    [`${surfaceApi(context)}/labels/${context.fixture.surfaceLabelId}`, { method: 'DELETE' }],
  ]),
);

const repoReadSurface = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/network'));
    await context.waitForSelector('section[aria-labelledby="stargazers-title"][aria-busy="false"]');
    await context.waitForSelector('section[aria-labelledby="forks-title"][aria-busy="false"]');
    await context.navigate(surfacePage(context, `/commits/${context.fixture.surfaceCommitSha}`));
    await context.waitForSelector('.commit-status-page .commit-info, .commit-status-page .error-container');
    for (const path of [
      '/packages',
      '/packages/generic',
      '/packages/generic/browser-sweep',
      '/releases',
      '/releases/new',
      '/settings/branches',
      '/settings/collaborators',
      '/settings/environments',
      '/settings/tags',
      '/pipelines',
    ]) await context.navigate(surfacePage(context, path));
    await context.waitForSelector('.job-card');
    await context.click('.job-card');
    await context.waitForSelector('.log-modal');
    await context.click('.log-header .btn-close');
    await context.waitForSelector('.pipeline-trigger button[type="submit"]:not([disabled])');
    const previousPipelines = await context.fetchJson(`${surfaceApi(context)}/pipelines`);
    await context.click('.pipeline-trigger button[type="submit"]');
    await waitForNewPipeline(context, previousPipelines.data?.map((pipeline) => pipeline.id) || []);
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/stargazers`],
    [`${surfaceApi(context)}/forks`],
    [`${surfaceApi(context)}/commits/${context.fixture.surfaceCommitSha}/status`],
    [`${surfaceApi(context)}/commits/${context.fixture.surfaceCommitSha}/statuses`],
    [`${surfaceApi(context)}/commits/${context.fixture.surfaceCommitSha}/signature`],
    [`${surfaceApi(context)}/packages/generic/list`],
    [`${surfaceApi(context)}/packages/generic/browser-sweep`],
    [`${surfaceApi(context)}/packages/generic/browser-sweep/versions`],
    [`${surfaceApi(context)}/tags`],
    [`${surfaceApi(context)}/branches/protection`],
    [`${surfaceApi(context)}/collaborators`],
    [`${surfaceApi(context)}/actions/environments`],
    [`${surfaceApi(context)}/tags/protection`],
    [`${surfaceApi(context)}/pipelines`],
    [`${surfaceApi(context)}/pipelines`, { method: 'POST', json: { ref: 'main', inputs: {} } }],
    [`${surfaceApi(context)}/pipelines/${context.fixture.surfacePipelineId}`],
    [`${surfaceApi(context)}/pipelines/${context.fixture.surfacePipelineId}/jobs/${context.fixture.surfacePipelineJobId}`],
    [`${surfaceApi(context)}/pipelines/${context.fixture.surfacePipelineId}/artifacts`],
    [`${surfaceApi(context)}/pipelines/workflow-dispatch`],
    ['/api/v1/instance'],
  ]),
);

const repoPullCreate = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/pulls'));
          await context.clickByText('.pulls-toolbar button', 'New Pull Request');
    await context.select('.create-form select', 'browser-feature', 0);
    await context.fill('.create-form input[type="text"]', 'Browser surface pull request');
    await context.fill('.create-form textarea', 'Created through the live pull request form');
    await context.click('.create-form button[type="submit"]');
    await context.waitForText('.pr-list', 'Browser surface pull request');
    const pulls = await context.fetchJson(`${surfaceApi(context)}/pulls?state=open`);
    const pull = pulls.data?.find((row) => row.title === 'Browser surface pull request');
    if (!pull?.number) throw new Error('browser-created pull request is absent from the list');
    context.fixture.surfacePullNumber = pull.number;
    const comment = await context.fetchJson(`${surfaceApi(context)}/pulls/${pull.number}/comments`, {
      method: 'POST',
      json: { path: 'feature.txt', line: 1, body: 'Seeded review thread' },
    });
    context.fixture.surfaceReviewCommentId = comment.id;
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/pulls`],
    [`${surfaceApi(context)}/pull_request_template`],
    [`${surfaceApi(context)}/pulls`, {
      method: 'POST',
      json: { title: 'Denied pull', head_branch: 'browser-feature', base_branch: 'main' },
    }],
  ]),
);

const repoPullDetail = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, `/pulls/${context.fixture.surfacePullNumber}`));
    await context.clickByText('.pr-meta button', 'Convert to draft');
    await context.clickByText('.pr-meta button', 'Mark ready');

    await context.fill('.reviewer-form input', context.fixture.resourceUsername);
    await context.clickByText('.reviewer-form button', 'Request review');
    await context.waitForText('.reviewer-list', context.fixture.resourceUsername);
    await context.click('.reviewer-chip button');
    await context.waitForTextAbsent('.reviewers-box', context.fixture.resourceUsername);

    await context.clickByText('.thread footer button', 'Resolve');
    await context.waitForText('.thread footer button', 'Reopen');
    await context.clickByText('.pr-tabs button', 'Changes');
    await context.click('.comment-gutter button');
    await context.fill('.inline-comment-form textarea', 'Browser-created inline comment');
    await context.clickByText('.inline-comment-form button', 'Submit comment');
    await context.waitForText('.inline-thread', 'Browser-created inline comment');
    await context.clickByText('.pr-tabs button', 'Reviews');
    await context.fill('.review-form textarea', 'Browser-created review');
    await context.clickByText('.review-form button', 'Submit Review');
    await context.waitForText('.pr-tabs button', 'Reviews (1)');

    await context.clickByText('.pr-tabs button', 'Conversation');
    await context.clickByText('.merge-row button', 'Auto-merge when ready');
    await context.clickByText('.auto-merge-pending button', 'Disable auto-merge');
    await context.clickByText('.merge-row button', 'Join merge queue');
    await context.clickByText('.auto-merge-pending button', 'Leave queue');
  },
  (context) => requestSequence(context, [
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}`],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/diff`],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/reviews`],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/comments`],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/timeline`],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/reviewers`],
    [`${surfaceApi(context)}/merge-queue`],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}`, { method: 'PATCH', json: { draft: true } }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/reviewers`, { method: 'POST', json: { username: context.fixture.resourceUsername } }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/reviewers/${context.fixture.resourceUsername}`, { method: 'DELETE' }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/comments/${context.fixture.surfaceReviewCommentId}/resolution`, { method: 'PATCH', json: { resolved: true } }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/comments`, { method: 'POST', json: { path: 'feature.txt', line: 1, body: 'Denied comment' } }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/reviews`, { method: 'POST', json: { action: 'comment', body: 'Denied review' } }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/auto-merge`, { method: 'PUT', json: { strategy: 'merge' } }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/auto-merge`, { method: 'DELETE' }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/merge-queue`, { method: 'PUT', json: { strategy: 'merge' } }],
    [`${surfaceApi(context)}/pulls/${context.fixture.surfacePullNumber}/merge-queue`, { method: 'DELETE' }],
  ]),
);

const repoReleaseAssetDelete = privileged(
  async (context) => {
    await context.navigate(surfacePage(context, '/releases'));
    await context.clickWithin('.asset-row', 'browser-sweep.txt', 'button', 'Delete');
    await context.clickWithin('.asset-row', 'browser-sweep.txt', 'button', 'Delete');
    await context.waitForTextAbsent('.asset-list', 'browser-sweep.txt');
  },
  (context) => context.request(`${surfaceApi(context)}/releases/assets/${context.fixture.surfaceReleaseAssetId}`, {
    method: 'DELETE',
  }),
);

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

const repoTransfer = privileged(
  async (context) => {
    await context.navigate(
      `/${context.ownerUsername}/${context.fixture.settingsRepository}/settings`,
    );
    await context.fill('#new-owner', context.fixture.managedOrg);
    await context.setConfirm(true);
    await context.click('.transfer-section button.btn-warning');
    await context.waitForPath(
      `/${context.fixture.managedOrg}/${context.fixture.settingsRepository}`,
    );
  },
  (context) => context.request(settingsApi(context, '/transfer'), {
    method: 'POST',
    json: { new_owner: context.fixture.managedOrg },
  }),
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
  ['repo-content-create-and-seed', repoContentCreateAndSeed],
  ['repo-content-delete', repoContentDelete],
  ['repo-watch', repoWatch],
  ['repo-fork', repoFork],
  ['repo-issues', repoIssues],
  ['repo-milestones', repoMilestones],
  ['repo-boards', repoBoards],
  ['repo-time-tracking', repoTimeTracking],
  ['repo-wiki', repoWiki],
  ['repo-labels', repoLabels],
  ['repo-read-surface', repoReadSurface],
  ['repo-pull-create', repoPullCreate],
  ['repo-pull-detail', repoPullDetail],
  ['repo-release-asset-delete', repoReleaseAssetDelete],
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
  ['repo-transfer', repoTransfer],
  ['organization-admin', organizationAdmin],
]);
