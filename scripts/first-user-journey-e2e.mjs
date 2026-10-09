#!/usr/bin/env node

// One browser-owned user journey against ONE already-running ephemeral stand.
// `first-user-journey-e2e.sh` owns the two-stand acceptance loop; keeping that
// outside this process makes a second run incapable of inheriting cookies,
// database rows, repositories, Chrome profiles or a surviving frontend.
//
// Optional timing overrides (positive integer milliseconds):
//   JOURNEY_STARTUP_TIMEOUT_MS=20000
//   JOURNEY_CDP_COMMAND_TIMEOUT_MS=10000
//   JOURNEY_UI_WAIT_TIMEOUT_MS=15000
//   JOURNEY_WAIT_MS=<legacy fallback for all three budgets>

import { execFileSync } from 'node:child_process';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

import { launchChromeCdp } from './lib/chrome-cdp.mjs';
import {
  firstUserJourneyTimeouts,
  waitForValue,
  withCdpCommandTimeout,
} from './lib/browser-smoke-timing.mjs';

const FRONTEND_URL = requiredUrl('STAND_FRONTEND_URL', process.env.STAND_FRONTEND_URL || process.env.FRONTEND_URL);
const BACKEND_URL = requiredUrl('STAND_BACKEND_URL', process.env.STAND_BACKEND_URL || process.env.BACKEND_URL);
const WORK_DIR = requiredEnv('STAND_WORK_DIR');
const CHROME = process.env.CHROME || '/usr/bin/google-chrome';
const {
  startupMs: JOURNEY_STARTUP_TIMEOUT_MS,
  cdpCommandMs: JOURNEY_CDP_COMMAND_TIMEOUT_MS,
  uiWaitMs: JOURNEY_UI_WAIT_TIMEOUT_MS,
} = firstUserJourneyTimeouts();

const USERNAME = 'journey-founder';
const EMAIL = 'journey-founder@example.com';
const PASSWORD = 'Qz7$wRtm';
// The one-time setup token the stand's server generated for its first account;
// `scripts/ephemeral-stand.sh` reads it from the data directory and exports it.
const SETUP_TOKEN = requiredEnv('STAND_SETUP_TOKEN');
const REPO = 'journey-repo';
const BLOB = 'journey.txt';
const BLOB_MARKER = 'plombir-git first-user journey reached the blob';
const ISSUE_TITLE = 'First-user journey issue';
const ISSUE_BODY = 'Created and closed through the real browser UI.';

let cdpRoot = '';
let allowLoggedOut401 = true;

function requiredEnv(name) {
  const value = String(process.env[name] || '').trim();
  if (!value) throw new Error(`${name} is required; run through scripts/first-user-journey-e2e.sh`);
  return value;
}

function requiredUrl(name, raw) {
  const value = String(raw || '').replace(/\/$/, '');
  if (!/^http:\/\/127\.0\.0\.1:\d+$/.test(value)) {
    throw new Error(`${name} must be a loopback ephemeral URL, got ${JSON.stringify(raw || '')}`);
  }
  return value;
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function normalizePath(raw = '') {
  const value = String(raw).split('?')[0].replace(/\/+$/, '');
  return value || '/';
}

function createSession(tabId, wsUrl) {
  const ws = new WebSocket(wsUrl);
  const pending = new Map();
  const problems = [];
  let messageId = 0;

  return new Promise((resolve, reject) => {
    const send = (method, params = {}) => {
      const id = ++messageId;
      const response = new Promise((resolveSend, rejectSend) => {
        pending.set(id, { resolve: resolveSend, reject: rejectSend });
      });
      return withCdpCommandTimeout({
        method,
        timeoutMs: JOURNEY_CDP_COMMAND_TIMEOUT_MS,
        run: () => {
          ws.send(JSON.stringify({ id, method, params }));
          return response;
        },
        onTimeout: () => pending.delete(id),
      }).finally(() => pending.delete(id));
    };

    ws.addEventListener('message', (event) => {
      let payload;
      try { payload = JSON.parse(event.data); } catch { return; }

      if (payload.id && pending.has(payload.id)) {
        const waiter = pending.get(payload.id);
        pending.delete(payload.id);
        if (payload.error) waiter.reject(new Error(payload.error.message || 'CDP error'));
        else waiter.resolve(payload.result || payload);
        return;
      }

      if (payload.method === 'Runtime.exceptionThrown') {
        const details = payload.params?.exceptionDetails;
        const message = details?.exception?.description || details?.text || 'runtime exception';
        problems.push(`browser exception: ${message.split('\n')[0]}`);
      }

      if (payload.method === 'Log.entryAdded' && payload.params?.entry?.level === 'error') {
        const message = payload.params.entry.text || 'console error';
        if (!(allowLoggedOut401 && /\b401\b/.test(message))) {
          problems.push(`console error: ${message.split('\n')[0]}`);
        }
      }

      if (payload.method === 'Network.responseReceived') {
        const response = payload.params?.response;
        const status = Number(response?.status || 0);
        const url = response?.url || '';
        if (status >= 400 && /\/api\//.test(url)) {
          const expectedLoggedOutProbe = allowLoggedOut401 && status === 401 && /\/users\/me(?:\?|$)/.test(url);
          if (!expectedLoggedOutProbe) problems.push(`HTTP ${status} on ${url}`);
        }
      }
    });

    ws.addEventListener('error', () => reject(new Error('Chrome tab WebSocket failed')));
    ws.addEventListener('open', async () => {
      try {
        await send('Page.enable');
        await send('Runtime.enable');
        await send('Log.enable');
        await send('Network.enable');
        resolve({
          send,
          problems,
          close: async () => {
            try { await fetch(`${cdpRoot}/json/close/${tabId}`); } catch {}
            ws.close();
          },
        });
      } catch (error) {
        ws.close();
        reject(error);
      }
    });
  });
}

async function openTab(url) {
  const response = await fetch(`${cdpRoot}/json/new?${encodeURIComponent(url)}`, { method: 'PUT' });
  const text = await response.text();
  let target;
  try { target = JSON.parse(text); } catch {
    throw new Error(`Chrome could not open a tab: HTTP ${response.status} ${text.slice(0, 120)}`);
  }
  if (!target?.id || !target.webSocketDebuggerUrl) throw new Error(`Chrome returned no debugger target for ${url}`);
  return createSession(target.id, target.webSocketDebuggerUrl);
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
    timeoutMs: JOURNEY_UI_WAIT_TIMEOUT_MS,
  });
}

async function waitForPath(tab, expected) {
  return waitFor(
    tab,
    `path ${expected}`,
    `(() => window.location.pathname === ${JSON.stringify(expected)} && window.location.pathname)()`,
  );
}

async function navigate(tab, path) {
  await tab.send('Page.navigate', { url: `${FRONTEND_URL}${path}` });
  await waitForPath(tab, path);
  await waitFor(tab, `document for ${path}`, 'document.readyState !== "loading"');
}

async function fill(tab, selector, value, index = 0) {
  const expression = `(() => {
    const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
    if (!element) return false;
    const prototype = element instanceof HTMLTextAreaElement ? HTMLTextAreaElement.prototype : HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(prototype, 'value').set.call(element, ${JSON.stringify(value)});
    element.dispatchEvent(new Event('input', { bubbles: true }));
    element.dispatchEvent(new Event('change', { bubbles: true }));
    return element.value === ${JSON.stringify(value)};
  })()`;
  await waitFor(tab, `${selector}[${index}]`, expression);
}

async function setChecked(tab, selector, checked, index = 0) {
  const expression = `(() => {
    const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
    if (!element) return false;
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'checked').set.call(element, ${checked});
    element.dispatchEvent(new Event('input', { bubbles: true }));
    element.dispatchEvent(new Event('change', { bubbles: true }));
    return element.checked === ${checked};
  })()`;
  await waitFor(tab, `${selector}[${index}] checked=${checked}`, expression);
}

async function click(tab, selector, index = 0) {
  await waitFor(
    tab,
    `enabled ${selector}[${index}]`,
    `(() => { const e = document.querySelectorAll(${JSON.stringify(selector)})[${index}]; return Boolean(e && !e.disabled); })()`,
  );
  const clicked = await evaluate(tab, `(() => {
    const element = document.querySelectorAll(${JSON.stringify(selector)})[${index}];
    if (!element || element.disabled) return false;
    element.click();
    return true;
  })()`);
  if (!clicked) throw new Error(`could not click ${selector}[${index}]`);
}

async function step(name, action) {
  process.stdout.write(`  ${name} ... `);
  await action();
  console.log('ok');
}

function git(args, options = {}) {
  return execFileSync('git', args, {
    cwd: WORK_DIR,
    env: { ...process.env, ...(options.env || {}) },
    encoding: 'utf8',
    stdio: options.quiet ? 'ignore' : ['ignore', 'pipe', 'pipe'],
  });
}

function pushFixture(jwt) {
  const seed = join(WORK_DIR, 'journey-seed');
  mkdirSync(seed);
  git(['init', '-q', '--initial-branch=main', seed]);
  git(['-C', seed, 'config', 'user.name', 'First User Journey']);
  git(['-C', seed, 'config', 'user.email', EMAIL]);
  writeFileSync(join(seed, 'README.md'), '# First-user journey\n', 'utf8');
  writeFileSync(join(seed, BLOB), `${BLOB_MARKER}\n`, 'utf8');
  git(['-C', seed, 'add', 'README.md', BLOB]);
  git(['-C', seed, 'commit', '-q', '-m', 'first-user journey fixture']);

  const remote = `${BACKEND_URL}/git/${USERNAME}/${REPO}`;
  git(['-C', seed, 'push', '-q', remote, 'main'], {
    env: {
      GIT_CONFIG_COUNT: '1',
      GIT_CONFIG_KEY_0: 'http.extraHeader',
      GIT_CONFIG_VALUE_0: `Authorization: Bearer ${jwt}`,
    },
    quiet: true,
  });
}

console.log(`First-user journey against ${FRONTEND_URL}`);
console.log(
  `Timeouts: Chrome startup=${JOURNEY_STARTUP_TIMEOUT_MS} ms, `
    + `CDP command=${JOURNEY_CDP_COMMAND_TIMEOUT_MS} ms, `
    + `UI wait=${JOURNEY_UI_WAIT_TIMEOUT_MS} ms`,
);

let browser = null;
let tab = null;
try {
  browser = await launchChromeCdp({
    chromePath: CHROME,
    chromeArgs: ['--headless=new', '--disable-gpu', '--no-sandbox', '--disable-dev-shm-usage', 'about:blank'],
    cdpPort: process.env.CDP_PORT,
    profilePrefix: 'plombir-git-first-user-',
    startupTimeoutMs: JOURNEY_STARTUP_TIMEOUT_MS,
  });
  cdpRoot = browser.cdpRoot;
  tab = await openTab(`${FRONTEND_URL}/register`);

  await step('register the first account through the UI', async () => {
    await waitForPath(tab, '/register');
    await fill(tab, 'input[autocomplete="username"]', USERNAME);
    await fill(tab, 'input[autocomplete="email"]', EMAIL);
    await fill(tab, 'input[autocomplete="new-password"]', PASSWORD);
    // The first account on an empty instance needs the one-time setup token
    // (security audit finding #13); the stand exports it from its data
    // directory, and the page shows the field once `GET /instance` answered.
    await waitFor(tab, 'the setup token field', `document.querySelector('input[name="setup_token"]') !== null`);
    await fill(tab, 'input[name="setup_token"]', SETUP_TOKEN);
    await click(tab, 'form button[type="submit"]');
    await waitForPath(tab, '/dashboard');
    await waitFor(tab, 'registered account profile', `(async () => {
      const response = await fetch('/api/v1/users/me');
      if (!response.ok) return false;
      const user = await response.json();
      return user.username === ${JSON.stringify(USERNAME)};
    })()`);
    allowLoggedOut401 = false;
  });

  await step('log in through the UI with a fresh session', async () => {
    allowLoggedOut401 = true;
    await tab.send('Network.clearBrowserCookies');
    await navigate(tab, '/login');
    await fill(tab, 'input[autocomplete="username"]', USERNAME);
    await fill(tab, 'input[autocomplete="current-password"]', PASSWORD);
    await click(tab, 'form button[type="submit"]');
    await waitForPath(tab, '/dashboard');
    await waitFor(tab, 'logged-in account profile', `(async () => {
      const response = await fetch('/api/v1/users/me');
      if (!response.ok) return false;
      const user = await response.json();
      return user.username === ${JSON.stringify(USERNAME)};
    })()`);
    allowLoggedOut401 = false;
  });

  let jwt = '';
  await step('create an empty repository through the UI', async () => {
    const cookies = await tab.send('Network.getCookies', { urls: [FRONTEND_URL] });
    jwt = cookies.cookies?.find((cookie) => cookie.name === 'plombir_git_token')?.value || '';
    if (!jwt) throw new Error('login set no plombir_git_token HttpOnly cookie');

    await click(tab, '.dashboard-header button.btn-primary');
    await fill(tab, '.create-form input[type="text"][required]', REPO);
    await setChecked(tab, '.create-form input[type="checkbox"]', false, 1);
    await click(tab, '.create-form form button[type="submit"]');
    await waitForPath(tab, `/${USERNAME}/${REPO}`);
    await waitFor(tab, 'empty repository guidance', 'Boolean(document.querySelector(".empty-repo"))');
  });

  await step('push a commit through git smart HTTP', async () => {
    pushFixture(jwt);
  });

  await step('open the pushed file through the blob UI', async () => {
    await navigate(tab, `/${USERNAME}/${REPO}`);
    await waitFor(tab, `${BLOB} in the repository tree`, `Array.from(document.querySelectorAll('a.file-entry')).some((a) => a.textContent.includes(${JSON.stringify(BLOB)}))`);
    const clicked = await evaluate(tab, `(() => {
      const link = Array.from(document.querySelectorAll('a.file-entry')).find((a) => a.textContent.includes(${JSON.stringify(BLOB)}));
      if (!link) return false;
      link.click();
      return true;
    })()`);
    if (!clicked) throw new Error(`${BLOB} could not be opened from the repository tree`);
    await waitForPath(tab, `/${USERNAME}/${REPO}/blob/${BLOB}`);
    await waitFor(tab, 'pushed blob contents', `document.querySelector('.file-content')?.textContent.includes(${JSON.stringify(BLOB_MARKER)})`);
  });

  await step('create an issue through the UI', async () => {
    await click(tab, `a[href="/${USERNAME}/${REPO}/issues"]`);
    await waitForPath(tab, `/${USERNAME}/${REPO}/issues`);
    await click(tab, '.issues-toolbar button.btn-primary');
    await waitFor(tab, 'issue create form', 'Boolean(document.querySelector(".create-form form"))');
    await fill(tab, '.create-form input[type="text"][required]', ISSUE_TITLE);
    await fill(tab, '.create-form textarea', ISSUE_BODY);
    await click(tab, '.create-form form button[type="submit"]');
    await waitFor(tab, 'created issue in the list', `Array.from(document.querySelectorAll('.issue-title')).some((e) => e.textContent.trim() === ${JSON.stringify(ISSUE_TITLE)})`);
    await click(tab, 'a.issue-item');
    await waitForPath(tab, `/${USERNAME}/${REPO}/issues/1`);
    await waitFor(tab, 'issue detail title', `document.querySelector('.issue-title-row h1')?.textContent.trim() === ${JSON.stringify(ISSUE_TITLE)}`);
  });

  await step('close the issue through the UI', async () => {
    await click(tab, 'button.btn-close');
    await waitFor(tab, 'closed issue state', 'Boolean(document.querySelector(".state-badge.closed"))');
  });

  if (tab.problems.length > 0) {
    throw new Error(`browser recorded ${tab.problems.length} problem(s): ${tab.problems.join(' | ')}`);
  }
  console.log('✅ first-user browser → git → blob → issue journey passed');
} catch (error) {
  console.error(`❌ first-user journey failed: ${error.message}`);
  process.exitCode = 1;
} finally {
  try { await tab?.close(); } catch {}
  try { await browser?.cleanup(); } catch (error) {
    console.error(`❌ first-user journey could not clean Chrome: ${error.message}`);
    process.exitCode = 1;
  }
}
