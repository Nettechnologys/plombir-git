#!/usr/bin/env node

import { writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { scratchDir, removeScratchDir } from './lib/scratch-dir.mjs';

const dir = scratchDir(join(tmpdir(), 'repo-path-check-'));
try {
  writeFileSync(join(dir, 'unsafe.ts'), 'request(`/repos/${owner}/${repo}/keys`);\n');
  const result = spawnSync(process.execPath, ['scripts/repo-path-encoding-contract-check.mjs'], {
    cwd: process.cwd(),
    env: { ...process.env, REPO_PATH_CLIENT_DIR: dir },
    encoding: 'utf8',
  });
  if (result.status !== 1 || !result.stderr.includes('unsafe.ts:1: raw owner/repo segment')) {
    console.error('repository path check did not reject a raw route parameter', result.stdout, result.stderr);
    process.exit(1);
  }
  console.log('repository path encoding check rejects raw route parameters');
} finally {
  removeScratchDir(dir);
}
