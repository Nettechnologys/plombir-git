#!/usr/bin/env node

// Every public function of the server's library surfaces must have a
// production consumer.
//
// `rg-db/src/ops` is the whole database surface of the server: one module per
// table, every entry point a `pub async fn`. A function here that nothing calls
// is not the usual dead code — rustc cannot see it (a `pub fn` in a library
// crate is reachable by definition, so no warning is ever emitted), and the
// name reads like a shipped feature. `mark_job_timeout` next to a live job
// timeout, `is_protected` next to live branch protection, `update_owner` next
// to a live repository transfer: each one is the question "does this feature
// work some other way, or not at all?", and nothing in the tree answered it.
// Eighteen of them had accumulated silently (card_b72bcf38e8d6).
//
// The same absence used to sit unchecked in `rg-core`, `rg-git` and `rg-mcp`:
// public wrappers, protocol variants and service entry points survived solely
// because a library's exported function is reachable as far as rustc is
// concerned.
// The arc is asserted from the producer side, which is the side no
// frontend/backend symmetry check can see. The HTTP boundary already has
// `openapi-route-coverage-contract-check.mjs`; this is the same idea one layer
// down.
//
// What counts as a consumer: a call `name(` in comment-stripped, test-stripped
// Rust source of any crate other than the file that defines it. Deliberately
// name-based rather than type-resolved — a check that needs a compiler plugin
// does not get run. The consequences of that choice, both ways:
//
//   * A same-named method on an unrelated type elsewhere in the tree used to
//     make a dead op look alive. It no longer does: the inventory is free
//     functions only (the declaration regex is anchored at column zero), and a
//     free function cannot be reached through a dot or through a `Type::`
//     qualifier, so `consumerCalls` drops both spellings without resolving a
//     type. That leniency was recorded here for months and never measured;
//     when it finally was, it was hiding four real orphans — `notification::
//     notify` and `rg-ci::resume_pipeline` behind a trait method of the same
//     name, `release::service::upload_asset` and `user_ops::enable_mfa` behind
//     a same-named handler's own DECLARATION reading as a call
//     (card_44b56ef6f938).
//   * What stays lenient: `<T as Trait>::name(` is counted, because a spelling
//     the reader cannot weigh must not manufacture an accusation.
//   * A caller reached only from tests does NOT count. `#[cfg(test)]` items and
//     `tests/` directories are stripped first, because "alive because its own
//     unit test calls it" is exactly the state this check exists to name.
//
// Comments are stripped before counting for a reason that already bit this
// inventory once: `password_reset_token_ops::delete_expired` was scored as
// used because a migration's `//!` header *mentions* it — the sentence "nobody
// calls this" reading as a call.

import { existsSync, readdirSync } from 'node:fs';
import path from 'node:path';

import {
  findPublicFunctionOrphans,
  loadProductionRust,
} from './lib/rust-consumer-contract.mjs';

const root = process.cwd();
const cratesDir = path.join(root, 'crates');
const failures = [];

// Every crate manifest under `crates/` owes this check a decision. Discovering
// manifests from the tree is deliberate: it catches both a new workspace
// member and a crate whose root `Cargo.toml` registration was forgotten.
function crateDirectories() {
  return readdirSync(cratesDir, { withFileTypes: true })
    .filter(
      (entry) =>
        entry.isDirectory() && existsSync(path.join(cratesDir, entry.name, 'Cargo.toml')),
    )
    .map((entry) => `crates/${entry.name}`)
    .sort();
}

// Crates whose public functions cannot be judged by the call-shaped heuristic.
//
// This is a ratchet, not an opt-out switch: every entry needs a reason, a stale
// entry fails below, and any crate not named here is scanned automatically.
// `rg-ci`, `rg-runner` and `rg-ssh` used to be silently absent. They are not
// exclusions: their public free functions use ordinary `name(...)` call sites,
// so the same consumer criterion applies to them without false positives.
const EXCLUDED_CRATES = new Map([
  [
    'crates/rg-http',
    'Axum handlers are consumed as function values in the router, for example ' +
      '`get(api::admin::get_user)`, so a call-shaped scan reports live handlers as orphans. ' +
      'The HTTP handler boundary is covered by openapi-route-coverage-contract-check.mjs.',
  ],
]);

const crates = crateDirectories();
const scannedDirs = [];

for (const [excluded, reason] of EXCLUDED_CRATES) {
  if (!crates.includes(excluded)) {
    failures.push(
      `EXCLUDED_CRATES names ${excluded} (${reason}), but that crate does not exist — remove the ` +
        'stale entry so the exemption list keeps describing the tree.',
    );
  }
  if (typeof reason !== 'string' || reason.trim() === '') {
    failures.push(`${excluded} is excluded from the consumer scan without a recorded reason.`);
  }
}

for (const crate of crates) {
  if (EXCLUDED_CRATES.has(crate)) continue;
  const sourceDir = `${crate}/src`;
  if (!existsSync(path.join(root, sourceDir))) {
    failures.push(
      `${crate} has a Cargo.toml but no scanned src/ directory. Add its source tree, or record why ` +
        'the consumer criterion does not apply in EXCLUDED_CRATES.',
    );
    continue;
  }
  scannedDirs.push(sourceDir);
}

// Production source of the whole workspace, keyed by file, comments and test
// items removed.
const production = loadProductionRust(cratesDir);
const { declarations, orphans, scannedFiles } = findPublicFunctionOrphans({
  root,
  scannedDirs,
  production,
});

for (const scanned of scannedDirs) {
  if (!scannedFiles.some((file) => file.startsWith(path.join(root, scanned)))) {
    failures.push(
      `This check found no modules under ${scanned}, so every verdict below means nothing. ` +
        'Fix the path, not the code.',
    );
  }
}

if (declarations.length === 0) {
  failures.push(
    'This check can no longer read a single `pub fn` out of the scanned directories, so its ' +
      'verdicts mean nothing. Fix the parsing, not the code.',
  );
}

// A handful of ops are deliberately kept without a caller. Each entry states
// why, and an entry naming a function that no longer exists is itself a
// failure — the allowlist is a ratchet, not a parking lot.
const ALLOWED_WITHOUT_CONSUMER = new Map([
  [
    'crates/rg-core/src/package_registry/adapters/npm.rs::build_npm_metadata',
    'the compatibility entry point for callers that still expect derived `latest`; production ' +
      'uses `build_npm_metadata_with_dist_tags` once persisted tag state is available',
  ],
  [
    'crates/rg-db/src/ops/user_ops.rs::enable_mfa',
    'the read-before-write half of MFA enrolment, and its own doc comment forbids the production ' +
      'caller this check looks for: enrolment must go through `enable_mfa_with_backup_codes`, ' +
      'because on its own this publishes a second factor whose recovery set is still a separate ' +
      'commit away. What is left calling it is ~15 test fixtures that want a user with MFA on and ' +
      'no backup codes. Not the defect this check hunts: the arc is live through the ' +
      'backup-code spelling, this one is the seam beneath it',
  ],
  [
    'crates/rg-db/src/ops/pipeline_ops.rs::create_pipeline',
    'a two-line alias for `create_pipeline_in_group(…, None)` kept for the ~39 test fixtures ' +
      'that build a pipeline row with no concurrency group. Not the defect this check hunts: the ' +
      'arc is live, it is the argument-less spelling that no producer needs, because every ' +
      'producer resolves a `concurrency:` group first (which is `None` for most workflows anyway)',
  ],
]);

function declarationKey({ name, file }) {
  return `${path.relative(root, file)}::${name}`;
}

// A caller inside the defining module counts. An op called by its own siblings
// has an arc — what is wrong with it is its visibility, not its wiring, and
// conflating the two buries the dead ones in a list of `pub` that should have
// been private.
for (const [key, reason] of ALLOWED_WITHOUT_CONSUMER) {
  if (!declarations.some((declaration) => declarationKey(declaration) === key)) {
    failures.push(
      `The allowlist keeps \`${key}\` (${reason}), but no scanned module defines it any more — ` +
        'delete the entry so the next reader is not told a function exists that does not.',
    );
  }
}

for (const { name, file: absoluteFile } of orphans) {
  const key = declarationKey({ name, file: absoluteFile });
  if (ALLOWED_WITHOUT_CONSUMER.has(key)) continue;
  const file = path.relative(root, absoluteFile);
  failures.push(
    `${file}: \`${name}\` is public and no production code anywhere calls it — not even its own ` +
      'module, and a test caller does not count. ' +
      'Wire it up where the feature it names is supposed to work, or delete it — an entry point ' +
      'with no arc reads as a working feature and is not one. If it is genuinely meant to stay ' +
      'callable with no caller, add it to ALLOWED_WITHOUT_CONSUMER with the reason.',
  );
}

if (failures.length > 0) {
  console.error('consumer contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log(
  `consumer contract ok (${declarations.length} public functions across ${scannedDirs.length}/${crates.length} ` +
    `crate source directories, every one consumed; ${EXCLUDED_CRATES.size} excluded with a reason)`,
);
