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
//   * A same-named method on an unrelated type elsewhere in the tree makes a
//     dead op look alive. That is the lenient direction: the gate under-reports
//     rather than blocking a correct tree, which is why the inventory below is
//     the floor and not the ceiling.
//   * A caller reached only from tests does NOT count. `#[cfg(test)]` items and
//     `tests/` directories are stripped first, because "alive because its own
//     unit test calls it" is exactly the state this check exists to name.
//
// Comments are stripped before counting for a reason that already bit this
// inventory once: `password_reset_token_ops::delete_expired` was scored as
// used because a migration's `//!` header *mentions* it — the sentence "nobody
// calls this" reading as a call.

import path from 'node:path';

import {
  findPublicFunctionOrphans,
  loadProductionRust,
} from './lib/rust-consumer-contract.mjs';

const root = process.cwd();
const cratesDir = path.join(root, 'crates');
const failures = [];

// The directories whose public functions owe a caller.
//
// These are places where a call always *looks* like a call, which is what makes
// the name-based counting below meaningful. `rg-http` is deliberately absent:
// its handlers are named in the router without parentheses (`get(api::admin::
// get_user)`), so the same scan reports 178 of its 349 public functions as
// orphans, nearly all of them false. That boundary already has
// `openapi-route-coverage-contract-check.mjs`.
const SCANNED = [
  'crates/rg-db/src/ops',
  'crates/rg-core/src',
  'crates/rg-git/src',
  'crates/rg-mcp/src',
];

// Production source of the whole workspace, keyed by file, comments and test
// items removed.
const production = loadProductionRust(cratesDir);
const { declarations, orphans, scannedFiles } = findPublicFunctionOrphans({
  root,
  scannedDirs: SCANNED,
  production,
});

for (const scanned of SCANNED) {
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
  `consumer contract ok (${declarations.length} public functions across ${SCANNED.length} ` +
    'directories, every one consumed)',
);
