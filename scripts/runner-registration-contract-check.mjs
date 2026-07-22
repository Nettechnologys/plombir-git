#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const pagePath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/runners/+page.svelte');
const settingsLayoutPath = path.join(root, 'web/src/routes/[owner]/[repo]/settings/+layout.svelte');
const adminPagePath = path.join(root, 'web/src/routes/admin/runners/+page.svelte');
const adminIndexPath = path.join(root, 'web/src/routes/admin/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/runners.rs');

const client = readFileSync(clientPath, 'utf8');
const page = readFileSync(pagePath, 'utf8');
const settingsLayout = readFileSync(settingsLayoutPath, 'utf8');
const adminPage = readFileSync(adminPagePath, 'utf8');
const adminIndex = readFileSync(adminIndexPath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');

const failures = [];

if (!/pub\s+struct\s+RegisterRunnerResponse\s*\{[\s\S]*\btoken:\s+String[\s\S]*\}/.test(backend)) {
  failures.push('Backend runner registration response must include the runner token');
}

if (!/export\s+interface\s+RegisterRunnerResponse\s*\{[\s\S]*token:\s*string[\s\S]*\}/.test(client)) {
  failures.push('API client must type the runner registration token response');
}

if (!/register:\s*\([^)]*\)\s*=>\s*\n?\s*request<RegisterRunnerResponse>\('\/runners\/register'/.test(client)) {
  failures.push('API client runners.register must return RegisterRunnerResponse');
}

if (/from\s+['"]\$lib\/api\/client\.svelte['"]/.test(page) || /\brunners\.(?:list|register|delete|get)\(/.test(page)) {
  failures.push('Repo runner settings page must not call global/admin runner APIs');
}

if (!/href="\/admin\/runners"/.test(page) || !/admin\.runners\.repo_handoff/.test(page)) {
  failures.push('Repo runner settings page must hand off to admin runner management');
}

if (!/settings\/runners/.test(settingsLayout) || !/admin\.runners\.title/.test(settingsLayout)) {
  failures.push('Repository settings navigation must expose the runner handoff page');
}

if (!/path\s*=\s*"\/admin\/runners"/.test(backend) || !/pub\s+async\s+fn\s+list_runners_admin/.test(backend)) {
  failures.push('Backend must expose the admin-only runner list contract');
}

if (!/href="\/admin\/runners"/.test(adminIndex)) {
  failures.push('Admin dashboard must link to runner management');
}

if (!/isAdmin\(\)/.test(adminPage) || !/runners\.list\(/.test(adminPage)) {
  failures.push('Admin runner page must guard admin access and list runners');
}

if (!/const\s+response\s*=\s*await\s+runners\.register\(/.test(adminPage)) {
  failures.push('Admin runner page must register runners through the typed API client');
}

if (!/registeredRunner\s*=\s*\{[\s\S]*token:\s*response\.token[\s\S]*\}/.test(adminPage)) {
  failures.push('Admin runner page must display the one-time runner token after registration');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Runner registration frontend/backend contract ok');
