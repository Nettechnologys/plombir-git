#!/usr/bin/env node

// A script that makes a scratch directory removes it however it ends — and so
// do the node processes it started (card_40a2c3c4254b).
//
// The stands removed their tree copies in `finally`, and Node runs no
// `finally` on `process.exit()` and nothing at all on a signal: every red run
// and every Ctrl-C left a full copy under $TMPDIR. `scripts/lib/scratch-dir.mjs`
// moves the removal to the `exit` event. This check holds both halves:
//
// * Behaviour, driven through real processes: a script that exits from inside
//   its `try` leaves nothing; one stopped by SIGINT exits 130 and leaves
//   neither its directory nor the child it started. Each probe is paired with
//   the same script written without the helper, which must leak — a probe that
//   cannot see a leak would pass vacuously.
// * Coverage: no script under `scripts/` calls `mkdtempSync` itself. A new
//   stand written the old way is refused here rather than discovered as a
//   pile of copies in /tmp.

import { readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

import { jsCodeView } from './lib/js-source.mjs';
import { scratchDir } from './lib/scratch-dir.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const helper = pathToFileURL(join(scriptsDir, 'lib', 'scratch-dir.mjs')).href;
const failures = [];

// ── Coverage ────────────────────────────────────────────────────────────────

function mjsFiles(dir, prefix) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    if (entry.isFile() && entry.name.endsWith('.mjs')) return [`${prefix}${entry.name}`];
    return [];
  });
}

const scripts = [...mjsFiles(scriptsDir, ''), ...mjsFiles(join(scriptsDir, 'lib'), 'lib/')];
for (const script of scripts) {
  if (script === 'lib/scratch-dir.mjs') continue;
  const code = jsCodeView(readFileSync(join(scriptsDir, script), 'utf8'));
  if (code.includes('mkdtempSync(')) {
    failures.push(
      `scripts/${script} calls mkdtempSync directly; use scratchDir from scripts/lib/scratch-dir.mjs so a red run or a signal cannot leave the directory behind`,
    );
  }
}

// ── Behaviour ───────────────────────────────────────────────────────────────

const root = scratchDir(join(tmpdir(), 'plombir-git-scratch-contract.'));

function probe(name, body) {
  const path = join(root, `${name}.mjs`);
  writeFileSync(path, body);
  return path;
}

function leftovers(prefix) {
  return readdirSync(root).filter((entry) => entry.startsWith(prefix));
}

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    if (error.code === 'ESRCH') return false;
    throw error;
  }
}

async function waitUntil(condition, ms) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    if (condition()) return true;
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
  return condition();
}

/** Run a probe; `onReady` gets the child pid it prints, once it prints it. */
function run(path, { onReady } = {}) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [path], {
      env: { ...process.env, PROBE_ROOT: root },
      stdio: ['ignore', 'pipe', 'pipe'],
    });
    let output = '';
    let grandchild = null;
    child.stdout.setEncoding('utf8');
    child.stderr.setEncoding('utf8');
    child.stderr.on('data', (chunk) => { output += chunk; });
    child.stdout.on('data', (chunk) => {
      output += chunk;
      const ready = /READY (\d+)/.exec(output);
      if (ready && grandchild === null) {
        grandchild = Number(ready[1]);
        onReady?.(child, grandchild);
      }
    });
    const timer = setTimeout(() => child.kill('SIGKILL'), 30_000);
    child.on('close', (status, signal) => {
      clearTimeout(timer);
      resolve({ status, signal, output, grandchild });
    });
  });
}

const exitInsideTry = (make, imports) => `${imports}
import { join } from 'node:path';
const dir = ${make}(join(process.env.PROBE_ROOT, 'exit-'));
try {
  process.exit(1);
} finally {
  rmSync(dir, { recursive: true, force: true });
}
`;

const helped = await run(probe('probe-exit-helped', exitInsideTry(
  'scratchDir',
  `import { rmSync } from 'node:fs';\nimport { scratchDir } from ${JSON.stringify(helper)};`,
)));
if (helped.status !== 1 || leftovers('exit-').length > 0) {
  failures.push(`a script exiting inside its try left ${JSON.stringify(leftovers('exit-'))} (status ${helped.status}): ${helped.output}`);
}
const bare = await run(probe('probe-exit-bare', exitInsideTry(
  'mkdtempSync',
  `import { mkdtempSync, rmSync } from 'node:fs';`,
)));
if (bare.status !== 1 || leftovers('exit-').length !== 1) {
  failures.push(`the exit probe cannot see a leak: the unhelped script left ${JSON.stringify(leftovers('exit-'))}`);
}

const interrupted = (make, imports, adopt) => `${imports}
import { spawn } from 'node:child_process';
import { join } from 'node:path';
${make}(join(process.env.PROBE_ROOT, 'signal-'));
const child = ${adopt}(spawn(process.execPath, ['-e', 'setInterval(() => {}, 1000)'], { stdio: 'ignore' }));
console.log('READY ' + child.pid);
setInterval(() => {}, 1000);
`;

const sigint = (child) => child.kill('SIGINT');
const stopped = await run(probe('probe-signal-helped', interrupted(
  'scratchDir',
  `import { adoptChild, scratchDir } from ${JSON.stringify(helper)};`,
  'adoptChild',
)), { onReady: sigint });
const reaped = stopped.grandchild !== null
  && await waitUntil(() => !alive(stopped.grandchild), 2_000);
if (stopped.grandchild !== null && !reaped) process.kill(stopped.grandchild, 'SIGKILL');
if (stopped.status !== 130 || leftovers('signal-').length > 0 || !reaped) {
  failures.push(
    `SIGINT must exit 130 and leave nothing: status ${stopped.status} signal ${stopped.signal}, `
      + `directories ${JSON.stringify(leftovers('signal-'))}, child ${reaped ? 'reaped' : 'still running'}: ${stopped.output}`,
  );
}
const unhelped = await run(probe('probe-signal-bare', interrupted(
  'mkdtempSync',
  `import { mkdtempSync } from 'node:fs';`,
  '',
)), { onReady: sigint });
const orphan = unhelped.grandchild !== null && alive(unhelped.grandchild);
if (orphan) process.kill(unhelped.grandchild, 'SIGKILL');
if (leftovers('signal-').length !== 1 || !orphan) {
  failures.push(
    `the signal probe cannot see a leak: the unhelped script left ${JSON.stringify(leftovers('signal-'))}, child ${orphan ? 'orphaned' : 'gone'}`,
  );
}

if (failures.length > 0) {
  for (const failure of failures) console.error(`❌ ${failure}`);
  process.exit(1);
}
console.log(`✅ scratch directories and their children are removed on every exit (${scripts.length} scripts checked)`);
