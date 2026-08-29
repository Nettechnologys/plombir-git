#!/usr/bin/env node

// The UI inventory is exposed through web/package.json, so npm deliberately
// launches it with `web/` as cwd. Both the imported checker and the CLI writer
// must still read and write the repository tree anchored beside this script.

import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, relative, resolve } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';

function run(label, command, args) {
  const result = spawnSync(command, args, {
    cwd: root,
    encoding: 'utf8',
    timeout: 60_000,
  });
  if (result.status !== 0) {
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`.trim();
    throw new Error(`${label} failed (exit ${result.status}):\n${output}`);
  }
  return `${result.stdout ?? ''}${result.stderr ?? ''}`;
}

function assertSame(actual, expected, label) {
  if (readFileSync(actual, 'utf8') !== readFileSync(expected, 'utf8')) {
    throw new Error(`${label} does not match ${expected}`);
  }
}

const fixture = mkdtempSync(join(tmpdir(), 'forgekeep-ui-inventory-root.'));
try {
  const rootJson = join(fixture, 'root.json');
  const rootMd = join(fixture, 'root.md');
  run('direct inventory generator from repository root', process.execPath, [
    join(root, 'scripts/ui-inventory.mjs'),
    '--json', relative(root, rootJson),
    '--md', relative(root, rootMd),
  ]);

  const npmJson = join(fixture, 'npm.json');
  const npmMd = join(fixture, 'npm.md');
  run('npm inventory generator from web cwd', npm, [
    '--prefix', 'web', 'run', 'inventory:ui', '--',
    '--json', relative(root, npmJson),
    '--md', relative(root, npmMd),
  ]);
  run('npm inventory checker from web cwd', npm, [
    '--prefix', 'web', 'run', 'inventory:ui:check',
  ]);

  for (const [actual, expected, label] of [
    [rootJson, join(root, 'docs/ui-inventory.json'), 'direct JSON output'],
    [rootMd, join(root, 'docs/UI_INVENTORY.md'), 'direct Markdown output'],
    [npmJson, join(root, 'docs/ui-inventory.json'), 'npm JSON output'],
    [npmMd, join(root, 'docs/UI_INVENTORY.md'), 'npm Markdown output'],
  ]) {
    assertSame(actual, expected, label);
  }
} catch (error) {
  console.error(`❌ UI inventory repository-root contract failed: ${error?.message || String(error)}`);
  process.exitCode = 1;
} finally {
  rmSync(fixture, { recursive: true, force: true });
}

if (!process.exitCode) {
  console.log('✅ UI inventory works through direct and npm entrypoints from their real working directories');
}
