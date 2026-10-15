#!/usr/bin/env node

// Exercise the shared Chrome launcher against a cheap stand-in. Parallel
// default launches must consume the endpoint Chrome publishes inside each
// private profile; a fixed port or a guessed endpoint makes one launch fail or
// connects a run to the other fixture. A separate run covers the operator's
// explicit diagnostic override.

import { spawn } from 'node:child_process';
import { createServer } from 'node:http';
import { existsSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { launchChromeCdp } from './lib/chrome-cdp.mjs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const fixtureExecutable = fileURLToPath(import.meta.url);

function flagValue(name) {
  const prefix = `${name}=`;
  return process.argv.find((arg) => arg.startsWith(prefix))?.slice(prefix.length);
}

// A browser that cannot start: says why on stderr, then hangs without
// publishing an endpoint or exits. 64 KiB of noise goes first, so the report
// has to keep the tail, not the head. On the exit path the reason comes from a
// helper that outlives the main process, the way Chrome's crashpad and zygote
// do — it lands after `exit`, and a report written at `exit` would miss it.
function runFailingChromeFixture(reason) {
  process.stderr.write('chrome noise\n'.repeat(5_000), () => {
    if (process.env.PLOMBIR_GIT_CDP_FIXTURE_FAIL_EXIT) {
      spawn(process.execPath, ['-e', `setTimeout(() => process.stderr.write(${JSON.stringify(`${reason}\n`)}), 300)`], {
        stdio: ['ignore', 'ignore', 'inherit'],
      });
      process.exit(1);
    }
    process.stderr.write(`${reason}\n`, () => setInterval(() => {}, 60_000));
  });
}

async function stderrReachesTheFailure() {
  for (const exits of [false, true]) {
    const reason = `FATAL:zygote_host_impl_linux.cc no usable sandbox ${exits ? 'exit' : 'hang'}-${process.pid}`;
    process.env.PLOMBIR_GIT_CDP_FIXTURE_FAIL = reason;
    if (exits) process.env.PLOMBIR_GIT_CDP_FIXTURE_FAIL_EXIT = '1';
    let failure = null;
    try {
      const launched = await launchChromeCdp({ chromePath: fixtureExecutable, startupTimeoutMs: 1_500 });
      await launched.cleanup();
    } catch (error) {
      failure = error;
    } finally {
      delete process.env.PLOMBIR_GIT_CDP_FIXTURE_FAIL;
      delete process.env.PLOMBIR_GIT_CDP_FIXTURE_FAIL_EXIT;
    }
    const expected = exits ? /ended before its debugger was ready \(exit 1\)/ : /did not publish .*DevToolsActivePort/;
    if (!failure || !expected.test(failure.message)) {
      throw new Error(`a Chrome that ${exits ? 'exits' : 'hangs'} at startup was not reported as such: ${failure?.message ?? 'it launched'}`);
    }
    if (!failure.message.includes(reason)) {
      throw new Error(`the startup failure does not carry Chrome's stderr:\n${failure.message}`);
    }
    if (failure.message.length > 8_192) {
      throw new Error(`the startup failure carries ${failure.message.length} characters of stderr, not its tail`);
    }
  }
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
  const resultFile = process.env.PLOMBIR_GIT_CDP_FIXTURE_RESULT;
  if (resultFile) {
    writeFileSync(resultFile, JSON.stringify({ marker, profileDir, requestedPort, port: address.port }));
  }
}

function runSmoke(fixtureRoot, scriptName, marker, cdpPort = null) {
  const resultFile = join(fixtureRoot, `${marker}.json`);
  const env = {
    ...process.env,
    CHROME: fixtureExecutable,
    PLOMBIR_GIT_CDP_FIXTURE_MARKER: marker,
    PLOMBIR_GIT_CDP_FIXTURE_RESULT: resultFile,
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

  const fixtureRoot = scratchDir(join(tmpdir(), 'plombir-git-browser-cdp-contract-'));
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

  await stderrReachesTheFailure();

  console.log('✅ browser smoke CDP: both entrypoints isolate parallel endpoints, preserve overrides, clean profiles, and report Chrome\'s stderr when it cannot start');
}

if (process.env.PLOMBIR_GIT_CDP_FIXTURE_FAIL) {
  runFailingChromeFixture(process.env.PLOMBIR_GIT_CDP_FIXTURE_FAIL);
} else if (process.env.PLOMBIR_GIT_CDP_FIXTURE_MARKER) {
  await runChromeFixture(process.env.PLOMBIR_GIT_CDP_FIXTURE_MARKER);
} else {
  await runContract();
}
