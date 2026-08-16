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
  parseMountedHandlers,
  parseRouteTable,
  parseUtoipaPaths,
  rustFnBlock,
  rustFnHead,
  rustParamType,
  splitRustParams,
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

// Route-call boundaries must be read from the code-only view, not from the
// string-bearing source. The `,)` inside this valid raw string used to close the
// call early, so both route parsers silently returned an empty inventory row
// (card_dde2599cbfab).
const delimiterShapedRoute = String.raw`
fn routes(table: RouteTable) -> RouteTable {
    table.get(
        Foreign(r#"{"label": "reader,)"}"#),
        "/raw-aware",
        api::demo::handler,
    )
}
`;
const routeRows = parseRouteTable(delimiterShapedRoute);
const mountedRows = parseMountedHandlers(delimiterShapedRoute);
const expectedAccess = String.raw`Foreign(r#"{"label": "reader,)"}"#)`;
if (
  routeRows.length !== 1 ||
  routeRows[0].access !== expectedAccess ||
  routeRows[0].path !== '/raw-aware' ||
  routeRows[0].handler !== 'api::demo::handler'
) {
  throw new Error(
    `route parser lost an argument from the delimiter-shaped raw-string fixture: ${JSON.stringify(routeRows)}`,
  );
}
if (
  mountedRows.length !== 1 ||
  mountedRows[0].method !== 'GET' ||
  mountedRows[0].path !== '/raw-aware' ||
  mountedRows[0].handler !== 'api::demo::handler'
) {
  throw new Error(
    `mounted-handler parser lost the delimiter-shaped raw-string route: ${JSON.stringify(mountedRows)}`,
  );
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

// Attribute keys and delimiter boundaries must be located in the code-only
// view. The `,)` inside this valid description used to take the attribute
// helpers below depth zero, hiding every declaration that followed it even
// though parseUtoipaPaths had already built a byte-aligned string-free body.
const delimiterShapedAnnotation = String.raw`
#[utoipa::path(
    get,
    description = r#"{"label": "reader,)"}"#,
    path = "/raw-aware",
    request_body = DemoRequest,
    params(
        ("id" = i64, Query, description = "real param"),
    ),
    responses(
        (status = 200, description = "ok"),
    ),
)]
pub async fn raw_aware(
    Query(_): Query<DemoQuery>,
) -> impl IntoResponse {
    todo!()
}
`;
const rawAwareRows = parseUtoipaPaths(delimiterShapedAnnotation, 'api::demo', 'raw-aware.rs');
const rawAware = utoipaRowFor(rawAwareRows, 'api::demo::raw_aware');
if (
  rawAwareRows.length !== 1 ||
  rawAware?.path !== '/raw-aware' ||
  rawAware.declaresRequestBody !== true ||
  rawAware.declaresParams !== true ||
  !rawAware.paramsBody?.includes('real param') ||
  !rawAware.responsesBody?.includes('status = 200')
) {
  throw new Error(
    `annotation parser lost declarations after a delimiter-shaped raw string: ${JSON.stringify(rawAwareRows)}`,
  );
}

// Handler-signature boundaries use the same two-view rule as route calls and
// annotations. The `,)` inside this valid parameter attribute used to close
// `rustFnBlock.params` in the middle of the raw string; the truncated slice
// then made both parameter splitting and type extraction unreadable.
const delimiterShapedSignature = String.raw`
#[utoipa::path(
    get,
    path = "/raw-signature",
)]
pub async fn raw_signature(
    #[doc = r#"{"label": "reader,)", "tail": "{"}"#]
    Query(query): Query<DemoQuery>,
    State(state): State<AppState>,
) -> impl IntoResponse {
    (query, state)
}
`;
const signatureBlock = rustFnBlock(delimiterShapedSignature, 'raw_signature');
const signatureHead = rustFnHead(delimiterShapedSignature, 'raw_signature');
const signatureParams = signatureBlock && splitRustParams(signatureBlock.params);
const signatureRows = parseUtoipaPaths(delimiterShapedSignature, 'api::demo', 'raw-signature.rs');
const signatureRow = utoipaRowFor(signatureRows, 'api::demo::raw_signature');
if (
  signatureParams?.length !== 2 ||
  !signatureParams[0].includes(String.raw`r#"{"label": "reader,)", "tail": "{"}"#`) ||
  rustParamType(signatureParams[0]) !== 'Query<DemoQuery>' ||
  rustParamType(signatureParams[1]) !== 'State<AppState>' ||
  !signatureHead?.includes('State(state): State<AppState>') ||
  signatureRow?.signatureParams !== signatureBlock.params
) {
  throw new Error(
    `handler signature parser lost a parameter around a delimiter-shaped raw attribute: ${JSON.stringify({
      block: signatureBlock,
      head: signatureHead,
      params: signatureParams,
      row: signatureRow,
    })}`,
  );
}

console.log('rust consumer parser contract ok');
