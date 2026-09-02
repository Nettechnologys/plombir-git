#!/usr/bin/env node

// Keep ARCHITECTURE.md's workspace dependency graph equal to Cargo's graph.
// The documentation used to omit rg-process and the rg-http -> rg-db edge while
// claiming rg-git had no internal dependencies. Reading cargo metadata makes
// the manifest side authoritative instead of maintaining a third edge list in
// this check.

import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(
  process.env.FORGEKEEP_ARCHITECTURE_DEPENDENCY_ROOT ?? resolve(scriptsDir, '..'),
);
const architecturePath = resolve(root, 'ARCHITECTURE.md');

function fail(message) {
  console.error(`❌ architecture crate dependencies: ${message}`);
  process.exit(1);
}

const metadataResult = spawnSync(
  'cargo',
  ['metadata', '--no-deps', '--format-version', '1'],
  { cwd: root, encoding: 'utf8', env: { ...process.env, CARGO_TERM_COLOR: 'never' } },
);
if (metadataResult.error) {
  fail(`could not run cargo metadata: ${metadataResult.error.message}`);
}
if (metadataResult.status !== 0) {
  fail(`cargo metadata exited ${metadataResult.status}: ${metadataResult.stderr.trim()}`);
}

let metadata;
try {
  metadata = JSON.parse(metadataResult.stdout);
} catch (error) {
  fail(`cargo metadata returned invalid JSON: ${error.message}`);
}

const workspaceIds = new Set(metadata.workspace_members);
const packages = metadata.packages
  .filter((pkg) => workspaceIds.has(pkg.id) && pkg.name.startsWith('rg-'))
  .sort((left, right) => left.name.localeCompare(right.name));
if (packages.length === 0) {
  fail('cargo metadata found no rg-* workspace crates');
}

const crateNames = new Set(packages.map((pkg) => pkg.name));
const actual = new Map(
  packages.map((pkg) => [
    pkg.name,
    new Set(
      pkg.dependencies
        .filter((dependency) => dependency.kind !== 'dev' && crateNames.has(dependency.name))
        .map((dependency) => dependency.name),
    ),
  ]),
);

let architecture;
try {
  architecture = readFileSync(architecturePath, 'utf8');
} catch (error) {
  fail(`cannot read ${architecturePath}: ${error.message}`);
}

const heading = '### Crate dependency direction';
const headingAt = architecture.indexOf(heading);
if (headingAt < 0) fail(`missing ${JSON.stringify(heading)} heading`);
const afterHeading = architecture.slice(headingAt + heading.length);
const fenceAt = afterHeading.indexOf('```');
if (fenceAt < 0) fail('dependency heading has no fenced graph');
const graphStart = fenceAt + 3;
const graphEnd = afterHeading.indexOf('```', graphStart);
if (graphEnd < 0) fail('dependency graph fence is not closed');

const documented = new Map();
for (const [offset, rawLine] of afterHeading.slice(graphStart, graphEnd).split('\n').entries()) {
  const line = rawLine.trim();
  if (line === '') continue;
  const match = /^(rg-[a-z0-9-]+)\s+──>\s+(.+)$/.exec(line);
  if (!match) {
    fail(`graph line ${offset + 1} is not \`rg-crate ──> dep, dep\`: ${JSON.stringify(line)}`);
  }
  const [, source, rawTargets] = match;
  if (documented.has(source)) fail(`graph declares ${source} more than once`);
  const targets = rawTargets === 'none'
    ? []
    : rawTargets.split(',').map((target) => target.trim());
  if (targets.some((target) => !/^rg-[a-z0-9-]+$/.test(target))) {
    fail(`${source} has an invalid dependency list: ${JSON.stringify(rawTargets)}`);
  }
  documented.set(source, new Set(targets));
}

const problems = [];
for (const name of crateNames) {
  if (!documented.has(name)) problems.push(`graph is missing workspace crate ${name}`);
}
for (const name of documented.keys()) {
  if (!crateNames.has(name)) problems.push(`graph names non-workspace crate ${name}`);
}
for (const [name, expectedTargets] of actual) {
  const documentedTargets = documented.get(name);
  if (!documentedTargets) continue;
  const missing = [...expectedTargets].filter((target) => !documentedTargets.has(target));
  const extra = [...documentedTargets].filter((target) => !expectedTargets.has(target));
  if (missing.length > 0 || extra.length > 0) {
    problems.push(
      `${name}: missing [${missing.sort().join(', ')}], extra [${extra.sort().join(', ')}]`,
    );
  }
}

if (problems.length > 0) {
  for (const problem of problems) console.error(`❌ ${problem}`);
  process.exit(1);
}

const edgeCount = [...actual.values()].reduce((sum, targets) => sum + targets.size, 0);
console.log(
  `architecture crate dependencies: ${packages.length} workspace crate(s), ${edgeCount} internal edge(s) match cargo metadata`,
);
