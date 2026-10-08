#!/usr/bin/env node

// Copied-tree proof that the root contract rejects both halves of the original
// defect: repository reads relative to npm's cwd and CLI outputs written there.

import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const copied = [
  'scripts',
  'crates',
  'web/src',
  'web/package.json',
  'docs/ui-access-sweep.json',
  'docs/ui-inventory.json',
  'docs/UI_INVENTORY.md',
];

function baseline(fixture) {
  for (const path of copied) {
    const target = join(fixture, path);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(join(root, path), target, { recursive: true });
  }
}

function patch(fixture, from, to) {
  const target = join(fixture, 'scripts/ui-inventory.mjs');
  const source = readFileSync(target, 'utf8');
  if (!source.includes(from)) throw new Error(`mutation cannot find ${JSON.stringify(from)}`);
  writeFileSync(target, source.replace(from, to));
}

function run(fixture) {
  const result = spawnSync(
    process.execPath,
    [join(fixture, 'scripts/ui-inventory-root-contract-check.mjs')],
    { cwd: fixture, encoding: 'utf8', timeout: 60_000 },
  );
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

const mutations = [
  {
    name: 'repository reads fall back to npm cwd',
    from: 'const repoPath = (file) => resolve(ROOT, file);',
    to: 'const repoPath = (file) => file;',
    expected: 'npm inventory generator from web cwd failed',
  },
  {
    name: 'CLI outputs fall back to npm cwd',
    from: 'writeFileSync(repoPath(jsonPath),',
    to: 'writeFileSync(jsonPath,',
    expected: 'npm inventory generator from web cwd failed',
  },
];

let fixture = scratchDir(join(tmpdir(), 'plombir-git-ui-inventory-root-regression.'));
try {
  baseline(fixture);
  const clean = run(fixture);
  if (clean.status !== 0) {
    console.error(`❌ UI inventory root baseline fixture is red, so mutations prove nothing:\n${clean.output}`);
    process.exit(1);
  }

  for (const mutation of mutations) {
    rmSync(fixture, { recursive: true, force: true });
    fixture = scratchDir(join(tmpdir(), 'plombir-git-ui-inventory-root-regression.'));
    baseline(fixture);
    patch(fixture, mutation.from, mutation.to);
    const result = run(fixture);
    if (result.status === 0 || !result.output.includes(mutation.expected)) {
      console.error(
        `❌ mutation did not go red: ${mutation.name}\nstatus=${result.status}\n${result.output}`,
      );
      process.exit(1);
    }
    console.log(`✅ mutation rejected: ${mutation.name}`);
  }
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
