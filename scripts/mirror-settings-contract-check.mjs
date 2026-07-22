#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/mirrors.rs');
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const settingsLayoutPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+layout.svelte');
const settingsPagePath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/mirror/+page.svelte');

const backend = readFileSync(backendPath, 'utf8');
const client = readFileSync(clientPath, 'utf8');
const settingsLayout = readFileSync(settingsLayoutPath, 'utf8');
const settingsPage = readFileSync(settingsPagePath, 'utf8');
const failures = [];

for (const [method, route] of [
  ['post', '/repos/{owner}/{name}/mirror'],
  ['get', '/repos/{owner}/{name}/mirror'],
  ['patch', '/repos/{owner}/{name}/mirror'],
  ['delete', '/repos/{owner}/{name}/mirror'],
  ['post', '/repos/{owner}/{name}/mirror/sync'],
]) {
  const pattern = new RegExp(`${method},[\\s\\S]*path\\s*=\\s*"${route.replaceAll('/', '\\/')}"`);
  if (!pattern.test(backend)) {
    failures.push(`Backend mirror ${method.toUpperCase()} ${route} annotation is missing or changed`);
  }
}

for (const [name, method, pathPattern] of [
  ['get', undefined, /\/mirror`/],
  ['create', 'POST', /\/mirror`[\s\S]*method:\s*'POST'/],
  ['update', 'PATCH', /\/mirror`[\s\S]*method:\s*'PATCH'/],
  ['remove', 'DELETE', /\/mirror`[\s\S]*method:\s*'DELETE'/],
  ['sync', 'POST', /\/mirror\/sync`[\s\S]*method:\s*'POST'/],
]) {
  if (!new RegExp(`${name}\\s*:\\s*\\(`).test(client)) {
    failures.push(`API client must expose mirrors.${name}`);
  }
  if (method && !pathPattern.test(client)) {
    failures.push(`API client mirrors.${name} must call the backend ${method} route`);
  }
  if (!method && !pathPattern.test(client)) {
    failures.push('API client mirrors.get must call /mirror');
  }
}

if (!/settings\/mirror/.test(settingsLayout)) {
  failures.push('Repository settings nav must expose the mirror page');
}

for (const call of ['mirrors.get(', 'mirrors.create(', 'mirrors.update(', 'mirrors.sync(', 'mirrors.remove(']) {
  if (!settingsPage.includes(call)) {
    failures.push(`Mirror settings page must call ${call}`);
  }
}

if (!/no mirror configured/i.test(settingsPage)) {
  failures.push('Mirror settings page must treat backend no-mirror 404 as an empty state');
}

if (failures.length > 0) {
  console.error('Mirror settings frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Mirror settings frontend/backend contract ok');
