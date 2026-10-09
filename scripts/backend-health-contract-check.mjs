#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionTsSource } from './lib/ts-source.mjs';

const root = process.cwd();
const basePath = path.join(root, 'web/src/lib/api/_base.svelte.ts');
const layoutPath = path.join(root, 'web/src/routes/+layout.svelte');

const base = productionTsSource(readFileSync(basePath, 'utf8'));
const layout = productionTsSource(readFileSync(layoutPath, 'utf8'));

const failures = [];

if (!/export function withBackendBase\(/.test(base)) {
  failures.push('Shared API base must export withBackendBase for non-/api/v1 backend routes');
}

if (!/API_BASE\.replace\(\s*\/\\\/api\\\/v1\$\/,\s*''\s*\)/.test(base)) {
  failures.push('withBackendBase must derive the backend origin from the configured API base');
}

if (!/import\s+\{\s*withBackendBase\s*\}\s+from '\$lib\/api\/_base'/.test(layout)) {
  failures.push('Root layout must import withBackendBase for backend health checks');
}

if (!/fetch\(\s*withBackendBase\('\/health'\)/.test(layout)) {
  failures.push('Root layout must fetch backend /health through the configured backend base');
}

if (/fetch\(\s*['"`]\/health['"`]/.test(layout)) {
  failures.push('Root layout still fetches same-origin /health directly');
}

// `/health` answers anonymous callers with the verdict only
// (`{"status":"ok"|"degraded"}`); `checks`, `version` and `commit` require an
// instance admin. The layout is the anonymous consumer, so it may only look at
// `status` — and must keep accepting `ok`, the value the minimal body carries
// (its other accepted value, `healthy`, predates the split).
if (!/\[\s*'healthy'\s*,\s*'ok'\s*\]/.test(layout)) {
  failures.push(
    "Root layout must accept the anonymous /health verdict (`ok`) — the minimal body has no checks to inspect",
  );
}

if (/body\??\.\s*(checks|version|commit)|body\??\[\s*['"](checks|version|commit)['"]\s*\]/.test(layout)) {
  failures.push(
    'Root layout must not read /health checks/version/commit: anonymous responses no longer carry them (admin-only detail)',
  );
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Backend health frontend/backend contract ok');
