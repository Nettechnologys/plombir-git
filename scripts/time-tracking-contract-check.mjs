#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/client.svelte.ts');
const pagePath = path.join(root, 'web/src/routes/[owner]/[repo]/time_tracking/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/time_tracking.rs');
const repoHeaderPath = path.join(root, 'web/src/lib/components/RepoHeader.svelte');
const enTranslationsPath = path.join(root, 'web/src/lib/i18n/translations/en.json');
const zhTranslationsPath = path.join(root, 'web/src/lib/i18n/translations/zh-CN.json');

const client = readFileSync(clientPath, 'utf8');
const page = readFileSync(pagePath, 'utf8');
const backend = readFileSync(backendPath, 'utf8');
const repoHeader = readFileSync(repoHeaderPath, 'utf8');
const enTranslations = JSON.parse(readFileSync(enTranslationsPath, 'utf8'));
const zhTranslations = JSON.parse(readFileSync(zhTranslationsPath, 'utf8'));

const failures = [];

for (const route of [
  '/repos/{owner}/{name}/issues/{number}/time',
  '/repos/{owner}/{name}/issues/{number}/time/total',
  '/repos/{owner}/{name}/issues/{number}/time/{id}',
]) {
  if (!backend.includes(route)) {
    failures.push(`Backend time-tracking route missing from OpenAPI annotations: ${route}`);
  }
}

if (!/timeTracking\s*=\s*\{[\s\S]*request<PaginatedResponse<any>>\(`\/repos\/\$\{owner\}\/\$\{repo\}\/issues\/\$\{issueNumber\}\/time/.test(client)) {
  failures.push('API client must list issue time entries from the backend time route');
}

if (!/duration_minutes/.test(client)) {
  failures.push('API client timeTracking.add must send duration_minutes expected by the backend');
}

if (!/delete:\s*\([^)]*\)\s*=>\s*\n?\s*request<void>\(`\/repos\/\$\{owner\}\/\$\{repo\}\/issues\/\$\{issueNumber\}\/time\/\$\{id\}`,\s*\{\s*method:\s*'DELETE'\s*\}/.test(client)) {
  failures.push('API client timeTracking.delete must model the backend 204 response as void');
}

if (/request<\{\s*deleted:\s*boolean\s*\}>\(`\/repos\/\$\{owner\}\/\$\{repo\}\/issues\/\$\{issueNumber\}\/time\/\$\{id\}`/.test(client)) {
  failures.push('API client timeTracking.delete still expects a JSON deleted envelope');
}

if (!/StatusCode::NO_CONTENT/.test(backend)) {
  failures.push('Backend delete_time_entry must continue returning 204 No Content');
}

if (!/href=\{`\/\$\{owner\}\/\$\{repo\}\/issues\/\$\{selectedIssue\.number\}`\}/.test(page)) {
  failures.push('Time tracking selected issue link must interpolate owner/repo/issue number');
}

if (/href="\/\{owner\}\/\{repo\}\/issues\/\{selectedIssue\.number\}"/.test(page)) {
  failures.push('Time tracking selected issue link still uses literal Svelte braces');
}

if (!/\{\s*id:\s*'time_tracking'[\s\S]*label:\s*t\('repo\.tabs\.time_tracking'\)/.test(repoHeader)) {
  failures.push('Repo header must expose a time-tracking tab for the implemented page/API flow');
}

if (!repoHeader.includes("activeTab === tab.id")) {
  failures.push('Repo header tabs must continue using activeTab ids so time_tracking can be highlighted');
}

if (enTranslations?.repo?.tabs?.time_tracking !== 'Time') {
  failures.push('English repo tab translations must include repo.tabs.time_tracking');
}

if (zhTranslations?.repo?.tabs?.time_tracking !== '工时') {
  failures.push('Chinese repo tab translations must include repo.tabs.time_tracking');
}

if (failures.length > 0) {
  for (const failure of failures) {
    console.log(`FAIL ${failure}`);
  }
  process.exit(1);
}

console.log('Time tracking frontend/backend contract ok');
