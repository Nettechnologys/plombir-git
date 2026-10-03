#!/usr/bin/env node

// Mutation stand for cli-repo-root-contract-check.mjs.
//
// A green repository only proves the defect is absent today. These fixtures put
// it back — the exact shape `import` had when it wrote the repository's row
// into the real database and created a second `./repos` beside whatever
// directory the command was started from (card_cc8259eba428) — and assert that
// the sweep goes red on it.
//
// Both halves are exercised, because they fail in different ways. A command
// that resolves the root and never reaches `crate::repo_root` is the original
// defect; a command that *stops* asking, or a file that grows one more
// directory-creating site, is how the same defect comes back through an
// inventory that a filename allowlist would wave through — which is why both
// inventories carry counts. The floors are exercised too: an absence-based
// check that is quietly reading the wrong tree reports success forever.

import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { tmpdir } from 'node:os';
import { fileURLToPath } from 'node:url';

const scriptsDir = dirname(fileURLToPath(import.meta.url));
const check = join(scriptsDir, 'cli-repo-root-contract-check.mjs');

function run(fixture) {
  const result = spawnSync(process.execPath, [check], {
    env: { ...process.env, PLOMBIR_GIT_CLI_REPO_ROOT_ROOT: fixture },
    encoding: 'utf8',
  });
  return { status: result.status, output: `${result.stdout ?? ''}${result.stderr ?? ''}` };
}

const GATEWAY = `pub(crate) enum MissingRepoRoot { CreateOnACleanInstance, Refuse }

pub(crate) async fn check_repo_root_presence(
    db: &rg_db::DatabaseConnection,
    repo_root: &std::path::Path,
    operation: &str,
    missing: MissingRepoRoot,
) -> anyhow::Result<()> {
    Ok(())
}

pub(crate) fn announce_a_new_repo_root(resolved: &std::path::Path) {}
`;

/**
 * The tree the real inventories describe: five resolution sites and two
 * decisions in `commands.rs`, and eight directory-creating sites spread over
 * the three files that own one. Two resolvers decide neither way —
 * `list-tombstones` reads a deployment and refuses a root that is not there,
 * and `migrate` reads the root only to refuse a new database beside stored
 * repositories — so the baseline has to carry resolvers with no gateway
 * decision beside them or the count would only ever describe the other three. Every fixture starts from this, so a red below
 * is about the mutation rather than about a fixture that never matched.
 *
 * Both spellings appear on purpose. `create-repo` makes its root through the
 * owner-only helper and the `<name>.git` below it through a plain
 * `create_dir_all`, which is exactly the shape the real command has — a
 * baseline that knew only one of them would let the other drift unseen.
 */
function writeBaseline(fixture, { commandsExtra = '', adminExtra = '', serveExtra = '' } = {}) {
  const src = join(fixture, 'crates/rg-cli/src');
  mkdirSync(src, { recursive: true });
  writeFileSync(join(src, 'repo_root.rs'), GATEWAY);
  writeFileSync(
    join(src, 'config.rs'),
    `pub(crate) fn resolve_repo_root(cli: Option<String>, cfg: Option<&ConfigFile>) -> String {
    cli.unwrap_or_else(|| DEFAULT_REPO_ROOT.to_string())
}

pub(crate) fn resolve_settings(cli: CliSettings, cfg: Option<&ConfigFile>) -> ResolvedSettings {
    ResolvedSettings { repo_root: config::resolve_repo_root(cli.repo_root, cfg) }
}
`,
  );
  writeFileSync(
    join(src, 'commands.rs'),
    `fn resolve_db_url_and_repo_root(repo_root: Option<String>) -> anyhow::Result<String> {
    Ok(config::resolve_repo_root(repo_root, cfg.as_ref()))
}

pub(crate) fn cmd_create_repo(repo_root: Option<String>) -> anyhow::Result<()> {
    let repo_root = PathBuf::from(config::resolve_repo_root(repo_root, cfg.as_ref()));
    repo_root::announce_a_new_repo_root(&config::absolute_path(&repo_root));
    rg_core::platform::fs::create_dir_all_owner_only(&repo_root)?;
    std::fs::create_dir_all(&repo_dir)?;
    Ok(())
}

pub(crate) async fn cmd_import(repo_root: Option<String>) -> anyhow::Result<()> {
    let repo_root = config::resolve_repo_root(repo_root, cfg.as_ref());
    repo_root::check_repo_root_presence(db.connection(), &repo_root, "plombir-git import", repo_root::MissingRepoRoot::CreateOnACleanInstance).await?;
    std::fs::create_dir_all(&repo_root)?;
    Ok(())
}

pub(crate) async fn cmd_index_repo() -> anyhow::Result<()> {
    repo_root::check_repo_root_presence(&db, path, "plombir-git index-repo", repo_root::MissingRepoRoot::Refuse).await?;
    Ok(())
}

pub(crate) async fn cmd_migrate() -> anyhow::Result<()> {
    let repo_root = std::path::PathBuf::from(config::resolve_repo_root(None, cfg));
    dbconn::connect_offline_migration(&db_url, "plombir-git migrate", dbconn::MissingDatabase::CreateForANewInstance { repo_root: &repo_root }).await?;
    Ok(())
}

pub(crate) async fn cmd_list_tombstones(repo_root: Option<String>) -> anyhow::Result<()> {
    let repo_root = PathBuf::from(config::resolve_repo_root(repo_root, cfg.as_ref()));
    rg_core::deletion_inventory::inventory(&repo_root).await?;
    Ok(())
}
${commandsExtra}`,
  );
  writeFileSync(
    join(src, 'admin.rs'),
    `pub(crate) async fn backup_sqlite_db(output: &PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(parent)?;
    Ok(())
}

pub(crate) fn restore_sqlite_db(input: &PathBuf) -> anyhow::Result<()> {
    std::fs::create_dir_all(parent)?;
    Ok(())
}
${adminExtra}`,
  );
  writeFileSync(
    join(src, 'serve.rs'),
    `fn publish_listen_addresses(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(parent)?;
    Ok(())
}

fn ensure_key_file(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(parent)?;
    Ok(())
}

pub(crate) async fn run_serve() -> anyhow::Result<()> {
    std::fs::create_dir_all(&repo_root)?;
    Ok(())
}
${serveExtra}`,
  );
}

const cases = [
  {
    name: 'the tree the inventories describe passes',
    expect: 'green',
    build: (fixture) => writeBaseline(fixture),
    contains: 'CLI repository root',
  },
  {
    name: 'a command resolves the root and never reaches the gateway',
    expect: 'red',
    build: (fixture) => {
      writeBaseline(fixture);
      // The original defect, in the smallest form it can take: a new subcommand
      // copies its neighbour's resolution and none of its judgement.
      writeFileSync(
        join(fixture, 'crates/rg-cli/src/mirror_cmd.rs'),
        `pub(crate) async fn cmd_mirror(repo_root: Option<String>) -> anyhow::Result<()> {
    let repo_root = config::resolve_repo_root(repo_root, cfg.as_ref());
    std::fs::create_dir_all(&repo_root)?;
    Ok(())
}
`,
      );
    },
    contains: 'not in REPO_ROOT_DECIDERS',
  },
  {
    name: 'a listed command stops asking the missing-root question',
    expect: 'red',
    build: (fixture) => {
      writeBaseline(fixture);
      const src = join(fixture, 'crates/rg-cli/src/commands.rs');
      writeFileSync(
        src,
        `fn resolve_db_url_and_repo_root(repo_root: Option<String>) -> anyhow::Result<String> {
    Ok(config::resolve_repo_root(repo_root, cfg.as_ref()))
}

pub(crate) fn cmd_create_repo(repo_root: Option<String>) -> anyhow::Result<()> {
    let repo_root = PathBuf::from(config::resolve_repo_root(repo_root, cfg.as_ref()));
    repo_root::announce_a_new_repo_root(&config::absolute_path(&repo_root));
    rg_core::platform::fs::create_dir_all_owner_only(&repo_root)?;
    std::fs::create_dir_all(&repo_dir)?;
    Ok(())
}

pub(crate) async fn cmd_import(repo_root: Option<String>) -> anyhow::Result<()> {
    let repo_root = config::resolve_repo_root(repo_root, cfg.as_ref());
    std::fs::create_dir_all(&repo_root)?;
    Ok(())
}

pub(crate) async fn cmd_index_repo() -> anyhow::Result<()> {
    repo_root::check_repo_root_presence(&db, path, "plombir-git index-repo", repo_root::MissingRepoRoot::Refuse).await?;
    Ok(())
}

pub(crate) async fn cmd_list_tombstones(repo_root: Option<String>) -> anyhow::Result<()> {
    let repo_root = PathBuf::from(config::resolve_repo_root(repo_root, cfg.as_ref()));
    rg_core::deletion_inventory::inventory(&repo_root).await?;
    Ok(())
}
`,
      );
    },
    contains: 'at 1 site(s), REPO_ROOT_DECIDERS says 2',
  },
  {
    name: 'a file the inventory never heard of starts creating directories',
    expect: 'red',
    build: (fixture) => {
      writeBaseline(fixture);
      writeFileSync(
        join(fixture, 'crates/rg-cli/src/telemetry.rs'),
        `pub(crate) fn init() -> anyhow::Result<()> {
    std::fs::create_dir_all("./data/traces")?;
    Ok(())
}
`,
      );
    },
    contains: 'not in DIRECTORY_CREATORS',
  },
  {
    name: 'a file the inventory already lists grows one more creating site',
    expect: 'red',
    // The case a filename allowlist waves through, and the reason both
    // inventories carry a count rather than a set of names.
    build: (fixture) =>
      writeBaseline(fixture, {
        serveExtra: `fn spare(path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}
`,
      }),
    contains: 'DIRECTORY_CREATORS says 3',
  },
  {
    name: 'an extra site spelled the owner-only way is still a site',
    expect: 'red',
    // The narrowing helper creates exactly the directories `create_dir_all`
    // does — it only decides the mode of the ones it made — so a sweep that
    // knew one spelling and not the other would read a directory out of
    // existence by the act of protecting it.
    build: (fixture) =>
      writeBaseline(fixture, {
        adminExtra: `pub(crate) fn export_audit(output: &PathBuf) -> anyhow::Result<()> {
    rg_core::platform::fs::create_dir_all_owner_only(parent)?;
    Ok(())
}
`,
      }),
    // Named down to the file and the count: the drift message is the same
    // sentence for every file, so a bare "says 2" would be satisfied by any
    // other entry drifting and the case would pass without the spelling ever
    // being seen.
    contains: 'admin.rs creates directories at 3 site(s)',
  },
  {
    name: 'the async twin of the owner-only helper counts as a site too',
    expect: 'red',
    build: (fixture) =>
      writeBaseline(fixture, {
        serveExtra: `async fn spare(path: &Path) -> anyhow::Result<()> {
    rg_core::platform::fs::create_dir_all_owner_only_async(path).await?;
    Ok(())
}
`,
      }),
    contains: 'serve.rs creates directories at 4 site(s)',
  },
  {
    name: 'a `#[cfg(test)]` directory is not an inventory entry',
    expect: 'green',
    build: (fixture) =>
      writeBaseline(fixture, {
        commandsExtra: `#[cfg(test)]
mod tests {
    #[test]
    fn builds_a_scratch_root() {
        std::fs::create_dir_all(dir.path().join("repos")).unwrap();
    }
}
`,
      }),
    contains: 'CLI repository root',
  },
  {
    name: 'the gateway is imported bare, hiding its call sites',
    expect: 'red',
    build: (fixture) =>
      writeBaseline(fixture, {
        commandsExtra: 'use crate::repo_root::check_repo_root_presence;\n',
      }),
    contains: 'imported into scope bare',
  },
  {
    name: 'the gateway is not where the check thinks it is',
    expect: 'red',
    build: (fixture) => {
      writeBaseline(fixture);
      writeFileSync(join(fixture, 'crates/rg-cli/src/repo_root.rs'), 'pub(crate) fn nothing() {}\n');
    },
    contains: 'entry points it expects',
  },
  {
    name: 'nothing resolves the root any more',
    expect: 'red',
    build: (fixture) => {
      writeBaseline(fixture);
      writeFileSync(
        join(fixture, 'crates/rg-cli/src/commands.rs'),
        `pub(crate) async fn cmd_import() -> anyhow::Result<()> {
    repo_root::check_repo_root_presence(db, path, "plombir-git import", repo_root::MissingRepoRoot::Refuse).await?;
    std::fs::create_dir_all(&repo_root)?;
    std::fs::create_dir_all(&repo_dir)?;
    Ok(())
}
`,
      );
    },
    contains: 'found no `config::resolve_repo_root`',
  },
];

const failures = [];
for (const testCase of cases) {
  const fixture = mkdtempSync(join(tmpdir(), 'plombir-git-repo-root-stand-'));
  try {
    testCase.build(fixture);
    const { status, output } = run(fixture);
    const green = status === 0;
    if (green !== (testCase.expect === 'green')) {
      failures.push(
        `${testCase.name}: expected ${testCase.expect}, the check exited ${status}\n${output}`,
      );
      continue;
    }
    if (!output.includes(testCase.contains)) {
      failures.push(
        `${testCase.name}: the check ${testCase.expect === 'green' ? 'passed' : 'failed'} for a ` +
          `reason this stand cannot recognise — expected ${JSON.stringify(testCase.contains)}\n${output}`,
      );
    }
  } finally {
    rmSync(fixture, { recursive: true, force: true });
  }
}

if (failures.length > 0) {
  for (const failure of failures) console.error(`❌ ${failure}`);
  process.exit(1);
}

console.log(
  `✅ CLI repo-root stand: ${cases.length} fixtures — the sweep reddens on a resolver that never ` +
    'decides, on either inventory drifting, on a hidden import and on a tree where the gateway is ' +
    'missing, and stays quiet on test-only directories.',
);
