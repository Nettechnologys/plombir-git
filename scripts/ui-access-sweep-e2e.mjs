#!/usr/bin/env node

// Inventory-driven browser authorization sweep against ONE already-running
// ephemeral stand. The shell wrapper owns the stand; this process owns two
// accounts, Chrome, the scenario registry and the persona matrix.

import { dirname, resolve } from 'node:path';
import { createServer } from 'node:net';
import { fileURLToPath } from 'node:url';

import { openChromeTab, launchChromeCdp } from './lib/chrome-cdp.mjs';
import { waitForValue } from './lib/browser-smoke-timing.mjs';
import { UI_ACCESS_SWEEP_SCENARIOS } from './lib/ui-access-sweep-scenarios.mjs';
import {
  REQUIRED_PERSONAS,
  assertPersonaResults,
  loadUiAccessSweepSpec,
  ratchetFailure,
  routeTemplateRegex,
  validateUiAccessSweep,
} from './lib/ui-access-sweep.mjs';
import { buildInventory } from './ui-inventory.mjs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const UI_WAIT_MS = positiveMilliseconds('UI_ACCESS_SWEEP_WAIT_MS', 20_000);
const CDP_COMMAND_MS = positiveMilliseconds('UI_ACCESS_SWEEP_CDP_COMMAND_MS', 10_000);
const CHROME_STARTUP_MS = positiveMilliseconds('UI_ACCESS_SWEEP_CHROME_STARTUP_MS', 20_000);
const CHROME = process.env.CHROME || '/usr/bin/google-chrome';

const USER = {
  owner: { username: 'sweep-owner', email: 'sweep-owner@example.com', repository: 'owner-private' },
  outsider: { username: 'sweep-outsider', email: 'sweep-outsider@example.com', repository: 'outsider-private' },
};
const PASSWORD = 'Qz7$wRtm';
const ADMIN_FIXTURE = Object.freeze({
  targetUsername: 'sweep-target',
  targetEmail: 'sweep-target@example.com',
  resourceUsername: 'sweep-resource-user',
  resourceEmail: 'sweep-resource-user@example.com',
  targetOrg: 'sweep-org-target',
  managedOrg: 'sweep-managed-org',
  settingsRepository: 'settings-private',
  branchName: 'seeded-main',
  secretName: 'SWEEP_DELETE',
  deployKeyTitle: 'Seeded browser sweep key',
  deployKey: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA seeded-sweep',
  browserDeployKey: 'ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAICAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA browser-sweep',
  environmentName: 'seeded-environment',
  tagPattern: 'seeded-*',
  webhookUrl: 'https://seeded-sweep.example.invalid/hook',
  browserWebhookUrl: 'https://browser-sweep.example.invalid/hook',
  teamName: 'seeded-team',
  runnerName: 'sweep-admin-runner',
  ssoName: 'Sweep LDAP',
  ssoSlug: 'sweep-ldap',
});

function positiveMilliseconds(name, fallback) {
  const raw = process.env[name];
  if (raw === undefined || raw === '') return fallback;
  const value = Number(raw);
  if (!Number.isInteger(value) || value <= 0) throw new Error(`${name} must be a positive integer`);
  return value;
}

function requiredLoopbackUrl(name) {
  const value = String(process.env[name] || '').replace(/\/$/, '');
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(value)) {
    throw new Error(`${name} must be a loopback ephemeral URL, got ${JSON.stringify(process.env[name] || '')}`);
  }
  return value;
}

function berLength(length) {
  if (length < 0x80) return Buffer.from([length]);
  if (length < 0x100) return Buffer.from([0x81, length]);
  return Buffer.from([0x82, length >> 8, length & 0xff]);
}

function ldapTlv(tag, content) {
  return Buffer.concat([Buffer.from([tag]), berLength(content.length), content]);
}

function ldapInteger(value) {
  const bytes = [];
  for (let current = value; current > 0; current >>>= 8) bytes.unshift(current & 0xff);
  if (bytes.length === 0) bytes.push(0);
  if (bytes[0] & 0x80) bytes.unshift(0);
  return ldapTlv(0x02, Buffer.from(bytes));
}

function ldapBindSuccess(messageId) {
  const result = Buffer.concat([
    ldapTlv(0x0a, Buffer.from([0])),
    ldapTlv(0x04, Buffer.alloc(0)),
    ldapTlv(0x04, Buffer.alloc(0)),
  ]);
  return ldapTlv(0x30, Buffer.concat([ldapInteger(messageId), ldapTlv(0x61, result)]));
}

function readBerLength(buffer, offset) {
  const first = buffer[offset];
  if (first === undefined) throw new Error('truncated BER length');
  if ((first & 0x80) === 0) return { length: first, next: offset + 1 };
  const width = first & 0x7f;
  if (width === 0 || width > 4 || offset + width >= buffer.length) {
    throw new Error('invalid BER length');
  }
  let length = 0;
  for (let index = 0; index < width; index += 1) {
    length = (length << 8) | buffer[offset + 1 + index];
  }
  return { length, next: offset + 1 + width };
}

function ldapFrameLength(buffer) {
  if (buffer.length < 2) return null;
  if (buffer[0] !== 0x30) throw new Error('LDAP fixture expected a BER sequence');
  const first = buffer[1];
  if ((first & 0x80) === 0) return 2 + first;
  const width = first & 0x7f;
  if (width === 0 || width > 4) throw new Error('LDAP fixture received an invalid BER length');
  if (buffer.length < 2 + width) return null;
  let length = 0;
  for (let index = 0; index < width; index += 1) length = (length << 8) | buffer[2 + index];
  const total = 2 + width + length;
  if (total > 64 * 1024) throw new Error('LDAP fixture rejected an oversized request');
  return total;
}

function parseLdapRequest(frame) {
  let offset = readBerLength(frame, 1).next;
  if (frame[offset] !== 0x02) throw new Error('LDAP fixture received no message id');
  const messageIdLength = readBerLength(frame, offset + 1);
  offset = messageIdLength.next;
  let messageId = 0;
  for (let index = 0; index < messageIdLength.length; index += 1) {
    messageId = (messageId << 8) | frame[offset + index];
  }
  offset += messageIdLength.length;
  return { messageId, operation: frame[offset] };
}

async function startLdapFixture() {
  const sockets = new Set();
  const server = createServer((socket) => {
    sockets.add(socket);
    let pending = Buffer.alloc(0);
    socket.on('close', () => sockets.delete(socket));
    socket.on('error', () => {});
    socket.on('data', (chunk) => {
      try {
        pending = Buffer.concat([pending, chunk]);
        for (;;) {
          const total = ldapFrameLength(pending);
          if (total === null || pending.length < total) return;
          const request = parseLdapRequest(pending.subarray(0, total));
          pending = pending.subarray(total);
          if (request.operation === 0x60) socket.write(ldapBindSuccess(request.messageId));
          else if (request.operation === 0x42) socket.end();
          else throw new Error(`LDAP fixture does not implement operation 0x${request.operation.toString(16)}`);
        }
      } catch {
        socket.destroy();
      }
    });
  });
  await new Promise((accept, reject) => {
    const failed = (error) => reject(error);
    server.once('error', failed);
    server.listen(0, '127.0.0.1', () => {
      server.off('error', failed);
      accept();
    });
  });
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('LDAP fixture has no TCP address');
  return {
    port: address.port,
    close: async () => {
      for (const socket of sockets) socket.destroy();
      await new Promise((accept, reject) => server.close((error) => (error ? reject(error) : accept())));
    },
  };
}

function directInvocation() {
  return process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url);
}

async function jsonRequest(url, options = {}) {
  const response = await fetch(url, options);
  const text = await response.text();
  let body = null;
  try { body = text ? JSON.parse(text) : null; } catch {}
  if (!response.ok) {
    throw new Error(`${options.method || 'GET'} ${new URL(url).pathname} returned ${response.status}: ${text.slice(0, 240)}`);
  }
  return body;
}

async function registerPersonas(backendUrl) {
  const tokens = {};
  for (const persona of REQUIRED_PERSONAS) {
    const account = USER[persona];
    const body = await jsonRequest(`${backendUrl}/api/v1/users/register`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username: account.username, email: account.email, password: PASSWORD }),
    });
    if (!body?.token) throw new Error(`registration returned no token for ${persona}`);
    tokens[persona] = body.token;
  }

  for (const persona of REQUIRED_PERSONAS) {
    const profile = await jsonRequest(`${backendUrl}/api/v1/users/me`, {
      headers: { authorization: `Bearer ${tokens[persona]}` },
    });
    const expectedAdmin = persona === 'owner';
    if (profile?.username !== USER[persona].username || profile?.is_admin !== expectedAdmin) {
      throw new Error(
        `${persona} fixture has username=${JSON.stringify(profile?.username)} is_admin=${JSON.stringify(profile?.is_admin)}; `
          + `expected ${USER[persona].username} / ${expectedAdmin}`,
      );
    }
  }
  return tokens;
}

async function seedFixtures(backendUrl, tokens) {
  const ownerJson = (path, options = {}) => jsonRequest(`${backendUrl}/api/v1${path}`, {
    method: options.method || 'GET',
    headers: {
      authorization: `Bearer ${tokens.owner}`,
      ...(options.json === undefined ? {} : { 'content-type': 'application/json' }),
    },
    ...(options.json === undefined ? {} : { body: JSON.stringify(options.json) }),
  });

  const target = await jsonRequest(`${backendUrl}/api/v1/users/register`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      username: ADMIN_FIXTURE.targetUsername,
      email: ADMIN_FIXTURE.targetEmail,
      password: PASSWORD,
    }),
  });
  if (!target?.token) throw new Error('target-user registration returned no token');
  const targetProfile = await jsonRequest(`${backendUrl}/api/v1/users/me`, {
    headers: { authorization: `Bearer ${target.token}` },
  });
  if (!Number.isInteger(targetProfile?.id)) throw new Error('target-user profile returned no numeric id');

  const resourceUser = await jsonRequest(`${backendUrl}/api/v1/users/register`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      username: ADMIN_FIXTURE.resourceUsername,
      email: ADMIN_FIXTURE.resourceEmail,
      password: PASSWORD,
    }),
  });
  if (!resourceUser?.token) throw new Error('resource-user registration returned no token');
  const resourceProfile = await jsonRequest(`${backendUrl}/api/v1/users/me`, {
    headers: { authorization: `Bearer ${resourceUser.token}` },
  });
  if (!Number.isInteger(resourceProfile?.id)) throw new Error('resource-user profile returned no numeric id');

  const failedLogin = await fetch(`${backendUrl}/api/v1/users/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ login: ADMIN_FIXTURE.targetUsername, password: `${PASSWORD}-wrong` }),
  });
  if (failedLogin.status !== 401) {
    throw new Error(`target-user failed-login fixture returned ${failedLogin.status}, expected 401`);
  }

  await jsonRequest(`${backendUrl}/api/v1/orgs`, {
    method: 'POST',
    headers: {
      authorization: `Bearer ${tokens.owner}`,
      'content-type': 'application/json',
    },
    body: JSON.stringify({
      name: ADMIN_FIXTURE.targetOrg,
      display_name: 'Browser Sweep Organization',
      visibility: 'private',
    }),
  });

  await ownerJson('/orgs', {
    method: 'POST',
    json: {
      name: ADMIN_FIXTURE.managedOrg,
      display_name: 'Managed Browser Sweep Organization',
      visibility: 'private',
    },
  });
  await ownerJson('/repos', {
    method: 'POST',
    json: { name: ADMIN_FIXTURE.settingsRepository, is_private: true },
  });

  const repoPath = `/repos/${USER.owner.username}/${ADMIN_FIXTURE.settingsRepository}`;
  const branchRule = await ownerJson(`${repoPath}/branches/protection`, {
    method: 'POST',
    json: {
      branch_name: ADMIN_FIXTURE.branchName,
      require_pr: true,
      require_approval: true,
      required_approvals: 1,
      require_status_check: false,
      required_status_checks: [],
      allow_force_push: false,
      require_signed_commits: false,
      allowed_push_users: [],
    },
  });
  await ownerJson(`${repoPath}/actions/secrets/${ADMIN_FIXTURE.secretName}`, {
    method: 'PUT',
    json: { value: 'seeded-browser-secret' },
  });
  const collaborator = await ownerJson(`${repoPath}/collaborators`, {
    method: 'POST',
    json: { username: ADMIN_FIXTURE.resourceUsername, permission: 'read' },
  });
  const deployKey = await ownerJson(`${repoPath}/keys`, {
    method: 'POST',
    json: {
      title: ADMIN_FIXTURE.deployKeyTitle,
      public_key: ADMIN_FIXTURE.deployKey,
      read_only: true,
    },
  });
  const environment = await ownerJson(`${repoPath}/actions/environments`, {
    method: 'POST',
    json: {
      name: ADMIN_FIXTURE.environmentName,
      protected: true,
      required_approvals: 1,
      allowed_approvers: [],
    },
  });
  const tagRule = await ownerJson(`${repoPath}/tags/protection`, {
    method: 'POST',
    json: { pattern: ADMIN_FIXTURE.tagPattern, allowed_users: [] },
  });
  const webhook = await ownerJson(`${repoPath}/hooks`, {
    method: 'POST',
    json: {
      url: ADMIN_FIXTURE.webhookUrl,
      content_type: 'json',
      active: true,
      events: ['issue.opened'],
    },
  });
  await ownerJson(`${repoPath}/issues`, {
    method: 'POST',
    json: { title: 'Seed a webhook delivery', body: 'Browser sweep fixture', labels: [] },
  });

  let delivery = null;
  for (let attempt = 0; attempt < 50 && delivery === null; attempt += 1) {
    const deliveries = await ownerJson(`${repoPath}/hooks/${webhook.id}/deliveries`);
    delivery = deliveries[0] || null;
    if (delivery === null) await new Promise((accept) => setTimeout(accept, 100));
  }
  if (!Number.isInteger(delivery?.id)) throw new Error('webhook fixture produced no delivery id');

  const orgMember = await ownerJson(`/orgs/${ADMIN_FIXTURE.managedOrg}/members`, {
    method: 'POST',
    json: { username: ADMIN_FIXTURE.resourceUsername, role: 'member' },
  });
  const team = await ownerJson(`/orgs/${ADMIN_FIXTURE.managedOrg}/teams`, {
    method: 'POST',
    json: { name: ADMIN_FIXTURE.teamName, permission: 'read' },
  });
  const teamMember = await ownerJson(`/orgs/${ADMIN_FIXTURE.managedOrg}/teams/${team.id}/members`, {
    method: 'POST',
    json: { username: ADMIN_FIXTURE.resourceUsername, role: 'member' },
  });

  const audit = await jsonRequest(`${backendUrl}/api/v1/admin/audit/logs?page=1&per_page=20`, {
    headers: { authorization: `Bearer ${tokens.owner}` },
  });
  const auditLogId = audit?.logs?.[0]?.id;
  if (!Number.isInteger(auditLogId)) throw new Error('admin fixture setup produced no audit log id');

  return {
    ...ADMIN_FIXTURE,
    targetUserId: targetProfile.id,
    resourceUserId: resourceProfile.id,
    branchRuleId: branchRule.id,
    collaboratorId: collaborator.id,
    collaboratorUserId: collaborator.user_id,
    deployKeyId: deployKey.id,
    environmentId: environment.id,
    tagRuleId: tagRule.id,
    webhookId: webhook.id,
    webhookDeliveryId: delivery.id,
    orgMemberId: orgMember.id,
    teamId: team.id,
    teamMemberId: teamMember.id,
    auditLogId,
    runnerId: null,
    ssoProviderId: null,
  };
}

async function evaluate(tab, expression) {
  const result = await tab.send('Runtime.evaluate', {
    expression,
    returnByValue: true,
    awaitPromise: true,
  });
  if (result?.exceptionDetails) {
    throw new Error(result.exceptionDetails.exception?.description || result.exceptionDetails.text || 'browser evaluation failed');
  }
  return result?.result?.value;
}

async function waitFor(tab, description, expression) {
  return waitForValue({
    read: () => evaluate(tab, expression),
    accept: Boolean,
    description,
    timeoutMs: UI_WAIT_MS,
  });
}

function responseMatches(coverage, response) {
  let path;
  try { path = new URL(response.url).pathname; } catch { return false; }
  return response.method === coverage.method && routeTemplateRegex(coverage.routeUrl).test(path);
}

async function openPersonaTab({ browser, frontendUrl, token }) {
  const requests = new Map();
  const responses = [];
  const problems = [];
  const httpFailures = [];
  const tab = await openChromeTab({
    cdpRoot: browser.cdpRoot,
    commandTimeoutMs: CDP_COMMAND_MS,
    onEvent: (payload) => {
      if (payload.method === 'Network.requestWillBeSent') {
        requests.set(payload.params.requestId, payload.params.request.method);
      } else if (payload.method === 'Network.responseReceived') {
        const response = payload.params.response;
        responses.push({
          method: requests.get(payload.params.requestId) || 'GET',
          url: response.url,
          status: Number(response.status),
        });
        if (Number(response.status) >= 500 && /\/api\//.test(response.url)) {
          httpFailures.push({
            method: requests.get(payload.params.requestId) || 'GET',
            url: response.url,
            status: Number(response.status),
          });
        }
      } else if (payload.method === 'Runtime.exceptionThrown') {
        const details = payload.params?.exceptionDetails;
        const message = details?.exception?.description || details?.text || 'runtime exception';
        problems.push(`browser exception: ${message.split('\n')[0]}`);
      } else if (payload.method === 'Log.entryAdded' && payload.params?.entry?.level === 'error') {
        const message = payload.params.entry.text || 'console error';
        // Network.responseReceived is the status authority and every API 5xx
        // is rejected below. Chromium logs the same response a second time as
        // "Failed to load resource", which adds no independent evidence.
        if (!/Failed to load resource|server responded with a status/i.test(message)) {
          problems.push(`console error: ${message.split('\n')[0]}`);
        }
      }
    },
  });

  await tab.send('Network.clearBrowserCookies');
  const cookie = await tab.send('Network.setCookie', {
    name: 'forgekeep_token',
    value: token,
    url: frontendUrl,
    path: '/',
    httpOnly: true,
    sameSite: 'Strict',
  });
  if (!cookie.success) throw new Error('Chrome refused the persona auth cookie');
  return { ...tab, responses, problems, httpFailures };
}

function browserContext({ tab, frontendUrl, persona, fixture }) {
  const username = USER[persona].username;
  const ensureFrontendOrigin = async () => {
    const origin = await evaluate(tab, 'window.location.origin');
    if (origin !== frontendUrl) {
      await tab.send('Page.navigate', { url: `${frontendUrl}/dashboard` });
      await waitFor(tab, 'dashboard document for browser request', 'document.readyState !== "loading"');
    }
  };
  const request = async (path, options = {}) => {
    await ensureFrontendOrigin();
    const init = {
      method: options.method || 'GET',
      credentials: 'include',
      headers: { ...(options.headers || {}) },
    };
    if (options.json !== undefined) {
      init.headers['content-type'] = 'application/json';
      init.body = JSON.stringify(options.json);
    } else if (options.body !== undefined) {
      init.body = options.body;
    }
    return evaluate(tab, `(async () => {
      const response = await fetch(${JSON.stringify(path)}, ${JSON.stringify(init)});
      return { status: response.status, url: response.url, text: await response.text() };
    })()`);
  };
  return {
    persona,
    username,
    fixture,
    ownerUsername: USER.owner.username,
    ownerRepository: USER.owner.repository,
    repositoryFor: (which) => USER[which].repository,
    navigate: async (path) => {
      await tab.send('Page.navigate', { url: `${frontendUrl}${path}` });
      await waitFor(tab, `document for ${path}`, 'document.readyState !== "loading"');
    },
    waitForPath: (path) => waitFor(
      tab,
      `path ${path}`,
      `window.location.pathname === ${JSON.stringify(path)} && window.location.pathname`,
    ),
    waitForSelector: (selector) => waitFor(
      tab,
      `selector ${selector}`,
      `Boolean(document.querySelector(${JSON.stringify(selector)}))`,
    ),
    waitForText: (selector, expectedText) => waitFor(
      tab,
      `${selector} text ${expectedText}`,
      `[...document.querySelectorAll(${JSON.stringify(selector)})]
        .some((element) => element.textContent.includes(${JSON.stringify(expectedText)}))`,
    ),
    waitForTextAbsent: (selector, unexpectedText) => waitFor(
      tab,
      `${selector} without text ${unexpectedText}`,
      `![...document.querySelectorAll(${JSON.stringify(selector)})]
        .some((element) => element.textContent.includes(${JSON.stringify(unexpectedText)}))`,
    ),
    waitForEnabled: (selector, index = 0) => waitFor(
      tab,
      `enabled ${selector}[${index}]`,
      `(() => {
        const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
        return Boolean(element && !element.disabled);
      })()`,
    ),
    fill: async (selector, value, index = 0) => {
      await waitFor(tab, `${selector}[${index}]`, `(() => {
        const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
        if (!element || element.disabled) return false;
        const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(prototype, 'value').set.call(element, ${JSON.stringify(value)});
        element.dispatchEvent(new Event('input', { bubbles: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return element.value === ${JSON.stringify(value)};
      })()`);
    },
    setChecked: async (selector, checked, index = 0) => {
      await waitFor(tab, `${selector}[${index}] checked=${checked}`, `(() => {
        const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
        if (!element || element.disabled) return false;
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'checked').set.call(element, ${checked});
        element.dispatchEvent(new Event('input', { bubbles: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return element.checked === ${checked};
      })()`);
    },
    setCheckedWithin: async (containerSelector, containingText, targetSelector, checked, index = 0) => {
      const expression = `(() => {
        const container = [...document.querySelectorAll(${JSON.stringify(containerSelector)})]
          .find((element) => element.textContent.includes(${JSON.stringify(containingText)}));
        const element = container?.querySelectorAll(${JSON.stringify(targetSelector)})[${index}];
        return Boolean(element && !element.disabled);
      })()`;
      await waitFor(tab, `${targetSelector}[${index}] inside ${containingText} checked=${checked}`, expression);
      const changed = await evaluate(tab, `(() => {
        const container = [...document.querySelectorAll(${JSON.stringify(containerSelector)})]
          .find((element) => element.textContent.includes(${JSON.stringify(containingText)}));
        const element = container?.querySelectorAll(${JSON.stringify(targetSelector)})[${index}];
        if (!(element instanceof HTMLInputElement) || element.disabled) return false;
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'checked').set.call(element, ${checked});
        element.dispatchEvent(new Event('input', { bubbles: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return element.checked === ${checked};
      })()`);
      if (!changed) throw new Error(`could not set ${targetSelector}[${index}] inside ${containingText}`);
    },
    select: async (selector, value, index = 0) => {
      await waitFor(tab, `${selector}[${index}] value=${value}`, `(() => {
        const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
        if (!(element instanceof HTMLSelectElement) || element.disabled) return false;
        element.value = ${JSON.stringify(value)};
        element.dispatchEvent(new Event('input', { bubbles: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return element.value === ${JSON.stringify(value)};
      })()`);
    },
    click: async (selector, index = 0) => {
      await waitFor(tab, `enabled ${selector}[${index}]`, `(() => {
        const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
        return Boolean(element && !element.disabled);
      })()`);
      const clicked = await evaluate(tab, `(() => {
        const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
        if (!element || element.disabled) return false;
        element.click();
        return true;
      })()`);
      if (!clicked) throw new Error(`could not click ${selector}[${index}]`);
    },
    clickByText: async (selector, expectedText, index = 0) => {
      const expression = `(() => {
        const matches = [...document.querySelectorAll(${JSON.stringify(selector)})]
          .filter((element) => element.textContent.trim().includes(${JSON.stringify(expectedText)}));
        const element = matches[${index}];
        return Boolean(element && !element.disabled);
      })()`;
      await waitFor(tab, `${selector} text ${expectedText}`, expression);
      const clicked = await evaluate(tab, `(() => {
        const matches = [...document.querySelectorAll(${JSON.stringify(selector)})]
          .filter((element) => element.textContent.trim().includes(${JSON.stringify(expectedText)}));
        const element = matches[${index}];
        if (!element || element.disabled) return false;
        element.click();
        return true;
      })()`);
      if (!clicked) throw new Error(`could not click ${selector} with text ${expectedText}`);
    },
    clickWithin: async (containerSelector, containingText, targetSelector, targetText) => {
      const expression = `(() => {
        const container = [...document.querySelectorAll(${JSON.stringify(containerSelector)})]
          .find((element) => element.textContent.includes(${JSON.stringify(containingText)}));
        if (!container) return false;
        const target = [...container.querySelectorAll(${JSON.stringify(targetSelector)})]
          .find((element) => element.textContent.trim().includes(${JSON.stringify(targetText)}));
        return Boolean(target && !target.disabled);
      })()`;
      await waitFor(tab, `${targetText} inside ${containingText}`, expression);
      const clicked = await evaluate(tab, `(() => {
        const container = [...document.querySelectorAll(${JSON.stringify(containerSelector)})]
          .find((element) => element.textContent.includes(${JSON.stringify(containingText)}));
        if (!container) return false;
        const target = [...container.querySelectorAll(${JSON.stringify(targetSelector)})]
          .find((element) => element.textContent.trim().includes(${JSON.stringify(targetText)}));
        if (!target || target.disabled) return false;
        target.click();
        return true;
      })()`);
      if (!clicked) throw new Error(`could not click ${targetText} inside ${containingText}`);
    },
    setConfirm: (answer) => evaluate(tab, `window.confirm = () => ${answer ? 'true' : 'false'}`),
    request,
    fetchJson: async (path, options = {}) => {
      const response = await request(path, options);
      if (response.status < 200 || response.status >= 400) {
        throw new Error(`${options.method || 'GET'} ${path} returned ${response.status}: ${response.text.slice(0, 240)}`);
      }
      try { return response.text ? JSON.parse(response.text) : null; } catch {
        throw new Error(`${options.method || 'GET'} ${path} returned non-JSON`);
      }
    },
  };
}

async function runScenarioForPersona({ scenario, runner, persona, browser, frontendUrl, token, fixture }) {
  const tab = await openPersonaTab({ browser, frontendUrl, token });
  try {
    const action = typeof runner === 'function' ? runner : runner?.[persona];
    if (typeof action !== 'function') throw new Error(`${scenario.id} has no ${persona} browser action`);
    await action(browserContext({ tab, frontendUrl, persona, fixture }));
    await waitForValue({
      read: () => scenario.covers
        .filter((coverage) => !tab.responses.some((response) => responseMatches(coverage, response)))
        .map((coverage) => `${coverage.method} ${coverage.routeUrl}`),
      accept: (missing) => missing.length === 0,
      description: `${scenario.id} network calls for ${persona}`,
      timeoutMs: UI_WAIT_MS,
    });
    if (tab.eventErrors.length > 0) throw tab.eventErrors[0];
    if (tab.problems.length > 0) throw new Error(tab.problems.join(' | '));
    if (tab.httpFailures.length > 0) {
      throw new Error(tab.httpFailures
        .map((response) => `HTTP ${response.status} on ${response.url}`)
        .join(' | '));
    }
    return tab.responses;
  } finally {
    await tab.close();
  }
}

export async function main() {
  const frontendUrl = requiredLoopbackUrl('STAND_FRONTEND_URL');
  const backendUrl = requiredLoopbackUrl('STAND_BACKEND_URL');
  const inventory = buildInventory();
  const spec = loadUiAccessSweepSpec(ROOT);
  const report = validateUiAccessSweep(inventory, spec);
  const ratchet = ratchetFailure(inventory, spec);
  if (ratchet) throw new Error(ratchet);

  const missingRunners = spec.scenarios.filter((scenario) => !UI_ACCESS_SWEEP_SCENARIOS.has(scenario.id));
  if (missingRunners.length > 0) throw new Error(`no runtime for scenario(s): ${missingRunners.map((s) => s.id).join(', ')}`);

  console.log(
    `UI access sweep: ${report.inventoryEntries.length} inventory call entries traversed, `
      + `${report.coveredEntries.length} covered by ${spec.scenarios.length} scenario(s)`,
  );
  const tokens = await registerPersonas(backendUrl);
  const directory = await startLdapFixture();
  try {
    const fixture = { ...await seedFixtures(backendUrl, tokens), ldapPort: directory.port };
    const browser = await launchChromeCdp({
      chromePath: CHROME,
      chromeArgs: ['--headless=new', '--disable-gpu', '--no-sandbox', '--disable-dev-shm-usage', 'about:blank'],
      cdpPort: process.env.CDP_PORT,
      profilePrefix: 'forgekeep-ui-access-sweep-',
      startupTimeoutMs: CHROME_STARTUP_MS,
    });
    try {
      for (const scenario of spec.scenarios) {
        const observed = new Map();
        const runner = UI_ACCESS_SWEEP_SCENARIOS.get(scenario.id);
        const personaOrder = scenario.personaOrder || REQUIRED_PERSONAS;
        for (const persona of personaOrder) {
          process.stdout.write(`  ${scenario.id} [${persona}] ... `);
          const responses = await runScenarioForPersona({
            scenario,
            runner,
            persona,
            browser,
            frontendUrl,
            token: tokens[persona],
            fixture,
          });
          observed.set(persona, responses);
          console.log('observed');
        }
        assertPersonaResults(scenario, observed);
        console.log(`  ${scenario.id}: ${personaOrder.join(' + ')} satisfy Access`);
      }
    } finally {
      await browser.cleanup();
    }
  } finally {
    await directory.close();
  }

  console.log(
    `✅ UI access sweep passed: ${spec.scenarios.length} scenario(s), ${report.coveredEntries.length} inventory entries, `
      + `${inventory.browserSweep.coveredRoutes} UI route(s); ratchet ${spec.ratchet.maxUiRoutesWithoutFrontendTest}`,
  );
}

if (directInvocation()) {
  try { await main(); } catch (error) {
    console.error(`❌ UI access sweep failed: ${error.message}`);
    process.exitCode = 1;
  }
}
