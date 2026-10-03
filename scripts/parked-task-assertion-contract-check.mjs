#!/usr/bin/env node

// Reject the inverted "is the writer still blocked?" assertion across the
// workspace.
//
// A test that parks a spawned task behind a boundary — a held SQLite write
// lock, an upload session, a CI concurrency group — proves the boundary held by
// showing the task did not finish inside an observation window. Spelled
// `assert!(timeout(window, &mut task).await.is_err(), "…")`, that reads `Err`
// as "still parked", and it is wrong about the two outcomes that matter most: a
// task that returned an error and a task that panicked BOTH resolve the handle
// immediately. `is_err()` is then false, and the assertion fires printing the
// opposite of the truth — "the writer crossed the boundary" when in fact it
// never got in, with the real failure left in the task's own output, a screen
// above what the harness shows. That inversion hid the cause of a live flake
// for three runs (card_209f04ae7ab4) and stood in three more places
// (card_28fe058374ce).
//
// The honest spelling is a `match` on the three outcomes that names what
// actually happened; each crate that needs one has a helper for it
// (`assert_writer_stays_blocked`, `assert_task_stays_blocked`,
// `assert_request_stays_blocked`, `assert_probe_stays_blocked`).
//
// Truth boundary: this catches the concrete spelling that caused the defect —
// an `assert!` whose condition times out a `&mut` binding and reads `.is_err()`
// off it. It is not a Rust type checker: it cannot tell a `JoinHandle` from any
// other borrowed future, and it deliberately does not flag `timeout(...)` on a
// value that is not borrowed, because consuming the handle means nothing can
// wait on it afterwards and the shape does not arise. Comments are blanked
// first, so a commented-out assertion — or this file's own prose quoted in a
// doc comment — is not part of the program anybody runs.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { testInclusiveRustCode } from './lib/rust-source.mjs';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(process.env.PLOMBIR_GIT_PARKED_TASK_ROOT || join(scriptsDir, '..'));
const failures = [];

function rustFiles(dir) {
  if (!existsSync(dir)) {
    failures.push(`${relative(root, dir)}/ is missing, so the parked-task sweep is incomplete`);
    return [];
  }

  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) files.push(...rustFiles(path));
    else if (entry.name.endsWith('.rs')) files.push(path);
  }
  return files;
}

// `assert!(` … `timeout(` … `&mut <ident>` … `.is_err()`, all inside one
// condition. The spans are bounded so the scan cannot run past the assertion it
// started in and pair an unrelated `.is_err()` with it.
const INVERTED = /\bassert!\s*\(\s*(?:(?!\bassert!)[\s\S]){0,200}?\btimeout\s*\((?:(?!\bassert!)[\s\S]){0,200}?&mut\s+([A-Za-z_][A-Za-z0-9_]*)(?:(?!\bassert!)[\s\S]){0,200}?\.\s*is_err\s*\(\s*\)/g;

const scanned = rustFiles(join(root, 'crates'));
for (const file of scanned) {
  const source = testInclusiveRustCode(readFileSync(file, 'utf8'));
  for (const match of source.matchAll(INVERTED)) {
    const line = source.slice(0, match.index).split('\n').length;
    failures.push(
      `${relative(root, file)}:${line}: \`${match[1]}\` is asserted to be parked through ` +
        '`timeout(...).await.is_err()`, which reads a task that failed or panicked as one that ' +
        'crossed the boundary; match the three outcomes and print the real one instead',
    );
  }
}

if (scanned.length === 0) {
  failures.push('the sweep read no Rust at all under crates/, so its green means nothing');
}

if (failures.length > 0) {
  console.error('❌ parked-task assertion contract failed:');
  for (const failure of failures) console.error(`  - ${failure}`);
  process.exit(1);
}

console.log(
  `✅ parked-task assertion contract: ${scanned.length} Rust files, no assertion reads a failed ` +
    'or panicking task as one that crossed the boundary',
);
