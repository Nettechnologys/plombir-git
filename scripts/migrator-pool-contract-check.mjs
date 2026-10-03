#!/usr/bin/env node

// SQLite schema churn can leave an old pooled connection reporting a one-shot
// `no such table` even though the rebuilt table exists. `run_migrations` closes
// that gap by refreshing the pool; tests that drive `Migrator::up` or
// `Migrator::down` directly do not, so their throwaway pool must contain one
// connection unless multiple live connections are the behavior under test.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  productionRustSource,
  testInclusiveRustCode,
  testInclusiveRustSource,
} from './lib/rust-source.mjs';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(
  process.env.PLOMBIR_GIT_MIGRATOR_POOL_ROOT ?? path.resolve(scriptsDir, '..'),
);
const cratesDir = path.join(root, 'crates');
const MIN_MIGRATOR_TEST_FILES = 7;

// These tests deliberately keep several connections alive. One primes every
// pooled connection before rebuilding tables; the other proves concurrent
// readers and writers stay isolated while a rebuild holds SQLite's lock.
const MULTI_CONNECTION_ALLOWLIST = new Map([
  [
    'crates/rg-db/src/migrations/m20260805_000002_uploads_outlive_their_uploader_tests.rs',
    'primes every pooled connection and proves each one refreshes after the rebuild',
  ],
  [
    'crates/rg-db/src/migrations/m20260730_000001_repositories_namespace_unique_tests.rs',
    'needs distinct pooled connections to prove concurrent writers wait for the rebuild',
  ],
]);

function rustFiles(dir) {
  const files = [];
  for (const entry of readdirSync(dir, { withFileTypes: true }).sort((a, b) =>
    a.name.localeCompare(b.name),
  )) {
    if (entry.name.startsWith('.') || entry.name === 'target') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) files.push(...rustFiles(full));
    else if (entry.isFile() && entry.name.endsWith('.rs')) files.push(full);
  }
  return files;
}

function relative(file) {
  return path.relative(root, file).split(path.sep).join('/');
}

function lineAt(source, index) {
  return source.slice(0, index).split('\n').length;
}

function removedAt(production, index, length) {
  return !/\S/.test(production.slice(index, index + length));
}

function testFileByLayout(file) {
  const name = relative(file);
  return /^crates\/[^/]+\/tests\//.test(name) || /(?:^|\/)(?:tests|[^/]+_tests)\.rs$/.test(name);
}

// Split a Rust call with structure from the code-only view and values from the
// comment-free, string-bearing view. Both are byte-aligned with the raw file.
function callArguments(source, structure, open) {
  const args = [];
  let depth = 0;
  let start = open + 1;
  for (let index = open; index < structure.length; index += 1) {
    const character = structure[index];
    if ('([{'.includes(character)) {
      depth += 1;
    } else if (')]}'.includes(character)) {
      depth -= 1;
      if (depth === 0) {
        args.push(source.slice(start, index).trim());
        if (args.at(-1) === '') args.pop();
        return args;
      }
    } else if (character === ',' && depth === 1) {
      args.push(source.slice(start, index).trim());
      start = index + 1;
    }
  }
  return null;
}

if (!existsSync(cratesDir)) {
  console.error(`❌ ${relative(cratesDir)}/ does not exist; the Migrator pool sweep cannot run.`);
  process.exit(1);
}

const migratorPattern =
  /\b(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*Migrator\s*::\s*(up|down)\s*\(/g;
const openerPattern =
  /\b(?:(?:rg_db|crate|super|self)\s*::\s*)?connect_with_pool\s*\(/g;
const candidates = new Map();

for (const file of rustFiles(cratesDir)) {
  const bytes = readFileSync(file, 'utf8');
  const source = testInclusiveRustSource(bytes);
  const structure = testInclusiveRustCode(bytes);
  // Required production-aware boundary: an inline `#[cfg(test)]` call is test
  // code precisely when this view blanks it. Whole test files are test-only by
  // layout because their parent crate, not an attribute inside the file, owns
  // that classification.
  const production = productionRustSource(bytes);
  const wholeTestFile = testFileByLayout(file);
  const migratorCalls = [...structure.matchAll(migratorPattern)].filter(
    (match) => wholeTestFile || removedAt(production, match.index, match[0].length),
  );
  if (migratorCalls.length === 0) continue;

  const openers = [];
  for (const match of structure.matchAll(openerPattern)) {
    const prefix = structure.slice(Math.max(0, match.index - 80), match.index);
    if (/\bfn\s*$/.test(prefix)) continue;
    if (!wholeTestFile && !removedAt(production, match.index, match[0].length)) continue;
    const open = structure.indexOf('(', match.index + match[0].indexOf('('));
    openers.push({
      args: callArguments(source, structure, open),
      index: match.index,
    });
  }

  candidates.set(relative(file), { bytes, migratorCalls, openers });
}

const violations = [];
let singleConnectionFiles = 0;
let allowedFiles = 0;

for (const [file, candidate] of candidates) {
  const allowance = MULTI_CONNECTION_ALLOWLIST.get(file);
  if (candidate.openers.length === 0) {
    violations.push({
      file,
      index: candidate.migratorCalls[0].index,
      reason: 'drives Migrator directly but has no test-local connect_with_pool call',
    });
    continue;
  }

  const malformed = candidate.openers.find(({ args }) => args === null || args.length < 4);
  if (malformed) {
    violations.push({
      file,
      index: malformed.index,
      reason: 'cannot read connect_with_pool max_connections argument',
    });
    continue;
  }

  const multiConnection = candidate.openers.filter(
    ({ args }) => args[3].replace(/\s+/g, '') !== '1',
  );
  if (allowance) {
    if (multiConnection.length === 0) {
      violations.push({
        file,
        index: candidate.openers[0].index,
        reason: `allowlist entry is stale (${allowance}); remove it`,
      });
    } else {
      allowedFiles += 1;
    }
    continue;
  }

  for (const opener of multiConnection) {
    violations.push({
      file,
      index: opener.index,
      reason: 'connect_with_pool must use literal 1 for max_connections when the test drives Migrator directly',
    });
  }
  if (multiConnection.length === 0) singleConnectionFiles += 1;
}

for (const [file, reason] of MULTI_CONNECTION_ALLOWLIST) {
  if (!candidates.has(file)) {
    violations.push({
      file,
      index: 0,
      reason: `allowlist entry no longer names a direct-Migrator test (${reason})`,
    });
  }
}

if (candidates.size < MIN_MIGRATOR_TEST_FILES) {
  console.error(
    `❌ Migrator pool sweep found ${candidates.size} direct-Migrator test file(s); ` +
      `expected at least ${MIN_MIGRATOR_TEST_FILES}. The inventory is incomplete, not green.`,
  );
  process.exit(1);
}

if (violations.length > 0) {
  for (const violation of violations) {
    const source = candidates.get(violation.file)?.bytes ?? '';
    console.error(`❌ ${violation.file}:${lineAt(source, violation.index)}: ${violation.reason}`);
  }
  process.exit(1);
}

console.log(
  `migrator pool contract ok (${candidates.size} direct-Migrator test files: ` +
    `${singleConnectionFiles} single-connection, ${allowedFiles} explicit multi-connection regressions)`,
);
