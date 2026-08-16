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
//
// Exit code 0 = all clean, 1 = errors found (CI-friendly).
//
// Requires Google Chrome installed; no npm dependencies (uses Node's built-in
// fetch + WebSocket, Node >= 21).

import { existsSync, readdirSync, statSync } from 'node:fs';
import { dirname, join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launchChromeCdp } from './lib/chrome-cdp.mjs';

const BASE = process.env.BASE || 'http://localhost:8080';
const WAIT_MS = Number(process.env.WAIT_MS || 4000);
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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let cdpRoot = '';

async function checkRoute(path) {
  const target = await (await fetch(
    `${cdpRoot}/json/new?${encodeURIComponent(BASE + path)}`,
    { method: 'PUT' },
  )).json();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  const problems = [];
  let id = 0;
  const send = (method, params) => ws.send(JSON.stringify({ id: ++id, method, params }));
  await new Promise((res) => { ws.onopen = res; });
  send('Runtime.enable');
  send('Log.enable');
  send('Network.enable');
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.method === 'Runtime.exceptionThrown') {
      const e = d.params.exceptionDetails;
      problems.push('  ✖ EXCEPTION: ' + (e.exception?.description || e.text).split('\n')[0]);
    } else if (d.method === 'Log.entryAdded' && d.params.entry.level === 'error') {
      const txt = d.params.entry.text;
      if (!IGNORE.some((re) => re.test(txt))) problems.push('  ✖ console.error: ' + txt);
    } else if (d.method === 'Network.responseReceived') {
      const { url, status } = d.params.response;
      // API 4xx responses are expected while crawling logged-out and with sample dynamic route params.
      const expectedApiClientError = status >= 400 && status < 500 && url.includes('/api/');
      if (status >= 400 && !expectedApiClientError && !IGNORE.some((re) => re.test(url)))
        problems.push(`  ✖ HTTP ${status}: ${url}`);
    }
  };
  await sleep(WAIT_MS);
  // Close the tab so it doesn't keep running.
  try {
    await fetch(`${cdpRoot}/json/close/${target.id}`);
  } catch {
    // The tab or browser may already have closed after a page-level failure.
  }
  ws.close();
  return problems;
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
      const problems = await checkRoute(r);
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
