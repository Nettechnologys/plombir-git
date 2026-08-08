#!/usr/bin/env node

import { readFileSync } from 'node:fs';

const backendPath = 'crates/rg-http/src/api/repo_content.rs';
const splitClientPath = 'web/src/lib/api/repos.ts';
const newReleasePagePath = 'web/src/routes/[owner]/[repo]/releases/new/+page.svelte';

const backend = readFileSync(backendPath, 'utf8');
const splitClient = readFileSync(splitClientPath, 'utf8');
const newReleasePage = readFileSync(newReleasePagePath, 'utf8');

const failures = [];

// Branches carry a default-branch marker, tags are bare names. Both halves of
// each contract are asserted here so a change on one side cannot quietly leave
// the other side reading a field that is never sent (card_ee7e4c250ca0).
if (!/struct BranchRef\s*\{[\s\S]*?is_default:\s*bool/.test(backend)) {
  failures.push('Backend branch listing must carry a default-branch marker the client can render');
}

if (!/fn list_branch_refs[\s\S]*?anyhow::Result<Vec<BranchRef>>/.test(backend)) {
  failures.push('Backend branch listing contract changed; update client types or this check');
}

if (!/fn list_tag_names[\s\S]*?anyhow::Result<Vec<String>>/.test(backend)) {
  failures.push('Backend tag listing contract changed; update client normalization or this check');
}

for (const [name, source] of [
  ['repos.ts', splitClient],
]) {
  if (!/type BranchRefResponse\s*=\s*\{\s*name:\s*string;\s*is_default:\s*boolean\s*\}/.test(source)) {
    failures.push(`${name} must type backend branch objects, including the default-branch marker`);
  }

  if (!/type TagRefResponse\s*=\s*string;/.test(source)) {
    failures.push(`${name} must accept backend tag string responses`);
  }

  if (!/branches:\s*\([^)]*\)\s*=>\s*\n?\s*request<BranchRefResponse\[\]>/.test(source)) {
    failures.push(`${name} repos.branches must pass backend branch objects through unchanged`);
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
