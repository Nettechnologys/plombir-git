#!/usr/bin/env node

// Every entity column the server WRITES must have something that READS it.
//
// `db-ops-consumer-contract-check.mjs` asks this question about functions and
// stops there. One layer down, the same absence sits in the schema: a column
// nothing reads back is a producer with no consumer, only its key is a column
// name rather than a function name — and rustc cannot see it either, because a
// struct field of a `pub` model is reachable by definition.
//
// The cost is higher here than for a dead function, and that is the reason this
// check exists rather than being tidy-up. A dead function stores nothing. A
// dead column stores — and the last one found by hand was holding somebody
// else's live GitHub / GitLab / OIDC credentials, encrypted, rotated on every
// rekey, for no feature at all (card_51dd82b6dc82). Four separate drop
// migrations have now removed eight such columns:
// `m20260808_000001` (`oci_blob.ref_count`, "half of a garbage collector nobody
// wrote"), `m20260808_000004` (`import_tasks.user_mapping`, "documented, never
// written"), `m20260810_000001` (three storage-metadata columns, "writers but
// no readers") and `m20260822_000002` (three `oauth_accounts` columns). Not one
// of the four started from a gate. Every one started from a person reading the
// code and noticing.
//
// What counts as a WRITER: `field: Set(` in an `ActiveModel` literal, or
// `.field = Set(`, in comment-stripped, test-stripped production Rust anywhere
// in the workspace. A column nothing writes is not this check's subject — that
// is either a read-only projection or a column the migrations fill, and neither
// is the defect here.
//
// What counts as a READER: `Column::<Variant>` (a filter, an ordering, a
// projection, a relation used outside the entity's own module), or a `.field`
// access that is not the left-hand side of one of the writes above. Both are
// name-based rather than type-resolved, deliberately and for the same reason
// the sibling check gives: a gate that needs a compiler plugin does not get
// run. The leniency runs toward NOT accusing — a same-named field on an
// unrelated struct answers for this one — which is the right direction for a
// check whose red means "delete stored data".
//
// The measured false-positive rate, on the tree this check was written against
// (693 model fields across 80 entities, 9 reported):
//
//   * `board.created_by` and `pr_review.dismissed_by` — the whole `Model` is
//     serialized into an API response (`Json(serde_json::json!(board))`,
//     `Json(review)`), so serde reads every field and no `.field` appears
//     anywhere. Two.
//   * `package_version.protocol_variant_key` — read by the database rather
//     than by Rust: it is a member of the UNIQUE index the rubygems migration
//     built. One.
//   * the remaining six are real, and are what this check was written to find.
//
// Three false positives out of 693 fields is what settles the scope question
// the card left open: the straightforward check is honest enough to run on
// every column, and does not have to be narrowed to the `encrypted_columns`
// registry. The three carry their reason in ALLOWED_WITHOUT_CONSUMER below,
// which is a ratchet — an entry naming a field that no longer exists, or one
// that has grown a reader since, fails this check.

import { readdirSync, readFileSync } from 'node:fs';
import path from 'node:path';

import { loadProductionRust } from './lib/rust-consumer-contract.mjs';
import { rustStructBody } from './lib/rust-source.mjs';

const root = process.cwd();
const cratesDir = path.join(root, 'crates');
const entitiesDir = path.join(cratesDir, 'rg-db', 'src', 'entities');
const failures = [];

/** `created_by` → `CreatedBy`, which is how SeaORM spells the column. */
function columnVariant(field) {
  return field
    .split('_')
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join('');
}

// ── Inventory: every column of every entity ──────────────────────────
//
// `rustStructBody` reads the executable view, so a field that is only
// commented out is not inventoried — the same reason the sibling checks use it.
const columns = [];
let entityFiles = [];
try {
  entityFiles = readdirSync(entitiesDir)
    .filter((name) => name.endsWith('.rs') && name !== 'mod.rs')
    .sort();
} catch (error) {
  failures.push(
    `This check cannot read ${path.relative(root, entitiesDir)} (${error.message}), so every ` +
      'verdict below means nothing. Fix the path, not the code.',
  );
}

for (const name of entityFiles) {
  const file = path.join(entitiesDir, name);
  const body = rustStructBody(readFileSync(file, 'utf8'), 'Model');
  if (body === null) {
    failures.push(
      `${path.relative(root, file)}: no \`struct Model\` could be parsed out of this entity, so ` +
        'its columns are invisible to this check. Fix the parsing, not the code.',
    );
    continue;
  }
  for (const field of body.matchAll(/^\s*pub\s+(\w+)\s*:/gm)) {
    columns.push({
      entity: name.slice(0, -'.rs'.length),
      field: field[1],
      variant: columnVariant(field[1]),
      file,
    });
  }
}

if (columns.length === 0 && failures.length === 0) {
  failures.push(
    'This check can no longer read a single model field out of the entity modules, so its ' +
      'verdicts mean nothing. Fix the parsing, not the code.',
  );
}

// ── Evidence: production Rust, minus the entity declarations ─────────
//
// The entity's own module is excluded from both sides. Everything in it is
// declaration: the field itself, and the `Relation` blocks whose
// `from = "Column::X"` names a column without anybody having traversed the
// relation. Counting those as readers would make every foreign key answer for
// itself.
const production = loadProductionRust(cratesDir);
const evidence = [];
for (const [file, source] of production) {
  if (file.startsWith(entitiesDir + path.sep)) continue;
  evidence.push(source);
}

if (evidence.length === 0) {
  failures.push(
    'This check found no production Rust outside the entity modules, so every column below would ' +
      'read as unwritten. Fix the path, not the code.',
  );
}

const WRITE_SPELLINGS = String.raw`(?:Set|ActiveValue::Set)`;

function hasWriter({ field }) {
  const literal = new RegExp(String.raw`(?:^|[^\w.])${field}\s*:\s*${WRITE_SPELLINGS}\s*\(`, 'm');
  const assignment = new RegExp(String.raw`\.${field}\s*=\s*${WRITE_SPELLINGS}\s*\(`);
  return evidence.some((source) => literal.test(source) || assignment.test(source));
}

function hasReader({ field, variant }) {
  const column = new RegExp(String.raw`Column::${variant}\b`);
  const access = new RegExp(String.raw`\.${field}\b`, 'g');
  const isWrite = new RegExp(String.raw`^\s*=\s*${WRITE_SPELLINGS}\s*\(`);
  return evidence.some((source) => {
    if (column.test(source)) return true;
    for (const match of source.matchAll(access)) {
      const after = source.slice(match.index + match[0].length, match.index + match[0].length + 24);
      if (!isWrite.test(after)) return true;
    }
    return false;
  });
}

// ── The ratchet ──────────────────────────────────────────────────────
//
// Each entry states why a written column may stay unread. An entry naming a
// column that no longer exists, or one that has since grown a reader, fails
// this check: the list describes the tree or it describes nothing.
const ALLOWED_WITHOUT_CONSUMER = new Map([
  [
    'board.created_by',
    'serde reads it. `board::Model` is serialized whole into the API response ' +
      '(`Json(serde_json::json!(board))` in `api/boards.rs`), so the field reaches the client ' +
      'without any Rust naming it',
  ],
  [
    'pr_review.dismissed_by',
    'serde reads it, the same way `board.created_by` is read: `pr_review::Model` goes out whole ' +
      'through `Json(review)` in `api/reviews.rs`',
  ],
  [
    'package_version.protocol_variant_key',
    'the database reads it. It is a member of the UNIQUE index ' +
      '`m20260811_000002_rubygems_protocol_version_key` built over ' +
      '`(package_id, version, protocol_variant_key)`, which is what keeps two rubygems platform ' +
      'variants of one version from colliding; no Rust has to read it back for that to work',
  ],
  [
    'instance_signing_key.rotated_at',
    'card_b70de2169bd6 — written when a key is replaced and read by nothing. Kept until that ' +
      'card decides between giving it a reader and dropping it',
  ],
  [
    'oci_manifest.push_by',
    'card_b70de2169bd6 — who pushed a manifest, written on every push and read by nothing',
  ],
  [
    'user.ldap_dn',
    'card_b70de2169bd6 — written by LDAP first-login and sync, read by nothing outside tests',
  ],
  [
    'user.mfa_type',
    'card_b70de2169bd6 — written by enrolment, read by nothing: the challenge path branches on ' +
      '`mfa_enabled` and the stored TOTP secret',
  ],
  [
    'user.backup_codes',
    'card_b70de2169bd6 — the legacy pre-`mfa_backup_codes` store. Every live path uses the table; ' +
      'this column is only ever set to `None`, and it is credential material',
  ],
  [
    'webauthn_ceremony_spend.spent_at',
    'card_b70de2169bd6 — an audit stamp with no reader; the retention sweep in ' +
      '`webauthn_ceremony_ops::delete_expired` reads `expires_at`',
  ],
]);

const orphans = columns.filter((column) => hasWriter(column) && !hasReader(column));
const orphanKeys = new Set(orphans.map(({ entity, field }) => `${entity}.${field}`));
const known = new Set(columns.map(({ entity, field }) => `${entity}.${field}`));

for (const [key, reason] of ALLOWED_WITHOUT_CONSUMER) {
  if (typeof reason !== 'string' || reason.trim() === '') {
    failures.push(`${key} is allowed without a reader without a recorded reason.`);
  }
  if (!known.has(key)) {
    failures.push(
      `ALLOWED_WITHOUT_CONSUMER names ${key} (${reason}), but no entity declares that column — ` +
        'delete the entry so the exemption list keeps describing the schema.',
    );
    continue;
  }
  if (!orphanKeys.has(key)) {
    failures.push(
      `ALLOWED_WITHOUT_CONSUMER names ${key} (${reason}), but that column is written and read ` +
        'like any other now — delete the entry so the list stays a ratchet rather than a parking ' +
        'lot.',
    );
  }
}

for (const { entity, field, variant } of orphans) {
  if (ALLOWED_WITHOUT_CONSUMER.has(`${entity}.${field}`)) continue;
  failures.push(
    `crates/rg-db/src/entities/${entity}.rs: \`${field}\` is written and nothing reads it back — ` +
      `no \`Column::${variant}\` in a filter, ordering or projection, and no \`.${field}\` access ` +
      'outside the writes themselves. Give it the reader the feature it names is supposed to ' +
      'have, or drop the column with a migration: a column that only ever receives is storage ' +
      'nobody has decided to keep, and when it holds credential material that is a liability. ' +
      'If it genuinely has to stay unread, add it to ALLOWED_WITHOUT_CONSUMER with the reason.',
  );
}

if (failures.length > 0) {
  console.error('entity column consumer contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log(
  `entity column consumer contract ok (${columns.length} columns across ${entityFiles.length} ` +
    `entities, every written one read back; ${ALLOWED_WITHOUT_CONSUMER.size} exempt with a reason)`,
);
