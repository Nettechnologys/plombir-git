#!/usr/bin/env node

import { existsSync, readFileSync } from 'node:fs';

const allPackagesPagePath = 'web/src/routes/[owner]/[repo]/packages/+page.svelte';
const formatPackagesPagePath = 'web/src/routes/[owner]/[repo]/packages/[format]/+page.svelte';
const detailPagePath = 'web/src/routes/[owner]/[repo]/packages/[format]/[...name]/+page.svelte';
const clientPath = 'web/src/lib/api/client.svelte.ts';

const failures = [];

if (!existsSync(detailPagePath)) {
  failures.push('Package detail page must use a catch-all route so scoped package names can contain slash separators.');
}

const allPackagesPage = readFileSync(allPackagesPagePath, 'utf8');
const formatPackagesPage = readFileSync(formatPackagesPagePath, 'utf8');
const client = readFileSync(clientPath, 'utf8');

for (const [label, source] of [
  ['All packages page', allPackagesPage],
  ['Package format page', formatPackagesPage],
]) {
  if (!/function\s+encodePackageRouteName\s*\(\s*name:\s*string\s*\)/.test(source)) {
    failures.push(`${label} must centralize package route-name encoding.`);
  }

  if (!/name\.split\(['"]\/['"]\)\.map\(encodeURIComponent\)\.join\(['"]\/['"]\)/.test(source)) {
    failures.push(`${label} links must encode package name segments while preserving slash separators for the catch-all route.`);
  }

  if (/encodeURIComponent\(pkg\.name\)/.test(source)) {
    failures.push(`${label} links must not encode the full package name into one path segment.`);
  }
}

if (!/params\.name/.test(readFileSync(detailPagePath, 'utf8'))) {
  failures.push('Package detail page must read the catch-all name route param.');
}

if (!/packages\/\$\{encodeURIComponent\(pkg_type\)\}\/\$\{encodeURIComponent\(pkg_name\)\}/.test(client)) {
  failures.push('API client package detail calls must still encode the package name as one backend path parameter.');
}

if (failures.length > 0) {
  console.error('Package detail route frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Package detail route frontend/backend contract ok');
