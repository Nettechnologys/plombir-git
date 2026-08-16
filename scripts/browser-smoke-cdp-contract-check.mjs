#!/usr/bin/env node

// Exercise the shared Chrome launcher against a cheap stand-in. Parallel
// default launches must consume the endpoint Chrome publishes inside each
// private profile; a fixed port or a guessed endpoint makes one launch fail or
// connects a run to the other fixture. A separate run covers the operator's
// explicit diagnostic override.

import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const fixtureExecutable = fileURLToPath(import.meta.url);

function flagValue(name) {
  const prefix = `${name}=`;
  return process.argv.find((arg) => arg.startsWith(prefix))?.slice(prefix.length);
}

async function runChromeFixture(marker) {
  const requestedPort = Number(flagValue('--remote-debugging-port'));
  const profileDir = flagValue('--user-data-dir');
  if (!Number.isInteger(requestedPort) || !profileDir) {
    throw new Error('fixture did not receive Chrome CDP/profile arguments');
  }

  const server = createServer((request, response) => {
    if (request.url !== '/json/version') {
      response.writeHead(404).end();
      return;
    }
    const address = server.address();
    response.writeHead(200, { 'content-type': 'application/json' });
    response.end(JSON.stringify({
      fixture: marker,
      requestedPort,
      webSocketDebuggerUrl: `ws://127.0.0.1:${address.port}/devtools/browser/${marker}`,
    }));
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(requestedPort, '127.0.0.1', resolve);
  });

  const address = server.address();
  writeFileSync(
    join(profileDir, 'DevToolsActivePort'),
    `${address.port}\n/devtools/browser/${marker}\n`,
  );
  const resultFile = process.env.FORGEKEEP_CDP_FIXTURE_RESULT;
  if (resultFile) {
    writeFileSync(resultFile, JSON.stringify({ marker, profileDir, requestedPort, port: address.port }));
  }
}

function runSmoke(fixtureRoot, scriptName, marker, cdpPort = null) {
  const resultFile = join(fixtureRoot, `${marker}.json`);
  const env = {
    ...process.env,
    CHROME: fixtureExecutable,
    FORGEKEEP_CDP_FIXTURE_MARKER: marker,
    FORGEKEEP_CDP_FIXTURE_RESULT: resultFile,
  };
  if (cdpPort === null) delete env.CDP_PORT;
  else env.CDP_PORT = String(cdpPort);

  return new Promise((resolveRun) => {
    const child = spawn(process.execPath, [join(scriptsDir, scriptName), '--cdp-endpoint-only'], {
      env,
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    child.stdout.on('data', (chunk) => { output += chunk; });
    child.stderr.on('data', (chunk) => { output += chunk; });
    const timeout = setTimeout(() => child.kill('SIGKILL'), 5_000);
    child.on('error', (error) => {
      clearTimeout(timeout);
      resolveRun({ code: null, marker, output: `${output}${error.message}`, resultFile });
    });
    child.on('exit', (code, signal) => {
      clearTimeout(timeout);
      resolveRun({ code, signal, marker, output, resultFile });
    });
  });
}

function checkedRun(run) {
  if (run.code !== 0) {
    throw new Error(`${run.marker} failed: exit=${run.code} signal=${run.signal}\n${run.output}`);
  }
  const endpoint = run.output.match(/^cdp: (http:\/\/127\.0\.0\.1:[0-9]+)$/m)?.[1];
  if (!endpoint) throw new Error(`${run.marker} did not print its CDP endpoint:\n${run.output}`);
  const fixture = JSON.parse(readFileSync(run.resultFile, 'utf8'));
  if (endpoint !== `http://127.0.0.1:${fixture.port}` || fixture.marker !== run.marker) {
    throw new Error(`${run.marker} connected to another fixture: ${JSON.stringify({ endpoint, fixture })}`);
  }
  if (existsSync(fixture.profileDir)) {
    throw new Error(`${run.marker} left its Chrome profile behind: ${fixture.profileDir}`);
  }
  return { endpoint, fixture };
}

async function explicitOverrideRun(fixtureRoot, scriptName, marker, offset) {
  // Try the requested port directly: probing it first would recreate the
  // released-port TOCTOU this repository rejects. A rare collision advances
  // to another deterministic operator override.
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const port = 20_000 + ((process.pid * 31 + offset + attempt * 977) % 30_000);
    const run = await runSmoke(fixtureRoot, scriptName, `${marker}-${attempt}`, port);
    if (run.code === 0) {
      const checked = checkedRun(run);
      if (checked.fixture.requestedPort !== port || checked.fixture.port !== port) {
        throw new Error(`${marker} did not preserve explicit CDP_PORT=${port}`);
      }
      return checked;
    }
    if (!/ended before its debugger was ready/.test(run.output)) {
      throw new Error(`${marker} explicit override failed for a non-collision reason:\n${run.output}`);
    }
  }
  throw new Error(`${marker} could not find an unused explicit CDP override`);
}

async function runContract() {
  const smokeFiles = ['browser-admin-smoke.mjs', 'console-smoke.mjs'];
  for (const name of smokeFiles) {
    const source = readFileSync(join(scriptsDir, name), 'utf8');
    if (!source.includes("import { launchChromeCdp } from './lib/chrome-cdp.mjs';") ||
        !source.includes('await launchChromeCdp({')) {
      throw new Error(`${name} must launch Chrome through the owned-CDP helper`);
    }
    if (/9223|--remote-debugging-port|DevToolsActivePort/.test(source)) {
      throw new Error(`${name} must not duplicate a fixed port or the owned-endpoint protocol`);
    }
  }

  const helperSource = readFileSync(join(scriptsDir, 'lib', 'chrome-cdp.mjs'), 'utf8');
  if (/9223/.test(helperSource) || !helperSource.includes("join(profileDir, 'DevToolsActivePort')")) {
    throw new Error('chrome-cdp helper must discover Chrome-owned default endpoints from DevToolsActivePort');
  }

  const fixtureRoot = mkdtempSync(join(tmpdir(), 'forgekeep-browser-cdp-contract-'));
  try {
    const defaultRuns = await Promise.all(smokeFiles.flatMap((scriptName, scriptIndex) => [
      runSmoke(fixtureRoot, scriptName, `default-${scriptIndex}-A`),
      runSmoke(fixtureRoot, scriptName, `default-${scriptIndex}-B`),
    ]));
    const defaults = defaultRuns.map(checkedRun);
    if (new Set(defaults.map(({ endpoint }) => endpoint)).size !== defaults.length) {
      throw new Error(`parallel smoke runs reused a CDP endpoint: ${JSON.stringify(defaults)}`);
    }
    if (defaults.some(({ fixture }) => fixture.requestedPort !== 0)) {
      throw new Error(`default smoke did not ask Chrome for port 0: ${JSON.stringify(defaults)}`);
    }

    await Promise.all([
      explicitOverrideRun(fixtureRoot, smokeFiles[0], 'override-admin', 0),
      explicitOverrideRun(fixtureRoot, smokeFiles[1], 'override-console', 431),
    ]);
  } finally {
    rmSync(fixtureRoot, { recursive: true, force: true });
  }

  console.log('✅ browser smoke CDP: both entrypoints isolate parallel endpoints, preserve overrides, and clean profiles');
}

if (process.env.FORGEKEEP_CDP_FIXTURE_MARKER) {
  await runChromeFixture(process.env.FORGEKEEP_CDP_FIXTURE_MARKER);
} else {
  await runContract();
}
