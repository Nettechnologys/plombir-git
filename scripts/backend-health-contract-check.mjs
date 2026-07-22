#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const basePath = path.join(root, 'web/src/lib/api/_base.svelte.ts');
const layoutPath = path.join(root, 'web/src/routes/+layout.svelte');

const base = readFileSync(basePath, 'utf8');
const layout = readFileSync(layoutPath, 'utf8');

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

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Backend health frontend/backend contract ok');
