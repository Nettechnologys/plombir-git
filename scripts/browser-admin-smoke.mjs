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
//   CDP_PORT=9223
//   WAIT_MS=3500

import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const FRONTEND_URL = (process.env.FRONTEND_URL || 'http://127.0.0.1:5173').replace(/\/$/, '');
const ADMIN_TOKEN = process.env.ADMIN_TOKEN || process.env.ADMIN_JWT || process.env.ACCESS_TOKEN || '';
const CDP_PORT = Number(process.env.CDP_PORT || 9223);
const WAIT_MS = Number(process.env.WAIT_MS || 3500);

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
const profileDir = mkdtempSync(join(tmpdir(), 'if-browser-smoke-'));

const chrome = spawn(CHROME_PATH, [
  '--headless=new',
  '--disable-gpu',
  '--no-sandbox',
  '--disable-dev-shm-usage',
  `--remote-debugging-port=${CDP_PORT}`,
  `--user-data-dir=${profileDir}`,
  'about:blank',
], { stdio: 'ignore' });

const cdpRoot = `http://localhost:${CDP_PORT}`;

function cleanup() {
  try { chrome.kill('SIGKILL'); } catch {}
  try { rmSync(profileDir, { recursive: true, force: true }); } catch {}
}

async function waitDebugger() {
  for (let i = 0; i < 60; i++) {
    try {
      const res = await fetch(`${cdpRoot}/json/version`);
      if (res.ok) return;
    } catch {}
    await sleep(200);
  }
  throw new Error('chrome debugger did not become available');
}

function createSession(tabId, wsUrl) {
  const ws = new WebSocket(wsUrl);
  const pending = new Map();
  let msgId = 0;
  const errors = [];
  const loadWaiters = [];

  return new Promise((resolve, reject) => {
    const onMessage = (event) => {
      let payload;
      try { payload = JSON.parse(event.data); } catch { return; }

      if (payload.id && pending.has(payload.id)) {
        const r = pending.get(payload.id);
        pending.delete(payload.id);
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
        while (loadWaiters.length > 0) {
          const fn = loadWaiters.shift();
          fn();
        }
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

    ws.addEventListener('message', onMessage);
    ws.addEventListener('error', () => {
      while (loadWaiters.length > 0) loadWaiters.shift()();
    });

    const send = (method, params = {}) => new Promise((resolveSend, rejectSend) => {
      const id = ++msgId;
      const payload = { id, method, params };
      pending.set(id, { resolve: resolveSend, reject: rejectSend });
      ws.send(JSON.stringify(payload));
      setTimeout(() => {
        if (pending.has(id)) {
          pending.delete(id);
          rejectSend(new Error(`cdp timeout: ${method}`));
        }
      }, WAIT_MS);
    });

    const waitForLoad = async () => new Promise((r) => {
      const timer = setTimeout(() => {
        while (loadWaiters.length > 0) {
          loadWaiters.shift()();
        }
        clearTimeout(timer);
      }, WAIT_MS);
      loadWaiters.push(() => {
        clearTimeout(timer);
        r();
      });
    });

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

async function waitForPath(tab, predicate) {
  const deadline = Date.now() + WAIT_MS;
  let path = await currentPath(tab);
  while (Date.now() < deadline) {
    if (predicate(path)) return path;
    await sleep(100);
    path = await currentPath(tab);
  }
  return path;
}

async function checkAdminRoute(route, hasToken) {
  const routeName = hasToken ? 'authenticated' : 'unauthenticated';
  const tab = await openTab(`${FRONTEND_URL}/login`);

  try {
    // Ensure origin is loaded first so localStorage is writeable without
    // triggering unrelated logged-out dashboard API calls.
    await tab.send('Page.navigate', { url: `${FRONTEND_URL}/login` });
    await tab.waitForLoad();
    await sleep(300);

    if (hasToken) {
      await tab.send('Runtime.evaluate', {
        expression: `window.localStorage.setItem('ironforge_token', ${JSON.stringify(ADMIN_TOKEN)});`,
        awaitPromise: true,
      });
      await sleep(200);
    }

    await tab.send('Page.navigate', { url: `${FRONTEND_URL}${route}` });
    await tab.waitForLoad();
    const pathNormalized = await waitForPath(tab, (path) => {
      if (hasToken) return path === normalizePath(route);
      return path === '/login' || path.startsWith('/login');
    });

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
  } finally {
    await tab.close();
  }
}

console.log('Browser-admin smoke start');
console.log(`frontend: ${FRONTEND_URL}`);
console.log(`admin token: ${ADMIN_TOKEN ? 'provided' : 'not provided (skip positive path)'}`);

try {
  await waitDebugger();

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
    process.exit(1);
  }

  console.log('\n✅ browser-admin smoke passed');
  process.exit(0);
} catch (e) {
  console.log(`❌ browser-admin smoke failed to start: ${e.message}`);
  process.exit(1);
} finally {
  cleanup();
}
