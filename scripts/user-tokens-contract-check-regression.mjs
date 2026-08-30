#!/usr/bin/env node

// Copied-tree mutation proof for the token revoke consumer contract. The page
// may call `tokens.delete(token.id)` directly or snapshot `token.id` before an
// await, but an arbitrary local value must not satisfy the guard merely because
// a delete call still exists somewhere in the component.

import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const copied = [
  'scripts',
  'crates/rg-http/src/routes.rs',
  'crates/rg-http/src/api/users.rs',
  'web/src/lib/api/tokens.ts',
  'web/src/lib/components/Navbar.svelte',
  'web/src/routes/settings/tokens/+page.svelte',
];

function fixtureTree() {
  const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-user-tokens-contract.'));
  for (const path of copied) {
    const target = join(fixture, path);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(join(root, path), target, { recursive: true });
  }
  return fixture;
}

function patch(fixture, from, to) {
  const target = join(fixture, 'web/src/routes/settings/tokens/+page.svelte');
  const source = readFileSync(target, 'utf8');
  if (!source.includes(from)) throw new Error(`mutation cannot find ${JSON.stringify(from)}`);
  writeFileSync(target, source.replace(from, to));
}

function run(fixture) {
  const result = spawnSync(
    process.execPath,
    [join(fixture, 'scripts/user-tokens-contract-check.mjs')],
    { cwd: fixture, encoding: 'utf8', timeout: 30_000 },
  );
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

function expectRejected(name, mutate) {
  const fixture = fixtureTree();
  try {
    mutate(fixture);
    const result = run(fixture);
    const expected = 'Tokens settings page must revoke tokens through the API client';
    if (result.status === 0 || !result.output.includes(expected)) {
      console.error(`❌ ${name} did not make the token consumer contract red:\n${result.output}`);
      process.exit(1);
    }
    console.log(`✅ mutation rejected: ${name}`);
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

const clean = fixtureTree();
try {
  const result = run(clean);
  if (result.status !== 0) {
    console.error(`❌ token consumer contract baseline is red, so mutations prove nothing:\n${result.output}`);
    process.exit(1);
  }
} finally {
  rmSync(clean, { recursive: true, force: true });
}

expectRejected('removing the API delete call', (fixture) => patch(
  fixture,
  'await tokens.delete(tokenId);',
  'await tokens.list();',
));

expectRejected('breaking the snapshot provenance', (fixture) => patch(
  fixture,
  'const tokenId = token.id;',
  'const tokenId = 999;',
));
