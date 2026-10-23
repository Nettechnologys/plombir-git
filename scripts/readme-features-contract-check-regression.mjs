#!/usr/bin/env node

import { readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import os from 'node:os';
import path from 'node:path';
import { removeScratchDir, scratchDir } from './lib/scratch-dir.mjs';

const root = process.cwd();
const original = readFileSync(path.join(root, 'docs/features.md'), 'utf8');
const temp = scratchDir(path.join(os.tmpdir(), 'readme-features-'));
try {
  for (const [label, mutated, expected] of [
    ['missing evidence', original.replace('[Wiki API](../crates/rg-http/src/api/wiki.rs)', '—'), 'Wiki: Implementation must link'],
    ['false wiki claim', original.replace('revision history stored in the database (no `.wiki.git` repository)', 'pages backed by Git'), 'Wiki: describe database revisions'],
  ]) {
    if (mutated === original) throw new Error(`${label}: mutation did not apply`);
    const fixture = path.join(temp, 'features.md');
    writeFileSync(fixture, mutated);
    const result = spawnSync(process.execPath, [path.join(root, 'scripts/readme-features-contract-check.mjs')], {
      cwd: root,
      env: { ...process.env, FEATURE_README_OVERRIDE: fixture },
      encoding: 'utf8',
    });
    if (result.status === 0 || !result.stderr.includes(expected)) {
      throw new Error(`${label}: gate did not reject the mutation: ${result.stderr}`);
    }
  }
  console.log('docs/features.md gate rejects missing evidence and a false wiki claim');
} finally {
  removeScratchDir(temp);
}
