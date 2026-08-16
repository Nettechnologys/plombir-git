#!/usr/bin/env node

// Mutation-sized fixture for the source parser behind the public-function
// consumer gate. These are precisely the forms that have produced false green
// or false red verdicts in the real inventory: test-only callers, call-shaped
// strings, and explicit Rust generic arguments.

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';

import {
  findPublicFunctionOrphans,
  loadProductionRust,
} from './lib/rust-consumer-contract.mjs';
import {
  parseUtoipaPaths,
  stripRustComments,
  utoipaRowFor,
} from './lib/rust-source.mjs';

const root = mkdtempSync(path.join(tmpdir(), 'forgekeep-consumer-contract-'));
try {
  const src = path.join(root, 'crates/demo/src');
  mkdirSync(src, { recursive: true });
  writeFileSync(
    path.join(src, 'lib.rs'),
    String.raw`
pub fn live_generic<T>() {}
fn calls_generic() { live_generic::<u8>(); }

pub extern "C" fn live_extern() {}
fn calls_extern() { live_extern(); }

// Neither a comment nor a string literal is a production caller.
pub fn truly_orphan() {}
const CALL_SHAPED_TEXT: &str = "truly_orphan()";
fn a_char_literal_does_not_open_a_string() { let _ = '"'; }

#[cfg(any(test, feature = "test-support"))]
mod tests {
    fn a_test_caller_is_not_a_consumer() { super::truly_orphan(); }
    const BRACES_IN_A_RAW_STRING: &str = r#"{ not an item boundary }"#;
}

pub fn after_test_module() {}
fn calls_after_test_module() { after_test_module(); }
`,
  );

  const production = loadProductionRust(path.join(root, 'crates'));
  const { declarations, orphans } = findPublicFunctionOrphans({
    root,
    scannedDirs: ['crates/demo/src'],
    production,
  });
  const names = declarations.map(({ name }) => name).sort();
  const orphanNames = orphans.map(({ name }) => name).sort();

  const expectedNames = ['after_test_module', 'live_extern', 'live_generic', 'truly_orphan'];
  if (JSON.stringify(names) !== JSON.stringify(expectedNames)) {
    throw new Error(`consumer parser read ${JSON.stringify(names)}, expected ${JSON.stringify(expectedNames)}`);
  }
  if (JSON.stringify(orphanNames) !== JSON.stringify(['truly_orphan'])) {
    throw new Error(
      `consumer parser reported ${JSON.stringify(orphanNames)}, expected only truly_orphan`,
    );
  }
} finally {
  rmSync(root, { recursive: true, force: true });
}

// The comment-preserving view used by 16 contract checks must share the same
// Rust literal lexer as the consumer parser. The inner `"` closes the old
// normal-string state early; the raw string's real closing `"#` then opened a
// second string that swallowed the rest of the file. Both comments below were
// consequently returned as live source (card_1b8cdb49512d).
const rawStringBeforeComments = String.raw`
const TRICKY_RAW: &str = r#"the " character"#;
// pub async fn line_commented_handler() {}
/* pub async fn block_commented_handler() {} */
pub async fn live_handler() {}
`;
const commentsBlanked = stripRustComments(rawStringBeforeComments);
if (
  commentsBlanked.includes('line_commented_handler') ||
  commentsBlanked.includes('block_commented_handler')
) {
  throw new Error(
    'Rust comments after a raw string were returned as live source by stripRustComments',
  );
}
if (
  !commentsBlanked.includes('r#"the " character"#') ||
  !commentsBlanked.includes('pub async fn live_handler()')
) {
  throw new Error('stripRustComments damaged a raw string or executable code after it');
}
if (commentsBlanked.length !== rawStringBeforeComments.length) {
  throw new Error('stripRustComments must preserve source offsets while blanking comments');
}

// The same claim for the annotation parser: a `#[utoipa::path(...)]` written
// inside a Rust string is data, and data must not mint an annotation row. It
// used to — the parser scanned a view that still had the string literals in it,
// so a raw string carrying a well-formed annotation *plus* a `pub async fn`
// line parsed as a live, attributed annotation. A check reading such a row
// asserted against text no compiler ever sees, and went green while the
// published annotation right below it was broken (card_4a7a66d05983).
const decoyed = String.raw`
#[allow(dead_code)]
const DECOY: &str = r#"
#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/collaborators/{id}",
    params(
        ("id" = i64, Path, description = "forged"),
    ),
)]
pub async fn update_permission(
"#;

// #[utoipa::path(
//     patch,
//     path = "/commented/out",
//     params(("id" = i64, Path, description = "forged")),
// )]
// pub async fn update_permission(

#[utoipa::path(
    patch,
    path = "/repos/{owner}/{name}/collaborators/{id}",
    params(
        ("id" = i64, Path, description = "the real one"),
    ),
)]
pub async fn update_permission(
    State(state): State<AppState>,
) -> impl IntoResponse {
    todo!()
}
`;

const rows = parseUtoipaPaths(decoyed, 'api::demo', 'demo.rs');
if (rows.length !== 1) {
  throw new Error(
    `annotation parser read ${rows.length} #[utoipa::path] rows in the decoy fixture, expected 1 — ` +
      'an annotation-shaped comment or string is being read as a live annotation.',
  );
}
const row = utoipaRowFor(rows, 'api::demo::update_permission');
if (row === undefined || !row.paramsBody.includes('the real one')) {
  throw new Error('annotation parser attributed the decoy instead of the executable annotation');
}

console.log('rust consumer parser contract ok');
