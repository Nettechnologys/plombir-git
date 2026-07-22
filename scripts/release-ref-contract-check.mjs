#!/usr/bin/env node

import { readFileSync } from 'node:fs';

const backendPath = 'crates/rg-http/src/api/repo_content.rs';
const mainClientPath = 'web/src/lib/api/client.svelte.ts';
const splitClientPath = 'web/src/lib/api/repos.ts';
const newReleasePagePath = 'web/src/routes/[owner]/[repo]/releases/new/+page.svelte';

const backend = readFileSync(backendPath, 'utf8');
const mainClient = readFileSync(mainClientPath, 'utf8');
const splitClient = readFileSync(splitClientPath, 'utf8');
const newReleasePage = readFileSync(newReleasePagePath, 'utf8');

const failures = [];

if (!/fn list_branch_names[\s\S]*?anyhow::Result<Vec<String>>/.test(backend)) {
  failures.push('Backend branch listing contract changed; update client normalization or this check');
}

if (!/fn list_tag_names[\s\S]*?anyhow::Result<Vec<String>>/.test(backend)) {
  failures.push('Backend tag listing contract changed; update client normalization or this check');
}

for (const [name, source] of [
  ['client.svelte.ts', mainClient],
  ['repos.ts', splitClient],
]) {
  if (!/type BranchRefResponse\s*=\s*string\s*\|/.test(source)) {
    failures.push(`${name} must accept backend branch string responses`);
  }

  if (!/type TagRefResponse\s*=\s*string\s*\|/.test(source)) {
    failures.push(`${name} must accept backend tag string responses`);
  }

  if (!/branches:\s*\([^)]*\)\s*=>[\s\S]*?request<BranchRefResponse\[\]>[\s\S]*?\.then\(\(branches\)\s*=>\s*branches\.map\(normalizeBranchRef\)\)/.test(source)) {
    failures.push(`${name} repos.branches must normalize string branch refs to objects`);
  }

  if (!/tags:\s*\([^)]*\)\s*=>[\s\S]*?request<TagRefResponse\[\]>[\s\S]*?\.then\(\(tags\)\s*=>\s*tags\.map\(normalizeTagRef\)\)/.test(source)) {
    failures.push(`${name} repos.tags must normalize string tag refs to objects`);
  }
}

if (!/branches\s*=\s*branchList\.map\(b\s*=>\s*b\.name\)/.test(newReleasePage)) {
  failures.push('New release page should consume normalized branch objects');
}

if (!/tags\s*=\s*tagList\.map\(t\s*=>\s*t\.name\)/.test(newReleasePage)) {
  failures.push('New release page should consume normalized tag objects');
}

if (failures.length > 0) {
  console.error('Release ref frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Release ref frontend/backend contract ok');
