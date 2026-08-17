#!/usr/bin/env node

// Mutation stand for local-gate-coverage-contract-check.mjs. It runs the real
// checker against a copied workflow and hook so a line-oriented YAML reader
// cannot return unnoticed: invalid YAML must fail before any coverage count is
// reported, while the existing accounting diagnostics remain intact.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'local-gate-coverage-contract-check.mjs');

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-local-gate-coverage-'));
  mkdirSync(join(fixture, '.github', 'workflows'), { recursive: true });
  mkdirSync(join(fixture, '.githooks'), { recursive: true });
  cpSync(
    join(root, '.github', 'workflows', 'regression.yml'),
    join(fixture, '.github', 'workflows', 'regression.yml'),
  );
  cpSync(join(root, '.githooks', 'pre-push'), join(fixture, '.githooks', 'pre-push'));
  return fixture;
}

function replaceRequired(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

function runFixture(name, mutate, expectedStatus, expectedOutput, forbiddenOutput = '') {
  const fixture = fixtureRoot();
  try {
    if (mutate) mutate(join(fixture, '.github', 'workflows', 'regression.yml'));
    const result = spawnSync(process.execPath, [check], {
      cwd: fixture,
      env: { ...process.env, FORGEKEEP_LOCAL_GATE_COVERAGE_ROOT: fixture },
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (
      result.status !== expectedStatus
      || !output.includes(expectedOutput)
      || (forbiddenOutput && output.includes(forbiddenOutput))
    ) {
      throw new Error(
        `${name}: expected exit ${expectedStatus}, ${JSON.stringify(expectedOutput)}`
          + `${forbiddenOutput ? ` and no ${JSON.stringify(forbiddenOutput)}` : ''}, `
          + `got exit ${result.status}\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

runFixture(
  'the parsed graph preserves the clean-tree classification',
  null,
  0,
  '12 job(s) in regression.yml — 4 mirrored by run-local-gates.mjs, 2 by pre-push, '
    + '1 excluded by design, 5 running nowhere',
);

runFixture(
  'malformed regression.yml fails before coverage is reported',
  (workflow) => writeFileSync(workflow, `${readFileSync(workflow, 'utf8')}\nbroken: [\n`),
  1,
  '.github/workflows/regression.yml is not valid YAML',
  'local gate coverage:',
);

runFixture(
  'renaming a job preserves the stale mirror diagnostic',
  (workflow) => replaceRequired(workflow, '  contract-checks:\n', '  contract-checks-renamed:\n'),
  1,
  'run-local-gates.mjs mirrors `contract-checks`, which is not a job in regression.yml — renamed or removed.',
);
