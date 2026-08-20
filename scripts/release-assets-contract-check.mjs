#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import {
  loadRouteTable,
  parseUtoipaPaths,
  productionRustCode,
  productionRustSource,
  routeFailures,
  rustFnBlock,
} from './lib/rust-source.mjs';
import { productionTsSource } from './lib/ts-source.mjs';

const root = process.cwd();
const splitClientPath = path.join(root, 'web/src/lib/api/releases.ts');
const baseClientPath = path.join(root, 'web/src/lib/api/_base.svelte.ts');
const releasesPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/releases/+page.svelte');
const repoHeaderPath = path.join(root, 'web/src/lib/components/RepoHeader.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/releases.rs');
const archiveBackendPath = path.join(root, 'crates/rg-http/src/api/archive.rs');

const splitClient = productionTsSource(readFileSync(splitClientPath, 'utf8'));
const baseClient = productionTsSource(readFileSync(baseClientPath, 'utf8'));
const page = productionTsSource(readFileSync(releasesPagePath, 'utf8'));
const repoHeader = productionTsSource(readFileSync(repoHeaderPath, 'utf8'));
const backend = readFileSync(backendPath, 'utf8');
const archiveBackend = readFileSync(archiveBackendPath, 'utf8');

const failures = [];

// Read out of the parsed annotations, not out of the file's bytes. The message
// says "missing from OpenAPI annotations", but `backend.includes(route)` was
// satisfied by the route appearing in a comment or a doc example — commenting
// all three annotations out left this green (card_64b6ede78939).
const assetAnnotations = parseUtoipaPaths(
  backend,
  'api::releases',
  path.relative(root, backendPath).split(path.sep).join('/'),
);
const requiredRoutes = [
  'releases/{release_id}/assets',
  'releases/assets/{asset_id}/download',
  'releases/assets/{asset_id}',
];

for (const route of requiredRoutes) {
  if (!assetAnnotations.some((row) => row.path?.endsWith(`/${route}`))) {
    failures.push(`Backend release asset route missing from OpenAPI annotations: ${route}`);
  }
}

for (const [name, source] of [
  ['releases.ts', splitClient],
]) {
  for (const method of ['listAssets', 'uploadAsset', 'downloadAsset', 'deleteAsset']) {
    if (!new RegExp(`\\b${method}\\s*:`).test(source)) {
      failures.push(`${name} must expose releases.${method}`);
    }
  }

  if (!/Content-Disposition/.test(source) || !/contentDispositionAttachment\(file\.name \|\| 'asset'\)/.test(source)) {
    failures.push(`${name} uploadAsset must send release asset filenames through Content-Disposition`);
  }

  if (/x-asset-filename['"]:\s*file\.name/.test(source)) {
    failures.push(`${name} uploadAsset must not send raw file.name in x-asset-filename`);
  }

  if (/\b(?:getAsset|assetDownloadUrl)\s*:/.test(source)) {
    failures.push(`${name} must not expose dead release-asset metadata/raw-URL duplicates`);
  }

  if (!/downloadAsset:\s*\([^)]*owner[^)]*repo[^)]*assetId[^)]*filename[^)]*\)\s*=>[\s\S]*?downloadApiFile\(/.test(source)) {
    failures.push(`${name} downloadAsset must fetch through the API helper so Bearer auth is sent`);
  }
}

if (!/export async function downloadApiFile/.test(baseClient) || !/headers\['Authorization'\]\s*=\s*`Bearer \$\{token\}`/.test(baseClient)) {
  failures.push('_base.svelte.ts downloadApiFile must attach Bearer auth when a token exists');
}

// `filename*=` is read out of a string literal, so this needs the view that
// keeps literals but drops comments — a commented-out parser satisfied the raw
// grep while the upload path had stopped decoding RFC 5987 names.
const backendProduction = productionRustSource(backend);
if (!/parse_filename_from_disposition/.test(backendProduction) || !/filename\*=/.test(backendProduction)) {
  failures.push('Backend release asset upload must parse RFC 5987 Content-Disposition filenames');
}

if (!/releases\.listAssets\(/.test(page)) {
  failures.push('Releases page must load release assets from the backend');
}

if (!/releases\.downloadAsset\(/.test(page)) {
  failures.push('Releases page must download release assets through the authenticated API helper');
}

if (!/releases\.uploadAsset\(/.test(page) || !/releases\.deleteAsset\(/.test(page)) {
  failures.push('Releases page must expose production upload and delete callers for release assets');
}

if (!/asset-upload-progress/.test(page) || !/confirmDeleteAssetId/.test(page)) {
  failures.push('Releases page must show upload progress and require delete confirmation');
}

if (/<a\s+class="asset-link"\s+href=\{releases\.assetDownloadUrl\(/.test(page)) {
  failures.push('Releases page must not use raw asset download links because they drop Bearer auth');
}

// ── The read gate on the two download handlers ────────────────────────────
//
// This assertion used to grep the whole file for `extract_user_id` and
// `can_read_repo`. Both names were gone — the gate had moved into
// `api::repo_access` and then into extractors — so the check reported "no
// protection" about protection that had in fact become stricter, and the two
// reds were quarantined instead of read (card_0c926648b67d).
//
// It is asserted twice now, because one half alone is satisfiable without the
// other:
//
//   1. The router *declares* `RepoRead` for both routes. A downgrade to
//      `Public` is then a red here — and the declaration is what
//      `route_access_sweep_tests` drives three personas against, so a
//      declaration that does not match behaviour is red there.
//   2. The handler *takes* one of the gates. A route can keep its declared
//      level while the handler quietly stops asking, and (1) would not notice.
//
// The gate names for (2) are read out of the module that defines them rather
// than spelled out here — a second hard-coded list of symbol names is exactly
// what rotted the first time.

failures.push(
  ...routeFailures(loadRouteTable(path.join(root, 'crates/rg-http/src/routes.rs')), [
    {
      method: 'GET',
      path: '/repos/{owner}/{name}/releases/assets/{asset_id}/download',
      handler: 'api::releases::download_asset',
      access: 'RepoRead',
    },
    {
      method: 'GET',
      path: '/repos/{owner}/{name}/archive/{archive}',
      handler: 'api::archive::download_archive',
      access: 'RepoRead',
    },
  ]),
);

const repoAccess = readFileSync(path.join(root, 'crates/rg-http/src/api/repo_access.rs'), 'utf8');

// Extractors are the braced/generic `pub struct`s; the unit structs next to
// them (`RepoContents`, `Packages`) are scope markers, not gates.
// Derived from executable declarations only. A `pub struct` inside a block
// comment sits at column 0 exactly like a live gate, and so does a
// `#[cfg(test)]` fixture — both minted phantom gate names off the raw file
// (verified: `/* pub struct GhostGate { … } */` and a `#[cfg(test)] pub struct
// FixtureGate` were both listed). The assertion below is satisfied by *any*
// name in this list appearing in a handler signature, so one phantom entry
// weakens it without a word (card_64b6ede78939).
const repoAccessCode = productionRustCode(repoAccess);
const gates = [
  ...[...repoAccessCode.matchAll(/^pub struct (\w+)(?:<[^>]*>)?\s*\{/gm)].map((m) => m[1]),
  ...[...repoAccessCode.matchAll(/^pub(?:\(crate\))? async fn (require_\w+)/gm)].map((m) => m[1]),
];

if (gates.length === 0) {
  failures.push(
    'No gates found in api/repo_access.rs — this check can no longer read the module it ' +
      'derives them from, so its verdicts below mean nothing. Fix the parsing, not the handlers.',
  );
}

/// The parameter list of a handler, or null (with a failure recorded) if the
/// handler is not where this check expects it.
function handlerParams(source, file, handler) {
  // `rustFnBlock` anchors in the production code-only view and closes the
  // parameter list by matching parens. The regex it replaces read the raw
  // file, so a handler whose gate had been commented out — signature and all —
  // still handed back a parameter list carrying the gate name, and the check
  // below passed over a door that had stopped asking (card_64b6ede78939).
  const block = rustFnBlock(source, handler);
  if (!block) {
    failures.push(`${file} no longer defines a \`pub async fn ${handler}\` this check can read`);
    return null;
  }
  return block.params;
}

for (const [file, source, handler, what] of [
  ['crates/rg-http/src/api/releases.rs', backend, 'download_asset', 'Release asset downloads'],
  ['crates/rg-http/src/api/archive.rs', archiveBackend, 'download_archive', 'Repository archive downloads'],
]) {
  const params = handlerParams(source, file, handler);
  if (params === null || gates.length === 0) {
    continue;
  }

  if (!gates.some((gate) => new RegExp(`\\b${gate}\\b`).test(params))) {
    failures.push(
      `${what} must enforce repo read access: \`${handler}\` in ${file} takes none of the ` +
        `api::repo_access gates (${gates.join(', ')})`,
    );
  }
}

if (!/downloadApiFile\([\s\S]*?\/archive\/\$\{encodeURIComponent\(archiveRef\)\}\.zip/.test(repoHeader)) {
  failures.push('RepoHeader must download repository archives through the authenticated API helper');
}

if (/<a\s+href=\{archiveUrl\}/.test(repoHeader) || /let archiveUrl\s*=/.test(repoHeader)) {
  failures.push('RepoHeader must not use raw archive hrefs because they drop Bearer auth');
}

if (!/params\.set\('ref',\s*tag\)/.test(page)) {
  failures.push('Releases Browse files link must pass the release tag as ref, not path');
}

if (/params\.set\('path',\s*tag\)/.test(page)) {
  failures.push('Releases Browse files link still maps release tag into path');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Release assets frontend/backend contract ok');
