#!/usr/bin/env node

// Mutation stand for encrypted-column-registry-contract-check.mjs.
//
// That check is an absence check on both sides — "no unaccounted sealing site"
// and "no registry entry without a producer" — and an absence check has two
// ways to lie. It can stop reading its subject (a macro that changed shape, a
// workspace it cannot walk) and report a registry it never parsed as clean; or
// it can read the subject and accept the wrong evidence (a `#[cfg(test)]`
// fixture answering for a production column).
//
// Each fixture below breaks exactly one thing and names the sentence the check
// must produce. A green stand is the only reason to believe a green check.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'encrypted-column-registry-contract-check.mjs');

const REGISTRY = join('crates', 'rg-core', 'src', 'auth', 'encrypted_columns.rs');
const MIRROR = join('crates', 'rg-core', 'src', 'mirror', 'service.rs');

/** A copy of the Rust workspace plus the check's own library. */
function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-encrypted-columns-'));
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

// The copied tree is the shipped tree: if this one is not green, every red
// below is about the copy rather than about the mutation.
runFixture('an unmutated copy of the tree passes', null, 0, 'each sealed by a recorded producer');

// The defect this check exists for, in the shape it will actually arrive: a
// feature seals a new value in a file that has never sealed anything, and the
// registry line is the step nobody remembers.
runFixture(
  'a new sealing site in an unlisted file is reported',
  (fixture) =>
    edit(
      join(fixture, 'crates', 'rg-core', 'src', 'user', 'service.rs'),
      'use anyhow::{bail, Context, Result};',
      `use anyhow::{bail, Context, Result};

pub fn seal_recovery_hint(hint: &str, key: &[u8; 32]) -> anyhow::Result<String> {
    Ok(crate::auth::encryption::encrypt(hint, key)?)
}`,
    ),
  1,
  'crates/rg-core/src/user/service.rs produces ciphertext (1 site(s)) and is not in CIPHERTEXT_PRODUCERS',
);

// The subtler half, and the reason the inventory carries a count rather than a
// list of filenames: a file that already seals one column grows a second site
// for a new one, and a filename allowlist would wave it through.
runFixture(
  'a second sealing site inside an already-listed file is reported',
  (fixture) =>
    edit(
      join(fixture, MIRROR),
      'fn encrypt_password(',
      `fn seal_mirror_token(token: &str, key: &[u8; 32]) -> Result<String> {
    Ok(crate::auth::encryption::encrypt(token, key)?)
}

fn encrypt_password(`,
    ),
  1,
  'crates/rg-core/src/mirror/service.rs holds 2 sealing site(s), CIPHERTEXT_PRODUCERS says 1',
);

// The other direction, and the one that has actually happened: an entry stays
// in the registry after its column stops being written, so `rekey` rewrites a
// column for nobody. Restores the `oauth_accounts` shape card_51dd82b6dc82
// removed by hand.
runFixture(
  'a registry entry nothing seals is reported',
  (fixture) =>
    edit(
      join(fixture, REGISTRY),
      '            mirror, PasswordEncrypted, password_encrypted,',
      `            oauth_account, AccessToken, access_token,
                "oauth_accounts.access_token", optional;
            mirror, PasswordEncrypted, password_encrypted,`,
    ),
  1,
  '`oauth_accounts.access_token` is in `with_encrypted_columns!` and nothing recorded here seals it',
);

// A sealing site in a test fixture is not a production producer, and treating
// it as one would make the inventory unmaintainable — every new test that seals
// a value would demand an entry. `loadProductionRust` strips `#[cfg(test)]`;
// this proves it still does.
runFixture(
  'a sealing site inside a test module is not reported',
  (fixture) =>
    edit(
      join(fixture, 'crates', 'rg-core', 'src', 'user', 'service.rs'),
      'use anyhow::{bail, Context, Result};',
      `use anyhow::{bail, Context, Result};

#[cfg(test)]
mod sealing_fixture_tests {
    #[test]
    fn a_fixture_may_seal_a_value() {
        let key = crate::auth::encryption::derive_key("k");
        let _ = crate::auth::encryption::encrypt("fixture", &key).unwrap();
    }
}`,
    ),
  0,
  'each sealed by a recorded producer',
);

// Calling the sealer through an import spells it without the module name, which
// is how a real site becomes invisible to a name-based sweep. Named rather than
// silently missed.
runFixture(
  'importing the sealer instead of qualifying it is reported',
  (fixture) =>
    edit(
      join(fixture, MIRROR),
      'fn encrypt_password(',
      `use crate::auth::encryption::encrypt;

fn encrypt_password(`,
    ),
  1,
  'is imported into scope, which spells the sealing site without the module name',
);

// Reading nothing is not the same as finding nothing wrong. A registry the
// check can no longer parse must be red, or the day the macro changes shape is
// the day this gate silently stops existing.
runFixture(
  'a registry the check can no longer parse is reported',
  (fixture) => writeFileSync(join(fixture, REGISTRY), '// the macro moved somewhere else\n'),
  1,
  'The macro has changed shape, so this check is reading nothing',
);
