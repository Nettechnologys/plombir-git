#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const pagePath = path.join(root, 'web/src/routes/[owner]/[repo]/pulls/[number]/+page.svelte');
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const backendPath = path.join(root, 'crates/rg-core/src/review/service.rs');

const page = readFileSync(pagePath, 'utf8');
const client = readFileSync(clientPath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');
const failures = [];

if (!/"approve"\s*=>\s*Ok\(Self::Approve\)/.test(backend)) {
  failures.push('Backend review action parser must accept the canonical approve action.');
}

if (/value="approved"/.test(page)) {
  failures.push('PR review approve radio must submit backend action "approve", not display label "approved".');
}

if (!/value="approve"/.test(page)) {
  failures.push('PR review approve radio must use value="approve".');
}

if (!/body:\s*JSON\.stringify\(\{\s*body,\s*action:\s*verdict\s*\}\)/.test(client)) {
  failures.push('API client must send PR review verdict as the backend action field.');
}

if (!/reviewAction\(review\)/.test(page) || !/review\.action\s*\|\|\s*review\.verdict/.test(page)) {
  failures.push('PR review list must render backend action values, with verdict only as a compatibility fallback.');
}

if (/class:approved=\{review\.verdict/.test(page) || /pulls\.verdict\.\$\{review\.verdict/.test(page)) {
  failures.push('PR review list must not render directly from the absent backend verdict field.');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('PR review frontend/backend contract ok');
