#!/usr/bin/env node

const BACKEND_URL = process.env.BACKEND_URL || 'http://127.0.0.1:18080';
const OPENAPI_REQUIRE_AUTH = process.env.OPENAPI_REQUIRE_AUTH || '1';
const OPENAPI_SMOKE_TIMEOUT_MS = process.env.OPENAPI_SMOKE_TIMEOUT_MS || '20000';
const fs = require('node:fs');

console.log(`Starting codex hourly automation for ${BACKEND_URL}`);

const { spawn } = require('node:child_process');

function runCommand(cmd, args, env = {}) {
  return new Promise((resolve, reject) => {
    const p = spawn(cmd, args, {
      stdio: 'inherit',
      env: { ...process.env, ...env },
    });

    p.on('error', reject);
    p.on('exit', (code) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`${cmd} ${args.join(' ')} exited with ${code}`));
      }
    });
  });
}

(async () => {
  let server = null;
  let logStream = null;
  let shutdown = async () => {};

  try {
    await runCommand('cargo', [
      'build',
      '--release',
      '-p',
      'rg-cli',
    ], {});

    await runCommand('mkdir', ['-p', '/tmp/ironforge-codex-automation/repos']);

    const dbPath = '/tmp/ironforge-codex-smoke.db';
    ['', '-shm', '-wal'].forEach((suffix) => {
      try {
        fs.rmSync(`${dbPath}${suffix}`, { force: true });
      } catch (_) {
        // ignore
      }
    });

    const serverLog = '/tmp/ironforge-codex-server.log';

    server = spawn('./target/release/ironforge', [
      'serve',
      '--repo-root', '/tmp/ironforge-codex-automation/repos',
      '--http-addr', '127.0.0.1:18080',
      '--db-url', `sqlite:////tmp/ironforge-codex-smoke.db?mode=rwc`,
    ], {
      stdio: ['ignore', 'pipe', 'pipe'],
      env: process.env,
    });

    logStream = fs.createWriteStream(serverLog, { flags: 'a' });
    server.stdout.pipe(logStream);
    server.stderr.pipe(logStream);

    shutdown = async () => {
      if (!server) {
        return;
      }

      if (server && !server.killed) {
        server.kill();
      }
      server?.removeAllListeners();
      await new Promise((res) => server.on('exit', () => res()));
      if (logStream) {
        logStream.end();
      }
    };

    const fetch = global.fetch;
    if (!fetch) {
      throw new Error('node >= 18 required for fetch');
    }

    const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
    let ready = false;
    for (let i = 0; i < 40; i++) {
      try {
        const r = await fetch(`${BACKEND_URL}/health`);
        if (r.ok) {
          ready = true;
          break;
        }
      } catch (_) {
        // ignore
      }
      await wait(1000);
    }

    if (!ready) {
      throw new Error('backend failed to start in codex hourly automation');
    }

    await runCommand('node', [
      'scripts/openapi-interface-smoke.mjs',
    ], {
      BACKEND_URL,
      OPENAPI_REQUIRE_AUTH,
      OPENAPI_SMOKE_TIMEOUT_MS,
    });

    await shutdown();
    process.exit(0);
  } catch (error) {
    console.error(error && error.message ? error.message : error);
    await shutdown();
    process.exit(1);
  }
})();
