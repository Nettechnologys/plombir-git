#!/usr/bin/env node

// Mutation stand for the real receipt-producing verifier. Fake tool binaries
// keep it cheap while preserving the shell, streaming, status and receipt path.

import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');

function command(command, args, cwd, env = process.env) {
  const result = spawnSync(command, args, { cwd, env, encoding: 'utf8' });
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} failed (${result.status})\n${result.stdout}${result.stderr}`);
  }
}

function fixture(mode) {
  const dir = mkdtempSync(join(tmpdir(), 'forgekeep-push-warning-'));
  mkdirSync(join(dir, 'scripts'), { recursive: true });
  mkdirSync(join(dir, 'bin'), { recursive: true });
  writeFileSync(
    join(dir, 'scripts', 'verifier-under-test.sh'),
    readFileSync(join(root, 'scripts', 'verify-push-gates.sh')),
  );
  writeFileSync(
    join(dir, 'bin', 'cargo'),
    `#!/bin/sh\ncase "${mode}:$1" in\n  warning:doc) printf '%s\\n' 'warning: synthetic rustdoc warning' >&2 ;;\n  fail:clippy) exit 17 ;;\nesac\nexit 0\n`,
    { mode: 0o755 },
  );
  writeFileSync(join(dir, 'bin', 'node'), '#!/bin/sh\nexit 0\n', { mode: 0o755 });
  writeFileSync(join(dir, 'tracked'), 'fixture\n');

  command('git', ['init', '-q', '-b', 'main'], dir);
  command('git', ['config', 'user.name', 'ForgeKeep gate fixture'], dir);
  command('git', ['config', 'user.email', 'fixture@example.invalid'], dir);
  command('git', ['add', '.'], dir);
  command('git', ['commit', '-q', '-m', 'fixture'], dir);
  return dir;
}

function run(mode) {
  const dir = fixture(mode);
  try {
    const result = spawnSync('sh', ['scripts/verifier-under-test.sh'], {
      cwd: dir,
      env: { ...process.env, PATH: `${join(dir, 'bin')}:${process.env.PATH}` },
      encoding: 'utf8',
    });
    const output = `${result.stdout ?? ''}${result.stderr ?? ''}`;
    const receipt = join(dir, '.git', 'forgekeep-push-gates.receipt');
    return { status: result.status, output, receipt: existsSync(receipt) };
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

const clean = run('clean');
assert.equal(clean.status, 0, clean.output);
assert.equal(clean.receipt, true, 'a clean verifier run must record its exact-HEAD receipt');

const warning = run('warning');
assert.equal(warning.status, 1, warning.output);
assert.match(warning.output, /warning: synthetic rustdoc warning/);
assert.equal(warning.receipt, false, 'a rustdoc warning must prevent receipt creation');

const failure = run('fail');
assert.equal(failure.status, 17, failure.output);
assert.equal(failure.receipt, false, 'a command failure must keep its status and prevent receipt creation');

console.log('✅ verifier streams output, preserves failures, and refuses a rustdoc warning receipt');
