#!/usr/bin/env node

import {
  appendFileSync,
  cpSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { tmpdir } from 'node:os';

const scriptsDir = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(scriptsDir, '..');
const check = path.join(scriptsDir, 'server-side-ref-policy-contract-check.mjs');
const fixture = scratchDir(path.join(tmpdir(), 'plombir-git-ref-policy-'));
const sources = [
  'crates/rg-git/src/protocol/receive_pack.rs',
  'crates/rg-core/src/repo/service.rs',
  'crates/rg-core/src/pull_request/service.rs',
  'crates/rg-core/src/pull_request/merge_queue.rs',
  'crates/rg-core/src/mirror/service.rs',
  'crates/rg-core/src/import/service.rs',
];

for (const relative of sources) {
  const target = path.join(fixture, relative);
  mkdirSync(path.dirname(target), { recursive: true });
  cpSync(path.join(root, relative), target);
}

function run() {
  return spawnSync(process.execPath, [check], {
    cwd: root,
    env: { ...process.env, PLOMBIR_GIT_SERVER_SIDE_REF_POLICY_ROOT: fixture },
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

  writeFileSync(
    repoService,
    cleanRepoService.replace(
      'git.run(&["clone", "--bare", &source_url, &target_arg], None)',
      'git.run(&["clone", "--mirror", &source_url, &target_arg], None)',
    ),
  );
  expectRed('fork created with clone --mirror mutation', 'never `--mirror`');

  const pullService = path.join(fixture, 'crates/rg-core/src/pull_request/service.rs');
  const cleanPullService = readFileSync(pullService, 'utf8');
  // The path nearly every merge takes, and the one the census used to miss
  // (card_4f51d61a783f): a new `commit_as` outside every classified merge.
  appendFileSync(
    pullService,
    '\nfn unclassified_commit_writer(repo: &gix::Repository, tree: gix::ObjectId, parent: gix::ObjectId) {\n' +
      '    let signature = merge_signature("0 +0000");\n' +
      '    let _ = repo.commit_as(signature, signature, "refs/heads/main", "m", tree, [parent]);\n' +
      '}\n',
  );
  expectRed('new commit_as mutation', 'Unclassified production ref mover');

  writeFileSync(pullService, cleanPullService);
  appendFileSync(
    pullService,
    '\nfn unclassified_fetch(git: &rg_git::cli_gateway::GitCommandGateway, from: &str) {\n' +
      '    let _ = git.run(\n        &[\n            "fetch",\n            from,\n' +
      '            "+refs/heads/*:refs/heads/*",\n        ],\n        None,\n    );\n}\n',
  );
  expectRed('new fetch-into-branches mutation', 'Unclassified production ref mover');

  writeFileSync(
    pullService,
    cleanPullService.replace(
      '            "fetch",\n            "--no-tags",\n            &head_repo_path',
      '            "fetch",\n            &head_repo_path',
    ),
  );
  expectRed('fork fetch follows tags mutation', 'without `--no-tags`');
  writeFileSync(pullService, cleanPullService);

  const mirrorService = path.join(fixture, 'crates/rg-core/src/mirror/service.rs');
  const cleanMirrorService = readFileSync(mirrorService, 'utf8');
  writeFileSync(
    mirrorService,
    cleanMirrorService.replace('"+refs/tags/*:refs/tags/*",', '"+refs/*:refs/*",'),
  );
  expectRed('mirror publishes every ref mutation', 'a pull mirror may write only');
  writeFileSync(mirrorService, cleanMirrorService);

  const mergeQueue = path.join(fixture, 'crates/rg-core/src/pull_request/merge_queue.rs');
  const cleanMergeQueue = readFileSync(mergeQueue, 'utf8');
  writeFileSync(
    mergeQueue,
    cleanMergeQueue.replace(
      '"fetch",\n                    &head_repo_path',
      '"fetch",\n                    "--tags",\n                    &head_repo_path',
    ),
  );
  expectRed('object transfer stores tags mutation', 'is classified object-transfer-only but stores refs');
  writeFileSync(mergeQueue, cleanMergeQueue);

  writeFileSync(repoService, cleanRepoService);
  appendFileSync(
    repoService,
    '\nasync fn database_commit_decoy(txn: sea_orm::DatabaseTransaction) {\n' +
      '    let _ = txn.commit().await;\n}\n' +
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
