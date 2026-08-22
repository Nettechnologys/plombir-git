#!/usr/bin/env node

// Mutation stand for entity-column-consumer-contract-check.mjs.
//
// That check answers "is this column read back by anything", and it can lie in
// both directions. It can go quiet — stop parsing the entities, or stop
// loading the workspace it looks for readers in — and report a schema it never
// examined as clean. Or it can go blind the other way and accept a test, or the
// write itself, as the reader it was looking for.
//
// Each fixture below breaks exactly one thing and names the sentence the check
// must produce. A green stand is the only reason to believe a green check.

import { spawnSync } from 'node:child_process';
import {
  appendFileSync,
  cpSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'entity-column-consumer-contract-check.mjs');

// The fixture is a copy of the Rust workspace sources plus the check's own
// library. Both sides of the question live in `crates/`: the columns in
// `rg-db/src/entities`, the readers and writers everywhere else.
function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-entity-column-'));
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
    if (mutate) {
      mutate({
        fixture,
        entities: join(fixture, 'crates', 'rg-db', 'src', 'entities'),
        userOps: join(fixture, 'crates', 'rg-db', 'src', 'ops', 'user_ops.rs'),
        board: join(fixture, 'crates', 'rg-db', 'src', 'entities', 'board.rs'),
      });
    }
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

/** Add a column to `users` and a production writer for it, with no reader. */
function addWrittenColumn(paths, { field, reader = null }) {
  edit(
    join(paths.entities, 'user.rs'),
    '    pub backup_codes: Option<String>,',
    `    pub backup_codes: Option<String>,\n    pub ${field}: Option<String>,`,
  );
  edit(
    paths.userOps,
    '        backup_codes: Set(None),',
    `        backup_codes: Set(None),\n        ${field}: Set(None),`,
  );
  if (reader) appendFileSync(paths.userOps, reader);
}

// The copied tree is the shipped tree: if this one is not green, every red
// below is about the copy rather than about the mutation.
runFixture(
  'an unmutated copy of the tree passes',
  null,
  0,
  'entity column consumer contract ok',
);

// The defect the check exists for, in the shape the four drop migrations kept
// finding by hand: a column the server fills and nothing ever looks at.
runFixture(
  'a column with a writer and no reader is reported',
  (paths) => addWrittenColumn(paths, { field: 'legacy_recovery_hint' }),
  1,
  '`legacy_recovery_hint` is written and nothing reads it back',
);

// A reader inside a test module is exactly the state this check exists to name
// — "alive because its own test looks at it" — so `#[cfg(test)]` must not
// answer for a column.
runFixture(
  'a column read only from a test module is still reported',
  (paths) =>
    addWrittenColumn(paths, {
      field: 'legacy_recovery_hint',
      reader:
        '\n#[cfg(test)]\nmod legacy_recovery_hint_tests {\n' +
        '    fn read(user: &crate::entities::user::Model) -> bool {\n' +
        '        user.legacy_recovery_hint.is_some()\n    }\n}\n',
    }),
  1,
  '`legacy_recovery_hint` is written and nothing reads it back',
);

// And the other direction: one production reader is enough, so the check does
// not accuse a column that is genuinely used.
runFixture(
  'a column with one production reader passes',
  (paths) =>
    addWrittenColumn(paths, {
      field: 'legacy_recovery_hint',
      reader:
        '\npub fn legacy_recovery_hint(user: &crate::entities::user::Model) -> bool {\n' +
        '    user.legacy_recovery_hint.is_some()\n}\n',
    }),
  0,
  'entity column consumer contract ok',
);

// The write itself must not be mistaken for a read. `.field = Set(...)` is the
// assignment spelling of a writer, and a check that counted the `.field` in it
// would report every column in the schema as alive.
runFixture(
  'the assignment spelling of a write is not counted as a read',
  (paths) =>
    addWrittenColumn(paths, {
      field: 'legacy_recovery_hint',
      reader:
        '\npub fn clear(active: &mut crate::entities::user::ActiveModel) {\n' +
        '    active.legacy_recovery_hint = Set(None);\n}\n',
    }),
  1,
  '`legacy_recovery_hint` is written and nothing reads it back',
);

// The allowlist is a ratchet, not a parking lot: an entry that no longer
// describes the schema has to fail rather than sit there.
runFixture(
  'an allowlist entry naming a column that no longer exists is reported',
  (paths) => {
    edit(paths.board, '    pub created_by: Option<i64>,', '');
  },
  1,
  'but no entity declares that column',
);

// The vacuous-green failure mode, which is what a passing run also looks like:
// a check that cannot find the entities must say so instead of reporting a
// schema it never read as clean.
runFixture(
  'a check that cannot read the entities fails instead of passing',
  ({ entities }) => {
    rmSync(entities, { recursive: true, force: true });
  },
  1,
  'every verdict below means nothing',
);

// The same failure mode on the other side: without the workspace to look in,
// every column would read as unwritten and the check would be silently green.
runFixture(
  'a check that cannot read the production sources fails instead of passing',
  ({ fixture, entities }) => {
    const kept = mkdtempSync(join(tmpdir(), 'forgekeep-entity-column-kept-'));
    cpSync(entities, join(kept, 'entities'), { recursive: true });
    rmSync(join(fixture, 'crates'), { recursive: true, force: true });
    mkdirSync(join(fixture, 'crates', 'rg-db', 'src'), { recursive: true });
    cpSync(join(kept, 'entities'), join(fixture, 'crates', 'rg-db', 'src', 'entities'), {
      recursive: true,
    });
    rmSync(kept, { recursive: true, force: true });
  },
  1,
  'no production Rust outside the entity modules',
);

console.log('entity column consumer regression stand ok');
