#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const mainClientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const splitClientPath = path.join(root, 'web/src/lib/api/releases.ts');
const baseClientPath = path.join(root, 'web/src/lib/api/_base.svelte.ts');
const releasesPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/releases/+page.svelte');
const repoHeaderPath = path.join(root, 'web/src/lib/components/RepoHeader.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/releases.rs');
const archiveBackendPath = path.join(root, 'crates/rg-http/src/api/archive.rs');

const mainClient = readFileSync(mainClientPath, 'utf8');
const splitClient = readFileSync(splitClientPath, 'utf8');
const baseClient = readFileSync(baseClientPath, 'utf8');
const page = readFileSync(releasesPagePath, 'utf8');
const repoHeader = readFileSync(repoHeaderPath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');
const archiveBackend = readFileSync(archiveBackendPath, 'utf8');

const failures = [];

const requiredRoutes = [
  'releases/{release_id}/assets',
  'releases/assets/{asset_id}/download',
  'releases/assets/{asset_id}',
];

for (const route of requiredRoutes) {
  if (!backend.includes(route)) {
    failures.push(`Backend release asset route missing from OpenAPI annotations: ${route}`);
  }
}

for (const [name, source] of [
  ['client.svelte.ts', mainClient],
  ['releases.ts', splitClient],
]) {
  for (const method of ['listAssets', 'uploadAsset', 'getAsset', 'assetDownloadUrl', 'downloadAsset', 'deleteAsset']) {
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

  if (!/assetDownloadUrl:\s*\([^)]*owner[^)]*repo[^)]*assetId[^)]*\)\s*=>\s*\n?\s*`\$\{API_BASE\}\/repos\/\$\{encodeURIComponent\(owner\)\}\/\$\{encodeURIComponent\(repo\)\}\/releases\/assets\/\$\{assetId\}\/download`/.test(source)) {
    failures.push(`${name} assetDownloadUrl must URL-encode owner/repo path segments`);
  }

  if (!/downloadAsset:\s*\([^)]*owner[^)]*repo[^)]*assetId[^)]*filename[^)]*\)\s*=>[\s\S]*?downloadApiFile\(/.test(source)) {
    failures.push(`${name} downloadAsset must fetch through the API helper so Bearer auth is sent`);
  }
}

if (!/export async function downloadApiFile/.test(baseClient) || !/headers\['Authorization'\]\s*=\s*`Bearer \$\{token\}`/.test(baseClient)) {
  failures.push('_base.svelte.ts downloadApiFile must attach Bearer auth when a token exists');
}

if (!/parse_filename_from_disposition/.test(backend) || !/filename\*=/.test(backend)) {
  failures.push('Backend release asset upload must parse RFC 5987 Content-Disposition filenames');
}

if (!/releases\.listAssets\(/.test(page)) {
  failures.push('Releases page must load release assets from the backend');
}

if (!/releases\.downloadAsset\(/.test(page)) {
  failures.push('Releases page must download release assets through the authenticated API helper');
}

if (/<a\s+class="asset-link"\s+href=\{releases\.assetDownloadUrl\(/.test(page)) {
  failures.push('Releases page must not use raw asset download links because they drop Bearer auth');
}

if (!/extract_user_id/.test(backend) || !/can_read_repo/.test(backend)) {
  failures.push('Backend release asset downloads must enforce repo read access');
}

if (!/extract_user_id/.test(archiveBackend) || !/can_read_repo/.test(archiveBackend)) {
  failures.push('Backend repository archive downloads must enforce repo read access');
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
