#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const backendPath = path.join(root, 'crates/rg-http/src/api/imports.rs');
const entityPath = path.join(root, 'crates/rg-db/src/entities/import_task.rs');
const routerPath = path.join(root, 'crates/rg-http/src/lib.rs');
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const navbarPath = path.join(root, 'web/src/lib/components/Navbar.svelte');
const pagePath = path.join(root, 'web/src/routes/imports/+page.svelte');

const backend = readFileSync(backendPath, 'utf8');
const entity = readFileSync(entityPath, 'utf8');
const router = readFileSync(routerPath, 'utf8');
const client = readFileSync(clientPath, 'utf8');
const navbar = readFileSync(navbarPath, 'utf8');
const page = readFileSync(pagePath, 'utf8');
const failures = [];

for (const [method, route] of [
  ['post', '/imports'],
  ['get', '/imports'],
  ['get', '/imports/{id}'],
  ['delete', '/imports/{id}'],
]) {
  const pattern = new RegExp(`${method},[\\s\\S]*path\\s*=\\s*"${route.replaceAll('/', '\\/')}"`);
  if (!pattern.test(backend)) {
    failures.push(`Backend import ${method.toUpperCase()} ${route} annotation is missing or changed`);
  }
}

if (!/route\(\s*"\/imports"[\s\S]*post\(api::imports::start_import\)\.get\(api::imports::list_imports\)/.test(router)) {
  failures.push('HTTP router must register POST/GET /imports');
}

if (!/route\(\s*"\/imports\/\{id\}"[\s\S]*get\(api::imports::get_import_status\)\.delete\(api::imports::delete_import\)/.test(router)) {
  failures.push('HTTP router must register GET/DELETE /imports/{id}');
}

if (!/pub async fn get_import_status\([\s\S]*headers: HeaderMap,[\s\S]*extract_bearer_claims\(&headers, &state\.jwt_secret\)[\s\S]*task\.user_id == user_id/.test(backend)) {
  failures.push('GET /imports/{id} must authenticate and only return the current user task');
}

if (!/pub async fn delete_import\([\s\S]*headers: HeaderMap,[\s\S]*extract_bearer_claims\(&headers, &state\.jwt_secret\)[\s\S]*task\.user_id == user_id/.test(backend)) {
  failures.push('DELETE /imports/{id} must authenticate and only delete the current user task');
}

for (const [name, pattern] of [
  ['list', /list:\s*\(\)\s*=>\s*\n\s*request<ImportTask\[]>\('\/imports'\)/],
  ['start', /start:\s*\(payload:[\s\S]*request<ImportTask>\('\/imports'[\s\S]*method:\s*'POST'/],
  ['get', /get:\s*\(id: number\)\s*=>\s*\n\s*request<ImportTask>\(`\/imports\/\$\{id\}`\)/],
  ['remove', /remove:\s*\(id: number\)\s*=>\s*\n\s*request<void>\(`\/imports\/\$\{id\}`,\s*\{\s*method:\s*'DELETE'\s*\}\)/],
]) {
  if (!pattern.test(client)) {
    failures.push(`API client must expose imports.${name} with the backend route`);
  }
}

for (const field of [
  'platform',
  'source_url',
  'target_owner',
  'target_name',
  'auth_token',
  'import_repo',
  'import_issues',
  'import_pull_requests',
  'import_wiki',
  'import_releases',
  'import_labels',
  'import_milestones',
]) {
  if (!client.includes(field) || !page.includes(field)) {
    failures.push(`Import frontend must preserve backend field ${field}`);
  }
}

for (const field of ['repo_id', 'stage', 'error', 'stats']) {
  if (!client.includes(field)) {
    failures.push(`ImportTask client model must expose backend field ${field}`);
  }
}

for (const field of ['task.error', 'task.stage']) {
  if (!page.includes(field)) {
    failures.push(`Imports page must render ${field}`);
  }
}

if (client.includes('error_message') || page.includes('error_message')) {
  failures.push('Import frontend must render backend ImportTask.error, not non-existent error_message');
}

for (const backendField of ['pub repo_id: Option<i64>', 'pub stage: Option<String>', 'pub error: Option<String>', 'pub stats: Option<String>']) {
  if (!entity.includes(backendField)) {
    failures.push(`Backend import task contract check could not find ${backendField}`);
  }
}

if (!navbar.includes('href="/imports"')) {
  failures.push('Authenticated navbar must expose the imports page');
}

for (const call of ['imports.list(', 'imports.start(', 'imports.remove(']) {
  if (!page.includes(call)) {
    failures.push(`Imports page must call ${call}`);
  }
}

if (!/goto\('\/login'\)/.test(page)) {
  failures.push('Imports page must redirect unauthenticated users before calling user-scoped APIs');
}

if (failures.length > 0) {
  console.error('Imports frontend/backend contract failed:');
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exit(1);
}

console.log('Imports frontend/backend contract ok');
