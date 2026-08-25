#!/usr/bin/env node

import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'first-user-journey-contract-check.mjs');
const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-first-user-contract.'));

function copy(path) {
  const target = join(fixture, path);
  mkdirSync(dirname(target), { recursive: true });
  cpSync(join(root, path), target);
}

function mutate(path, from, to) {
  const target = join(fixture, path);
  const source = readFileSync(target, 'utf8');
  if (!source.includes(from)) throw new Error(`fixture mutation cannot find ${JSON.stringify(from)} in ${path}`);
  writeFileSync(target, source.replace(from, to));
}

function run(expected) {
  const result = spawnSync(process.execPath, [check], {
    cwd: root,
    env: { ...process.env, FORGEKEEP_FIRST_USER_JOURNEY_ROOT: fixture },
    encoding: 'utf8',
  });
  const output = `${result.stdout}${result.stderr}`;
  if (result.status === 0 || !output.includes(expected)) {
    throw new Error(`expected red contract containing ${JSON.stringify(expected)}; status=${result.status}\n${output}`);
  }
}

for (const path of [
  'scripts/first-user-journey-e2e.sh',
  'scripts/first-user-journey-e2e.mjs',
  'scripts/ephemeral-stand.sh',
  'web/package.json',
]) copy(path);

try {
  const cases = [
    ['scripts/first-user-journey-e2e.sh', 'JOURNEY_RUNS=${JOURNEY_RUNS:-2}', 'JOURNEY_RUNS=${JOURNEY_RUNS:-1}', 'defaults to two clean stands'],
    ['scripts/first-user-journey-e2e.sh', 'STAND_REBUILD_FRONTEND=1 ', '', 'may serve a stale web/build'],
    ['scripts/first-user-journey-e2e.sh', '    --no-founder \\\n', '', 'without pre-registering its user'],
    ['scripts/ephemeral-stand.sh', '--no-founder) REGISTER_FOUNDER=0', '--no-founder) REGISTER_FOUNDER=1', 'no longer accepts --no-founder'],
    ['scripts/first-user-journey-e2e.mjs', "git(['-C', seed, 'push'", "git(['-C', seed, 'fetch'", 'no longer performs a real git push'],
    ['scripts/first-user-journey-e2e.mjs', "await click(tab, 'button.btn-close');", "await click(tab, 'button.btn-primary');", 'no longer closes the created issue'],
  ];

  for (const [path, from, to, expected] of cases) {
    const original = readFileSync(join(fixture, path), 'utf8');
    mutate(path, from, to);
    run(expected);
    writeFileSync(join(fixture, path), original);
    console.log(`✅ mutation rejected: ${expected}`);
  }
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
