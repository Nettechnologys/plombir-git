#!/usr/bin/env node

// Mutation stand for migrator-pool-contract-check.mjs. The clean-tree pass
// proves the two multi-connection regression fixtures remain valid exceptions;
// changing the previously flaky identity migration fixture back to a pool of
// two proves the rule still rejects the defect that motivated the gate.

import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const check = join(root, 'scripts', 'migrator-pool-contract-check.mjs');
const identityFixture =
  'crates/rg-db/tests/integration/identity_keys_not_blank.rs';
const preparedPoolFixture =
  'crates/rg-db/src/migrations/m20260805_000002_uploads_outlive_their_uploader_tests.rs';
const candidateFiles = [
  identityFixture,
  'crates/rg-db/tests/integration/repositories_namespace_rebuild.rs',
  'crates/rg-db/tests/integration/legacy_column_upgrade.rs',
  'crates/rg-db/src/migrations/m20260805_000004_repo_config_outlives_its_author_tests.rs',
  'crates/rg-db/src/migrations/m20260805_000005_account_owned_rows_follow_their_parent_tests.rs',
  preparedPoolFixture,
  'crates/rg-db/src/migrations/m20260730_000001_repositories_namespace_unique_tests.rs',
];

function fixtureRoot() {
  const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-migrator-pool-'));
  for (const file of candidateFiles) {
    const target = join(fixture, file);
    mkdirSync(dirname(target), { recursive: true });
    cpSync(join(root, file), target);
  }
  const inline = join(fixture, 'crates/demo/src/lib.rs');
  mkdirSync(dirname(inline), { recursive: true });
  writeFileSync(
    inline,
    `#[cfg(test)]
mod tests {
    async fn direct_migrator_fixture() {
        let db = crate::connect_with_pool("sqlite::memory:", crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
            .await
            .unwrap();
        crate::migrations::Migrator::up(&db, None).await.unwrap();
    }
}

// crate::migrations::Migrator::up(&commented_out, None).await.unwrap();
const DECOY: &str = "Migrator::down(&string_literal, None) connect_with_pool(url, 60, 60, 2)";
`,
  );
  return fixture;
}

function run(fixture) {
  const result = spawnSync(process.execPath, [check], {
    cwd: fixture,
    env: { ...process.env, PLOMBIR_GIT_MIGRATOR_POOL_ROOT: fixture },
    encoding: 'utf8',
  });
  return {
    status: result.status,
    output: `${result.stdout ?? ''}${result.stderr ?? ''}`,
  };
}

const fixture = fixtureRoot();
try {
  const clean = run(fixture);
  if (clean.status !== 0 || !clean.output.includes('8 direct-Migrator test files')) {
    console.error(`❌ unmutated Migrator pool fixture did not pass:\n${clean.output}`);
    process.exit(1);
  }

  const identity = join(fixture, identityFixture);
  const source = readFileSync(identity, 'utf8');
  const oneConnection = 'rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)';
  if (!source.includes(oneConnection)) {
    console.error(`❌ identity fixture anchor disappeared: ${oneConnection}`);
    process.exit(1);
  }
  writeFileSync(identity, source.replace(oneConnection, 'rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)'));

  const mutated = run(fixture);
  if (
    mutated.status === 0 ||
    !mutated.output.includes(`${identityFixture}:72`) ||
    !mutated.output.includes('must use literal 1 for max_connections')
  ) {
    console.error(`❌ two-connection mutation was not rejected for the intended reason:\n${mutated.output}`);
    process.exit(1);
  }

  writeFileSync(identity, source);
  const preparedPool = join(fixture, preparedPoolFixture);
  const preparedSource = readFileSync(preparedPool, 'utf8');
  const fourConnections = 'crate::TEST_CONNECT_TIMEOUT_SECS, 60, 4)';
  const preparedPoolConnections = preparedSource.split(fourConnections).length - 1;
  if (preparedPoolConnections === 0) {
    console.error(`❌ prepared-pool fixture anchor disappeared: ${fourConnections}`);
    process.exit(1);
  }
  writeFileSync(
    preparedPool,
    preparedSource.replaceAll(fourConnections, 'crate::TEST_CONNECT_TIMEOUT_SECS, 60, 1)'),
  );

  const staleAllowance = run(fixture);
  if (
    staleAllowance.status === 0 ||
    !staleAllowance.output.includes(`${preparedPoolFixture}:54`) ||
    !staleAllowance.output.includes('allowlist entry is stale')
  ) {
    console.error(`❌ stale multi-connection allowance was not rejected:\n${staleAllowance.output}`);
    process.exit(1);
  }

  console.log(
    '✅ Migrator pool mutations: inline tests are covered, the flaky pool of two is rejected, and stale allowances fail',
  );
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
