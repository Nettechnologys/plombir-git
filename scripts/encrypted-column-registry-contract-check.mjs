#!/usr/bin/env node

// Every production site that produces ciphertext is accounted for, and every
// column in the `with_encrypted_columns!` registry has a producer.
//
// The registry (`crates/rg-core/src/auth/encrypted_columns.rs`) is the single
// list two operations have to agree on: the startup preflight samples it to
// answer "does the configured key still open this database", and `rekey`
// rewrites it to move the database onto a new key. Its own doc-comment names
// the price of a miss — "a rotation that reports success while leaving one
// column sealed under a secret the operator has just been told to discard".
// That is silent data loss in the worst direction: the operator has already
// destroyed the old key by the time anything reads the column.
//
// Nothing enforced the list. It held because somebody remembered the line, and
// it has already drifted the other way: `card_51dd82b6dc82` removed two entries
// by hand after the columns stopped being read. "In the tree, not in the
// registry" is the same drift with worse consequences.
//
// WHAT THIS CHECK ASKS. Not "which column does this ciphertext reach" — that
// answer crosses functions and crates (a handler seals a value and hands it to
// an `rg_db::ops` helper, which is where the `Set(...)` lives), and a gate that
// needs data-flow analysis is a gate that gets deleted the first time it
// misreads a refactor. It asks the question that has an exact answer instead:
//
//   1. every production `encryption::encrypt(...)` site is classified below,
//      and each classified file still holds exactly as many as it says;
//   2. every column the registry names is claimed by one of those sites.
//
// The two together bite on the event the registry exists for. A new encrypted
// column cannot appear without a new sealing site — and a new sealing site,
// wherever it is, turns this check red until somebody writes down which column
// it feeds and puts that column in the registry.
//
// WHAT IS DELIBERATELY NOT CHECKED HERE. That the registry's Rust field names
// and their `optional` / `required` shapes match the entities: the macro
// expands into `entity::Column::TotpSecret` and `model.totp_secret` in two
// consumers, so a renamed field or a field that changed Option-ness fails to
// compile. rustc is the better gate and it is already running.

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { loadProductionRust } from './lib/rust-consumer-contract.mjs';
import { productionRustSource } from './lib/rust-source.mjs';

const root = process.cwd();
const cratesDir = path.join(root, 'crates');
const REGISTRY = path.join('crates', 'rg-core', 'src', 'auth', 'encrypted_columns.rs');
// A floor, not a count: the registry held seven entries when this was written.
// Below this the parse has stopped reading the macro and every verdict is
// uninformed rather than clean.
const MIN_REGISTRY_ENTRIES = 5;

const failures = [];

/**
 * Every production file that seals a value, what column that ciphertext ends up
 * in, and how many sealing sites the file holds.
 *
 * The count is what makes this an inventory rather than an allowlist: a file
 * already listed here cannot grow a sealing site for a *new* column without
 * saying so. Both halves rot loudly — a file that moves, or a site that is
 * added or removed, fails this check until the entry is corrected.
 *
 * `writes: []` is for the two files that produce ciphertext without owning a
 * column, and each carries the reason it is not a registry consumer.
 */
const CIPHERTEXT_PRODUCERS = [
  {
    file: 'crates/rg-http/src/api/mfa.rs',
    sites: 1,
    writes: ['users.totp_secret'],
    why: 'TOTP enrolment seals the shared secret before `user_ops::update_totp_secret` stores it',
  },
  {
    file: 'crates/rg-http/src/api/ci_secrets.rs',
    sites: 1,
    writes: ['ci_secrets.encrypted_value'],
    why: 'a CI secret is sealed on the way in and only ever opened by a job',
  },
  {
    file: 'crates/rg-http/src/api/admin.rs',
    sites: 4,
    writes: ['sso_providers.client_secret_enc', 'sso_providers.ldap_bind_password_enc'],
    why: 'SSO provider create and update each seal both the OIDC client secret and the LDAP bind password — two columns, two handlers, four sites',
  },
  {
    file: 'crates/rg-core/src/auth/instance_key.rs',
    sites: 1,
    writes: ['instance_signing_key.seed_encrypted'],
    why: 'the instance signing key is stored sealed, so it survives a jwt-secret rotation',
  },
  {
    file: 'crates/rg-core/src/mirror/service.rs',
    sites: 1,
    writes: ['mirrors.password_encrypted'],
    why: 'a mirror password has to be replayed to the remote, so it is sealed rather than hashed',
  },
  {
    file: 'crates/rg-core/src/webhook/service.rs',
    sites: 2,
    writes: ['webhooks.secret_encrypted'],
    why: 'a signing secret is sealed when the webhook is saved, and again when a legacy plaintext row is migrated in place',
  },
  {
    file: 'crates/rg-core/src/auth/key_check.rs',
    sites: 2,
    writes: [],
    why: 'the startup preflight seals its own marker plaintext to answer whether the configured key still opens this database; the marker is not a registry column and must not be rekeyed as one',
  },
  {
    file: 'crates/rg-core/src/auth/rekey.rs',
    sites: 1,
    writes: [],
    why: 'the rotation pass re-seals whatever the registry names, so it is the registry\'s consumer rather than a column of its own',
  },
];

// ── The registry ─────────────────────────────────────────────────────
//
// Read out of the macro body rather than out of `LABELS`, which is what the
// macro expands to: a parse of the source is what notices a line that was
// commented out, and a commented-out entry is exactly the drift this checks.
const registryLabels = new Set();
try {
  // `productionRustSource` and not the raw bytes: a commented-out entry is not
  // in the registry, and reading one as if it were would hide exactly the drift
  // this checks. It keeps string literals, which is where the labels live.
  const registry = productionRustSource(readFileSync(path.join(root, REGISTRY), 'utf8'));
  const entry = /\b(\w+)\s*,\s*(\w+)\s*,\s*(\w+)\s*,\s*"([^"]+)"\s*,\s*(optional|required)\s*;/g;
  for (const match of registry.matchAll(entry)) {
    registryLabels.add(match[4]);
  }
} catch (error) {
  failures.push(
    `This check cannot read ${REGISTRY} (${error.message}), so every verdict below means ` +
      'nothing. Fix the path, not the code.',
  );
}

if (failures.length === 0 && registryLabels.size < MIN_REGISTRY_ENTRIES) {
  failures.push(
    `Only ${registryLabels.size} entr(ies) could be parsed out of \`with_encrypted_columns!\` in ` +
      `${REGISTRY}, and at least ${MIN_REGISTRY_ENTRIES} are expected. The macro has changed ` +
      'shape, so this check is reading nothing. Fix the parsing, not the code.',
  );
}

// ── The producers ────────────────────────────────────────────────────
const production = loadProductionRust(cratesDir);
if (production.size === 0) {
  failures.push(
    'This check found no production Rust under crates/, so every column below would read as ' +
      'unsealed. Fix the path, not the code.',
  );
}

const sealer = /\bencryption\s*::\s*encrypt\s*\(/g;
// The same call spelled so the module name is gone by the time it is written.
const hiddenSealerImports = [
  /\buse\s+[\w:\s]*\bencryption\s*::\s*encrypt\b[^;]*;/g,
  /\buse\s+[\w:\s]*\bencryption\s*::\s*\{[^;}]*\bencrypt\b[^;}]*\}\s*;/g,
];

const declared = new Map(CIPHERTEXT_PRODUCERS.map((entry) => [entry.file, entry]));
const found = new Map();

for (const [file, productionView] of production) {
  const name = path.relative(root, file).split(path.sep).join('/');
  const sites = [...productionView.matchAll(sealer)].length;
  if (sites > 0) found.set(name, sites);

  for (const pattern of hiddenSealerImports) {
    if (pattern.test(productionView)) {
      failures.push(
        `${name}: \`encryption::encrypt\` is imported into scope, which spells the sealing site ` +
          'without the module name and hides it from this sweep. Call it as ' +
          '`encryption::encrypt(...)` so the inventory below can see it.',
      );
    }
  }
}

for (const [name, sites] of [...found].sort()) {
  const entry = declared.get(name);
  if (entry === undefined) {
    failures.push(
      `${name} produces ciphertext (${sites} site(s)) and is not in CIPHERTEXT_PRODUCERS. If it ` +
        'seals a new column, add that column to `with_encrypted_columns!` first — a column ' +
        'outside the registry is skipped by the startup preflight AND by `rekey`, so the ' +
        'rotation that reports success leaves it sealed under the key the operator was told to ' +
        'discard. Then add an entry here naming the column and the reason.',
    );
    continue;
  }
  if (entry.sites !== sites) {
    failures.push(
      `${name} holds ${sites} sealing site(s), CIPHERTEXT_PRODUCERS says ${entry.sites}. If a ` +
        'site was added for a new column, register the column and update this entry; if one was ' +
        'removed, update the count so the next addition is still noticed.',
    );
  }
}

for (const entry of CIPHERTEXT_PRODUCERS) {
  if (!found.has(entry.file)) {
    failures.push(
      `CIPHERTEXT_PRODUCERS names ${entry.file}, which produces no ciphertext any more (was: ` +
        `${entry.why}). Drop the entry, or follow the code to wherever the sealing moved — an ` +
        'entry pointing at nothing is how this inventory stops being one.',
    );
  }
  for (const label of entry.writes) {
    if (!registryLabels.has(label)) {
      failures.push(
        `${entry.file} is recorded as sealing \`${label}\`, which \`with_encrypted_columns!\` ` +
          'does not name. Either the label is misspelled here, or a column really is being ' +
          'sealed outside the registry — which is the data-loss case this check exists for.',
      );
    }
  }
}

const claimed = new Set(CIPHERTEXT_PRODUCERS.flatMap((entry) => entry.writes));
for (const label of [...registryLabels].sort()) {
  if (!claimed.has(label)) {
    failures.push(
      `\`${label}\` is in \`with_encrypted_columns!\` and nothing recorded here seals it. Either ` +
        'the feature that wrote it is gone — in which case the entry follows the column out, as ' +
        '`oauth_accounts.access_token` did — or its writer moved and CIPHERTEXT_PRODUCERS is ' +
        'stale. A registry entry with no producer is a column `rekey` rewrites for nobody.',
    );
  }
}

if (failures.length > 0) {
  console.error('❌ Encrypted-column registry:');
  for (const failure of failures) console.error(`   - ${failure}`);
  process.exit(1);
}

console.log(
  `✅ Encrypted-column registry: ${registryLabels.size} registered column(s), each sealed by a ` +
    `recorded producer; ${found.size} production file(s) produce ciphertext and all are ` +
    'accounted for.',
);
