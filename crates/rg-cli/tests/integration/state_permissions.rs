//! Process-boundary coverage for one-shot state creation.
//!
//! `umask` is process-global, so these assertions must execute the real CLI in
//! child processes rather than changing the integration-test harness itself.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output};

fn write_config(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("write the CLI config");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("protect the CLI config");
}

fn run_with_umask(cwd: &Path, args: &[&str], umask: u32) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plombir-git"));
    command.args(args).current_dir(cwd);
    // SAFETY: `pre_exec` runs after fork and before exec in the child. `umask`
    // is async-signal-safe and touches no Rust-managed memory.
    unsafe {
        command.pre_exec(move || {
            libc::umask(umask as libc::mode_t);
            Ok(())
        });
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("run `plombir-git {}`: {error}", args.join(" ")))
}

fn diagnostic(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path)
        .unwrap_or_else(|error| panic!("read mode of {}: {error}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

#[test]
fn create_repo_and_first_sqlite_file_use_the_configured_policy() {
    let directory = tempfile::tempdir().expect("a temporary instance root");
    let repo_root = directory.path().join("repos");
    let database = directory.path().join("plombir-git.db");
    let config = directory.path().join("plombir-git.toml");
    write_config(
        &config,
        &format!(
            "[server]\nrepo_root = \"{}\"\nstate_permissions = \"group-readable\"\n\n[database]\nurl = \"sqlite://{}?mode=rwc\"\n",
            repo_root.display(),
            database.display()
        ),
    );

    let migrate = run_with_umask(
        directory.path(),
        &[
            "migrate",
            "--config",
            config.to_str().expect("UTF-8 config path"),
        ],
        0o002,
    );
    let migrate_text = diagnostic(&migrate);
    assert!(migrate.status.success(), "{migrate_text}");
    assert_eq!(
        mode(&database),
        0o640,
        "SQLite's first open must inherit the configured group-readable policy:\n{migrate_text}"
    );

    let create = run_with_umask(
        directory.path(),
        &[
            "create-repo",
            "alice",
            "private",
            "--config",
            config.to_str().expect("UTF-8 config path"),
        ],
        0o002,
    );
    let create_text = diagnostic(&create);
    assert!(create.status.success(), "{create_text}");
    assert_eq!(
        mode(&repo_root.join("alice/private.git/HEAD")),
        0o640,
        "gix must inherit group-readable rather than the launcher's 0002 umask:\n{create_text}"
    );
    assert!(
        create_text
            .contains("Installed the process-wide creation policy for one-shot server-owned state")
            && create_text.contains("group-readable")
            && create_text.contains("0027"),
        "the selected one-shot policy must be explicit in the log:\n{create_text}"
    );
}

#[test]
fn explicit_backup_output_keeps_the_operators_creation_contract() {
    let directory = tempfile::tempdir().expect("a temporary instance root");
    let database = directory.path().join("plombir-git.db");
    let config = directory.path().join("plombir-git.toml");
    write_config(
        &config,
        &format!(
            "[server]\nstate_permissions = \"owner-only\"\n\n[database]\nurl = \"sqlite://{}?mode=rwc\"\n",
            database.display()
        ),
    );

    let migrate = run_with_umask(
        directory.path(),
        &[
            "migrate",
            "--config",
            config.to_str().expect("UTF-8 config path"),
        ],
        0o002,
    );
    assert!(migrate.status.success(), "{}", diagnostic(&migrate));

    let output = directory.path().join("operator-backup.db");
    let backup = run_with_umask(
        directory.path(),
        &[
            "backup-db",
            output.to_str().expect("UTF-8 output path"),
            "--config",
            config.to_str().expect("UTF-8 config path"),
        ],
        0o002,
    );
    let backup_text = diagnostic(&backup);
    assert!(backup.status.success(), "{backup_text}");
    assert_eq!(
        mode(&output),
        0o644,
        "an explicit operator output must not be silently narrowed by server state policy:\n{backup_text}"
    );
    assert!(
        !backup_text.contains("one-shot server-owned state"),
        "backup-db must not claim it installed the server-state policy:\n{backup_text}"
    );
}
