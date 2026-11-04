#!/usr/bin/env node

// Exercise the real contract in copied trees. Each mutation removes one link
// in the page -> header -> visible/overflow navigation chain while leaving the
// other activeTab references intact, so a whole-file word search cannot pass.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { scratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const header = 'web/src/lib/components/RepoHeader.svelte';
const tabs = 'web/src/lib/components/OverflowTabs.svelte';
const dropdown = 'web/src/lib/components/Dropdown.svelte';
const page = 'web/src/routes/[owner]/[repo]/time_tracking/+page.svelte';
const check = 'scripts/time-tracking-contract-check.mjs';
const copied = [
  check, 'scripts/lib', header, tabs, dropdown, page,
  'web/src/lib/api/timeTracking.ts',
  'crates/rg-http/src/api/time_tracking.rs',
  'web/src/lib/i18n/translations/en.json',
  'web/src/lib/i18n/translations/zh-CN.json',
];

function patch(fixture, file, from, to) {
  const target = join(fixture, file);
  const source = readFileSync(target, 'utf8');
  const next = source.replace(from, to);
  assert.notEqual(next, source, `mutation did not change ${file}: ${from}`);
  writeFileSync(target, next);
}

function runCase(name, mutate, expectedFailure) {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-time-tracking-contract.'));
  try {
    for (const file of copied) {
      mkdirSync(dirname(join(fixture, file)), { recursive: true });
      cpSync(join(root, file), join(fixture, file), { recursive: true });
    }
    mutate?.(fixture);
    const result = spawnSync(process.execPath, [join(fixture, check)], {
      cwd: fixture, encoding: 'utf8', timeout: 30_000,
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    assert.equal(result.status, expectedFailure ? 1 : 0, `${name}: unexpected result\n${output}`);
    if (expectedFailure) assert.ok(output.includes(expectedFailure), `${name}: wrong failure\n${output}`);
    console.log(`✅ ${expectedFailure ? 'mutation rejected' : 'accepted'}: ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

const parentFailure = 'Repo header must pass its tab ids, URLs and activeTab';
const urlFailure = 'Repo header must link the time_tracking tab';
const moreFailure = 'OverflowTabs More trigger must identify the active hidden tab';
const styleFailure = 'Active repository tabs must retain visible row, More trigger and dropdown item styles';

runCase('clean production navigation');
runCase('explicit activeTab prop', fixture => patch(fixture, header, '    {activeTab}', '    activeTab={activeTab}'));
runCase('missing parent activeTab despite other activeTab references', fixture => patch(fixture, header, '    {activeTab}', ''), parentFailure);
runCase('constant parent activeTab', fixture => patch(fixture, header, '    {activeTab}', '    activeTab={"code"}'), parentFailure);
runCase('different imported child', fixture => patch(fixture, header, "from './OverflowTabs.svelte'", "from './OtherTabs.svelte'"), parentFailure);
runCase('commented-out component mount', fixture => patch(fixture, header, /<OverflowTabs\b[\s\S]*?\/>/, match => `<!-- ${match} -->`), parentFailure);
runCase('tab identities lost before delegation', fixture => patch(fixture, header, '...tab, href: tabHref(tab)', 'href: tabHref(tab)'), parentFailure);
runCase('URLs replaced before delegation', fixture => patch(fixture, header, 'href: tabHref(tab)', 'href: "/"'), parentFailure);
runCase('missing time tab', fixture => patch(fixture, header, /\s*\{ id: 'time_tracking'[^\n]+\n/, '\n'), 'Repo header must expose a time-tracking tab');
runCase('time tab points to a different route', fixture => patch(fixture, header, "id: 'time_tracking',", "id: 'time_tracking', path: 'issues',"), urlFailure);
runCase('URL builder loses the repository', fixture => patch(fixture, header, 'return `/${owner}/${repo}${suffix}`;', 'return `/${owner}${suffix}`;'), urlFailure);
runCase('page no longer selects Time', fixture => patch(fixture, page, 'activeTab="time_tracking"', 'activeTab="code"'), 'Time tracking page must select activeTab');
runCase('child ignores the activeTab prop', fixture => patch(fixture, tabs, 'tabs, activeTab, label, moreLabel, moreActiveLabel }: Props', 'tabs, label, moreLabel, moreActiveLabel }: Props'), 'OverflowTabs must consume the activeTab prop');

// Change only the requested path. The other path and the measurement samples
// still contain the correct active binding and cannot stand in for this link.
for (const collection of ['visible', 'hidden']) {
  for (const [name, from, to] of [
    ['URL', 'href={tab.href}', 'href="/"'],
    ['active class', 'class:active={activeTab === tab.id}', 'class:active={false}'],
    ['current-page label', "aria-current={activeTab === tab.id ? 'page' : undefined}", ''],
  ]) {
    runCase(`${collection} link loses ${name}`, fixture => patch(
      fixture, tabs,
      new RegExp(`(\\{#each ${collection} as tab \\(tab\\.id\\)\\}\\s*)(<a\\s[^>]*>)`),
      (_match, prefix, anchor) => prefix + anchor.replace(from, to),
    ), `OverflowTabs ${collection} links must preserve tab URLs, active styling and aria-current`);
  }
}

runCase('hidden activity is no longer derived from the active id', fixture => patch(fixture, tabs, 'hidden.some(tab => tab.id === activeTab)', 'hidden.some(tab => tab.id === "code")'), moreFailure);
runCase('More loses its active styling', fixture => patch(fixture, tabs, 'triggerClass="overflow-trigger {hiddenActive ? \'active\' : \'\'}"', 'triggerClass="overflow-trigger"'), moreFailure);
runCase('More loses its active accessible label', fixture => patch(fixture, tabs, 'ariaLabel={hiddenActive ? moreActiveLabel : moreLabel}', 'ariaLabel={moreLabel}'), moreFailure);
runCase('visible tab styling is removed', fixture => patch(fixture, tabs, 'border-bottom-color: var(--accent);', 'border-bottom-color: transparent;'), styleFailure);
runCase('overflow item styling is removed', fixture => patch(fixture, dropdown, '.dropdown-menu :global(a.active),', '.dropdown-menu :global(a.inactive),'), styleFailure);
