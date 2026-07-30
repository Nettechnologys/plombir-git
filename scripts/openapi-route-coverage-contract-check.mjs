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

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { loadMountedHandlers, loadUtoipaPaths, stripRustComments } from './lib/rust-source.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ROUTER = join(root, 'crates/rg-http/src/routes.rs');
const OPENAPI = join(root, 'crates/rg-http/src/openapi.rs');
const API_DIR = join(root, 'crates/rg-http/src/api');

// The nest prefix of the REST router. Annotations are written both with it
// (`/api/v1/ai/…`) and without (`/repos/…`), so both sides are canonicalised
// to the prefix-less spelling before comparison — the same convention
// `api-client-contract-check.mjs` normalises to via OPENAPI_BASE_PATH.
//
// That the two spellings coexist in the spec at all is a separate defect
// (card_b23fa617838f): utoipa publishes the annotation string verbatim and the
// document declares no `servers`, so most paths are short by this prefix. It is
// deliberately NOT this gate's business — this one asserts that the two
// declarations of a route agree, not which of them the spec should print.
const API_PREFIX = '/api/v1';

/** A URL as the spec spells it: relative to the REST prefix, if it carries one. */
function canonicalUrl(url) {
  if (url === API_PREFIX) return '/';
  return url.startsWith(`${API_PREFIX}/`) ? url.slice(API_PREFIX.length) : url;
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
  ['api::packages::composer_packages_json', 'Composer repository protocol — layout fixed by composer'],
  ['api::packages::helm_index', 'Helm chart repository index.yaml — layout fixed by helm'],
  ['api::packages::maven_metadata', 'Maven repository layout — layout fixed by mvn/Gradle'],
  ['api::packages::maven_download', 'Maven repository layout — layout fixed by mvn/Gradle'],
  ['api::packages::npm_registry_metadata', 'npm registry metadata document — layout fixed by npm'],
  ['api::packages::nuget_service_index', 'NuGet V3 service index — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_registration_index', 'NuGet V3 registration index — layout fixed by dotnet/nuget'],
  ['api::packages::nuget_search', 'NuGet V3 search service — layout fixed by dotnet/nuget'],
  ['api::packages::pypi_simple_root_index', 'PyPI Simple repository API (PEP 503) — layout fixed by pip'],
  ['api::packages::pypi_simple_index', 'PyPI Simple repository API (PEP 503) — layout fixed by pip'],
  ['api::packages::rubygems_compact_versions', 'RubyGems compact index — layout fixed by gem/bundler'],
  ['api::packages::rubygems_compact_info', 'RubyGems compact index — layout fixed by gem/bundler'],
  ['api::packages::rubygems_compact_names', 'RubyGems compact index — layout fixed by gem/bundler'],
  ['api::packages::rubygems_dependencies', 'RubyGems dependency API — layout fixed by gem/bundler'],
  ['api::packages::rubygems_gem_info', 'RubyGems v1 gem info — layout fixed by gem/bundler'],
  ['api::packages::rubygems_gem_download', 'RubyGems .gem download path — derived by the client from the source URL'],
]);

// Handlers the spec advertises that no route mounts.
//
// Unlike the list above, an entry here is a KNOWN DEFECT under a card, not a
// design decision — the spec is lying and the fix belongs to whoever owns that
// card. It is listed so the gate can start blocking new drift today instead of
// waiting for the backlog, and it fails the moment the card lands.
const UNMOUNTED = new Map([
  [
    'api::runners::get_runner_admin',
    'card_a76ad95240d5 — GET /admin/runners/{id} is implemented and documented but never mounted',
  ],
]);

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

const documented = documentedHandlers(readFileSync(OPENAPI, 'utf8'));
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
for (const entry of uncomparable) console.log(`   - URL not comparable: ${entry}`);
