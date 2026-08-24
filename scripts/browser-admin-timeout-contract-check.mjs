#!/usr/bin/env node

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  browserAdminTimeouts,
  createPageReadinessWaiters,
  waitForValue,
} from './lib/browser-smoke-timing.mjs';
import { jsCodeView } from './lib/js-source.mjs';
import { tsFunctionBody } from './lib/ts-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const smokeSource = readFileSync(join(scriptsDir, 'browser-admin-smoke.mjs'), 'utf8');
const smokeCode = jsCodeView(smokeSource);
const createSessionBody = tsFunctionBody(smokeSource, 'createSession');
const navigateAndWaitBody = tsFunctionBody(smokeSource, 'navigateAndWait');
const checkAdminRouteBody = tsFunctionBody(smokeSource, 'checkAdminRoute');
assert.notEqual(createSessionBody, null, 'browser-admin-smoke.mjs must retain createSession()');
assert.notEqual(navigateAndWaitBody, null, 'browser-admin-smoke.mjs must retain navigateAndWait()');
assert.notEqual(checkAdminRouteBody, null, 'browser-admin-smoke.mjs must retain checkAdminRoute()');

assert.match(smokeCode, /browserAdminTimeouts\s*\(\s*\)/);
assert.match(createSessionBody, /createPageReadinessWaiters\s*\(\s*\{ timeoutMs: PAGE_LOAD_TIMEOUT_MS \}\s*\)/);
assert.match(createSessionBody, /pageLoads\.observe\(payload\.params\)/);
assert.match(
  createSessionBody,
  /await send\('Page\.setLifecycleEventsEnabled',\s*\{ enabled: true \}\)/,
);
assert.doesNotMatch(createSessionBody, /Page\.loadEventFired/);
assert.match(navigateAndWaitBody, /tab\.waitForLoad\(description\)/);
assert.match(navigateAndWaitBody, /tab\.send\('Page\.navigate',\s*\{ url \}\)/);
assert.match(navigateAndWaitBody, /loading\.followNavigation\(result\)/);
assert.match(navigateAndWaitBody, /await Promise\.all\(\[loading\.promise, navigation\]\)/);
assert.match(checkAdminRouteBody, /openTab\('about:blank'\)/);
assert.match(smokeCode, /timeoutMs: REDIRECT_TIMEOUT_MS/);
assert.doesNotMatch(smokeCode, /const WAIT_MS\s*=/, 'the smoke must not recreate one shared default budget');

const defaults = browserAdminTimeouts({});
assert.deepEqual(defaults, {
  cdpCommandMs: 10_000,
  pageLoadMs: 20_000,
  redirectMs: 8_000,
});
assert.equal(new Set(Object.values(defaults)).size, 3, 'browser-admin defaults must remain separate budgets');

assert.deepEqual(browserAdminTimeouts({ WAIT_MS: '1234' }), {
  cdpCommandMs: 1234,
  pageLoadMs: 1234,
  redirectMs: 1234,
});
assert.deepEqual(browserAdminTimeouts({
  WAIT_MS: '1234',
  CDP_COMMAND_TIMEOUT_MS: '1500',
  PAGE_LOAD_TIMEOUT_MS: '2500',
  REDIRECT_TIMEOUT_MS: '3500',
}), {
  cdpCommandMs: 1500,
  pageLoadMs: 2500,
  redirectMs: 3500,
});
assert.throws(
  () => browserAdminTimeouts({ PAGE_LOAD_TIMEOUT_MS: '0' }),
  /PAGE_LOAD_TIMEOUT_MS must be a positive integer/,
);

let nowMs = 0;
await assert.rejects(
  waitForValue({
    read: async () => '/dashboard',
    accept: (path) => path === '/login',
    description: 'unauthenticated redirect to /login',
    timeoutMs: 300,
    pollIntervalMs: 100,
    now: () => nowMs,
    sleep: async (ms) => { nowMs += ms; },
  }),
  /timed out after 300 ms waiting for unauthenticated redirect to \/login; last value="\/dashboard"/,
);

nowMs = 0;
const paths = ['/admin', '/admin', '/login'];
assert.equal(await waitForValue({
  read: async () => paths.shift(),
  accept: (path) => path === '/login',
  description: 'unauthenticated redirect to /login',
  timeoutMs: 300,
  pollIntervalMs: 100,
  now: () => nowMs,
  sleep: async (ms) => { nowMs += ms; },
}), '/login');

const correlated = createPageReadinessWaiters({ timeoutMs: 100 });
const currentRoute = correlated.wait('route /admin');
let currentSettled = false;
currentRoute.promise.then(
  () => { currentSettled = true; },
  () => { currentSettled = true; },
);
correlated.observe({ frameId: 'frame-1', loaderId: 'old-loader', name: 'load' });
currentRoute.followNavigation({ frameId: 'frame-1', loaderId: 'current-loader' });
await new Promise((resolve) => setImmediate(resolve));
assert.equal(currentSettled, false, 'a previous loader must not confirm the current navigation');
correlated.observe({ frameId: 'frame-1', loaderId: 'current-loader', name: 'load' });
await currentRoute.promise;

const early = createPageReadinessWaiters({ timeoutMs: 100 });
const earlyRoute = early.wait('route /admin/users');
early.observe({ frameId: 'frame-2', loaderId: 'loader-2', name: 'load' });
earlyRoute.followNavigation({ frameId: 'frame-2', loaderId: 'loader-2' });
await earlyRoute.promise;

const elapsed = createPageReadinessWaiters({ timeoutMs: 5 });
const elapsedRoute = elapsed.wait('route /admin');
elapsedRoute.followNavigation({ frameId: 'frame-3', loaderId: 'loader-3' });
elapsed.observe({ frameId: 'frame-3', loaderId: 'loader-3', name: 'DOMContentLoaded' });
await assert.rejects(
  elapsedRoute.promise,
  /page readiness timed out after 5 ms while waiting for route \/admin; last state="dom-content-loaded"/,
);

const disconnected = createPageReadinessWaiters({ timeoutMs: 100 });
const disconnectedRoute = disconnected.wait('route /admin');
disconnectedRoute.followNavigation({ frameId: 'frame-4', loaderId: 'loader-4' });
disconnected.observe({ frameId: 'frame-4', loaderId: 'loader-4', name: 'init' });
disconnected.rejectAll(new Error('Chrome tab WebSocket closed'));
await assert.rejects(
  disconnectedRoute.promise,
  /page readiness aborted while waiting for route \/admin; last state="navigation-started"; Chrome tab WebSocket closed/,
);

console.log('✅ browser-admin timing: split budgets and loader-correlated load/redirect waits');
