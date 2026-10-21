#!/usr/bin/env node

// Stand for the red-main guard of `.githooks/pre-push` (card_5b91d8c6f3c2).
//
// Drives the real hook and the real `scripts/red-main-push-guard.mjs` in a
// throwaway repository, with a fake `gh` that answers as GitHub would. PATH
// holds only the fixture's `bin/`, so the host's own `gh` — or its absence —
// cannot decide a verdict. Each case asserts both the exit status and what the
// pusher is told: a refusal that names no run, or a pass that never asked,
// would otherwise read green.

import assert from 'node:assert/strict';
import { readFileSync, symlinkSync, writeFileSync, mkdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { removeScratchDir, scratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ZERO = '0'.repeat(40);

function which(name) {
  const result = spawnSync('sh', ['-c', `command -v ${name}`], { encoding: 'utf8' });
  assert.equal(result.status, 0, `the stand needs ${name} on PATH`);
  return result.stdout.trim();
}
const tools = { git: which('git'), sh: which('sh'), node: process.execPath };

function git(dir, ...args) {
  const result = spawnSync(tools.git, args, { cwd: dir, encoding: 'utf8' });
  assert.equal(result.status, 0, `git ${args.join(' ')}: ${result.stderr}`);
  return result.stdout.trim();
}

// `gh` answering `run list` with `runs` and `run view` with `jobs` — or, given
// a string, failing with it. `null` leaves `gh` out of PATH altogether.
function fakeGh(answer) {
  if (answer === null) return null;
  if (typeof answer === 'string') return `#!/bin/sh\necho '${answer}' >&2\nexit 1\n`;
  const list = JSON.stringify(answer.runs);
  const view = JSON.stringify({ jobs: answer.jobs ?? [] });
  return `#!/bin/sh\necho "$*" >> "\${0%/*}/gh.calls"\ncase "$1 $2" in\n`
    + `  'run list') printf '%s' '${list}' ;;\n`
    + `  'run view') printf '%s' '${view}' ;;\n`
    + '  *) exit 2 ;;\nesac\n';
}

function push({ gh, remoteRef = 'refs/heads/main', override = false }) {
  const dir = scratchDir(join(tmpdir(), 'plombir-git-red-main-'));
  try {
    mkdirSync(join(dir, 'scripts'));
    mkdirSync(join(dir, '.githooks'));
    mkdirSync(join(dir, 'bin'));
    for (const file of ['.githooks/pre-push', 'scripts/red-main-push-guard.mjs']) {
      writeFileSync(join(dir, file), readFileSync(join(root, file)));
    }
    // Reached only without a receipt, which this fixture always has.
    writeFileSync(join(dir, 'scripts/verify-push-gates.sh'), 'echo VERIFIER RAN\nexit 9\n');
    for (const [name, target] of Object.entries(tools)) symlinkSync(target, join(dir, 'bin', name));
    const ghScript = fakeGh(gh);
    if (ghScript !== null) writeFileSync(join(dir, 'bin/gh'), ghScript, { mode: 0o755 });

    git(dir, 'init', '-q', '-b', 'main');
    git(dir, 'config', 'user.name', 'Plombir Git gate fixture');
    git(dir, 'config', 'user.email', 'fixture@example.invalid');
    git(dir, 'add', '.');
    git(dir, 'commit', '-q', '-m', 'fixture');
    const head = git(dir, 'rev-parse', 'HEAD');
    writeFileSync(join(dir, '.git/plombir-git-push-gates.receipt'), `${head}\n`);

    const env = { PATH: join(dir, 'bin'), HOME: dir };
    if (override) env.PUSH_OVER_RED_MAIN = '1';
    const result = spawnSync(tools.sh, ['.githooks/pre-push', 'origin', 'git@example.invalid:x.git'], {
      cwd: dir,
      env,
      input: `refs/heads/main ${head} ${remoteRef} ${ZERO}\n`,
      encoding: 'utf8',
    });
    let calls = '';
    try {
      calls = readFileSync(join(dir, 'bin/gh.calls'), 'utf8');
    } catch {
      // `gh` was never asked.
    }
    return { status: result.status, output: `${result.stdout}${result.stderr}`, calls };
  } finally {
    removeScratchDir(dir);
  }
}

const RED = {
  // The newest finished run was cancelled by a newer push; the one before it
  // is the one that says something about main.
  runs: [
    { databaseId: 3, conclusion: 'cancelled', headSha: 'c'.repeat(40), url: 'https://example.invalid/runs/3', createdAt: '2026-10-21T06:00:00Z' },
    { databaseId: 2, conclusion: 'failure', headSha: 'b'.repeat(40), url: 'https://example.invalid/runs/2', createdAt: '2026-10-21T05:00:00Z' },
    { databaseId: 1, conclusion: 'success', headSha: 'a'.repeat(40), url: 'https://example.invalid/runs/1', createdAt: '2026-10-20T05:00:00Z' },
  ],
  jobs: [
    { name: 'MySQL smoke', conclusion: 'failure' },
    { name: 'Rust tests', conclusion: 'success' },
    { name: 'Dependency audit', conclusion: 'failure' },
  ],
};
const GREEN = { runs: [{ ...RED.runs[2] }] };

const refused = push({ gh: RED });
assert.equal(refused.status, 1, refused.output);
assert.match(refused.output, /regression\.yml run on main is failure/);
assert.match(refused.output, /https:\/\/example\.invalid\/runs\/2/);
assert.match(refused.output, /failed: MySQL smoke, Dependency audit/);
assert.match(refused.output, /PUSH_OVER_RED_MAIN=1 git push/);
assert.doesNotMatch(refused.output, /push gates already green/, 'a refused push must stop before the gates');

const overridden = push({ gh: RED, override: true });
assert.equal(overridden.status, 0, overridden.output);
assert.match(overridden.output, /failed: MySQL smoke, Dependency audit/, 'the override still shows the red');
assert.match(overridden.output, /pushing over it/);
assert.match(overridden.output, /push gates already green/);

const branch = push({ gh: RED, remoteRef: 'refs/heads/feature' });
assert.equal(branch.status, 0, branch.output);
assert.equal(branch.calls, '', 'a push to another branch must not ask about main');

const green = push({ gh: GREEN });
assert.equal(green.status, 0, green.output);
assert.match(green.calls, /run list --workflow regression\.yml --branch main --status completed/);
assert.doesNotMatch(green.output, /⚠️|❌/, green.output);

const noGh = push({ gh: null });
assert.equal(noGh.status, 0, noGh.output);
assert.match(noGh.output, /cannot see whether .* is red — `gh` is not installed/);

const offline = push({ gh: 'error connecting to api.github.com' });
assert.equal(offline.status, 0, offline.output);
assert.match(offline.output, /gh run list failed: error connecting to api\.github\.com/);

console.log('✅ a push to main over a red Regression run is refused, shown when overridden, and never blocked by a missing gh');
