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
  rustStructBody,
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

// Every finder anchors on a top-level item at column 0 and takes the FIRST hit
// — and a `#[cfg(test)]` double sits at column 0 exactly like the production
// item it doubles. Anchoring in the raw source therefore let a fixture win: the
// check went green while asserting about a function no server ever calls. The
// production item declared *after* the test module must stay visible, which is
// what separates masking the item from truncating the file at the first
// `#[cfg(test)]` (card_a6192a228b1b).
const testDoubleBeforeProduction = String.raw`
#[cfg(test)]
pub async fn store_object(fixture: i64) -> u8 {
    0
}

#[cfg(test)]
pub struct RegisterRunnerResponse {
    fixture: String,
}

pub async fn store_object(real: i64) -> u8 {
    1
}

pub struct RegisterRunnerResponse {
    token: String,
}
`;
const doubledBlock = rustFnBlock(testDoubleBeforeProduction, 'store_object');
const doubledHead = rustFnHead(testDoubleBeforeProduction, 'store_object');
const doubledStruct = rustStructBody(testDoubleBeforeProduction, 'RegisterRunnerResponse');
if (
  doubledBlock?.params !== 'real: i64' ||
  !doubledHead?.includes('real: i64') ||
  doubledHead.includes('fixture') ||
  !doubledStruct?.includes('token: String') ||
  doubledStruct.includes('fixture')
) {
  throw new Error(
    `a #[cfg(test)] double outranked the production declaration: ${JSON.stringify({
      block: doubledBlock,
      head: doubledHead,
      struct: doubledStruct,
    })}`,
  );
}

// The same claim for the two table parsers and the annotation parser: a route
// or a `#[utoipa::path]` inside a `#[cfg(test)]` module registers nothing the
// server serves, and counting it would let a fixture answer "is this mounted?"
// and "is this documented?" for a surface that does not exist.
const testOnlyRegistrations = String.raw`
fn routes(table: RouteTable) -> RouteTable {
    table.get(Public, "/live", api::demo::live)
}

#[utoipa::path(get, path = "/live")]
pub async fn live() {}

#[cfg(test)]
mod tests {
    fn fixture_routes(table: RouteTable) -> RouteTable {
        table.get(Public, "/fixture-only", api::demo::fixture)
    }

    #[utoipa::path(get, path = "/fixture-only")]
    pub async fn fixture() {}
}
`;
const liveRoutes = parseRouteTable(testOnlyRegistrations).map((route) => route.path);
const liveMounted = parseMountedHandlers(testOnlyRegistrations).map((route) => route.handler);
const liveAnnotations = parseUtoipaPaths(testOnlyRegistrations, 'api::demo', 'demo.rs').map(
  (annotation) => annotation.path,
);
if (
  JSON.stringify(liveRoutes) !== JSON.stringify(['/live']) ||
  JSON.stringify(liveMounted) !== JSON.stringify(['api::demo::live']) ||
  JSON.stringify(liveAnnotations) !== JSON.stringify(['/live'])
) {
  throw new Error(
    `a #[cfg(test)] module contributed registrations to the production inventory: ${JSON.stringify({
      routes: liveRoutes,
      mounted: liveMounted,
      annotations: liveAnnotations,
    })}`,
  );
}

// `rustStructBody` read the caller's raw text, so a `struct` written inside a
// raw string won the column-0 anchor and the `\n}` closing that literal cut the
// body short. The check then asserted about fields nobody declared — the very
// failure mode this helper exists to prevent, one view lower down.
const structDecoy = String.raw`
#[allow(dead_code)]
const DECOY: &str = r#"
pub struct SsoProviderInfo {
    decoy: String,
}
"#;

pub struct SsoProviderInfo {
    pub name: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
}
`;
const ssoProviderInfo = rustStructBody(structDecoy, 'SsoProviderInfo');
if (
  ssoProviderInfo === null ||
  ssoProviderInfo.includes('decoy') ||
  !ssoProviderInfo.includes('pub name: String') ||
  !ssoProviderInfo.includes('rename = "displayName"') ||
  !ssoProviderInfo.includes('pub display_name: String')
) {
  throw new Error(
    `struct body parser read a raw-string decoy instead of the declaration: ${JSON.stringify(ssoProviderInfo)}`,
  );
}

// The three declaration readers hand back the *production* text of what they
// found, not the caller's raw bytes. They already anchor in the code-only view,
// so a decoy could not win the declaration — but the block they returned was
// sliced out of the raw source, so a commented-out parameter, field or call
// inside a live declaration was handed to the caller as if it were code. Every
// positive assertion built on `params` / `body` / a struct body read a deleted
// line as live: `download_asset` kept "taking" a repo-read gate that had been
// commented out, and the gate stayed green (card_64b6ede78939).
//
// String literals must survive the same slice — the callers read `#[serde(rename
// = "…")]` and route paths out of it — so this asserts both directions at once.
const commentedOutMembers = String.raw`
pub struct TokenResponse {
    pub id: i64,
    // pub token_hash: String,
    #[serde(rename = "lastUsedAt")]
    pub last_used_at: Option<String>,
}

pub async fn download_asset(
    State(state): State<AppState>,
    // RepoRead { repo }: RepoRead,
    Path(asset_id): Path<i64>,
) -> impl IntoResponse {
    // require_read(&state, &repo).await?;
    stream_asset(&state, asset_id).await
}
`;

const tokenResponse = rustStructBody(commentedOutMembers, 'TokenResponse');
if (
  tokenResponse === null ||
  tokenResponse.includes('token_hash') ||
  !tokenResponse.includes('pub id: i64') ||
  !tokenResponse.includes('rename = "lastUsedAt"')
) {
  throw new Error(
    `struct body parser returned commented-out fields or dropped a serde rename: ${JSON.stringify(tokenResponse)}`,
  );
}

const downloadAsset = rustFnBlock(commentedOutMembers, 'download_asset');
if (
  downloadAsset === null ||
  downloadAsset.params.includes('RepoRead') ||
  downloadAsset.body.includes('require_read') ||
  !downloadAsset.params.includes('Path(asset_id): Path<i64>') ||
  !downloadAsset.body.includes('stream_asset')
) {
  throw new Error(
    `fn block parser returned commented-out signature or body text: ${JSON.stringify(downloadAsset)}`,
  );
}

const downloadAssetHead = rustFnHead(commentedOutMembers, 'download_asset');
if (
  downloadAssetHead === null ||
  downloadAssetHead.includes('RepoRead') ||
  !downloadAssetHead.includes('impl IntoResponse')
) {
  throw new Error(
    `fn head parser returned a commented-out parameter: ${JSON.stringify(downloadAssetHead)}`,
  );
}

console.log('rust consumer parser contract ok');
