#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const files = {
  monolithClient: path.join(root, 'web/src/lib/api/client.svelte.ts'),
  splitClient: path.join(root, 'web/src/lib/api/repos.ts'),
  orgPage: path.join(root, 'web/src/routes/orgs/[name]/+page.svelte'),
  backend: path.join(root, 'crates/rg-http/src/api/repos.rs'),
};

const monolithClient = readFileSync(files.monolithClient, 'utf8');
const splitClient = readFileSync(files.splitClient, 'utf8');
const orgPage = readFileSync(files.orgPage, 'utf8');
const backend = readFileSync(files.backend, 'utf8');
const failures = [];

function expectCreateObjectContract(label, source) {
  const createBlock = source.match(/create:\s*\([^)]*\)\s*=>\s*\n?\s*request<[\s\S]*?\/repos[\s\S]*?\n\s*\}\),/);
  if (!createBlock) {
    failures.push(`${label} must expose repos.create for POST /repos`);
    return;
  }

  if (!/create:\s*\(\s*opts\s*:/.test(createBlock[0])) {
    failures.push(`${label} repos.create must accept the backend CreateRepoRequest object, not positional args`);
  }

  if (!/body:\s*JSON\.stringify\(opts\)/.test(createBlock[0])) {
    failures.push(`${label} repos.create must serialize the full options object so org/template fields reach the backend`);
  }
}

expectCreateObjectContract('web/src/lib/api/client.svelte.ts', monolithClient);
expectCreateObjectContract('web/src/lib/api/repos.ts', splitClient);

if (!/pub struct CreateRepoRequest[\s\S]*pub org:\s*Option<String>/.test(backend)) {
  failures.push('Backend CreateRepoRequest must keep org as an optional repository owner field');
}

if (!/body\.org[\s\S]*get_org_by_name/.test(backend)) {
  failures.push('Backend create_repo must resolve the org field instead of ignoring it');
}

if (!/repos\.create\(\s*\{[\s\S]*name:\s*newRepoName[\s\S]*is_private:\s*newRepoPrivate[\s\S]*org:\s*page\.params\.name!/.test(orgPage)) {
  failures.push('Organization page must create repositories with an object payload including the org owner');
}

if (/repos\.create\(\s*newRepoName\s*,/.test(orgPage)) {
  failures.push('Organization page must not call the stale positional repos.create API');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Organization repository creation frontend/backend contract ok');
