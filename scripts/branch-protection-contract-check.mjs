#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/branch_protection.rs');
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const settingsLayoutPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+layout.svelte');
const settingsPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/branches/+page.svelte');

const backend = readFileSync(backendPath, 'utf8');
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

if (!/Path\(\(owner,\s*repo,\s*id\)\):\s*Path<\(String,\s*String,\s*i64\)>/.test(backend)) {
  failures.push('Branch protection get/update/delete handlers must destructure owner, repo, and rule id from the route.');
}

for (const call of [
  'get_protection_for_repo(',
  'update_protection_for_repo(',
  'delete_protection_for_repo(',
]) {
  if (!backend.includes(call)) {
    failures.push(`Branch protection backend must scope routed rule ids with ${call}`);
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
