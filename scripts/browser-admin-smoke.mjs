#!/usr/bin/env node
// Browser smoke test for admin routes and auth-guard behavior.
//
// - Launches local Chrome with remote debugging enabled
// - Verifies admin pages redirect to /login when unauthenticated
// - Optionally verifies admin routes are reachable when ADMIN_TOKEN is provided
//
// Optional env:
//   BACKEND_URL=http://127.0.0.1:8080
//   FRONTEND_URL=http://127.0.0.1:5173
//   ADMIN_TOKEN=<admin_jwt>
//   CDP_PORT=<1-65535>  # diagnostic override; default is a Chrome-owned ephemeral port
//   CDP_COMMAND_TIMEOUT_MS=10000
//   PAGE_LOAD_TIMEOUT_MS=20000
//   REDIRECT_TIMEOUT_MS=8000
//   WAIT_MS=<positive integer>  # legacy fallback for all three budgets
//
// Launcher diagnostic (starts Chrome, prints its owned endpoint, then cleans up):
//   node scripts/browser-admin-smoke.mjs --cdp-endpoint-only

import { launchChromeCdp } from './lib/chrome-cdp.mjs';
import {
  browserAdminTimeouts,
  createEventWaiters,
  waitForValue,
} from './lib/browser-smoke-timing.mjs';

const FRONTEND_URL = (process.env.FRONTEND_URL || 'http://127.0.0.1:5173').replace(/\/$/, '');
const ADMIN_TOKEN = process.env.ADMIN_TOKEN || process.env.ADMIN_JWT || process.env.ACCESS_TOKEN || '';
const {
  cdpCommandMs: CDP_COMMAND_TIMEOUT_MS,
  pageLoadMs: PAGE_LOAD_TIMEOUT_MS,
  redirectMs: REDIRECT_TIMEOUT_MS,
} = browserAdminTimeouts();
const CDP_ENDPOINT_ONLY = process.argv.includes('--cdp-endpoint-only');

const ADMIN_ROUTES = ['/admin', '/admin/users', '/admin/orgs', '/admin/audit', '/admin/settings'];
const IGNORE_LOG = [
  /Failed to load resource.*\b(401|403)\b/,
  /the server responded with a status of (401|403)/,
  // The unauthenticated guard smoke can run with only the Vite dev server.
  // In that mode the app-level /health probe may produce Vite proxy 502 noise.
  /Failed to load resource.*\b502\b/,
  /the server responded with a status of 502 \(Bad Gateway\)/,
  /NotAllowedError: Failed to execute 'localStorage'|DOMException/,
  /net::ERR_FILE_NOT_FOUND/,
];

const checks = [];
let failed = 0;

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function normalizePath(raw = '') {
  const p = String(raw || '').trim();
  if (!p) return '/';
  return p.split('?')[0].replace(/\/+$/, '') || '/';
}

function shouldIgnoreLog(text = '') {
  return IGNORE_LOG.some((re) => re.test(text));
}

const CHROME_PATH = process.env.CHROME ||
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
let cdpRoot = '';

function createSession(tabId, wsUrl) {
  const ws = new WebSocket(wsUrl);
  const pending = new Map();
  let msgId = 0;
  const errors = [];
  const loadWaiters = createEventWaiters({
    eventName: 'Page.loadEventFired',
    timeoutMs: PAGE_LOAD_TIMEOUT_MS,
  });

  return new Promise((resolve, reject) => {
    const onMessage = (event) => {
      let payload;
      try { payload = JSON.parse(event.data); } catch { return; }

      if (payload.id && pending.has(payload.id)) {
        const r = pending.get(payload.id);
        pending.delete(payload.id);
        clearTimeout(r.timer);
        if (payload.error) r.reject(new Error(payload.error.message || 'CDP error'));
        else r.resolve(payload.result || payload);
        return;
      }

      if (payload.method === 'Runtime.exceptionThrown') {
        const msg = payload.params?.exceptionDetails?.exception?.description || payload.params?.exceptionDetails?.text || 'Runtime exception';
        if (!shouldIgnoreLog(msg)) {
          errors.push(`CDP exception: ${msg.split('\n')[0]}`);
        }
      }

      if (payload.method === 'Log.entryAdded') {
        const txt = payload.params?.entry?.text;
        if (payload.params?.entry?.level === 'error' && typeof txt === 'string' && !shouldIgnoreLog(txt)) {
          errors.push(`console.error: ${txt.split('\n')[0]}`);
        }
      }

      if (payload.method === 'Page.loadEventFired') {
        loadWaiters.resolveAll();
      }

      if (payload.method === 'Network.responseReceived') {
        const url = payload.params?.response?.url || '';
        const status = Number(payload.params?.response?.status || 0);
        if (status >= 400 && status < 600 && /\/api\//.test(url)) {
          if (![401, 403].includes(status)) {
            errors.push(`HTTP ${status} on ${url}`);
          }
        }
      }
    };

    const safeClose = async () => {
      try { await fetch(`${cdpRoot}/json/close/${tabId}`); } catch {}
      ws.close();
    };

    const failWaiters = (error) => {
      for (const waiter of pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(error);
      }
      pending.clear();
      loadWaiters.rejectAll(error);
    };

    ws.addEventListener('message', onMessage);
    ws.addEventListener('error', () => {
      const error = new Error('Chrome tab WebSocket failed');
      failWaiters(error);
      reject(error);
    });
    ws.addEventListener('close', () => {
      const error = new Error('Chrome tab WebSocket closed');
      failWaiters(error);
      reject(error);
    });

    const send = (method, params = {}) => new Promise((resolveSend, rejectSend) => {
      const id = ++msgId;
      const payload = { id, method, params };
      const timer = setTimeout(() => {
        if (pending.has(id)) {
          pending.delete(id);
          rejectSend(new Error(`CDP command timed out after ${CDP_COMMAND_TIMEOUT_MS} ms: ${method}`));
        }
      }, CDP_COMMAND_TIMEOUT_MS);
      pending.set(id, { resolve: resolveSend, reject: rejectSend, timer });
      try {
        ws.send(JSON.stringify(payload));
      } catch (error) {
        clearTimeout(timer);
        pending.delete(id);
        rejectSend(error);
      }
    });

    const waitForLoad = (description) => loadWaiters.wait(description);

    ws.addEventListener('open', async () => {
      try {
        await send('Page.enable');
        await send('Runtime.enable');
        await send('Log.enable');
        await send('Network.enable');
        resolve({ ws, send, waitForLoad, errors, close: safeClose });
      } catch (e) {
        ws.close();
        reject(e);
      }
    });
  });
}

async function openTab(url) {
  const res = await fetch(`${cdpRoot}/json/new?${encodeURIComponent(url)}`, { method: 'PUT' });
  const text = await res.text();
  let target;
  try {
    target = JSON.parse(text);
  } catch {
    throw new Error(`failed to open debug tab: Chrome returned ${res.status} ${text.slice(0, 120)}`);
  }
  if (!target || !target.webSocketDebuggerUrl || !target.id) {
    throw new Error(`failed to open debug tab for ${url}`);
  }
  const session = await createSession(target.id, target.webSocketDebuggerUrl);
  return { id: target.id, ...session };
}

async function currentPath(tab) {
  const loc = await tab.send('Runtime.evaluate', {
    expression: 'window.location.pathname',
    returnByValue: true,
  });
  return normalizePath(loc?.result?.value || '');
}

async function waitForPath(tab, predicate, description) {
  return waitForValue({
    read: () => currentPath(tab),
    accept: predicate,
    description,
    timeoutMs: REDIRECT_TIMEOUT_MS,
  });
}

async function navigateAndWait(tab, url, description) {
  // Register the event waiter before Page.navigate: a fast page can emit the
  // load event before the command response reaches us.
  await Promise.all([
    tab.waitForLoad(description),
    tab.send('Page.navigate', { url }),
  ]);
}

async function checkAdminRoute(route, hasToken) {
  const routeName = hasToken ? 'authenticated' : 'unauthenticated';
  let tab = null;

  try {
    tab = await openTab(`${FRONTEND_URL}/login`);
    // Ensure the origin is loaded first so the auth cookie is scoped to it
    // without triggering unrelated logged-out dashboard API calls.
    await navigateAndWait(tab, `${FRONTEND_URL}/login`, 'initial login page');
    await sleep(300);

    if (hasToken) {
      // Auth is cookie-based: the frontend no longer reads the token from
      // localStorage — it relies on the HttpOnly `forgekeep_token` cookie the
      // backend sets on login (crates/rg-http/src/api/auth.rs). Inject that
      // cookie via CDP so fetchUser() (GET /users/me) and the notification
      // WebSocket authenticate and the admin route becomes reachable.
      const { success } = await tab.send('Network.setCookie', {
        name: 'forgekeep_token',
        value: ADMIN_TOKEN,
        url: FRONTEND_URL,
        path: '/',
        httpOnly: true,
        sameSite: 'Strict',
      });
      if (!success) {
        checks.push(`❌ admin route [${routeName}] ${route}: failed to set auth cookie`);
        failed += 1;
      }
      await sleep(200);
    }

    await navigateAndWait(tab, `${FRONTEND_URL}${route}`, `route ${route}`);
    const pathNormalized = await waitForPath(tab, (path) => {
      if (hasToken) return path === normalizePath(route);
      return path === '/login' || path.startsWith('/login');
    }, hasToken ? `authenticated route ${route}` : `unauthenticated redirect from ${route} to /login`);

    if (hasToken) {
      if (pathNormalized !== normalizePath(route)) {
        checks.push(`❌ admin route [${routeName}] ${route}: ended at ${pathNormalized}`);
        failed += 1;
      } else {
        checks.push(`✅ admin route [${routeName}] ${route}: ${pathNormalized}`);
      }
    } else if (pathNormalized === '/login' || pathNormalized.startsWith('/login')) {
      checks.push(`✅ admin route [${routeName}] ${route}: redirected to ${pathNormalized}`);
    } else {
      checks.push(`❌ admin route [${routeName}] ${route}: not redirected (${pathNormalized})`);
      failed += 1;
    }

    if (tab.errors.length > 0) {
      checks.push(`❌ admin route [${routeName}] ${route}: ${tab.errors.slice(0, 3).join(' | ')}`);
      failed += 1;
    }
  } catch (error) {
    checks.push(`❌ admin route [${routeName}] ${route}: ${error.message}`);
    failed += 1;
  } finally {
    await tab?.close();
  }
}

console.log('Browser-admin smoke start');
console.log(`frontend: ${FRONTEND_URL}`);
console.log(`admin token: ${ADMIN_TOKEN ? 'provided' : 'not provided (skip positive path)'}`);
console.log(
  `timeouts: CDP command=${CDP_COMMAND_TIMEOUT_MS} ms, page load=${PAGE_LOAD_TIMEOUT_MS} ms, `
    + `redirect=${REDIRECT_TIMEOUT_MS} ms`,
);

let browser = null;
try {
  browser = await launchChromeCdp({
    chromePath: CHROME_PATH,
    chromeArgs: [
      '--headless=new',
      '--disable-gpu',
      '--no-sandbox',
      '--disable-dev-shm-usage',
      'about:blank',
    ],
    cdpPort: process.env.CDP_PORT,
    profilePrefix: 'if-browser-smoke-',
  });
  cdpRoot = browser.cdpRoot;
  console.log(`cdp: ${cdpRoot}`);

  if (!CDP_ENDPOINT_ONLY) {
    for (const route of ADMIN_ROUTES) {
      await checkAdminRoute(route, false);
    }

    if (ADMIN_TOKEN) {
      for (const route of ADMIN_ROUTES) {
        await checkAdminRoute(route, true);
      }
    }

    for (const line of checks) {
      console.log(line);
    }

    if (failed > 0) {
      console.log(`\n❌ ${failed} check(s) failed`);
      process.exitCode = 1;
    } else {
      console.log('\n✅ browser-admin smoke passed');
    }
  } else {
    console.log('✅ browser-admin Chrome CDP endpoint ready');
  }
} catch (e) {
  console.log(`❌ browser-admin smoke failed to start: ${e.message}`);
  process.exitCode = 1;
} finally {
  try {
    await browser?.cleanup();
  } catch (error) {
    console.error(`❌ browser-admin smoke could not remove its Chrome profile: ${error.message}`);
    process.exitCode = 1;
  }
}
