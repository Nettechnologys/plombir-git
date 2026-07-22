#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const pagePath = path.join(root, 'web/src/routes/[owner]/[repo]/packages/+page.svelte');
const formatPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/packages/[format]/+page.svelte');
const uploadPath = path.join(root, 'web/src/routes/[owner]/[repo]/packages/upload/+page.svelte');
const packageFormatsPath = path.join(root, 'web/src/lib/packageFormats.ts');
const backendPackageServicePath = path.join(root, 'crates/rg-core/src/package_registry/service.rs');
const httpLibPath = path.join(root, 'crates/rg-http/src/lib.rs');

const client = readFileSync(clientPath, 'utf8');
const page = readFileSync(pagePath, 'utf8');
const formatPage = readFileSync(formatPagePath, 'utf8');
const uploadPage = readFileSync(uploadPath, 'utf8');
const detailPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/packages/[format]/[...name]/+page.svelte');
const detailPage = readFileSync(detailPagePath, 'utf8');
const packageFormats = readFileSync(packageFormatsPath, 'utf8');
const backendPackageService = readFileSync(backendPackageServicePath, 'utf8');
const httpLib = readFileSync(httpLibPath, 'utf8');

const failures = [];

function extractQuotedArray(source, name) {
  const match = source.match(new RegExp(`const\\s+${name}\\s*=\\s*\\[([\\s\\S]*?)\\]`));
  if (!match) return null;
  return [...match[1].matchAll(/['"]([^'"]+)['"]/g)].map((m) => m[1]);
}

function extractBackendPackageTypes(source) {
  const constants = new Map();
  const moduleMatch = source.match(/pub\s+mod\s+package_types\s*\{([\s\S]*?)\n\}/);
  const moduleSource = moduleMatch?.[1] || source;

  for (const match of moduleSource.matchAll(/pub\s+const\s+([A-Z0-9_]+)\s*:\s*&str\s*=\s*"([^"]+)"/g)) {
    constants.set(match[1], match[2]);
  }

  const allMatch = moduleSource.match(/pub\s+const\s+ALL\s*:\s*&\[&str\]\s*=\s*&\[([\s\S]*?)\];/);
  if (!allMatch) return null;

  return [...allMatch[1].matchAll(/\b([A-Z0-9_]+)\b/g)]
    .map((m) => constants.get(m[1]))
    .filter(Boolean);
}

if (!/list:\s*async\s*\([^)]*query\?:\s*string/.test(client)) {
  failures.push('packages.list must accept an optional search query');
}

if (!/function\s+filterPackagesByQuery\s*\(/.test(client)) {
  failures.push('packages.list must normalize package search through filterPackagesByQuery');
}

if (!/res\.status\s*===\s*204[\s\S]*return\s+undefined\s+as\s+T/.test(client)) {
  failures.push('API client request() must treat 204 No Content as a successful empty response');
}

if (/delete:\s*\([^)]*version[^)]*\)\s*=>\s*\n?\s*request<\{\s*deleted:\s*boolean\s*\}>/.test(client)) {
  failures.push('packages.delete must not expect a JSON deleted envelope from the backend 204 response');
}

if (!/interface\s+PackageFileResponse[\s\S]*filename:\s*string[\s\S]*size:\s*number/.test(client)) {
  failures.push('API client must type package version files returned by the backend');
}

if (/getVersions:[\s\S]*versions:\s*\(res\.versions\s*\|\|\s*\[\]\)\.map\(\(v\)\s*=>\s*v\.version\)/.test(client)) {
  failures.push('packages.getVersions must preserve backend version file metadata');
}

if (!/downloadUrl:\s*\([^)]*filename:\s*string[\s\S]*\/packages\/\$\{encodeURIComponent\(pkg_type\)\}\/\$\{encodeURIComponent\(pkg_name\)\}\/\$\{encodeURIComponent\(version\)\}\/\$\{encodeRepoPath\(filename\)\}/.test(client)) {
  failures.push('API client must expose a package file download URL builder for the backend download route');
}

if (!/\/repos\/\{owner\}\/\{name\}\/packages\/\{pkg_type\}\/\{pkg_name\}\/\{version\}\/\{\*file\}/.test(httpLib)) {
  failures.push('Backend package download route must use Axum rest capture so filenames with subpaths can be downloaded');
}

const filteredIndex = client.indexOf('filteredList');
const sliceIndex = client.indexOf('filteredList.slice');
if (filteredIndex === -1 || sliceIndex === -1 || filteredIndex > sliceIndex) {
  failures.push('packages.list must filter package results before paginating them');
}

if (!/packages\.list\([\s\S]*searchQuery[\s\S]*\)/.test(page)) {
  failures.push('Packages page must pass searchQuery into packages.list');
}

if (!/function\s+encodePackageRouteName\s*\(\s*name:\s*string\s*\)[\s\S]*name\.split\(['"]\/['"]\)\.map\(encodeURIComponent\)\.join\(['"]\/['"]\)/.test(page)) {
  failures.push('Packages page must encode package name segments while preserving slash separators for scoped names');
}

if (!/function\s+packageHref\s*\([\s\S]*encodeURIComponent\(pkg\.format\)[\s\S]*encodePackageRouteName\(pkg\.name\)/.test(page)) {
  failures.push('Packages page must use catch-all-safe package name links');
}

if (/href="\/\{owner\}\/\{repo\}\/packages\/upload"/.test(page)) {
  failures.push('Packages page upload link must interpolate the current owner/repo route params');
}

if (!/href=\{`\/\$\{owner\}\/\$\{repo\}\/packages\/upload`\}/.test(page)) {
  failures.push('Packages page upload link must point to the current repository upload route');
}

if (!/function\s+encodePackageRouteName\s*\(\s*name:\s*string\s*\)[\s\S]*name\.split\(['"]\/['"]\)\.map\(encodeURIComponent\)\.join\(['"]\/['"]\)/.test(formatPage)) {
  failures.push('Package format page must encode package name segments while preserving slash separators for scoped names');
}

if (!/function\s+packageHref\s*\([\s\S]*encodeURIComponent\(format!\)[\s\S]*encodePackageRouteName\(pkg\.name\)/.test(formatPage)) {
  failures.push('Package format page must use catch-all-safe package name links');
}

if (/encodeURIComponent\(pkg\.name\)/.test(page + formatPage)) {
  failures.push('Package list pages must not encode scoped package names into one path segment');
}

if (!detailPagePath.includes('[...name]')) {
  failures.push('Package detail route must use a rest parameter so scoped package names containing slashes are routable');
}

if (/href="\/\{owner\}\/\{repo\}\/packages\/\{[^"]*\}\//.test(page + formatPage)) {
  failures.push('Package list pages must not interpolate raw package names into href attributes');
}

if (!/packageDownloadUrl\(version\.version,\s*file\.filename\)/.test(detailPage)) {
  failures.push('Package detail page must link version files to the backend package download route');
}

if (!/version\.files[\s\S]*file\.filename/.test(detailPage)) {
  failures.push('Package detail page must render backend package version files');
}

if (!/handleDeleteVersion[\s\S]*packages\.delete\([\s\S]*await\s+loadPackage\(\)/.test(detailPage)) {
  failures.push('Package detail page must reload package detail after deleting a version so latest_version stays in sync');
}

const backendTypes = extractBackendPackageTypes(backendPackageService);
const sharedTypes = extractQuotedArray(packageFormats, 'PACKAGE_FORMATS');

if (!backendTypes || backendTypes.length === 0) {
  failures.push('Could not extract backend package_types::ALL');
}

if (!sharedTypes || sharedTypes.length === 0) {
  failures.push('Could not extract shared package format list');
}

if (!/PACKAGE_FORMATS/.test(page)) {
  failures.push('Packages page must use the shared package format list');
}

if (!/PACKAGE_FORMATS/.test(uploadPage)) {
  failures.push('Package upload selector must use the shared package format list');
}

if (!/packageFormatLabel/.test(page + formatPage + uploadPage)) {
  failures.push('Package pages must use shared package format labels');
}

if (backendTypes && sharedTypes) {
  const missingFromShared = backendTypes.filter((type) => !sharedTypes.includes(type));
  const extraInShared = sharedTypes.filter((type) => !backendTypes.includes(type));
  if (missingFromShared.length > 0) {
    failures.push(`Shared package format list is missing backend package types: ${missingFromShared.join(', ')}`);
  }
  if (extraInShared.length > 0) {
    failures.push(`Shared package format list contains types absent from backend: ${extraInShared.join(', ')}`);
  }
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Package search frontend/backend contract ok');
