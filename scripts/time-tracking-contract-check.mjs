#!/usr/bin/env node

import { readFileSync } from 'node:fs';
import path from 'node:path';

import { productionRustSource, rustFnBlock } from './lib/rust-source.mjs';
import { productionTsSource, tsFunctionBody } from './lib/ts-source.mjs';
import { parsePageInventory } from './lib/ui-surface.mjs';

const root = process.cwd();
const clientPath = path.join(root, 'web/src/lib/api/timeTracking.ts');
const pagePath = path.join(root, 'web/src/routes/[owner]/[repo]/time_tracking/+page.svelte');
const backendPath = path.join(root, 'crates/rg-http/src/api/time_tracking.rs');
const repoHeaderPath = path.join(root, 'web/src/lib/components/RepoHeader.svelte');
const overflowTabsPath = path.join(root, 'web/src/lib/components/OverflowTabs.svelte');
const dropdownPath = path.join(root, 'web/src/lib/components/Dropdown.svelte');
const enTranslationsPath = path.join(root, 'web/src/lib/i18n/translations/en.json');
const zhTranslationsPath = path.join(root, 'web/src/lib/i18n/translations/zh-CN.json');

const client = productionTsSource(readFileSync(clientPath, 'utf8'));
const page = productionTsSource(readFileSync(pagePath, 'utf8'));
// The production view: comments and `#[cfg(test)]` items alike are blanked, so
// a commented-out handler reads as a deleted one and a test double cannot
// stand in for the handler the server ships.
const backend = productionRustSource(readFileSync(backendPath, 'utf8'));
const repoHeader = productionTsSource(readFileSync(repoHeaderPath, 'utf8'));
const overflowTabs = productionTsSource(readFileSync(overflowTabsPath, 'utf8'));
const dropdown = productionTsSource(readFileSync(dropdownPath, 'utf8'));
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

if (!/timeTracking\s*=\s*\{[\s\S]*request<PaginatedResponse<any>>\(`\$\{repoPath\(owner, repo\)\}\/issues\/\$\{issueNumber\}\/time/.test(client)) {
  failures.push('API client must list issue time entries from the backend time route');
}

if (!/duration_minutes/.test(client)) {
  failures.push('API client timeTracking.add must send duration_minutes expected by the backend');
}

if (!/delete:\s*\([^)]*\)\s*=>\s*\n?\s*request<void>\(`\$\{repoPath\(owner, repo\)\}\/issues\/\$\{issueNumber\}\/time\/\$\{id\}`,\s*\{\s*method:\s*'DELETE'\s*\}/.test(client)) {
  failures.push('API client timeTracking.delete must model the backend 204 response as void');
}

if (/request<\{\s*deleted:\s*boolean\s*\}>\(`\$\{repoPath\(owner, repo\)\}\/issues\/\$\{issueNumber\}\/time\/\$\{id\}`/.test(client)) {
  failures.push('API client timeTracking.delete still expects a JSON deleted envelope');
}

// Read inside `delete_time_entry`, not across the module. The claim is about one
// handler's status, and any sibling that grows a `204` of its own would mask
// this one moving to something else. Proven on 2026-08-06 (HEAD `c9e12cb`) by
// moving the delete to `200 OK` and giving `add_time` a stray `204`: the check
// stayed green. A handler this cannot read is a failure too — a gate that
// silently stops reading its subject is worse than one that goes red.
const deleteTimeEntry = rustFnBlock(backend, 'delete_time_entry');
if (deleteTimeEntry === null) {
  failures.push(
    'api/time_tracking.rs no longer defines a `pub async fn delete_time_entry` this check can read',
  );
} else if (!/StatusCode::NO_CONTENT/.test(deleteTimeEntry.body)) {
  failures.push('Backend delete_time_entry must continue returning 204 No Content');
}

if (!/href=\{`\/\$\{owner\}\/\$\{repo\}\/issues\/\$\{selectedIssue\.number\}`\}/.test(page)) {
  failures.push('Time tracking selected issue link must interpolate owner/repo/issue number');
}

if (/href="\/\{owner\}\/\{repo\}\/issues\/\{selectedIssue\.number\}"/.test(page)) {
  failures.push('Time tracking selected issue link still uses literal Svelte braces');
}

const timeTab = repoHeader.match(/\{\s*id:\s*'time_tracking'[^{}]*\}/)?.[0] ?? '';
if (!/label:\s*t\('repo\.tabs\.time_tracking'\)/.test(timeTab)) {
  failures.push('Repo header must expose a time-tracking tab for the implemented page/API flow');
}

const tabHref = tsFunctionBody(repoHeader, 'tabHref') ?? '';
if ((/\bpath\s*:/.test(timeTab) && !/\bpath\s*:\s*'time_tracking'/.test(timeTab))
  || !/const\s+suffix\s*=\s*tab\.id\s*===\s*'code'\s*\?\s*''\s*:\s*`\/\$\{tab\.path\s*\|\|\s*tab\.id\}`/.test(tabHref)
  || !/return\s+`\/\$\{owner\}\/\$\{repo\}\$\{suffix\}`/.test(tabHref)) {
  failures.push('Repo header must link the time_tracking tab to /{owner}/{repo}/time_tracking');
}

// Follow the mounted component, not an incidental activeTab reference in a
// script, measurement span or sibling link. The inventory reader resolves the
// real import and tabs prop; markup checks cover the shorthand activeTab prop.
const markup = (source) => source.replace(/<(script|style)\b[^>]*>[\s\S]*?<\/\1>/g, '');
const headerMarkup = markup(repoHeader);
const overflowMarkup = markup(overflowTabs);
const mount = headerMarkup.match(/<OverflowTabs\b[\s\S]*?\/>/)?.[0] ?? '';
const mounted = parsePageInventory(repoHeader, repoHeaderPath, { includeDirectTransports: false }).mounts
  .filter(component => component.name === 'OverflowTabs' && component.source === './OverflowTabs.svelte');
if (mounted.length !== 1
  || mounted[0].props.tabs?.replace(/\s+/g, '') !== 'tabs.map(tab=>({...tab,href:tabHref(tab)}))'
  || !/(?:\s\{\s*activeTab\s*\}|\sactiveTab\s*=\s*\{\s*activeTab\s*\})/.test(mount)) {
  failures.push('Repo header must pass its tab ids, URLs and activeTab to the mounted OverflowTabs');
}

if (!/<RepoHeader\b[^>]*\bactiveTab="time_tracking"/.test(markup(page))) {
  failures.push('Time tracking page must select activeTab="time_tracking" on RepoHeader');
}

if (!/let\s*\{[^}]*\bactiveTab\b[^}]*\}\s*:\s*Props\s*=\s*\$props\(\)/.test(overflowTabs)) {
  failures.push('OverflowTabs must consume the activeTab prop from RepoHeader');
}

for (const collection of ['visible', 'hidden']) {
  const anchor = overflowMarkup.match(new RegExp(`\\{#each\\s+${collection}\\s+as\\s+tab\\s*\\(tab\\.id\\)\\}\\s*(<a\\s[^>]*>)`))?.[1] ?? '';
  if (!/\bhref=\{tab\.href\}/.test(anchor)
    || !/\bclass:active=\{activeTab\s*===\s*tab\.id\}/.test(anchor)
    || !/\baria-current=\{activeTab\s*===\s*tab\.id\s*\?\s*'page'\s*:\s*undefined\}/.test(anchor)) {
    failures.push(`OverflowTabs ${collection} links must preserve tab URLs, active styling and aria-current`);
  }
}

const menuMount = overflowMarkup.match(/<Dropdown\b[^>]*>/)?.[0] ?? '';
if (!/hiddenActive\s*=\s*\$derived\(hidden\.some\(tab\s*=>\s*tab\.id\s*===\s*activeTab\)\)/.test(overflowTabs)
  || !/triggerClass="overflow-trigger\s+\{hiddenActive\s*\?\s*'active'\s*:\s*''\}"/.test(menuMount)
  || !/ariaLabel=\{hiddenActive\s*\?\s*moreActiveLabel\s*:\s*moreLabel\}/.test(menuMount)) {
  failures.push('OverflowTabs More trigger must identify the active hidden tab visually and accessibly');
}

if (!/\.tab\.active\s*,\s*\.repo-tabs\s+:global\(\.overflow-trigger\.active\)\s*\{[^}]*border-bottom-color:\s*var\(--accent\)/.test(overflowTabs)
  || !/\.dropdown-menu\s+:global\(a\.active\)\s*,\s*\.dropdown-menu\s+:global\(button\.active\)\s*\{[^}]*color:\s*var\(--accent\)/.test(dropdown)) {
  failures.push('Active repository tabs must retain visible row, More trigger and dropdown item styles');
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
