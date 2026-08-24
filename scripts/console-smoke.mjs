#!/usr/bin/env node
// Headless console smoke test.
//
// Loads a list of routes in headless Chrome and reports any uncaught
// exceptions, console.error output, or failed network requests. Catches
// runtime-only bugs (e.g. Svelte runes leaking into a plain .ts module ->
// "$state is not defined") across EVERY page, not just whichever one you
// happened to open by hand.
//
// Usage:
//   node scripts/console-smoke.mjs                 # default base + route list
//   BASE=http://localhost:8080 node scripts/console-smoke.mjs /login /dashboard
//   node scripts/console-smoke.mjs --cdp-endpoint-only  # launcher diagnostic
//   CDP_COMMAND_TIMEOUT_MS=10000 PAGE_LOAD_TIMEOUT_MS=20000 node scripts/console-smoke.mjs
//   WAIT_MS=<positive integer>  # legacy fallback for PAGE_LOAD_TIMEOUT_MS
//
// Exit code 0 = all clean, 1 = errors found (CI-friendly).
//
// Requires Google Chrome installed; no npm dependencies (uses Node's built-in
// fetch + WebSocket, Node >= 21).

import { existsSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launchChromeCdp } from './lib/chrome-cdp.mjs';
import {
  consoleSmokeTimeouts,
  createPageReadinessWaiters,
} from './lib/browser-smoke-timing.mjs';

const BASE = process.env.BASE || 'http://localhost:8080';
const {
  cdpCommandMs: CDP_COMMAND_TIMEOUT_MS,
  pageLoadMs: PAGE_LOAD_TIMEOUT_MS,
} = consoleSmokeTimeouts();
const CDP_ENDPOINT_ONLY = process.argv.includes('--cdp-endpoint-only');
const SCRIPT_DIR = dirname(fileURLToPath(import.meta.url));
const ROOT = join(SCRIPT_DIR, '..');
const ROUTES_DIR = join(ROOT, 'web', 'src', 'routes');
// Expected noise while crawling logged-out: the browser logs a generic
// "Failed to load resource" console.error for every 401/403 fetch.
const IGNORE = [
  /Failed to load resource.*\b(401|403)\b/,
  /the server responded with a status of (401|403)/,
];

const DYNAMIC_SEGMENTS = {
  owner: 'testuser',
  repo: 'testrepo',
  name: 'testorg',
  id: '1',
  number: '1',
  sha: 'main',
  branch: 'main',
  path: 'README.md',
};

function walkRouteFiles(dir, files = []) {
  if (!existsSync(dir)) return files;
  for (const entry of readdirSync(dir).sort()) {
    const fullPath = join(dir, entry);
    const stat = statSync(fullPath);
    if (stat.isDirectory()) {
      walkRouteFiles(fullPath, files);
    } else if (entry === '+page.svelte') {
      files.push(fullPath);
    }
  }
  return files;
}

function segmentValue(segment) {
  const name = segment.replace(/^\[\[?/, '').replace(/\]?\]$/, '').replace(/^\.\.\./, '');
  return DYNAMIC_SEGMENTS[name] || 'demo';
}

function routeFromPageFile(file) {
  const rel = relative(ROUTES_DIR, dirname(file));
  if (!rel || rel === '.') return '/';
  const parts = rel.split(sep).filter(Boolean).map((part) => {
    if (part.startsWith('(') && part.endsWith(')')) return null;
    if (part.startsWith('[') && part.endsWith(']')) return segmentValue(part);
    return part;
  }).filter(Boolean);
  return `/${parts.join('/')}`;
}

function discoverRoutes() {
  return Array.from(new Set(walkRouteFiles(ROUTES_DIR).map(routeFromPageFile))).sort((a, b) => {
    if (a === '/') return -1;
    if (b === '/') return 1;
    return a.localeCompare(b);
  });
}

const cliRoutes = process.argv.slice(2).filter(
  (arg) => !['--list-routes', '--cdp-endpoint-only'].includes(arg),
);
const ROUTES = cliRoutes.length
  ? cliRoutes
  : discoverRoutes();

if (process.argv.includes('--list-routes')) {
  for (const route of ROUTES) console.log(route);
  process.exit(0);
}

const CHROME = process.env.CHROME ||
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';

let cdpRoot = '';

function createSession(tabId, wsUrl) {
  const ws = new WebSocket(wsUrl);
  const pending = new Map();
  const problems = [];
  const pageLoads = createPageReadinessWaiters({ timeoutMs: PAGE_LOAD_TIMEOUT_MS });
  let msgId = 0;
  let closing = false;

  return new Promise((resolve, reject) => {
    const failWaiters = (error) => {
      for (const waiter of pending.values()) {
        clearTimeout(waiter.timer);
        waiter.reject(error);
      }
      pending.clear();
      pageLoads.rejectAll(error);
    };

    const onMessage = (event) => {
      let payload;
      try {
        payload = JSON.parse(event.data);
      } catch (error) {
        const protocolError = new Error(`Chrome tab sent malformed CDP data: ${error.message}`);
        problems.push(`  ✖ CDP protocol: ${protocolError.message}`);
        failWaiters(protocolError);
        return;
      }

      if (payload.id && pending.has(payload.id)) {
        const waiter = pending.get(payload.id);
        pending.delete(payload.id);
        clearTimeout(waiter.timer);
        if (payload.error) waiter.reject(new Error(payload.error.message || 'CDP error'));
        else waiter.resolve(payload.result || payload);
        return;
      }

      if (payload.method === 'Runtime.exceptionThrown') {
        const e = payload.params.exceptionDetails;
        problems.push('  ✖ EXCEPTION: ' + (e.exception?.description || e.text).split('\n')[0]);
      } else if (payload.method === 'Log.entryAdded' && payload.params.entry.level === 'error') {
        const txt = payload.params.entry.text;
        if (!IGNORE.some((re) => re.test(txt))) problems.push('  ✖ console.error: ' + txt);
      } else if (payload.method === 'Network.responseReceived') {
        const { url, status } = payload.params.response;
        // API 4xx responses are expected while crawling logged-out and with sample dynamic route params.
        const expectedApiClientError = status >= 400 && status < 500 && url.includes('/api/');
        if (status >= 400 && !expectedApiClientError && !IGNORE.some((re) => re.test(url)))
          problems.push(`  ✖ HTTP ${status}: ${url}`);
      } else if (payload.method === 'Page.lifecycleEvent') {
        pageLoads.observe(payload.params);
      }
    };

    const send = (method, params = {}) => new Promise((resolveSend, rejectSend) => {
      const id = ++msgId;
      const timer = setTimeout(() => {
        if (!pending.delete(id)) return;
        rejectSend(new Error(`CDP command timed out after ${CDP_COMMAND_TIMEOUT_MS} ms: ${method}`));
      }, CDP_COMMAND_TIMEOUT_MS);
      pending.set(id, { resolve: resolveSend, reject: rejectSend, timer });
      try {
        ws.send(JSON.stringify({ id, method, params }));
      } catch (error) {
        clearTimeout(timer);
        pending.delete(id);
        rejectSend(error);
      }
    });

    const safeClose = async () => {
      closing = true;
      try { await fetch(`${cdpRoot}/json/close/${tabId}`); } catch {}
      ws.close();
    };

    ws.addEventListener('message', onMessage);
    ws.addEventListener('error', () => {
      const error = new Error('Chrome tab WebSocket failed');
      failWaiters(error);
      if (!closing) reject(error);
    });
    ws.addEventListener('close', () => {
      const error = new Error('Chrome tab WebSocket closed');
      failWaiters(error);
      if (!closing) reject(error);
    });
    ws.addEventListener('open', async () => {
      try {
        await send('Page.enable');
        await send('Page.setLifecycleEventsEnabled', { enabled: true });
        await send('Runtime.enable');
        await send('Log.enable');
        await send('Network.enable');
        resolve({ send, waitForLoad: pageLoads.wait, problems, close: safeClose });
      } catch (error) {
        ws.close();
        reject(error);
      }
    });
  });
}

async function openTab(url) {
  const response = await fetch(
    `${cdpRoot}/json/new?${encodeURIComponent(url)}`,
    { method: 'PUT' },
  );
  const text = await response.text();
  let target;
  try {
    target = JSON.parse(text);
  } catch {
    throw new Error(`failed to open debug tab: Chrome returned ${response.status} ${text.slice(0, 120)}`);
  }
  if (!target?.webSocketDebuggerUrl || !target.id) {
    throw new Error(`failed to open debug tab for ${url}`);
  }
  const session = await createSession(target.id, target.webSocketDebuggerUrl);
  return { id: target.id, ...session };
}

async function checkRoute(path) {
  let tab = null;
  try {
    // Open a neutral target and attach observers before navigation. Creating the
    // target at the route URL races fast lifecycle events against WebSocket setup.
    tab = await openTab('about:blank');
    const loading = tab.waitForLoad(`route ${path}`);
    const navigation = tab.send('Page.navigate', { url: BASE + path }).then((result) => {
      loading.followNavigation(result);
    });
    await Promise.all([loading.promise, navigation]);
    return tab.problems;
  } finally {
    await tab?.close();
  }
}

let failed = 0;
let browser = null;
try {
  browser = await launchChromeCdp({
    chromePath: CHROME,
    chromeArgs: ['--headless=new', '--disable-gpu', '--no-sandbox', 'about:blank'],
    cdpPort: process.env.CDP_PORT,
    profilePrefix: 'cdp-smoke-',
    startupTimeoutMs: 10_000,
  });
  cdpRoot = browser.cdpRoot;
  console.log(`cdp: ${cdpRoot}`);
  if (CDP_ENDPOINT_ONLY) {
    console.log('✅ console-smoke Chrome CDP endpoint ready');
  } else {
    console.log(`Console smoke against ${BASE} (${ROUTES.length} routes)\n`);
    for (const r of ROUTES) {
      let problems;
      try {
        problems = await checkRoute(r);
      } catch (error) {
        problems = [`  ✖ ${error.message}`];
      }
      if (problems.length) { failed++; console.log(`✗ ${r}`); console.log(problems.join('\n')); }
      else console.log(`✓ ${r}`);
    }
    console.log(`\n${failed ? `❌ ${failed} route(s) with errors` : '✅ all routes clean'}`);
  }
} catch (error) {
  console.error(`❌ console smoke failed to start: ${error.message}`);
  failed += 1;
} finally {
  try {
    await browser?.cleanup();
  } catch (error) {
    console.error(`❌ console smoke could not remove its Chrome profile: ${error.message}`);
    failed += 1;
  }
}
if (failed) process.exitCode = 1;
