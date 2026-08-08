#!/usr/bin/env node

// Every public function of the `rg-db` ops layer must have a production
// consumer.
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
// So the arc is asserted from the producer side, which is the side no
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

import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';

import { stripRustComments } from './lib/rust-source.mjs';

const root = process.cwd();
const opsDir = path.join(root, 'crates/rg-db/src/ops');
const cratesDir = path.join(root, 'crates');
const failures = [];

/**
 * Drop `#[cfg(test)]` items (and `#[cfg(all(test, …))]`) with their bodies.
 *
 * A test module is not a consumer. Without this, deleting the last production
 * caller of an op leaves the check green as long as the op still has a unit
 * test — the precise shape of `oci_ops::insert_manifest`, which outlived the
 * transactional pair that replaced it because one fixture still called it.
 */
function stripCfgTest(source) {
  let out = '';
  let i = 0;
  const marker = /^#\[cfg\((?:all\()?test\b/;
  while (i < source.length) {
    if (source[i] === '#' && marker.test(source.slice(i, i + 24))) {
      // Skip the attribute, then the item it annotates: everything up to the
      // first `{` and its matching `}`. A `#[cfg(test)] use …;` item has no
      // brace before its `;` — stop there instead of eating the rest of the file.
      while (i < source.length && source[i] !== '{' && source[i] !== ';') i += 1;
      if (source[i] === ';') {
        i += 1;
        continue;
      }
      let depth = 0;
      while (i < source.length) {
        if (source[i] === '{') depth += 1;
        if (source[i] === '}') {
          depth -= 1;
          if (depth === 0) {
            i += 1;
            break;
          }
        }
        i += 1;
      }
      continue;
    }
    out += source[i];
    i += 1;
  }
  return out;
}

function rustFiles(dir) {
  const found = [];
  for (const entry of readdirSync(dir)) {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) {
      // `tests/` is an integration-test tree: a caller there is a test caller.
      if (entry === 'tests' || entry === 'target' || entry === 'benches') continue;
      found.push(...rustFiles(full));
      continue;
    }
    if (entry.endsWith('.rs')) found.push(full);
  }
  return found;
}

const opsFiles = readdirSync(opsDir)
  .filter((entry) => entry.endsWith('.rs') && entry !== 'mod.rs')
  .map((entry) => path.join(opsDir, entry));

if (opsFiles.length === 0) {
  failures.push(
    `This check found no ops modules under ${path.relative(root, opsDir)}, so every verdict below ` +
      'means nothing. Fix the path, not the ops layer.',
  );
}

// Production source of the whole workspace, keyed by file, comments and test
// items removed.
const production = new Map();
for (const file of rustFiles(cratesDir)) {
  production.set(file, stripCfgTest(stripRustComments(readFileSync(file, 'utf8'))));
}

/** Every `pub async fn` / `pub fn` defined by an ops module. */
const declarations = [];
for (const file of opsFiles) {
  const source = production.get(file) ?? stripCfgTest(stripRustComments(readFileSync(file, 'utf8')));
  for (const match of source.matchAll(/^pub (?:async )?fn (\w+)/gm)) {
    declarations.push({ name: match[1], file });
  }
}

if (declarations.length === 0) {
  failures.push(
    'This check can no longer read a single `pub fn` out of the ops layer, so its verdicts mean ' +
      'nothing. Fix the parsing, not the ops layer.',
  );
}

// A handful of ops are deliberately kept without a caller. Each entry states
// why, and an entry naming a function that no longer exists is itself a
// failure — the allowlist is a ratchet, not a parking lot.
const ALLOWED_WITHOUT_CONSUMER = new Map([
  [
    'create_pipeline',
    'a two-line alias for `create_pipeline_in_group(…, None)` kept for the ~39 test fixtures ' +
      'that build a pipeline row with no concurrency group. Not the defect this check hunts: the ' +
      'arc is live, it is the argument-less spelling that no producer needs, because every ' +
      'producer resolves a `concurrency:` group first (which is `None` for most workflows anyway)',
  ],
]);

// A caller inside the defining module counts. An op called by its own siblings
// has an arc — what is wrong with it is its visibility, not its wiring, and
// conflating the two buries the dead ones in a list of `pub` that should have
// been private.
const orphans = [];
for (const { name, file } of declarations) {
  const call = new RegExp(`\\b${name}\\s*\\(`, 'g');
  let consumers = 0;
  for (const [candidate, source] of production) {
    const hits = (source.match(call) || []).length;
    // The definition itself matches once; anything beyond it is a call.
    consumers += candidate === file ? Math.max(0, hits - 1) : hits;
    if (consumers > 0) break;
  }
  if (consumers === 0) orphans.push({ name, file: path.relative(root, file) });
}

for (const [name, reason] of ALLOWED_WITHOUT_CONSUMER) {
  if (!declarations.some((declaration) => declaration.name === name)) {
    failures.push(
      `The allowlist keeps \`${name}\` (${reason}), but no ops module defines it any more — ` +
        'delete the entry so the next reader is not told a function exists that does not.',
    );
  }
}

for (const { name, file } of orphans) {
  if (ALLOWED_WITHOUT_CONSUMER.has(name)) continue;
  failures.push(
    `${file}: \`${name}\` is public and no production code anywhere calls it — not even its own ` +
      'module, and a test caller does not count. ' +
      'Wire it up where the feature it names is supposed to work, or delete it — a database entry ' +
      'point with no arc reads as a working feature and is not one. If it is genuinely meant to ' +
      'stay callable with no caller, add it to ALLOWED_WITHOUT_CONSUMER with the reason.',
  );
}

if (failures.length > 0) {
  console.error('rg-db ops consumer contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log(`rg-db ops consumer contract ok (${declarations.length} public ops, every one consumed)`);
