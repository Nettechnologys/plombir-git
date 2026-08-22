#!/usr/bin/env node

// Mutation stand for cli-db-opener-contract-check.mjs.
//
// A green repository only proves the bypass is absent today. This fixture puts
// it back — the exact shape `backup-db` had when it VACUUMed an empty database
// into a backup file and printed success (card_8baddb74fa82) — and asserts the
// sweep goes red on it, stays silent on the gateway's own openers and on a
// `#[cfg(test)]` pool that another gate governs, and cannot pass over a tree
// where the gateway is missing.

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = join(scriptsDir, 'cli-db-opener-contract-check.mjs');

function run(fixture) {
  const result = spawnSync(process.execPath, [check], {
    env: { ...process.env, FORGEKEEP_CLI_DB_OPENER_ROOT: fixture },
    encoding: 'utf8',
  });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

function writeGateway(fixture) {
  writeFileSync(
    join(fixture, 'crates/rg-cli/src/dbconn.rs'),
    `//! The one place any \`forgekeep\` subcommand opens the database.

pub(crate) async fn connect(db_url: &str, operation: &str) -> anyhow::Result<DatabaseConnection> {
    check_database_presence(db_url, operation, MissingDatabase::Refuse)?;
    rg_db::connect(db_url).await
}

pub(crate) async fn connect_server_with_timeouts(db_url: &str) -> anyhow::Result<()> {
    rg_db::connect_with_timeouts(db_url, 10, 60).await
}

pub(crate) async fn connect_offline_migration(db_url: &str) -> anyhow::Result<()> {
    rg_db::connect(db_url).await
}

pub(crate) async fn connect_offline_maintenance(db_url: &str) -> anyhow::Result<()> {
    rg_db::connect(db_url).await
}
`,
  );
}

const bypass = mkdtempSync(join(tmpdir(), 'forgekeep-cli-db-opener-bypass.'));
const missingGateway = mkdtempSync(join(tmpdir(), 'forgekeep-cli-db-opener-gateway.'));
const honest = mkdtempSync(join(tmpdir(), 'forgekeep-cli-db-opener-honest.'));

try {
  for (const fixture of [bypass, missingGateway, honest]) {
    mkdirSync(join(fixture, 'crates/rg-cli/src'), { recursive: true });
  }

  // 1. The defect: a subcommand helper opening the database itself, plus the
  //    quieter spelling that imports the opener instead of qualifying it.
  writeGateway(bypass);
  writeFileSync(
    join(bypass, 'crates/rg-cli/src/admin.rs'),
    `pub(crate) async fn backup_sqlite_db(db_url: &str) -> anyhow::Result<()> {
    let db = rg_db::connect(db_url).await?;
    db.execute_unprepared("VACUUM INTO 'out.db'").await?;
    println!("Backup written");
    Ok(())
}
`,
  );
  writeFileSync(
    join(bypass, 'crates/rg-cli/src/report.rs'),
    `use rg_db::connect_with_pool;

pub(crate) async fn dump(db_url: &str) -> anyhow::Result<()> {
    let _db = connect_with_pool(db_url, 10, 60, 2).await?;
    Ok(())
}
`,
  );

  const mutated = run(bypass);
  if (mutated.status === 0) {
    console.error(
      `❌ CLI DB opener mutation passed: the check no longer rejects a direct opener:\n${mutated.output}`,
    );
    process.exit(1);
  }
  for (const expected of ['crates/rg-cli/src/admin.rs:2', 'crates/rg-cli/src/report.rs:1']) {
    if (!mutated.output.includes(expected)) {
      console.error(`❌ CLI DB opener mutation did not name ${expected}:\n${mutated.output}`);
      process.exit(1);
    }
  }
  if (mutated.output.includes('dbconn.rs:')) {
    console.error(
      `❌ CLI DB opener sweep flagged the gateway's own openers, which are the point of it:\n${mutated.output}`,
    );
    process.exit(1);
  }

  // 2. A gateway that is not where the sweep looks must be red, not green: an
  //    absence-based check that reads nothing passes vacuously.
  writeFileSync(
    join(missingGateway, 'crates/rg-cli/src/commands.rs'),
    'pub(crate) fn cmd_gen_secret() {}\n',
  );
  const vacuous = run(missingGateway);
  if (vacuous.status === 0) {
    console.error(
      `❌ CLI DB opener check reported green over a tree with no gateway:\n${vacuous.output}`,
    );
    process.exit(1);
  }

  // 3. The honest tree: every production opener behind the gateway, and a test
  //    pool that belongs to test-db-connect-timeout-contract-check.mjs instead.
  writeGateway(honest);
  writeFileSync(
    join(honest, 'crates/rg-cli/src/admin.rs'),
    `pub(crate) async fn backup_sqlite_db(db_url: &str) -> anyhow::Result<()> {
    let db = crate::dbconn::connect(db_url, "forgekeep backup-db").await?;
    db.execute_unprepared("VACUUM INTO 'out.db'").await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_throwaway_pool_is_not_this_gate_s_business() {
        let _db = rg_db::connect_with_pool(URL, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2).await;
    }
}
`,
  );
  const clean = run(honest);
  if (clean.status !== 0) {
    console.error(
      `❌ CLI DB opener check went red on a tree that routes every production opener through dbconn:\n${clean.output}`,
    );
    process.exit(1);
  }

  console.log(
    '✅ CLI DB opener mutation: a direct opener and an imported one are both rejected, the ' +
      'gateway and a cfg(test) pool are not, and a tree without the gateway cannot pass',
  );
} finally {
  for (const fixture of [bypass, missingGateway, honest]) {
    rmSync(fixture, { recursive: true, force: true });
  }
}
