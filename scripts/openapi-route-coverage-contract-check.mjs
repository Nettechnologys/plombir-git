#!/usr/bin/env node

// Every mounted `api::*` handler must appear in the OpenAPI spec — and every
// handler the spec advertises must actually be mounted.
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
// Deliberate exemptions live in the two allowlists below, each with a reason.
// They are ratchets, not dumping grounds: an entry that stops applying — the
// handler was documented, or deleted, or finally mounted — fails the check, so
// the list cannot rot into a permanent blanket exemption.

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { loadMountedHandlers, stripRustComments } from './lib/rust-source.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const ROUTER = join(root, 'crates/rg-http/src/routes.rs');
const OPENAPI = join(root, 'crates/rg-http/src/openapi.rs');

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
// one entry whose value carries every URL, so a failure names all of them.
const mounted = new Map();
for (const row of loadMountedHandlers(ROUTER)) {
  if (!row.handler.startsWith('api::')) continue;
  const urls = mounted.get(row.handler) ?? [];
  urls.push(`${row.method} ${row.path ?? '<path built at registration>'}`);
  mounted.set(row.handler, urls);
}

for (const [handler, urls] of [...mounted].sort()) {
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

console.log(
  `✅ OpenAPI route coverage: ${covered}/${mounted.size} mounted api::* handlers documented ` +
    `(${exempt} protocol handlers intentionally exempt, ${UNMOUNTED.size} documented-but-unmounted tracked)`,
);
