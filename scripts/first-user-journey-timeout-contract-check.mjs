#!/usr/bin/env node

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  firstUserJourneyTimeouts,
  waitForValue,
  withCdpCommandTimeout,
} from './lib/browser-smoke-timing.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const journeySource = readFileSync(join(scriptsDir, 'first-user-journey-e2e.mjs'), 'utf8');
for (const required of [
  'firstUserJourneyTimeouts()',
  'startupTimeoutMs: JOURNEY_STARTUP_TIMEOUT_MS',
  'timeoutMs: JOURNEY_CDP_COMMAND_TIMEOUT_MS',
  'timeoutMs: JOURNEY_UI_WAIT_TIMEOUT_MS',
  'withCdpCommandTimeout({',
  'waitForValue({',
]) {
  assert.ok(journeySource.includes(required), `first-user-journey-e2e.mjs must retain ${required}`);
}
assert.doesNotMatch(
  journeySource,
  /const WAIT_MS\s*=/,
  'the journey must not recreate one shared startup/CDP/UI budget',
);

const defaults = firstUserJourneyTimeouts({});
assert.deepEqual(defaults, {
  startupMs: 20_000,
  cdpCommandMs: 10_000,
  uiWaitMs: 15_000,
});
assert.equal(new Set(Object.values(defaults)).size, 3, 'journey defaults must remain separate budgets');

assert.deepEqual(firstUserJourneyTimeouts({ JOURNEY_WAIT_MS: '1234' }), {
  startupMs: 1234,
  cdpCommandMs: 1234,
  uiWaitMs: 1234,
});
assert.deepEqual(firstUserJourneyTimeouts({
  JOURNEY_WAIT_MS: '1234',
  JOURNEY_STARTUP_TIMEOUT_MS: '2500',
  JOURNEY_CDP_COMMAND_TIMEOUT_MS: '1500',
  JOURNEY_UI_WAIT_TIMEOUT_MS: '3500',
}), {
  startupMs: 2500,
  cdpCommandMs: 1500,
  uiWaitMs: 3500,
});
assert.throws(
  () => firstUserJourneyTimeouts({ JOURNEY_CDP_COMMAND_TIMEOUT_MS: '0' }),
  /JOURNEY_CDP_COMMAND_TIMEOUT_MS must be a positive integer/,
);
assert.throws(
  () => firstUserJourneyTimeouts({ JOURNEY_WAIT_MS: '1.5' }),
  /JOURNEY_WAIT_MS must be a positive integer/,
);

const independent = firstUserJourneyTimeouts({
  JOURNEY_CDP_COMMAND_TIMEOUT_MS: '5',
  JOURNEY_UI_WAIT_TIMEOUT_MS: '300',
});
let removedTimedOutCommand = false;
await assert.rejects(
  withCdpCommandTimeout({
    method: 'Runtime.evaluate',
    timeoutMs: independent.cdpCommandMs,
    run: () => new Promise(() => {}),
    onTimeout: () => { removedTimedOutCommand = true; },
  }),
  /CDP command timed out after 5 ms: Runtime\.evaluate/,
);
assert.equal(removedTimedOutCommand, true, 'a timed-out CDP command must remove pending state');
assert.equal(independent.uiWaitMs, 300, 'the CDP command deadline must not consume the UI budget');

let nowMs = 0;
await assert.rejects(
  waitForValue({
    read: async () => '/register',
    accept: (path) => path === '/dashboard',
    description: 'path /dashboard',
    timeoutMs: independent.uiWaitMs,
    pollIntervalMs: 100,
    now: () => nowMs,
    sleep: async (ms) => { nowMs += ms; },
  }),
  /timed out after 300 ms waiting for path \/dashboard; last value="\/register"/,
);

console.log('✅ first-user journey timing: split startup/CDP/UI budgets with diagnostic deadlines');
