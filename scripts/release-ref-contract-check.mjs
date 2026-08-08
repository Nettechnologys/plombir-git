#!/usr/bin/env node

import { readFileSync } from 'node:fs';

import { rustFnHead, rustStructBody, stripRustComments } from './lib/rust-source.mjs';

const backendPath = 'crates/rg-http/src/api/repo_content.rs';
const splitClientPath = 'web/src/lib/api/repos.ts';
const newReleasePagePath = 'web/src/routes/[owner]/[repo]/releases/new/+page.svelte';

// Comments are stripped so a commented-out field or signature reads as deleted.
const backend = stripRustComments(readFileSync(backendPath, 'utf8'));
const splitClient = readFileSync(splitClientPath, 'utf8');
const newReleasePage = readFileSync(newReleasePagePath, 'utf8');

const failures = [];

// Branches carry a default-branch marker, tags are bare names. Both halves of
// each contract are asserted here so a change on one side cannot quietly leave
// the other side reading a field that is never sent (card_ee7e4c250ca0).
// Each subject is read on its own rather than reached through `[\s\S]*?`: the
// lazy bridge walks out of `BranchRef` into the next `is_default: bool` in the
// file, and out of a function's own signature into the next one returning the
// same type — so the gate stayed green about a declaration it no longer
// described (card_3e8da1ff84d9).
const branchRef = rustStructBody(backend, 'BranchRef');
if (branchRef === null) {
  failures.push('api/repo_content.rs no longer defines a `struct BranchRef` this check can read');
} else if (!/\bis_default:\s*bool/.test(branchRef)) {
  failures.push('Backend BranchRef must carry a default-branch marker the client can render');
}

for (const [fn, returnType, message] of [
  ['list_branch_refs', /->\s*anyhow::Result<Vec<BranchRef>>\s*$/, 'Backend branch listing contract changed; update client types or this check'],
  ['list_tag_names', /->\s*anyhow::Result<Vec<String>>\s*$/, 'Backend tag listing contract changed; update client normalization or this check'],
]) {
  const head = rustFnHead(backend, fn);
  if (head === null) {
    failures.push(`api/repo_content.rs no longer defines a \`fn ${fn}\` this check can read`);
  } else if (!returnType.test(head)) {
    failures.push(message);
  }
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
