#!/usr/bin/env node

// Does `scripts/server-image-init-contract-check.mjs` still bite?
//
// Fixtures in the vocabulary of card_ca2b5f84cb69: the init removed from the
// image, the init declared but not installed, an ENTRYPOINT that swallows the
// command, and a compose file overriding the entrypoint. Plus the reader: the
// ENTRYPOINT named only in a comment does not count.
//
// Every fixture is a copied repository shape, never the checkout.

import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { scratchDir } from './lib/scratch-dir.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = 'scripts/server-image-init-contract-check.mjs';
const COPIED = [
  check,
  'scripts/lib',
  'Dockerfile',
  'deploy/docker-compose.yml',
  'deploy/docker-compose.hostdir.yml',
  'deploy/docker-compose.release.yml',
];

function fixtureRoot() {
  const fixture = scratchDir(join(tmpdir(), 'plombir-git-server-image-init-'));
  for (const path of COPIED) {
    mkdirSync(dirname(join(fixture, path)), { recursive: true });
    cpSync(join(root, path), join(fixture, path), { recursive: true });
  }
  return fixture;
}

function run(fixture) {
  const result = spawnSync(process.execPath, [join(fixture, check), fixture], { cwd: fixture, encoding: 'utf8' });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

let failures = 0;

function report(name, reason, result) {
  if (reason) {
    failures += 1;
    console.error(`❌ ${name}: ${reason}\n----- exit ${result?.status} -----\n${result?.output ?? ''}`);
  } else {
    console.log(`✅ ${name}`);
  }
}

function fixture(name, file, mutate, expect) {
  const dir = fixtureRoot();
  try {
    const path = join(dir, file);
    const source = readFileSync(path, 'utf8');
    const mutated = mutate(source);
    if (mutated === source) throw new Error('the mutation changed nothing — the anchor it edits is gone');
    writeFileSync(path, mutated);
    const result = run(dir);
    let reason = null;
    if (result.status === 0) reason = 'the check passed';
    else if (!result.output.includes(expect)) reason = `no diagnostic mentioning ${JSON.stringify(expect)}`;
    report(name, reason, result);
  } catch (error) {
    report(name, error.message, null);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

{
  const dir = fixtureRoot();
  const result = run(dir);
  rmSync(dir, { recursive: true, force: true });
  report('the unmutated fixture', result.status === 0 ? null : 'the check failed', result);
}

fixture(
  'an image with no ENTRYPOINT is red',
  'Dockerfile',
  (source) => source.replace(/^ENTRYPOINT \[.*\]\n/m, ''),
  'declares no ENTRYPOINT',
);

fixture(
  'an ENTRYPOINT left only in a comment is red',
  'Dockerfile',
  (source) => source.replace(/^ENTRYPOINT (\[.*\])$/m, '# ENTRYPOINT $1'),
  'declares no ENTRYPOINT',
);

fixture(
  'tini declared but not installed is red',
  'Dockerfile',
  (source) => source.replace(/^\s*tini \\\n/m, ''),
  'does not `apt-get install` tini',
);

fixture(
  'an ENTRYPOINT that swallows the command is red',
  'Dockerfile',
  (source) => source.replace(/^ENTRYPOINT \[.*\]$/m, 'ENTRYPOINT ["/usr/bin/tini", "--", "plombir-git"]'),
  'is not `ENTRYPOINT ["/usr/bin/tini", "--"]`',
);

fixture(
  'a compose entrypoint override is red',
  'deploy/docker-compose.yml',
  (source) => source.replace(/^(\s*)restart: unless-stopped$/m, '$1restart: unless-stopped\n$1entrypoint: ["plombir-git"]'),
  "replaces the image's init",
);

if (failures > 0) {
  console.error(`\n${failures} server image init fixture(s) did not behave`);
  process.exit(1);
}
console.log('\n✅ server image init contract check still bites');
