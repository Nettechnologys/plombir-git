#!/usr/bin/env node

import { spawn } from 'node:child_process';
import { randomBytes } from 'node:crypto';
import { once } from 'node:events';
import {
  closeSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  rmSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const OPENAPI_REQUIRE_AUTH = process.env.OPENAPI_REQUIRE_AUTH || '1';
const OPENAPI_SMOKE_TIMEOUT_MS = process.env.OPENAPI_SMOKE_TIMEOUT_MS || '20000';

function runCommand(cmd, args, env = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(cmd, args, {
      stdio: 'inherit',
      env: { ...process.env, ...env },
    });

    child.on('error', reject);
    child.on('exit', (code, signal) => {
      if (code === 0) {
        resolve();
      } else {
        const ending = signal ? `signal ${signal}` : `exit ${code}`;
        reject(new Error(`${cmd} ${args.join(' ')} ended with ${ending}`));
      }
    });
  });
}

function wait(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function stopped(child) {
  return child.pid === undefined || child.exitCode !== null || child.signalCode !== null;
}

function serverLogTail(serverLog) {
  try {
    return `\n${readFileSync(serverLog, 'utf8').slice(-4000)}`;
  } catch {
    return '';
  }
}

async function waitForPublishedHttpUrl(server, addressFile, serverLog, spawnError) {
  const deadline = Date.now() + 40_000;
  while (Date.now() < deadline) {
    if (spawnError.current) {
      throw new Error(`failed to spawn ForgeKeep server: ${spawnError.current.message}`);
    }
    if (stopped(server)) {
      throw new Error(
        `ForgeKeep server exited before publishing its listen address${serverLogTail(serverLog)}`,
      );
    }
    if (existsSync(addressFile)) {
      const http = readFileSync(addressFile, 'utf8')
        .split('\n')
        .find((line) => line.startsWith('http='))
        ?.slice('http='.length)
        .trim();
      if (http) return `http://${http}`;
    }
    await wait(100);
  }
  throw new Error(
    `ForgeKeep server did not publish its listen address within 40s${serverLogTail(serverLog)}`,
  );
}

async function waitForHealth(server, backendUrl, serverLog, spawnError) {
  const deadline = Date.now() + 40_000;
  while (Date.now() < deadline) {
    if (spawnError.current) {
      throw new Error(`failed to spawn ForgeKeep server: ${spawnError.current.message}`);
    }
    if (stopped(server)) {
      throw new Error(`ForgeKeep server exited before becoming healthy${serverLogTail(serverLog)}`);
    }
    try {
      const response = await fetch(`${backendUrl}/health`);
      if (response.ok) return;
    } catch {
      // The listener is bound before the application is ready to answer.
    }
    await wait(100);
  }
  throw new Error(`ForgeKeep server did not become healthy within 40s${serverLogTail(serverLog)}`);
}

async function stopServer(server) {
  if (!server || stopped(server)) return;
  const exited = once(server, 'exit');
  server.kill();
  await exited;
}

let server = null;
let runRoot = null;
let failure = null;

try {
  if (typeof fetch !== 'function') {
    throw new Error('node >= 18 required for fetch');
  }

  await runCommand('cargo', [
    'build',
    '--release',
    '-p',
    'rg-cli',
    '-j',
    '6',
  ]);

  runRoot = mkdtempSync(join(tmpdir(), 'forgekeep-codex-automation-'));
  const repoRoot = join(runRoot, 'repos');
  const dbPath = join(runRoot, 'forgekeep.db');
  const serverLog = join(runRoot, 'server.log');
  const addressFile = join(runRoot, 'listen-addresses');
  const hostKey = join(runRoot, 'host-key');
  mkdirSync(repoRoot, { recursive: true });

  // This driver owns a throwaway database, so it must also own every secret
  // and durable-key path needed to open it. Supplying these through the child
  // environment keeps the random values out of the process list and prevents
  // an operator's ambient FORGEKEEP_* secrets from coupling two smoke runs.
  const serverEnv = {
    ...process.env,
    FORGEKEEP_JWT_SECRET: randomBytes(32).toString('base64'),
    FORGEKEEP_ENCRYPTION_KEY: randomBytes(32).toString('base64'),
  };

  const logFd = openSync(serverLog, 'a', 0o600);
  try {
    server = spawn('./target/release/forgekeep', [
      'serve',
      '--repo-root', repoRoot,
      '--http-addr', '127.0.0.1:0',
      '--ssh-addr', '127.0.0.1:0',
      '--listen-address-file', addressFile,
      '--host-key', hostKey,
      '--db-url', `sqlite://${dbPath}?mode=rwc`,
    ], {
      stdio: ['ignore', logFd, logFd],
      env: serverEnv,
    });
  } finally {
    closeSync(logFd);
  }

  const spawnError = { current: null };
  server.on('error', (error) => {
    spawnError.current = error;
  });

  const backendUrl = await waitForPublishedHttpUrl(server, addressFile, serverLog, spawnError);
  console.log(`Starting codex hourly automation for ${backendUrl}`);
  await waitForHealth(server, backendUrl, serverLog, spawnError);

  await runCommand('node', [
    'scripts/openapi-interface-smoke.mjs',
  ], {
    BACKEND_URL: backendUrl,
    OPENAPI_REQUIRE_AUTH,
    OPENAPI_SMOKE_TIMEOUT_MS,
  });
} catch (error) {
  failure = error;
} finally {
  try {
    await stopServer(server);
  } catch (error) {
    failure ||= error;
  }

  if (runRoot) {
    try {
      rmSync(runRoot, { recursive: true, force: true });
    } catch (error) {
      failure ||= error;
    }
  }
}

if (failure) {
  console.error(failure && failure.message ? failure.message : failure);
  process.exitCode = 1;
}
