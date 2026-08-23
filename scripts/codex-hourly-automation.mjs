#!/usr/bin/env node

// Compatibility entry point for the hourly OpenAPI smoke. The stand itself
// belongs to scripts/ephemeral-stand.sh: keeping the boot, readiness polling,
// process-group teardown and temporary workspace in one loader is what makes a
// green smoke run repeatable and an interrupted one safe.

import { spawn } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
const standEntry = join(scriptsDir, 'ephemeral-stand.sh');
const signalExitCodes = new Map([
  ['SIGINT', 130],
  ['SIGTERM', 143],
]);

const child = spawn(
  standEntry,
  ['--build', '--', 'node', 'scripts/openapi-interface-smoke.mjs'],
  {
    cwd: root,
    stdio: 'inherit',
    // A direct signal to this Node wrapper must reach both the stand shell and
    // whichever smoke command it is currently waiting for. The stand gives its
    // backend/frontend children separate groups of their own and tears those
    // down through its shared TERM -> deadline -> KILL path.
    detached: process.platform !== 'win32',
    env: {
      ...process.env,
      OPENAPI_REQUIRE_AUTH: process.env.OPENAPI_REQUIRE_AUTH || '1',
      OPENAPI_SMOKE_TIMEOUT_MS: process.env.OPENAPI_SMOKE_TIMEOUT_MS || '20000',
    },
  },
);

let receivedSignal = null;
let relayError = null;

function childIsRunning() {
  return child.pid !== undefined && child.exitCode === null && child.signalCode === null;
}

function relay(signal) {
  if (receivedSignal !== null) return;
  receivedSignal = signal;
  if (!childIsRunning()) return;

  try {
    if (process.platform !== 'win32') process.kill(-child.pid, signal);
    else child.kill(signal);
  } catch (error) {
    // The child may have won the exit race between childIsRunning() and kill().
    if (error?.code !== 'ESRCH') relayError = error;
  }
}

const onSigint = () => relay('SIGINT');
const onSigterm = () => relay('SIGTERM');
process.on('SIGINT', onSigint);
process.on('SIGTERM', onSigterm);

const outcome = await new Promise((resolveOutcome) => {
  child.once('error', (error) => resolveOutcome({ error, code: null, signal: null }));
  child.once('exit', (code, signal) => resolveOutcome({ error: null, code, signal }));
});

process.removeListener('SIGINT', onSigint);
process.removeListener('SIGTERM', onSigterm);

const failure = outcome.error || relayError;
if (failure) {
  console.error(failure?.message || failure);
  process.exitCode = 1;
} else if (receivedSignal !== null) {
  process.exitCode = signalExitCodes.get(receivedSignal) ?? 1;
} else if (outcome.code !== 0) {
  const ending = outcome.signal ? `signal ${outcome.signal}` : `exit ${outcome.code}`;
  console.error(`${standEntry} ended with ${ending}`);
  process.exitCode = outcome.code ?? 1;
}
