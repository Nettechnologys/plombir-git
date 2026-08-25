#!/usr/bin/env node

// Inventory-driven browser authorization sweep against ONE already-running
// ephemeral stand. The shell wrapper owns the stand; this process owns two
// accounts, Chrome, the scenario registry and the persona matrix.

import { dirname, resolve } from 'node:path';
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
          problems.push(`HTTP ${response.status} on ${response.url}`);
        }
      } else if (payload.method === 'Runtime.exceptionThrown') {
        const details = payload.params?.exceptionDetails;
        const message = details?.exception?.description || details?.text || 'runtime exception';
        problems.push(`browser exception: ${message.split('\n')[0]}`);
      } else if (payload.method === 'Log.entryAdded' && payload.params?.entry?.level === 'error') {
        const message = payload.params.entry.text || 'console error';
        if (!/\b(401|403|404)\b/.test(message)) problems.push(`console error: ${message.split('\n')[0]}`);
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
  return { ...tab, responses, problems };
}

function browserContext({ tab, frontendUrl, persona }) {
  const username = USER[persona].username;
  return {
    persona,
    username,
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
    fill: async (selector, value, index = 0) => {
      await waitFor(tab, `${selector}[${index}]`, `(() => {
        const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
        if (!element) return false;
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
        if (!element) return false;
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'checked').set.call(element, ${checked});
        element.dispatchEvent(new Event('input', { bubbles: true }));
        element.dispatchEvent(new Event('change', { bubbles: true }));
        return element.checked === ${checked};
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
  };
}

async function runScenarioForPersona({ scenario, runner, persona, browser, frontendUrl, token }) {
  const tab = await openPersonaTab({ browser, frontendUrl, token });
  try {
    await runner(browserContext({ tab, frontendUrl, persona }));
    await waitForValue({
      read: () => scenario.covers.every((coverage) => tab.responses.some((response) => responseMatches(coverage, response))),
      accept: Boolean,
      description: `${scenario.id} network calls for ${persona}`,
      timeoutMs: UI_WAIT_MS,
    });
    if (tab.eventErrors.length > 0) throw tab.eventErrors[0];
    if (tab.problems.length > 0) throw new Error(tab.problems.join(' | '));
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
      for (const persona of REQUIRED_PERSONAS) {
        process.stdout.write(`  ${scenario.id} [${persona}] ... `);
        const responses = await runScenarioForPersona({
          scenario,
          runner,
          persona,
          browser,
          frontendUrl,
          token: tokens[persona],
        });
        observed.set(persona, responses);
        console.log('observed');
      }
      assertPersonaResults(scenario, observed);
      console.log(`  ${scenario.id}: ${REQUIRED_PERSONAS.join(' + ')} satisfy Access`);
    }
  } finally {
    await browser.cleanup();
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
