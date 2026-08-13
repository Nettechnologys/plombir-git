#!/usr/bin/env node

// Every mounted `api::*` handler must appear in the OpenAPI spec — and every
// handler the spec advertises must actually be mounted, at the same method and
// URL.
//
// Why this exists: the passkey module was implemented, mounted, and used by the
// frontend for months while `openapi.rs` listed none of its six handlers
// (card_4619acdf95a9). Fixing that one module said nothing about the rest, and a
// one-off comparison right after found 30 more handlers in the same state
// (card_5d62ded0bed1). The one-off is the problem: a spec drifts back the moment
// somebody adds a route and forgets the `paths(...)` line, and nothing in the
// build says a word about it. A generated client then undercounts the API, and
// a contract review reads a spec that is not the server.
//
// So the comparison is a gate, not an audit. Both directions are checked,
// because both are a lie the spec can tell:
//
//   mounted but undocumented — the endpoint exists and no client knows;
//   documented but unmounted — the spec advertises a door that answers 404.
//
// Names are not enough, though. Each handler declares its route TWICE — as a
// row in the `RouteTable` and as `#[utoipa::path(delete, path = "…")]` above
// the function — and only the second copy reaches the published spec. A gate
// that compares handler *names* leaves that second copy unread: rename the URL
// in `routes.rs`, leave the annotation alone, and the check stays green while
// the spec documents a door that is not there. So the third comparison is
// (method, URL), for every handler that is both mounted and documented.
//
// Deliberate exemptions live in the two allowlists below, each with a reason.
// They are ratchets, not dumping grounds: an entry that stops applying — the
// handler was documented, or deleted, or finally mounted — fails the check, so
// the list cannot rot into a permanent blanket exemption.

import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  loadMountedHandlers,
  loadUtoipaPaths,
  rustParamType,
  splitRustParams,
  stripRustComments,
} from './lib/rust-source.mjs';
import { stripRustNonCode } from './lib/rust-consumer-contract.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ROUTER = join(root, 'crates/rg-http/src/routes.rs');
const OPENAPI = join(root, 'crates/rg-http/src/openapi.rs');
const API_DIR = join(root, 'crates/rg-http/src/api');

// The nest prefix of the REST router, applied by `RouteTable::new("/api/v1")`.
// A route table row carries it; an `#[utoipa::path]` annotation does not, so
// the mounted URL is canonicalised to the annotation's prefix-less spelling
// before comparison — the same convention `api-client-contract-check.mjs`
// normalises to via OPENAPI_BASE_PATH.
//
// Annotations used to be written BOTH ways — six under `/api/v1/ai/…`, 282
// without — and the published document declared no `servers`, so most of it
// advertised URLs the server does not serve (card_b23fa617838f). The prefix now
// lives in exactly one place on the spec side, the `servers(...)` entry of
// `#[openapi(...)]`, and the two assertions below are what keep it there: with
// a document-level server, an annotation that spells the prefix again resolves
// to `/api/v1/api/v1/…`.
const API_PREFIX = '/api/v1';

/** A URL as the spec spells it: relative to the REST prefix, if it carries one. */
function canonicalUrl(url) {
  if (url === API_PREFIX) return '/';
  return url.startsWith(`${API_PREFIX}/`) ? url.slice(API_PREFIX.length) : url;
}

/**
 * The `url = "…"` strings of the `servers(...)` list in `#[openapi(...)]`.
 *
 * Returns `[]` when the list is absent — which is the defect state, not a parse
 * failure, and reads as such at the call site.
 */
function declaredServers(source) {
  const block = stripRustComments(source).match(/\bservers\(([\s\S]*?)\n {4}\),/);
  if (!block) return [];
  return [...block[1].matchAll(/url\s*=\s*"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]);
}

/**
 * The `&Ident` entries of the `modifiers(...)` list in `#[openapi(...)]`.
 *
 * Returns `[]` when the list is absent — the defect state, not a parse failure.
 */
function declaredModifiers(source) {
  const block = stripRustComments(source).match(/\bmodifiers\(([^)]*)\)/);
  if (!block) return [];
  return block[1]
    .split(',')
    .map((entry) => entry.trim().replace(/^&/, ''))
    .filter(Boolean);
}

// Mounted handlers that are intentionally absent from the spec.
//
// Every entry here is an ecosystem-native protocol surface: the URL, the media
// type and the response shape are dictated by a third-party client (cargo, npm,
// pip, dotnet, gem, helm, composer, mvn), not by ForgeKeep. Describing them in
// ForgeKeep's own REST spec would document somebody else's protocol badly —
// they are exercised by the package-registry protocol tests and the
// `*-contract-check.mjs` scripts for each ecosystem instead.
//
// ForgeKeep's own package API — publish, list, get, versions, yank, download —
// IS documented; only the client-dictated spellings are exempt.
const UNDOCUMENTED = new Map([
  ['api::packages::cargo_index_config', 'Cargo sparse index (RFC 2789) — layout fixed by cargo'],
  ['api::packages::cargo_sparse_index', 'Cargo sparse index (RFC 2789) — layout fixed by cargo'],
  ['api::packages::cargo_publish_new', 'Cargo write API — the verb, URL and length-prefixed body are fixed by cargo'],
  ['api::packages::cargo_yank', 'Cargo write API — the verb and URL are fixed by cargo'],
  ['api::packages::cargo_unyank', 'Cargo write API — the verb and URL are fixed by cargo'],
  ['api::packages::composer_packages_json', 'Composer repository protocol — layout fixed by composer'],
  ['api::packages::helm_index', 'Helm chart repository index.yaml — layout fixed by helm'],
  ['api::packages::maven_metadata', 'Maven repository layout — layout fixed by mvn/Gradle'],
  ['api::packages::maven_download', 'Maven repository layout — layout fixed by mvn/Gradle'],
  ['api::packages::maven_upload', 'Maven repository layout — the deploy verb and URL are fixed by mvn/Gradle'],
  ['api::packages::maven_upload_metadata', 'Maven repository layout — the deploy verb and URL are fixed by mvn/Gradle'],
  ['api::packages::npm_registry_metadata', 'npm registry metadata document — layout fixed by npm'],
  ['api::packages::nuget_service_index', 'NuGet V3 service index — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_registration_index', 'NuGet V3 registration index — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_registration_leaf', 'NuGet V3 registration leaf — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_search', 'NuGet V3 search service — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_autocomplete', 'NuGet V3 autocomplete service — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_flat_container_index', 'NuGet V3 flat container (PackageBaseAddress) — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_flat_container_download', 'NuGet V3 flat container package content — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_publish', 'NuGet V3 PackagePublish — the verb and URL are fixed by `dotnet nuget push`'],
  ['api::packages::pypi_simple_root_index', 'PyPI Simple repository API (PEP 503) — layout fixed by pip'],
  ['api::packages::pypi_simple_index', 'PyPI Simple repository API (PEP 503) — layout fixed by pip'],
  ['api::packages::rubygems_compact_versions', 'RubyGems compact index — layout fixed by gem/bundler'],
  ['api::packages::rubygems_compact_info', 'RubyGems compact index — layout fixed by gem/bundler'],
  ['api::packages::rubygems_compact_names', 'RubyGems compact index — layout fixed by gem/bundler'],
  ['api::packages::rubygems_dependencies_json', 'RubyGems JSON dependency API — layout fixed by RubyGems'],
  ['api::packages::rubygems_gem_info', 'RubyGems v1 gem info — layout fixed by gem/bundler'],
  ['api::packages::rubygems_gem_download', 'RubyGems .gem download path — derived by the client from the source URL'],
  ['api::packages::rubygems_push', 'RubyGems write API — the verb, URL and bare-body upload are fixed by `gem push`'],
]);

// Handlers the spec advertises that no route mounts.
//
// Unlike the list above, an entry here is a KNOWN DEFECT under a card, not a
// design decision — the spec is lying and the fix belongs to whoever owns that
// card. It is listed so the gate can start blocking new drift today instead of
// waiting for the backlog, and it fails the moment the card lands.
// Empty is the intended steady state: every documented handler is mounted.
const UNMOUNTED = new Map([]);

/** `crate::api::*` entries of the `paths(...)` list in `openapi.rs`. */
function documentedHandlers(source) {
  const src = stripRustComments(source);
  const block = src.match(/\bpaths\(([\s\S]*?)\n {4}\),/);
  if (!block) {
    throw new Error(
      `Could not find the paths(...) list in ${OPENAPI}. The #[openapi(...)] form changed — ` +
        'fix documentedHandlers() in this check rather than deleting the assertion.',
    );
  }
  return new Set(
    block[1]
      .split(',')
      .map((entry) => entry.trim())
      .filter((entry) => entry.startsWith('crate::api::'))
      .map((entry) => entry.replace(/^crate::/, '')),
  );
}

const failures = [];

const openapiSource = readFileSync(OPENAPI, 'utf8');

// ── Where the prefix is written ───────────────────────────────────────────
//
// Exactly one place, or the spec lies about its own URLs in one of two
// directions: no `servers` at all and every path is short by `/api/v1`, or a
// `servers` entry plus annotations that repeat the prefix and resolve to
// `/api/v1/api/v1/…`. Both are silent — utoipa publishes whatever it is given
// and no client complains until a request 404s.
const servers = declaredServers(openapiSource);
if (!servers.includes(API_PREFIX)) {
  failures.push(
    `crates/rg-http/src/openapi.rs declares servers(${servers.map((url) => `"${url}"`).join(', ') || '<none>'}) — ` +
      `the REST router mounts every documented path under ${API_PREFIX}, so the document must declare it as a ` +
      'server URL. Without it every path in the spec resolves against the document origin and misses the router.',
  );
}

// ── Where the access levels come from ─────────────────────────────────────
//
// `utoipa` derives nothing about authentication, so a document without
// `SecurityAddon` declares no `securitySchemes` and reads as an entirely
// anonymous API — which is what it did for its whole life, until
// card_018b2dd39652. Only the wiring is asserted here: which operation requires
// which scheme is derived from the `Access` level of its route table row, and
// comparing those two is a job for the build that can read both sides —
// `crates/rg-http/tests/integration/openapi_security_guard.rs` drives the served
// document against the facts of the same build. Re-deriving it here from the
// source would be a third copy of a decision that already exists twice, which is
// the failure mode this whole check was written against.
if (!declaredModifiers(openapiSource).includes('SecurityAddon')) {
  failures.push(
    'crates/rg-http/src/openapi.rs no longer declares modifiers(&SecurityAddon) — the published ' +
      'document would carry no components.securitySchemes, so Swagger UI loses its "Authorize" ' +
      'button and every operation reads as anonymous however the route table gates it.',
  );
}

const documented = documentedHandlers(openapiSource);
// A parse that understood almost nothing would report the whole surface as
// undocumented — loud, but for the wrong reason. Say so directly instead.
if (documented.size < 200) {
  throw new Error(
    `Only ${documented.size} handlers parsed out of the paths(...) list in ${OPENAPI} ` +
      '(expected at least 200). The list form probably changed — fix documentedHandlers().',
  );
}

// One row per registration; a handler mounted under several URLs collapses to
// one entry whose value carries every registration, so a failure names all of
// them.
const mounted = new Map();
for (const row of loadMountedHandlers(ROUTER)) {
  if (!row.handler.startsWith('api::')) continue;
  const rows = mounted.get(row.handler) ?? [];
  rows.push(row);
  mounted.set(row.handler, rows);
}

/** How a mounted registration reads in a failure message. */
const describeMount = (row) =>
  `${row.method} ${row.path === null ? '<path built at registration>' : (row.prefix ?? '<prefix unresolved>') + row.path}`;

for (const [handler, rows] of [...mounted].sort()) {
  const urls = rows.map(describeMount);
  if (documented.has(handler)) {
    if (UNDOCUMENTED.has(handler)) {
      failures.push(
        `${handler} is in the UNDOCUMENTED allowlist but IS documented — remove the entry so the ` +
          'exemption cannot outlive its reason.',
      );
    }
    continue;
  }
  if (UNDOCUMENTED.has(handler)) continue;
  failures.push(
    `${handler} is mounted (${urls.join(', ')}) but absent from paths(...) in crates/rg-http/src/openapi.rs — ` +
      'annotate it with #[utoipa::path(...)] and register it, or add it to UNDOCUMENTED with a reason.',
  );
}

for (const handler of [...documented].sort()) {
  if (mounted.has(handler)) {
    if (UNMOUNTED.has(handler)) {
      failures.push(
        `${handler} is in the UNMOUNTED allowlist but IS mounted — remove the entry, the drift it ` +
          'tracked is gone.',
      );
    }
    continue;
  }
  if (UNMOUNTED.has(handler)) continue;
  failures.push(
    `${handler} is documented in paths(...) but no route mounts it — the spec advertises an endpoint ` +
      'that answers 404. Mount it, drop it from paths(...), or add it to UNMOUNTED with a card id.',
  );
}

// ── The second declaration: (method, URL) ─────────────────────────────────
//
// Only handlers that are both mounted and documented are compared — the two
// loops above already own every other combination. A handler mounted under
// several URLs carries a single annotation and so can only match one of them;
// that is the spec's limit, not a drift, and the uncovered registrations are
// reported as a number rather than failed on.

const annotations = loadUtoipaPaths(API_DIR);
const uncomparable = [];
let extraMounts = 0;
let inputCompared = 0;

/** The outer extractor/type name of one handler parameter. */
function baseParamType(type) {
  const withoutReference = type
    .trim()
    .replace(/^&(?:'\w+\s+)?/, '')
    .replace(/^mut\s+/, '');
  const head = withoutReference.split(/[<\s]/, 1)[0];
  return head.split('::').at(-1);
}

/** The `T` in an outer `Query<T>` extractor type. */
function queryParamType(type) {
  const match = /(?:^|::)Query\s*<([\s\S]+)>$/.exec(type.trim());
  return match?.[1].trim() ?? null;
}

/**
 * What the handler signature says this operation consumes.
 *
 * These are transport inputs, not arbitrary values used by the handler:
 * `NamespaceCreate`/`NamespaceWrite` are the two local `FromRequest` body
 * extractors; the remaining names are axum's built-ins.
 */
function handlerInput(annotation) {
  if (annotation.signatureParams === null) return null;
  const params = splitRustParams(annotation.signatureParams);
  if (params === null) return null;
  const types = [];
  let queryType = null;
  for (const param of params) {
    const type = rustParamType(param);
    if (type === null) return null;
    types.push(baseParamType(type));
    queryType ??= queryParamType(type);
  }
  const bodyTypes = new Set([
    'Body',
    'Bytes',
    'Json',
    'Multipart',
    'NamespaceCreate',
    'NamespaceWrite',
    'String',
  ]);
  return {
    body: types.some((type) => bodyTypes.has(type)),
    query: types.includes('Query'),
    queryType,
  };
}

// The other half of the single-prefix rule: every annotation is relative to the
// `servers` entry asserted above. This is checked over every annotation found,
// not just the mounted-and-documented ones compared below, because a prefixed
// path is wrong wherever it is written.
for (const [handler, annotation] of [...annotations].sort()) {
  if (annotation.path === null) continue;
  if (annotation.path === API_PREFIX || annotation.path.startsWith(`${API_PREFIX}/`)) {
    failures.push(
      `${handler} is annotated path = "${annotation.path}" (${annotation.file}:${annotation.line}) — the ` +
        `${API_PREFIX} prefix belongs to the document's servers(...) entry alone, so this publishes ` +
        `${API_PREFIX}${annotation.path}. Write the path relative to the router's nest prefix.`,
    );
  }
}

// ── The third state: annotated, but neither mounted nor documented ────────
//
// The two loops above compare `mounted` against `documented`, so a handler that
// is in NEITHER set is invisible to both. `api::ai::ai_index_repository` sat in
// exactly that gap: a complete handler carrying a full `#[utoipa::path(post,
// …)]` annotation, absent from the router and absent from `paths(...)` — a door
// described but never cut, and the write half of a search feature whose read
// half was live (card_928d72df493a). Nothing here was wrong by the old rules,
// which is precisely why it survived.
//
// The annotation is what makes this checkable: writing one is a claim that the
// handler serves an endpoint, and an unmounted handler makes that claim false.
for (const [handler, annotation] of [...annotations].sort()) {
  if (mounted.has(handler) || documented.has(handler)) continue;
  failures.push(
    `${handler} carries a #[utoipa::path(...)] annotation (${annotation.file}:${annotation.line}) but is ` +
      'neither mounted by the route table nor listed in paths(...) — the endpoint is described and does ' +
      'not exist. Mount it and document it, or delete the handler together with its annotation.',
  );
}

// ── The third declaration: operation input ────────────────────────────────
//
// `utoipa` does not infer these inputs in this crate. Without the declaration,
// Swagger UI has no file picker/query field and generated clients do not expose
// the value at all. The handler signature is the production source of truth;
// the annotation is only accepted when it agrees in both directions.
for (const [handler, annotation] of [...annotations].sort()) {
  if (!documented.has(handler)) continue;
  const input = handlerInput(annotation);
  if (input === null) {
    failures.push(
      `${handler}: could not read the handler signature attributed to #[utoipa::path(...)] at ` +
        `${annotation.file}:${annotation.line} — fix parseUtoipaPaths()/splitRustParams(), not the assertion.`,
    );
    continue;
  }
  inputCompared += 1;

  if (input.body !== annotation.declaresRequestBody) {
    failures.push(
      `${handler} ${input.body ? 'takes a request body' : 'takes no request body'} but its annotation at ` +
        `${annotation.file}:${annotation.line} ${annotation.declaresRequestBody ? 'declares' : 'does not declare'} ` +
        '`request_body` — the published operation and handler signature disagree.',
    );
  }
  const queryTypeName = input.queryType?.split('::').at(-1);
  const queryDeclared =
    annotation.paramsBody !== null &&
    (/\bQuery\b/.test(annotation.paramsBody) ||
      (queryTypeName !== undefined &&
        new RegExp(`\\b${queryTypeName.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')}\\b`).test(
          annotation.paramsBody,
        )));
  if (input.query && !queryDeclared) {
    failures.push(
      `${handler} takes Query<${input.queryType ?? '?'}> but params(...) at ` +
        `${annotation.file}:${annotation.line} declares neither query tuples nor that IntoParams type — ` +
        'generated clients and Swagger UI cannot supply the query input.',
    );
  }
}

for (const [handler, rows] of [...mounted].sort()) {
  if (!documented.has(handler)) continue;

  const annotation = annotations.get(handler);
  // Rust would not compile a `paths(...)` entry without the annotation, so
  // failing to find one means this script cannot read it. Say so rather than
  // scoring an unread handler as agreeing.
  if (!annotation) {
    failures.push(
      `${handler} is listed in paths(...) but no #[utoipa::path(...)] annotation was found for it under ` +
        'crates/rg-http/src/api — fix loadUtoipaPaths() in scripts/lib/rust-source.mjs.',
    );
    continue;
  }
  if (annotation.method === null || annotation.path === null) {
    failures.push(
      `${handler}: could not read ${annotation.method === null ? 'the method' : 'path = "…"'} out of the ` +
        `#[utoipa::path(...)] at ${annotation.file}:${annotation.line} — fix the annotation or the parser.`,
    );
    continue;
  }

  const comparable = rows.filter((row) => row.path !== null && row.prefix !== null);
  if (comparable.length === 0) {
    // A loop-registered row has no URL literal to compare, and a row whose
    // sub-router could not be resolved has no base. Either way the method still
    // has to agree — asserting half of the pair beats asserting neither.
    uncomparable.push(`${handler} (${rows.map(describeMount).join(', ')})`);
    if (!rows.some((row) => row.method === annotation.method)) {
      failures.push(
        `${handler} is documented as ${annotation.method} (${annotation.file}:${annotation.line}) but is ` +
          `mounted as ${[...new Set(rows.map((row) => row.method))].join(', ')} — the URL could not be ` +
          'compared, the method disagrees outright.',
      );
    }
    continue;
  }

  const mountedUrls = comparable.map((row) => `${row.method} ${canonicalUrl(row.prefix + row.path)}`);
  const documentedUrl = `${annotation.method} ${canonicalUrl(annotation.path)}`;
  if (!mountedUrls.includes(documentedUrl)) {
    failures.push(
      `${handler} is documented as ${documentedUrl} (${annotation.file}:${annotation.line}) but mounted as ` +
        `${mountedUrls.join(', ')} — the annotation and the route table declare different endpoints, and ` +
        'the published spec is built from the annotation.',
    );
    continue;
  }
  extraMounts += mountedUrls.length - 1;
}

for (const handler of UNDOCUMENTED.keys()) {
  if (!mounted.has(handler)) {
    failures.push(`UNDOCUMENTED names ${handler}, which is not mounted — remove or fix the entry.`);
  }
}
for (const handler of UNMOUNTED.keys()) {
  if (!documented.has(handler)) {
    failures.push(`UNMOUNTED names ${handler}, which is not documented — remove or fix the entry.`);
  }
}

// ── The fourth comparison: a published schema no operation names ─────────
//
// `components(schemas(...))` is a separate declaration from `paths(...)`, and
// nothing ties the two together. A type listed there reaches the published
// document — and every generated client — whether or not a single operation
// refers to it. `ForkRequest { org }` sat there for months while the fork
// handler took no body at all: a client that believed the schema sent
// `{"org": "acme"}`, got `201`, and found its fork under a personal account.
// The field did not fail, it disappeared (card_98be888fb9fc).
//
// So: every registered schema must be named by at least one `#[utoipa::path]`
// annotation, or reached through another type that is. The second half matters
// — most schemas are nested response types no operation lists directly — so the
// rule is "somebody refers to this", not "an operation lists it".
// The list itself is idents, not strings, so it is read out of the view with
// string literals blanked as well: a Rust string spelling a `components(
// schemas(…))` block ahead of the real one would otherwise be the list this
// sweep quantifies over. Same reason the annotation bodies below are taken
// string-free.
const registeredSchemas = (() => {
  const src = stripRustNonCode(openapiSource);
  const block = src.match(/\bcomponents\(\s*schemas\(([\s\S]*?)\n {8}\)\n {4}\),/);
  if (!block) {
    throw new Error(
      `Could not find the components(schemas(...)) list in ${OPENAPI}. The #[openapi(...)] form ` +
        'changed — fix this check rather than deleting the assertion.',
    );
  }
  return block[1]
    .split(',')
    .map((entry) => entry.trim())
    .filter((entry) => entry.startsWith('crate::'));
})();

if (registeredSchemas.length < 60) {
  failures.push(
    `only ${registeredSchemas.length} schemas parsed out of components(...) — the parse broke, ` +
      'so this comparison would pass vacuously.',
  );
}

// Where a type can be named, in two views of the same sources.
//
// The whole crate, not just `api/`: a schema can live anywhere (`crate::
// pagination::PaginationMeta` does) and be reached from anywhere.
//
// `openapi.rs` itself is excluded, and that exclusion is the difference between
// a gate and a formality: the `components(schemas(...))` line being checked is
// itself a mention of the type, so leaving the file in makes every schema
// vindicate itself and the comparison can never fail. Found by re-adding
// `ForkRequest` and watching the check stay green.
//
// Strings are blanked, not kept. A type name that appears only inside some
// `description = "…"` or error message is prose about the schema, not a use of
// it, and counting it would let a schema vindicate itself with a sentence — the
// same self-vindication the `openapi.rs` exclusion above exists to prevent.
const HTTP_SRC = join(root, 'crates/rg-http/src');
const apiBlob = readdirSync(HTTP_SRC, { recursive: true })
  .filter((name) => String(name).endsWith('.rs') && join(HTTP_SRC, String(name)) !== OPENAPI)
  .map((name) => stripRustNonCode(readFileSync(join(HTTP_SRC, String(name)), 'utf8')))
  .join('\n');

// Every `#[utoipa::path(...)]` attribute body, concatenated: this is where
// `request_body(content = X)` and `body = X` are written.
//
// Taken from the annotations already parsed above, not re-derived here. This
// file used to cut them out of `apiBlob` with a private
// `/#\[utoipa::path\(([\s\S]*?)\n\)\]/g` — a third copy of a parser the same
// file already imports, and one that closed an annotation on the literal text
// `\n)]` instead of on paren balance. It read 267 bodies where the shared
// parser reads 297, and the 30 it lost were not lost cleanly: a non-greedy
// match that misses its own closer runs on to the *next* annotation's, so
// bodies were also silently fused with the handler code between them. The
// orphan sweep below was therefore quantified over a corpus ~10% smaller than
// the one its own comment described (card_94bf0d4fe5a5).
const annotationBodies = [...annotations.values()].map((row) => row.codeBody);
const annotationBlob = annotationBodies.join('\n');

// The floor the missing 30 slipped under. `registeredSchemas` had one; the
// annotation corpus — the other half of the same comparison — had none, so a
// parser that understood a third of the annotations would still have reported
// "all schemas are named".
//
// It is an equality against the openers actually present in the crate, not a
// magic number: `annotations` is swept from `api/` alone, so this also fails if
// an annotation is ever written outside that tree, where this sweep would never
// have seen it.
const declaredAnnotations = (apiBlob.match(/#\[utoipa::path\(/g) ?? []).length;
if (annotationBodies.length < declaredAnnotations) {
  failures.push(
    `${annotationBodies.length} #[utoipa::path] bodies were read for the schema sweep but ` +
      `${declaredAnnotations} are declared in ${HTTP_SRC} — the sweep would clear schemas it never ` +
      'looked for. Annotations outside the api/ tree are not swept; move them, or teach this check ' +
      'to load them.',
  );
}

let orphanSchemas = 0;
for (const schema of registeredSchemas) {
  const name = schema.split('::').pop();
  const word = new RegExp(`\\b${name}\\b`);
  if (word.test(annotationBlob)) continue;
  // Second chance: reached through another type's field rather than named by an
  // operation. Its own declaration does not count as a use of itself.
  const withoutDeclaration = apiBlob.replace(
    new RegExp(`(struct|enum)\\s+${name}\\b`, 'g'),
    '$1 __declaration__',
  );
  if (word.test(withoutDeclaration)) continue;
  orphanSchemas += 1;
  failures.push(
    `${schema} is published in components(schemas(...)) and nothing names it — the spec offers a ` +
      'type the server never reads. Wire it to a route, or drop it from components.',
  );
}

const exempt = UNDOCUMENTED.size;
const covered = [...mounted.keys()].filter((handler) => documented.has(handler)).length;

if (failures.length > 0) {
  console.log(`❌ OpenAPI route coverage: ${failures.length} problem(s)\n`);
  for (const failure of failures) console.log(`  - ${failure}`);
  process.exit(1);
}

// What the URL comparison could NOT reach is printed, not implied. A skip that
// nobody can see is indistinguishable from a check that passed, which is how a
// gate rots into a formality.
console.log(
  `✅ OpenAPI route coverage: ${covered}/${mounted.size} mounted api::* handlers documented ` +
    `(${exempt} protocol handlers intentionally exempt, ${UNMOUNTED.size} documented-but-unmounted tracked)`,
);
console.log(
  `   method+URL agreed for ${covered - uncomparable.length}/${covered} of them; ` +
    `${uncomparable.length} compared by method only, ${extraMounts} extra mount(s) no annotation can describe`,
);
console.log(`   request body/query declarations agreed with ${inputCompared} published handler signatures`);
console.log(`   ${registeredSchemas.length} published schemas are all named by an operation or reached through one`);
for (const entry of uncomparable) console.log(`   - URL not comparable: ${entry}`);
