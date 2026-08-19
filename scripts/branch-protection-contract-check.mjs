#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionRustSource, rustFnBlock } from './lib/rust-source.mjs';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/branch_protection.rs');
const clientPath = path.join(root, 'web/src/lib/api/branchProtections.ts');
const settingsLayoutPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+layout.svelte');
const settingsPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/branches/+page.svelte');

// The production view: comments and `#[cfg(test)]` items alike are blanked, so
// a commented-out handler reads as a deleted one and a test double cannot
// stand in for the handler the server ships.
const backend = productionRustSource(readFileSync(backendPath, 'utf8'));
const client = readFileSync(clientPath, 'utf8');
const settingsLayout = readFileSync(settingsLayoutPath, 'utf8');
const settingsPage = readFileSync(settingsPagePath, 'utf8');
const failures = [];

for (const [method, route] of [
  ['get', '/repos/{owner}/{name}/branches/protection'],
  ['post', '/repos/{owner}/{name}/branches/protection'],
  ['patch', '/repos/{owner}/{name}/branches/protection/{id}'],
  ['delete', '/repos/{owner}/{name}/branches/protection/{id}'],
]) {
  const pattern = new RegExp(`${method},[\\s\\S]*path\\s*=\\s*"${route.replaceAll('/', '\\/')}"`);
  if (!pattern.test(backend)) {
    failures.push(`Backend branch protection ${method.toUpperCase()} ${route} annotation is missing or changed`);
  }
}

for (const [name, method, pathPattern] of [
  ['list', undefined, /\/branches\/protection`/],
  ['create', 'POST', /\/branches\/protection`[\s\S]*method:\s*'POST'/],
  ['update', 'PATCH', /\/branches\/protection\/\$\{id\}`[\s\S]*method:\s*'PATCH'/],
  ['remove', 'DELETE', /\/branches\/protection\/\$\{id\}`[\s\S]*method:\s*'DELETE'/],
]) {
  if (!new RegExp(`${name}\\s*:\\s*\\(`).test(client)) {
    failures.push(`API client must expose branchProtections.${name}`);
  }
  if (!pathPattern.test(client)) {
    failures.push(`API client branchProtections.${name} must call the backend ${method || 'GET'} route`);
  }
}

if (!/settings\/branches/.test(settingsLayout)) {
  failures.push('Repository settings nav must expose the branch protection page');
}

// ── Each id-taking handler, read on its own ──────────────────────────────
//
// `{id}` is a global `protected_branches` primary key, so every door that takes
// one owes the same scoping: resolve the rule *within* the repository the route
// named, never by bare id. This used to be asserted file-wide — one
// `Path((owner, repo, id))` match anywhere in the module, plus three
// `..._for_repo(` substrings anywhere in the module — for a message that names
// three handlers. Any single occurrence satisfied it, so dropping `id` from two
// of the three destructurings left the gate green (card_c7aef378ad3d).
//
// Reading each handler's own signature and body is what makes the message true:
// a handler that stops scoping is named, and a handler this check can no longer
// read is a failure rather than a silent pass.
for (const [handler, scopingCall] of [
  ['get_protection', 'get_protection_for_repo'],
  ['update_protection', 'update_protection_for_repo'],
  ['delete_protection', 'delete_protection_for_repo'],
]) {
  const fn = rustFnBlock(backend, handler);
  if (fn === null) {
    failures.push(`api/branch_protection.rs no longer defines a \`pub async fn ${handler}\` this check can read`);
    continue;
  }

  const tuple = /Path\(\(\s*(\w+)\s*,\s*(\w+)\s*,\s*(\w+)\s*\)\)\s*:\s*Path<\(\s*String\s*,\s*String\s*,\s*i64\s*\)>/.exec(
    fn.params,
  );
  if (tuple === null) {
    failures.push(
      `Branch protection handler ${handler} must destructure Path<(String, String, i64)> — the owner, ` +
        'the repository and the rule id it is routed for.',
    );
    continue;
  }

  const [, owner, repo, id] = tuple;
  const scoped = new RegExp(
    // `[,)]` closes the argument on either shape: `…, id)` and the rustfmt-wrapped `…, id,`.
    `\\b${scopingCall}\\(\\s*&state\\.db\\s*,\\s*&${owner}\\s*,\\s*&${repo}\\s*,\\s*${id}\\s*[,)]`,
  );
  if (!scoped.test(fn.body)) {
    failures.push(
      `Branch protection handler ${handler} must resolve rule \`${id}\` inside \`${owner}/${repo}\` via ` +
        `${scopingCall}(&state.db, &${owner}, &${repo}, ${id}, …). \`{id}\` is a global ` +
        'protected_branches primary key, so admin of one repository must not reach another one\'s rules.',
    );
  }
}

for (const call of [
  'branchProtections.list(',
  'branchProtections.create(',
  'branchProtections.update(',
  'branchProtections.remove(',
]) {
  if (!settingsPage.includes(call)) {
    failures.push(`Branch protection settings page must call ${call}`);
  }
}

for (const key of [
  'branch_name',
  'require_pr',
  'require_status_check',
  'required_status_checks',
  'require_approval',
  'required_approvals',
  'allow_force_push',
  'allowed_push_user_ids',
]) {
  if (!settingsPage.includes(key)) {
    failures.push(`Branch protection settings page must map backend field ${key}`);
  }
}

if (failures.length > 0) {
  console.error('Branch protection frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Branch protection frontend/backend contract ok');
