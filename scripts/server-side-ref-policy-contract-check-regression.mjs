#!/usr/bin/env node

import {
  appendFileSync,
  cpSync,
  mkdtempSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(scriptsDir, '..');
const check = path.join(scriptsDir, 'server-side-ref-policy-contract-check.mjs');
const fixture = mkdtempSync(path.join(tmpdir(), 'forgekeep-ref-policy-'));
const sources = [
  'crates/rg-git/src/protocol/receive_pack.rs',
  'crates/rg-core/src/repo/service.rs',
  'crates/rg-core/src/pull_request/service.rs',
  'crates/rg-core/src/pull_request/merge_queue.rs',
];

for (const relative of sources) {
  const target = path.join(fixture, relative);
  mkdirSync(path.dirname(target), { recursive: true });
  cpSync(path.join(root, relative), target);
}

function run() {
  return spawnSync(process.execPath, [check], {
    cwd: root,
    env: { ...process.env, FORGEKEEP_SERVER_SIDE_REF_POLICY_ROOT: fixture },
    encoding: 'utf8',
  });
}

function expectGreen(label) {
  const result = run();
  if (result.status !== 0) {
    throw new Error(`${label} unexpectedly failed:\n${result.stdout}${result.stderr}`);
  }
}

function expectRed(label, needle) {
  const result = run();
  const output = `${result.stdout}${result.stderr}`;
  if (result.status === 0 || !output.includes(needle)) {
    throw new Error(`${label} did not fail on ${JSON.stringify(needle)}:\n${output}`);
  }
}

try {
  expectGreen('clean classified census');

  const repoService = path.join(fixture, 'crates/rg-core/src/repo/service.rs');
  const cleanRepoService = readFileSync(repoService, 'utf8');
  appendFileSync(
    repoService,
    '\nfn unclassified_ref_writer(git: &rg_git::cli_gateway::GitCommandGateway) {\n' +
      '    let _ = git.run_or_bail(&["update-ref", "refs/heads/main", "deadbeef"], None);\n' +
      '}\n',
  );
  expectRed('new mover mutation', 'Unclassified production ref mover');

  writeFileSync(
    repoService,
    cleanRepoService.replace(
      'push_policy.verify_created_commit(',
      'push_policy.observe_created_commit(',
    ),
  );
  expectRed(
    'dropped commit verification mutation',
    'no longer verifies the created commit before publishing',
  );

  writeFileSync(
    repoService,
    cleanRepoService.replaceAll('--force-with-lease=', '--force='),
  );
  expectRed('dropped lease mutation', 'does not enforce an explicit ref lease');

  writeFileSync(
    repoService,
    cleanRepoService.replace(
      '        push_branch_with_lease(\n',
      '        publish_branch_without_lease(\n',
    ),
  );
  expectRed(
    'producer bypasses leased publisher mutation',
    'no longer publishes through push_branch_with_lease',
  );

  writeFileSync(repoService, cleanRepoService);
  appendFileSync(
    repoService,
    '\n// decoy.run(&["push", "origin", "main"], None);\n' +
      'const REF_MOVER_DECOY: &str = r#"git.run(&["update-ref", "refs/heads/main", "x"], None)"#;\n' +
      '#[cfg(test)]\nfn test_only_ref_writer(git: &rg_git::cli_gateway::GitCommandGateway) {\n' +
      '    let _ = git.run(&["push", "origin", "main"], None);\n}\n',
  );
  expectGreen('comments, strings, and cfg(test) decoys');

  console.log('Server-side ref policy census mutation stand ok');
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
