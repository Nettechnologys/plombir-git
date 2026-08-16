#!/usr/bin/env node

// Exercise the real operator-run driver twice against cheap stand-ins. The two
// runs deliberately overlap after run A has created its database and log: a
// fixed-path implementation lets run B overwrite A's files or lose the fixed
// port, while an isolated implementation gives both runs a truthful green.

import { spawn } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(scriptsDir, '..');
const automation = join(root, 'scripts', 'codex-hourly-automation.mjs');
const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-codex-hourly-contract-'));
const results = join(fixture, 'results');
const bin = join(fixture, 'bin');

function executable(path, source) {
  writeFileSync(path, source, { mode: 0o755 });
}

function json(path) {
  return JSON.parse(readFileSync(path, 'utf8'));
}

function runAutomation(runId) {
  return new Promise((resolveRun) => {
    const child = spawn(process.execPath, [automation], {
      cwd: fixture,
      env: {
        ...process.env,
        PATH: `${bin}:${process.env.PATH}`,
        FIXTURE_RESULTS: results,
        FIXTURE_RUN_ID: runId,
        OPENAPI_REQUIRE_AUTH: '0',
        OPENAPI_SMOKE_TIMEOUT_MS: '1000',
      },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    child.stdout.on('data', (chunk) => { output += chunk; });
    child.stderr.on('data', (chunk) => { output += chunk; });

    const timeout = setTimeout(() => {
      child.kill('SIGKILL');
      resolveRun({ runId, code: null, signal: 'TIMEOUT', output });
    }, 20_000);
    child.on('error', (error) => {
      clearTimeout(timeout);
      resolveRun({ runId, code: null, signal: null, output: `${output}${error.message}` });
    });
    child.on('exit', (code, signal) => {
      clearTimeout(timeout);
      resolveRun({ runId, code, signal, output });
    });
  });
}

mkdirSync(results, { recursive: true });
mkdirSync(bin, { recursive: true });
mkdirSync(join(fixture, 'target', 'release'), { recursive: true });
mkdirSync(join(fixture, 'scripts'), { recursive: true });

executable(join(bin, 'cargo'), `#!/usr/bin/env node
const fs = require('node:fs');
const path = require('node:path');
const results = process.env.FIXTURE_RESULTS;
const runId = process.env.FIXTURE_RUN_ID;
fs.writeFileSync(path.join(results, \`cargo-\${runId}.json\`), JSON.stringify(process.argv.slice(2)));
if (runId === 'B') {
  const deadline = Date.now() + 10000;
  while (!fs.existsSync(path.join(results, 'server-A.json')) && Date.now() < deadline) {
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 20);
  }
  if (!fs.existsSync(path.join(results, 'server-A.json'))) process.exit(2);
}
`);

executable(join(fixture, 'target', 'release', 'forgekeep'), `#!/usr/bin/env node
const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');

const args = process.argv.slice(2);
const value = (flag) => {
  const at = args.indexOf(flag);
  return at === -1 ? null : args[at + 1];
};
const results = process.env.FIXTURE_RESULTS;
const runId = process.env.FIXTURE_RUN_ID;
const repoRoot = value('--repo-root');
const dbUrl = value('--db-url');
const dbPath = new URL(dbUrl).pathname;
const addressFile = value('--listen-address-file');
const hostKey = value('--host-key');
const requested = value('--http-addr');
const colon = requested.lastIndexOf(':');
const host = requested.slice(0, colon);
const port = Number(requested.slice(colon + 1));

fs.mkdirSync(repoRoot, { recursive: true });
fs.writeFileSync(path.join(repoRoot, 'owner'), runId);
fs.writeFileSync(dbPath, runId);
fs.writeFileSync(\`\${dbPath}-wal\`, runId);
fs.writeFileSync(hostKey, runId);
const attempt = {
  runId,
  repoRoot,
  dbPath,
  addressFile,
  hostKey,
  requested,
  jwtSecret: process.env.FORGEKEEP_JWT_SECRET,
  encryptionKey: process.env.FORGEKEEP_ENCRYPTION_KEY,
};
fs.writeFileSync(path.join(results, \`attempt-\${runId}.json\`), JSON.stringify(attempt));
console.log(\`fixture server \${runId}\`);

const server = http.createServer((request, response) => {
  response.writeHead(request.url === '/health' ? 200 : 404);
  response.end();
});
server.on('error', (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
server.listen(port, host, () => {
  const actual = server.address();
  const backendUrl = \`http://\${actual.address}:\${actual.port}\`;
  fs.writeFileSync(
    path.join(results, \`server-\${runId}.json\`),
    JSON.stringify({ ...attempt, backendUrl }),
  );
  if (addressFile) {
    fs.writeFileSync(addressFile, \`http=\${actual.address}:\${actual.port}\\nssh=127.0.0.1:0\\n\`);
  }
});
process.on('SIGTERM', () => server.close(() => process.exit(0)));
`);

writeFileSync(join(fixture, 'scripts', 'openapi-interface-smoke.mjs'), `
import { existsSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

const results = process.env.FIXTURE_RESULTS;
const runId = process.env.FIXTURE_RUN_ID;
const other = runId === 'A' ? 'B' : 'A';
const deadline = Date.now() + 10000;
while (!existsSync(join(results, \`attempt-\${other}.json\`)) && Date.now() < deadline) {
  await new Promise((resolve) => setTimeout(resolve, 20));
}
if (!existsSync(join(results, \`attempt-\${other}.json\`))) {
  throw new Error(\`run \${runId} never overlapped run \${other}\`);
}

const attempt = JSON.parse(readFileSync(join(results, \`attempt-\${runId}.json\`), 'utf8'));
const expected = [
  [attempt.dbPath, runId],
  [\`\${attempt.dbPath}-wal\`, runId],
  [join(attempt.repoRoot, 'owner'), runId],
  [attempt.hostKey, runId],
];
for (const [path, owner] of expected) {
  const actual = readFileSync(path, 'utf8');
  if (actual !== owner) throw new Error(\`\${path} belongs to \${actual}, expected \${owner}\`);
}
const log = readFileSync(join(dirname(attempt.repoRoot), 'server.log'), 'utf8');
if (!log.includes(\`fixture server \${runId}\`) || log.includes(\`fixture server \${other}\`)) {
  throw new Error(\`run \${runId} did not receive an isolated server log\`);
}
const response = await fetch(\`\${process.env.BACKEND_URL}/health\`);
if (!response.ok) throw new Error(\`health failed through \${process.env.BACKEND_URL}\`);
writeFileSync(
  join(results, \`smoke-\${runId}.json\`),
  JSON.stringify({ backendUrl: process.env.BACKEND_URL }),
);
`);

try {
  const runs = await Promise.all([runAutomation('A'), runAutomation('B')]);
  const failed = runs.filter(({ code }) => code !== 0);
  if (failed.length > 0) {
    const detail = failed.map(({ runId, code, signal, output }) =>
      `run ${runId}: exit=${code} signal=${signal}\n${output}`).join('\n');
    throw new Error(`parallel hourly automation failed:\n${detail}`);
  }

  const attempts = ['A', 'B'].map((runId) => json(join(results, `attempt-${runId}.json`)));
  const servers = ['A', 'B'].map((runId) => json(join(results, `server-${runId}.json`)));
  const smokes = ['A', 'B'].map((runId) => json(join(results, `smoke-${runId}.json`)));
  const cargoCalls = ['A', 'B'].map((runId) => json(join(results, `cargo-${runId}.json`)));

  const runRoots = attempts.map(({ repoRoot }) => dirname(repoRoot));
  if (new Set(runRoots).size !== 2) throw new Error('parallel runs reused one temp root');
  if (new Set(attempts.map(({ jwtSecret }) => jwtSecret)).size !== 2 ||
      new Set(attempts.map(({ encryptionKey }) => encryptionKey)).size !== 2) {
    throw new Error('parallel runs reused one runtime secret');
  }
  if (new Set(servers.map(({ backendUrl }) => backendUrl)).size !== 2) {
    throw new Error('parallel runs reused one HTTP listen address');
  }

  for (let index = 0; index < attempts.length; index += 1) {
    const attempt = attempts[index];
    const runRoot = runRoots[index];
    if (dirname(attempt.dbPath) !== runRoot || dirname(attempt.addressFile) !== runRoot ||
        dirname(attempt.hostKey) !== runRoot) {
      throw new Error(`run ${attempt.runId} placed runtime state outside its temp root`);
    }
    if (!attempt.jwtSecret || !attempt.encryptionKey) {
      throw new Error(`run ${attempt.runId} did not provide isolated auth secrets`);
    }
    if (smokes[index].backendUrl !== servers[index].backendUrl) {
      throw new Error(`run ${attempt.runId} smoke did not receive the server's actual URL`);
    }
    if (existsSync(runRoot)) throw new Error(`run ${attempt.runId} left ${runRoot} behind`);

    const cargo = cargoCalls[index];
    if (!cargo.includes('build') || !cargo.includes('--release') ||
        cargo[cargo.indexOf('-j') + 1] !== '6') {
      throw new Error(`run ${attempt.runId} did not cap the release build at -j 6: ${cargo.join(' ')}`);
    }
  }

  console.log('✅ codex hourly automation: parallel runs own their temp state and bound addresses');
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
