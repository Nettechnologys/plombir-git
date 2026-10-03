#!/usr/bin/env node

// Drive the real hourly wrapper through the shared stand with cheap process
// fixtures. This proves the compatibility entry point still delegates the
// release build and OpenAPI smoke, and that a signal sent to the Node wrapper
// reaches the stand's bounded process-group teardown.

import { spawn } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
const automation = join(scriptsDir, 'codex-hourly-automation.mjs');
const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-codex-hourly-contract-'));
const results = join(fixture, 'results');
const bin = join(fixture, 'bin');
const standTmp = join(fixture, 'tmp');
const fakeServer = join(fixture, 'plombir-git-fixture');
const activeRuns = new Set();

function executable(path, source) {
  writeFileSync(path, `${source}\n`, { mode: 0o755 });
}

function sleep(ms) {
  return new Promise((resolveSleep) => setTimeout(resolveSleep, ms));
}

async function waitForFile(path, timeoutMs = 10_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (existsSync(path)) return;
    await sleep(25);
  }
  throw new Error(`timed out waiting for ${path}`);
}

function processExists(pid) {
  if (!Number.isInteger(pid) || pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error?.code === 'ESRCH') return false;
    throw error;
  }
}

async function requireProcessGone(path) {
  if (!existsSync(path)) throw new Error(`fixture never recorded a pid in ${path}`);
  const pid = Number(readFileSync(path, 'utf8').trim());
  const deadline = Date.now() + 2_000;
  while (processExists(pid) && Date.now() < deadline) await sleep(25);
  if (processExists(pid)) throw new Error(`process ${pid} from ${path} survived teardown`);
}

function standWorkspaces() {
  return readdirSync(standTmp).filter((name) => name.startsWith('plombir-git-stand.'));
}

function startAutomation(runId, stubborn) {
  const child = spawn(process.execPath, [automation], {
    cwd: root,
    env: {
      ...process.env,
      PATH: `${bin}:${process.env.PATH}`,
      TMPDIR: standTmp,
      PLOMBIR_GIT_BIN: fakeServer,
      FIXTURE_RESULTS: results,
      FIXTURE_RUN_ID: runId,
      FIXTURE_STUBBORN: stubborn ? '1' : '0',
      OPENAPI_REQUIRE_AUTH: '0',
      OPENAPI_SMOKE_TIMEOUT_MS: '1234',
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  activeRuns.add(child);

  let output = '';
  child.stdout.on('data', (chunk) => { output += chunk; });
  child.stderr.on('data', (chunk) => { output += chunk; });
  const done = new Promise((resolveRun) => {
    child.once('error', (error) => {
      activeRuns.delete(child);
      resolveRun({ code: null, signal: null, error, output: () => output });
    });
    child.once('exit', (code, signal) => {
      activeRuns.delete(child);
      resolveRun({ code, signal, error: null, output: () => output });
    });
  });
  return { child, done };
}

async function waitForRun(run, timeoutMs = 20_000) {
  let timer;
  try {
    return await Promise.race([
      run.done,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('hourly automation fixture timed out')), timeoutMs);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function requireSuccessful(result, label) {
  if (result.error || result.code !== 0) {
    const ending = result.error?.message || `exit=${result.code} signal=${result.signal}`;
    throw new Error(`${label} failed (${ending}):\n${result.output()}`);
  }
}

function forceKillRecordedProcesses() {
  if (!existsSync(results)) return;
  for (const name of readdirSync(results)) {
    if (!name.includes('-pid-')) continue;
    const pid = Number(readFileSync(join(results, name), 'utf8').trim());
    if (!processExists(pid)) continue;
    try { process.kill(pid, 'SIGKILL'); } catch {}
  }
}

mkdirSync(results, { recursive: true });
mkdirSync(bin, { recursive: true });
mkdirSync(standTmp, { recursive: true });

executable(join(bin, 'cargo'), [
  '#!/usr/bin/env bash',
  'set -eu',
  'printf \'%s\\n\' "$@" >"${FIXTURE_RESULTS}/cargo-${FIXTURE_RUN_ID}.txt"',
].join('\n'));

// `ephemeral-stand.sh` deliberately invokes `node` by name for its consumer.
// Shadow only that command: the wrapper and fake Plombir Git server themselves
// still run under process.execPath.
executable(join(bin, 'node'), [
  '#!/usr/bin/env bash',
  'set -eu',
  '[[ "$1" == "scripts/openapi-interface-smoke.mjs" ]] || { echo "unexpected node command: $*" >&2; exit 2; }',
  'printf \'%s\\n\' "${BACKEND_URL:-}" "${STAND_TOKEN:-}" "${OPENAPI_REQUIRE_AUTH:-}" "${OPENAPI_SMOKE_TIMEOUT_MS:-}" >"${FIXTURE_RESULTS}/smoke-${FIXTURE_RUN_ID}.txt"',
  'printf \'%s\\n\' "$$" >"${FIXTURE_RESULTS}/smoke-pid-${FIXTURE_RUN_ID}"',
  'printf \'%s\\n\' "${PPID}" >"${FIXTURE_RESULTS}/stand-pid-${FIXTURE_RUN_ID}"',
  'touch "${FIXTURE_RESULTS}/smoke-started-${FIXTURE_RUN_ID}"',
  'if [[ "${FIXTURE_STUBBORN}" == "1" ]]; then',
  "  trap 'exit 130' INT",
  "  trap 'exit 143' TERM",
  '  while true; do sleep 0.1; done',
  'fi',
].join('\n'));

executable(fakeServer, [
  `#!${process.execPath}`,
  "const { spawn } = require('node:child_process');",
  "const fs = require('node:fs');",
  "const http = require('node:http');",
  "const path = require('node:path');",
  "const args = process.argv.slice(2);",
  "const value = (flag) => { const at = args.indexOf(flag); return at === -1 ? null : args[at + 1]; };",
  "const results = process.env.FIXTURE_RESULTS;",
  "const runId = process.env.FIXTURE_RUN_ID;",
  "const stubborn = process.env.FIXTURE_STUBBORN === '1';",
  "fs.writeFileSync(path.join(results, `server-pid-${runId}`), String(process.pid));",
  "if (stubborn) {",
  "  const helperCode = \"process.on('SIGTERM', () => {}); process.on('SIGINT', () => {}); setInterval(() => {}, 1000);\";",
  "  const helper = spawn(process.execPath, ['-e', helperCode], { stdio: 'ignore' });",
  "  fs.writeFileSync(path.join(results, `helper-pid-${runId}`), String(helper.pid));",
  "}",
  "const requested = value('--http-addr');",
  "const colon = requested.lastIndexOf(':');",
  "const host = requested.slice(0, colon);",
  "const port = Number(requested.slice(colon + 1));",
  "const server = http.createServer((request, response) => {",
  "  request.resume();",
  "  if (request.url === '/health') { response.writeHead(200); response.end(); return; }",
  "  if (request.url === '/api/v1/users/register') {",
  "    response.writeHead(200, { 'content-type': 'application/json' });",
  "    response.end(JSON.stringify({ token: `token-${runId}` }));",
  "    return;",
  "  }",
  "  response.writeHead(404); response.end();",
  "});",
  "server.listen(port, host, () => {",
  "  const actual = server.address();",
  "  fs.writeFileSync(value('--listen-address-file'), `http=${actual.address}:${actual.port}\\nssh=127.0.0.1:0\\n`);",
  "});",
  "process.on('SIGTERM', () => {",
  "  if (stubborn) return;",
  "  server.close(() => process.exit(0));",
  "});",
  "process.on('SIGINT', () => { if (!stubborn) server.close(() => process.exit(0)); });",
].join('\n'));

try {
  const clean = startAutomation('clean', false);
  const cleanResult = await waitForRun(clean);
  requireSuccessful(cleanResult, 'clean delegated run');

  const cargoArgs = readFileSync(join(results, 'cargo-clean.txt'), 'utf8').trim().split('\n');
  const expectedCargo = ['build', '--release', '-p', 'rg-cli', '-j', '6'];
  if (cargoArgs.join('\n') !== expectedCargo.join('\n')) {
    throw new Error(`hourly wrapper delegated the wrong build: ${cargoArgs.join(' ')}`);
  }
  const [backendUrl, token, auth, smokeTimeout] = readFileSync(
    join(results, 'smoke-clean.txt'),
    'utf8',
  ).trim().split('\n');
  if (!backendUrl.startsWith('http://127.0.0.1:') || token !== 'token-clean' ||
      auth !== '0' || smokeTimeout !== '1234') {
    throw new Error(`hourly smoke received the wrong stand contract: ${backendUrl}, ${token}, ${auth}, ${smokeTimeout}`);
  }
  if (standWorkspaces().length !== 0) {
    throw new Error(`clean hourly run left stand workspaces: ${standWorkspaces().join(', ')}`);
  }
  await requireProcessGone(join(results, 'server-pid-clean'));

  const interrupted = startAutomation('interrupted', true);
  await waitForFile(join(results, 'smoke-started-interrupted'));
  const interruptedAt = Date.now();
  interrupted.child.kill('SIGINT');
  const interruptedResult = await waitForRun(interrupted);
  const teardownMs = Date.now() - interruptedAt;
  if (interruptedResult.error || interruptedResult.code !== 130 || interruptedResult.signal !== null) {
    throw new Error(
      `SIGINT run ended incorrectly: exit=${interruptedResult.code} signal=${interruptedResult.signal} ` +
        `error=${interruptedResult.error?.message || 'none'}\n${interruptedResult.output()}`,
    );
  }
  if (teardownMs < 4_500 || teardownMs > 12_000) {
    throw new Error(`stubborn server teardown took ${teardownMs}ms, expected the bounded ~5s TERM -> KILL path`);
  }
  if (standWorkspaces().length !== 0) {
    throw new Error(`SIGINT hourly run left stand workspaces: ${standWorkspaces().join(', ')}`);
  }
  await requireProcessGone(join(results, 'server-pid-interrupted'));
  await requireProcessGone(join(results, 'helper-pid-interrupted'));
  await requireProcessGone(join(results, 'smoke-pid-interrupted'));

  console.log(
    `✅ codex hourly automation: shared stand cleans SIGINT and kills a stubborn process group in ${teardownMs}ms`,
  );
} finally {
  for (const child of activeRuns) {
    try { child.kill('SIGKILL'); } catch {}
  }
  forceKillRecordedProcesses();
  rmSync(fixture, { recursive: true, force: true });
}
