#!/usr/bin/env node

// Mutation stand for persisted-error-text-contract-check.mjs.
//
// That check is an absence check — "no failure-text column without a recorded
// decision" — and an absence check has two ways to lie. It can stop reading
// its subject and report a schema it never parsed as clean; or it can read the
// subject and accept the wrong evidence (a commented-out field, a test-only
// entity answering for production).
//
// Each fixture below breaks exactly one thing and names the sentence the check
// must produce. A green stand is the only reason to believe a green check.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'persisted-error-text-contract-check.mjs');

const ENTITIES = join('crates', 'rg-db', 'src', 'entities');

/** A copy of the Rust workspace plus the check's own library. */
function fixtureRoot() {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-persisted-error-text-'));
  mkdirSync(join(fixture, 'crates'), { recursive: true });
  cpSync(join(root, 'crates'), join(fixture, 'crates'), {
    recursive: true,
    filter: (source) => !source.includes(`${join('crates', 'target')}`),
  });
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
  return fixture;
}

function edit(file, before, after) {
  const source = readFileSync(file, 'utf8');
  if (!source.includes(before)) {
    throw new Error(`${file}: fixture anchor disappeared: ${JSON.stringify(before)}`);
  }
  writeFileSync(file, source.replace(before, after));
}

function runFixture(name, mutate, expectedStatus, expectedOutput) {
  const fixture = fixtureRoot();
  try {
    if (mutate) mutate(fixture);
    const result = spawnSync(process.execPath, [check], { cwd: fixture, encoding: 'utf8' });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    if (result.status !== expectedStatus || !output.includes(expectedOutput)) {
      throw new Error(
        `${name}: expected exit ${expectedStatus} and ${JSON.stringify(expectedOutput)}, got exit ` +
          `${result.status}\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

const MIRROR_FIELD = 'pub last_sync_error: Option<String>,';

// The copied tree is the shipped tree: if this one is not green, every red
// below is about the copy rather than about the mutation.
runFixture(
  'an unmutated copy of the tree passes',
  null,
  0,
  'every failure-text column found by name is accounted for',
);

// The case the check exists for: a new column that stores a reason, added
// without anybody writing down what it may say.
runFixture(
  'a new failure-text column without a decision is reported',
  (fixture) =>
    edit(
      join(fixture, ENTITIES, 'mirror.rs'),
      MIRROR_FIELD,
      `${MIRROR_FIELD}\n    pub last_lfs_failure: Option<String>,`,
    ),
  1,
  'Entity column `mirrors.last_lfs_failure` looks like it stores a failure',
);

// The registry cannot outlive its column: an entry naming nothing is how it
// stops being an inventory.
runFixture(
  'an entry whose column is gone is reported',
  (fixture) =>
    edit(
      join(fixture, ENTITIES, 'mirror.rs'),
      MIRROR_FIELD,
      'pub last_sync_note: Option<String>,',
    ),
  1,
  'PERSISTED_TEXT names `mirrors.last_sync_error`, which is not a text column',
);

// A field that is commented out is not in the schema, and must not demand an
// entry — the view the check reads drops comments.
runFixture(
  'a commented-out field is not a column',
  (fixture) =>
    edit(
      join(fixture, ENTITIES, 'mirror.rs'),
      MIRROR_FIELD,
      `${MIRROR_FIELD}\n    // pub last_lfs_failure: Option<String>,`,
    ),
  0,
  'every failure-text column found by name is accounted for',
);

// A numeric column with a matching name stores no text and needs no decision.
runFixture(
  'a non-text column is not failure text',
  (fixture) =>
    edit(
      join(fixture, ENTITIES, 'mirror.rs'),
      MIRROR_FIELD,
      `${MIRROR_FIELD}\n    pub failure_count: i64,`,
    ),
  0,
  'every failure-text column found by name is accounted for',
);

// A schema the check can no longer read must not read as clean.
runFixture(
  'an unreadable schema is not a clean one',
  (fixture) => rmSync(join(fixture, ENTITIES), { recursive: true, force: true }),
  1,
  'The schema walk is reading nothing',
);
