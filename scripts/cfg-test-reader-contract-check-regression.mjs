#!/usr/bin/env node

// Mutation stand for cfg-test-reader-contract-check.mjs.
//
// A green workspace proves only that no fourth reader of a test-gating
// `#[cfg(…)]` attribute exists today, and that the two canonical halves agree
// on today's table. It says nothing about whether the check would notice the
// next copy or the next drift — and a check that has stopped recognising either
// reports exactly the same clean corpus. That is the failure mode this whole
// family exists over: the class was closed by hand seven times, each round with
// a fresh manual sweep, because nothing objected to the next copy.
//
// Each case writes a small tree into a fixture, points the real check at it
// through `PLOMBIR_GIT_CFG_TEST_READER_ROOT`, and judges it by exit code AND by
// what the diagnostic names. The second half matters as much as the first: a
// ratchet that goes red at the wrong file sends the reader somewhere that is
// fine, and the two are indistinguishable from a non-zero exit.
//
// The green cases carry as much weight as the red ones here. This check accuses
// a *literal in a pattern position*, and the tree is full of fixture text
// spelling `#[cfg(test)]` for entirely honest reasons — every mutation stand in
// `scripts/` writes some. A ratchet that accuses those is one nobody can keep
// green, and a ratchet nobody can keep green is one somebody deletes.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const repoRoot = join(scriptsDir, '..');
const check = join(scriptsDir, 'cfg-test-reader-contract-check.mjs');

let failed = 0;

/**
 * The real parity table, copied into every fixture.
 *
 * Not a miniature of it: the check holds the table to a row floor, and a stand
 * that invented a three-row table would be exercising a shape the repository
 * never runs. Copying the real one also means a row added there is answered by
 * the JavaScript half in every case below, for free.
 */
const TABLE = readFileSync(join(repoRoot, 'tests/support/cfg-test-attribute-parity.txt'), 'utf8');

/**
 * A Rust file that reads the table and hands its rows to the canonical reader.
 *
 * Spelled the way `crates/rg-cli/src/cli.rs` spells it, because what the check
 * looks for is the wiring — an `include_str!` of the table, a call into the
 * reader, an assertion on the verdict — and a fixture that satisfies it by
 * accident would prove nothing about the file that has to satisfy it for real.
 */
const RUST_CONSUMER = `#[cfg(test)]
mod tests {
    #[test]
    fn the_shared_parity_table_reads_the_same_here() {
        const TABLE: &str = include_str!("../../tests/support/cfg-test-attribute-parity.txt");
        for row in TABLE.lines() {
            let answered = rust_source::production_rust_code_only(row);
            assert_eq!(answered.len(), row.len());
        }
    }
}
`;

/** The canonical readers, stubbed: the check exempts them by path, not by content. */
const RUST_CANONICAL = `pub(crate) fn is_test_only_cfg_attribute(attribute: &str) -> bool {
    attribute.trim() == "#[cfg(test)]"
}
`;
const JS_CANONICAL = `export function cfgTestItemRanges(source) {
  return source.includes('#[cfg(test)]') ? [[0, source.length]] : [];
}
`;

/** A fixture tree that the check passes over, before a case breaks one thing. */
function buildFixture() {
  const dir = scratchDir(join(tmpdir(), 'plombir-git-cfg-test-reader-'));
  for (const sub of ['crates/rg-x/src', 'crates/rg-x/tests/common', 'tests/support', 'scripts/lib']) {
    mkdirSync(join(dir, sub), { recursive: true });
  }
  writeFileSync(join(dir, 'tests/support/rust_source.rs'), RUST_CANONICAL);
  writeFileSync(join(dir, 'tests/support/cfg-test-attribute-parity.txt'), TABLE);
  writeFileSync(join(dir, 'scripts/lib/rust-consumer-contract.mjs'), JS_CANONICAL);
  writeFileSync(join(dir, 'crates/rg-x/src/lib.rs'), RUST_CONSUMER);
  return dir;
}

function run(dir) {
  const result = spawnSync(process.execPath, [check], {
    cwd: repoRoot,
    encoding: 'utf8',
    env: { ...process.env, PLOMBIR_GIT_CFG_TEST_READER_ROOT: dir },
  });
  return {
    red: result.status !== 0,
    output: `${result.stdout ?? ''}${result.stderr ?? ''}`,
  };
}

/**
 * Run one case against a fresh fixture.
 *
 * `mutate` receives the fixture root. `expect.red` is the verdict; `mentions`
 * are substrings the diagnostic must carry, and `silent` substrings it must
 * not — the half that keeps a red from being right by accident.
 */
function runCase(name, { mutate = () => {}, expect }) {
  const dir = buildFixture();
  try {
    mutate(dir);
    const { red, output } = run(dir);
    const problems = [];
    if (red !== expect.red) {
      problems.push(`expected ${expect.red ? 'RED' : 'GREEN'}, got ${red ? 'RED' : 'GREEN'}`);
    }
    for (const needle of expect.mentions ?? []) {
      if (!output.includes(needle)) problems.push(`diagnostic never mentions ${needle}`);
    }
    for (const needle of expect.silent ?? []) {
      if (output.includes(needle)) problems.push(`diagnostic mentions ${needle} and should not`);
    }
    if (problems.length === 0) {
      console.log(`✅ ${name}`);
    } else {
      failed += 1;
      console.error(`❌ ${name}`);
      for (const problem of problems) console.error(`   - ${problem}`);
      console.error(output.trimEnd().replace(/^/gm, '     '));
    }
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const COPY = join('crates', 'rg-x', 'tests', 'common', 'source_scan.rs');

/* ------------------------------------------------------------------ *
 * The corpus the check is meant to pass over
 * ------------------------------------------------------------------ */

runCase('a tree with two canonical readers and a wired table is green', {
  expect: { red: false },
});

runCase('a caller that delegates to the canonical reader is not a copy', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, COPY),
      `use crate::rust_source;

pub fn is_test_only_cfg_attribute(attribute: &str) -> bool {
    rust_source::is_test_only_cfg_attribute(attribute)
}
`,
    ),
  expect: { red: false },
});

runCase('fixture text spelling the attribute is data, not a reader', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, 'scripts', 'some-contract-check-regression.mjs'),
      `const SAMPLE = \`#[cfg(test)]
mod tests {
    fn scaffold() {}
}
\`;
export const cases = [SAMPLE];
`,
    ),
  expect: { red: false, silent: ['some-contract-check-regression.mjs'] },
});

runCase('a reader spelled inside a comment is not part of the program', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, COPY),
      `// The old shape was: line.trim() == "#[cfg(test)]"
pub fn nothing() {}
`,
    ),
  expect: { red: false, silent: ['source_scan.rs'] },
});

/* ------------------------------------------------------------------ *
 * A fourth copy of the reader, in each shape one has been written in
 * ------------------------------------------------------------------ */

runCase('a Rust copy comparing the line to the literal is named', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, COPY),
      `pub fn is_test_item(line: &str) -> bool {
    line.trim() == "#[cfg(test)]"
}
`,
    ),
  expect: { red: true, mentions: ['tests/common/source_scan.rs:2'] },
});

runCase('a Rust copy searching for the literal is named', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, COPY),
      `pub fn is_test_item(block: &str) -> bool {
    block.contains("#[cfg(test)]")
}
`,
    ),
  expect: { red: true, mentions: ['tests/common/source_scan.rs:2'] },
});

runCase('a Rust copy written as a regex is named', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, COPY),
      `pub fn is_test_item(line: &str) -> bool {
    Regex::new(r"^\\s*#\\[cfg\\(test\\)\\]").unwrap().is_match(line)
}
`,
    ),
  expect: { red: true, mentions: ['tests/common/source_scan.rs:2'] },
});

runCase('a Rust copy matching the bare `test` atom is named', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, COPY),
      `pub fn is_test_item(cfg_attribute: &str) -> bool {
    cfg_attribute.contains("test")
}
`,
    ),
  expect: { red: true, mentions: ['tests/common/source_scan.rs:2', 'bare `test` atom'] },
});

runCase('a JavaScript copy is named the same way', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, 'scripts', 'second-reader-contract-check.mjs'),
      `export function isTestItem(attribute) {
  return attribute.trim() === '#[cfg(test)]';
}
`,
    ),
  expect: { red: true, mentions: ['scripts/second-reader-contract-check.mjs:2'] },
});

/* ------------------------------------------------------------------ *
 * The two halves drifting apart on the shared table
 * ------------------------------------------------------------------ */

runCase('a row the JavaScript half answers the other way is named', {
  mutate: (dir) => {
    const table = join(dir, 'tests/support/cfg-test-attribute-parity.txt');
    // The verdict flips, the reader does not: the same disagreement a drifted
    // reader produces, arriving from the side the stand can reach.
    writeFileSync(
      table,
      readFileSync(table, 'utf8').replace(
        'production\n#[cfg(not(test))]',
        'test-only\n#[cfg(not(test))]',
      ),
    );
  },
  expect: { red: true, mentions: ['#[cfg(not(test))]', 'no longer agree'] },
});

runCase('a verdict the Rust half would reject too is named', {
  mutate: (dir) => {
    const table = join(dir, 'tests/support/cfg-test-attribute-parity.txt');
    writeFileSync(
      table,
      readFileSync(table, 'utf8').replace('test-only\n#[cfg(test)]', 'maybe\n#[cfg(test)]'),
    );
  },
  expect: { red: true, mentions: ['"maybe"'] },
});

runCase('a table shrunk below the floor is red rather than vacuously green', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, 'tests/support/cfg-test-attribute-parity.txt'),
      'test-only\n#[cfg(test)]\n\nproduction\n#[cfg(not(test))]\n',
    ),
  expect: { red: true, mentions: ['row(s), fewer than'] },
});

runCase('a missing table is red', {
  mutate: (dir) => rmSync(join(dir, 'tests/support/cfg-test-attribute-parity.txt')),
  expect: { red: true, mentions: ['is missing'] },
});

/* ------------------------------------------------------------------ *
 * The Rust half unwired from the table
 * ------------------------------------------------------------------ */

runCase('a table no Rust file reads is red', {
  mutate: (dir) => rmSync(join(dir, 'crates/rg-x/src/lib.rs')),
  expect: { red: true, mentions: ['no Rust file'] },
});

runCase('a Rust consumer that names the table without running it is red', {
  mutate: (dir) =>
    writeFileSync(
      join(dir, 'crates/rg-x/src/lib.rs'),
      `//! See tests/support/cfg-test-attribute-parity.txt for the shared rows.
pub const TABLE_PATH: &str = "tests/support/cfg-test-attribute-parity.txt";
`,
    ),
  expect: { red: true, mentions: ['on paper only', 'include_str!'] },
});

/* ------------------------------------------------------------------ *
 * The check losing its own footing
 * ------------------------------------------------------------------ */

runCase('a tree without the canonical Rust reader is red, not silently empty', {
  mutate: (dir) => rmSync(join(dir, 'tests/support/rust_source.rs')),
  expect: { red: true, mentions: ['tests/support/rust_source.rs'] },
});

runCase('a tree with no subject files at all is red', {
  mutate: (dir) => {
    rmSync(join(dir, 'crates'), { recursive: true, force: true });
    rmSync(join(dir, 'tests'), { recursive: true, force: true });
    rmSync(join(dir, 'scripts'), { recursive: true, force: true });
  },
  expect: { red: true, mentions: ['no corpus'] },
});

/* ------------------------------------------------------------------ *
 * The real tree, through the same fixture machinery
 * ------------------------------------------------------------------ */

// The fixtures above are small by design, and a check can be right on a small
// tree and blind on the repository's 900-odd files. This case runs the real
// corpus through the same override, so a green stand and a green check are the
// same statement about the same bytes.
runCase('the repository itself passes through the override', {
  mutate: (dir) => {
    rmSync(dir, { recursive: true, force: true });
    mkdirSync(dir, { recursive: true });
    for (const sub of ['crates', 'tests', 'scripts']) {
      cpSync(join(repoRoot, sub), join(dir, sub), { recursive: true });
    }
  },
  expect: { red: false },
});

if (failed > 0) {
  console.error(`\n❌ cfg-test reader stand: ${failed} case(s) failed.`);
  process.exit(1);
}
console.log('\n✅ cfg-test reader stand: every case held.');
