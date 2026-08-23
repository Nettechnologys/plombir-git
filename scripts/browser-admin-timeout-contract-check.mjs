#!/usr/bin/env node

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  browserAdminTimeouts,
  createEventWaiters,
  waitForValue,
} from './lib/browser-smoke-timing.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const smokeSource = readFileSync(join(scriptsDir, 'browser-admin-smoke.mjs'), 'utf8');
for (const required of [
  'browserAdminTimeouts()',
  "eventName: 'Page.loadEventFired'",
  'timeoutMs: PAGE_LOAD_TIMEOUT_MS',
  'timeoutMs: REDIRECT_TIMEOUT_MS',
  'tab.waitForLoad(description)',
  "tab.send('Page.navigate', { url })",
]) {
  assert.ok(smokeSource.includes(required), `browser-admin-smoke.mjs must retain ${required}`);
}
assert.doesNotMatch(smokeSource, /const WAIT_MS\s*=/, 'the smoke must not recreate one shared default budget');

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

const fired = createEventWaiters({ eventName: 'Page.loadEventFired', timeoutMs: 100 });
const firedPromise = fired.wait('route /admin');
fired.resolveAll();
await firedPromise;

const elapsed = createEventWaiters({ eventName: 'Page.loadEventFired', timeoutMs: 5 });
await assert.rejects(
  elapsed.wait('route /admin'),
  /Page\.loadEventFired timed out after 5 ms while waiting for route \/admin/,
);

const disconnected = createEventWaiters({ eventName: 'Page.loadEventFired', timeoutMs: 100 });
const disconnectedPromise = disconnected.wait('route /admin');
disconnected.rejectAll(new Error('Chrome tab WebSocket closed'));
await assert.rejects(
  disconnectedPromise,
  /Page\.loadEventFired aborted while waiting for route \/admin: Chrome tab WebSocket closed/,
);

console.log('✅ browser-admin timing: split budgets and fail-closed load/redirect waits');
