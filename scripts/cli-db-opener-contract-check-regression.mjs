#!/usr/bin/env node

// Mutation stand for cli-db-opener-contract-check.mjs.
//
// A green repository only proves the bypass is absent today. The first fixtures
// put it back — the exact shape `backup-db` had when it VACUUMed an empty
// database into a backup file and printed success (card_8baddb74fa82) — and
// assert the sweep goes red on it, stays silent on the gateway's own openers and
// on a `#[cfg(test)]` pool that another gate governs, and cannot pass over a
// tree where the gateway is missing.
//
// The last two are about the other half: a command joining the unleased set
// (card_8ef8170e1f5b), once in a file the inventory has never heard of and once
// inside a file it already lists — the case a filename allowlist would wave
// through, and the reason the inventory carries a count.

import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { scratchDir } from './lib/scratch-dir.mjs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = join(scriptsDir, 'cli-db-opener-contract-check.mjs');

function run(fixture) {
  const result = spawnSync(process.execPath, [check], {
    env: { ...process.env, PLOMBIR_GIT_CLI_DB_OPENER_ROOT: fixture },
    encoding: 'utf8',
  });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

/**
 * The unleased openers the check's ONLINE_POOL_COMMANDS inventory expects:
 * two in `commands.rs`, one in `admin.rs`. Every fixture starts from this, so
 * a red below is about the mutation rather than about a missing inventory.
 */
function writeOnlineInventory(fixture, { adminExtra = '', commandsExtra = '' } = {}) {
  writeFileSync(
    join(fixture, 'crates/rg-cli/src/commands.rs'),
    `pub(crate) async fn cmd_rotate_instance_key(db_url: &str) -> anyhow::Result<()> {
    let _db = dbconn::connect_online(db_url, "plombir-git rotate-instance-key", dbconn::OnlineAccess::SingleRowWrite).await?;
    Ok(())
}

pub(crate) async fn cmd_index_repo(db_url: &str) -> anyhow::Result<()> {
    let _db = dbconn::connect_online(db_url, "plombir-git index-repo", dbconn::OnlineAccess::SameWorkAsALiveHandler).await?;
    Ok(())
}
${commandsExtra}`,
  );
  writeFileSync(
    join(fixture, 'crates/rg-cli/src/admin.rs'),
    `pub(crate) async fn backup_sqlite_db(db_url: &str) -> anyhow::Result<()> {
    let _db = crate::dbconn::connect_online(db_url, "plombir-git backup-db", crate::dbconn::OnlineAccess::NoWriteLockOnTheSource).await?;
    Ok(())
}
${adminExtra}`,
  );
}

function writeGateway(fixture) {
  writeFileSync(
    join(fixture, 'crates/rg-cli/src/dbconn.rs'),
    `//! The one place any \`plombir-git\` subcommand opens the database.

pub(crate) async fn connect_online(
    db_url: &str,
    operation: &str,
    access: OnlineAccess,
) -> anyhow::Result<DatabaseConnection> {
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

const bypass = scratchDir(join(tmpdir(), 'plombir-git-cli-db-opener-bypass.'));
const missingGateway = scratchDir(join(tmpdir(), 'plombir-git-cli-db-opener-gateway.'));
const honest = scratchDir(join(tmpdir(), 'plombir-git-cli-db-opener-honest.'));
const newOnline = scratchDir(join(tmpdir(), 'plombir-git-cli-db-opener-new-online.'));
const grownOnline = scratchDir(join(tmpdir(), 'plombir-git-cli-db-opener-grown-online.'));

try {
  for (const fixture of [bypass, missingGateway, honest]) {
    mkdirSync(join(fixture, 'crates/rg-cli/src'), { recursive: true });
  }

  // 1. The defect: a subcommand helper opening the database itself, plus the
  //    quieter spelling that imports the opener instead of qualifying it.
  writeGateway(bypass);
  writeOnlineInventory(bypass, {
    adminExtra: `
pub(crate) async fn dump_sqlite_db(db_url: &str) -> anyhow::Result<()> {
    let db = rg_db::connect(db_url).await?;
    db.execute_unprepared("VACUUM INTO 'out.db'").await?;
    println!("Backup written");
    Ok(())
}
`,
  });
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
  for (const expected of ['crates/rg-cli/src/admin.rs:', 'crates/rg-cli/src/report.rs:1']) {
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
  writeOnlineInventory(missingGateway);
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
  writeOnlineInventory(honest, {
    adminExtra: `
#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn a_throwaway_pool_is_not_this_gate_s_business() {
        let _db = rg_db::connect_with_pool(URL, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2).await;
    }
}
`,
  });
  const clean = run(honest);
  if (clean.status !== 0) {
    console.error(
      `❌ CLI DB opener check went red on a tree that routes every production opener through dbconn:\n${clean.output}`,
    );
    process.exit(1);
  }

  // ── The unleased-opener half ────────────────────────────────────────
  //
  // rustc already refuses a command that reaches an ordinary pool without
  // naming an `OnlineAccess`. What it cannot ask is whether the SET of such
  // commands is still the one somebody thought about, which is what these two
  // fixtures are about.
  mkdirSync(join(newOnline, 'crates/rg-cli/src'), { recursive: true });
  writeGateway(newOnline);
  writeOnlineInventory(newOnline);
  writeFileSync(
    join(newOnline, 'crates/rg-cli/src/prune.rs'),
    `pub(crate) async fn cmd_prune(db_url: &str) -> anyhow::Result<()> {
    // Copied from a neighbour, variant and all — which is exactly how a new
    // command joins the unleased set without anybody deciding it should.
    let _db = dbconn::connect_online(db_url, "plombir-git prune", dbconn::OnlineAccess::SingleRowWrite).await?;
    Ok(())
}
`,
  );
  const joined = run(newOnline);
  if (joined.status === 0) {
    console.error(
      `❌ a new unleased opener in an unlisted file passed:\n${joined.output}`,
    );
    process.exit(1);
  }
  if (!joined.output.includes('crates/rg-cli/src/prune.rs')) {
    console.error(`❌ the new unleased opener was not named:\n${joined.output}`);
    process.exit(1);
  }

  mkdirSync(join(grownOnline, 'crates/rg-cli/src'), { recursive: true });
  writeGateway(grownOnline);
  writeOnlineInventory(grownOnline, {
    adminExtra: `
pub(crate) async fn cmd_compact(db_url: &str) -> anyhow::Result<()> {
    let _db = crate::dbconn::connect_online(db_url, "plombir-git compact", crate::dbconn::OnlineAccess::SingleRowWrite).await?;
    Ok(())
}
`,
  });
  const grown = run(grownOnline);
  if (grown.status === 0) {
    console.error(
      `❌ a second unleased opener inside an already-listed file passed — a filename allowlist ` +
        `would have waved it through, which is why the inventory carries a count:\n${grown.output}`,
    );
    process.exit(1);
  }
  if (!grown.output.includes('ONLINE_POOL_COMMANDS says 1')) {
    console.error(`❌ the count mismatch was not reported:\n${grown.output}`);
    process.exit(1);
  }

  console.log(
    '✅ CLI DB opener mutation: a direct opener and an imported one are both rejected, the ' +
      'gateway and a cfg(test) pool are not, a tree without the gateway cannot pass, and a ' +
      'command that joins the unleased set — in a new file or in a listed one — is named',
  );
} finally {
  for (const fixture of [bypass, missingGateway, honest, newOnline, grownOnline]) {
    rmSync(fixture, { recursive: true, force: true });
  }
}
