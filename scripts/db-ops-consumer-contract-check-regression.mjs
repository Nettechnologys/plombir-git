#!/usr/bin/env node

// Mutation stand for db-ops-consumer-contract-check.mjs.
//
// That check answers "does any production code call this public function
// through the module that declares it", and it can lie in the direction that
// hides a defect: a live function reported as an orphan is loud, but a dead one
// reported as live is silent. The silent direction is what this stand guards.
//
// The fixture is a child module that its parent glob-re-exports. The function
// lives in `packages/npm.rs`, and `packages/mod.rs` says `pub(crate) use
// npm::*;`, so the spelling a caller writes is `packages::list_npm(`. A checker
// that files the function under `npm` cannot see that spelling and reports a
// live function as dead. A checker that accepts any `name(` as a call reports a
// dead one as live. Each fixture names the sentence the check must produce or
// must not produce.
//
// A green stand is the only reason to believe a green check.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const checkName = 'db-ops-consumer-contract-check.mjs';
const orphanSentence = '`list_npm` is public and no production code anywhere calls it';

// The fixture crate. `caller.rs` is the only file each fixture varies.
function writeFixture(fixture, callerBody) {
  const crate = join(fixture, 'crates', 'rg-x');
  mkdirSync(join(crate, 'src', 'packages'), { recursive: true });
  writeFileSync(join(crate, 'Cargo.toml'), '[package]\nname = "rg-x"\nversion = "0.0.0"\n');
  writeFileSync(join(crate, 'src', 'lib.rs'), 'mod caller;\nmod packages;\n');
  writeFileSync(join(crate, 'src', 'packages', 'mod.rs'), 'mod npm;\npub(crate) use npm::*;\n');
  writeFileSync(join(crate, 'src', 'packages', 'npm.rs'), 'pub fn list_npm() {}\n');
  writeFileSync(join(crate, 'src', 'caller.rs'), callerBody);
  mkdirSync(join(fixture, 'scripts'), { recursive: true });
  cpSync(join(root, 'scripts', checkName), join(fixture, 'scripts', checkName));
  cpSync(join(root, 'scripts', 'lib'), join(fixture, 'scripts', 'lib'), { recursive: true });
}

// The check also reports the allowlist and EXCLUDED_CRATES entries as stale in
// a fixture that has none of those crates, so the exit status is not the
// signal. Only the sentence about `list_npm` is.
function runFixture(name, callerBody, mustContain) {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-db-ops-consumer-'));
  try {
    writeFixture(fixture, callerBody);
    const result = spawnSync(process.execPath, [join(fixture, 'scripts', checkName)], {
      cwd: fixture,
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    const reported = output.includes(orphanSentence);
    if (reported !== mustContain) {
      throw new Error(
        `${name}: expected ${mustContain ? 'list_npm reported as an orphan' : 'list_npm NOT reported'}` +
          `\n${output}`,
      );
    }
    console.log(`✅ ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

// The live spelling through the re-exporting parent. Before the fix this
// function was reported dead, because the child was filed under `npm` and the
// `pub(crate) use` was not read as a re-export.
runFixture(
  'a function called as packages::name through a glob re-export is live',
  'fn f() {\n    crate::packages::list_npm();\n}\n',
  false,
);

// The other live spelling: the child's own module path, which is still valid
// from inside the parent's subtree.
runFixture(
  'a function called as packages::npm::name is live',
  'fn f() {\n    crate::packages::npm::list_npm();\n}\n',
  false,
);

// The bite: no caller anywhere. The gate must still say so, otherwise the two
// cases above prove nothing about the checker.
runFixture('a re-exported function nobody calls is still reported', 'fn f() {}\n', true);

console.log('✅ db-ops consumer stand: 3 fixtures');
