//! A subcommand started from a directory that has no `forgekeep.db`.
//!
//! The default database URL is relative (`sqlite://./forgekeep.db?mode=rwc`)
//! and SQLite is asked to create what it cannot open, so every one-shot command
//! used to address a brand new empty database whenever it ran from somewhere
//! other than the data directory — `docker exec` without `-w`, a cron entry,
//! another shell. `backup-db` was the dangerous one: it VACUUMed that empty
//! database into the backup file and printed `Backup written`, and the operator
//! found out at the one moment the backup existed for (card_8baddb74fa82).
//!
//! These tests run the real binary with a real working directory, because the
//! defect lives in the relationship between the two and nothing smaller can
//! reproduce it.

use std::path::Path;
use std::process::{Command, Output};

use rg_db::sea_orm::{self, ConnectionTrait};

fn run_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_forgekeep"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap_or_else(|error| panic!("run `forgekeep {}`: {error}", args.join(" ")))
}

fn diagnostic(output: &Output) -> String {
    format!(
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Build a database that looks like an instance: several tables, two of which
/// carry the rows a restored backup would be judged by.
async fn seed_instance(path: &Path) {
    let database_url = format!("sqlite://{}?mode=rwc", path.display());
    let db = rg_db::connect_with_pool(
        &database_url,
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
        rg_db::DEFAULT_MAX_CONNECTIONS,
    )
    .await
    .expect("open the source fixture");
    for statement in [
        "CREATE TABLE seaql_migrations (version VARCHAR NOT NULL PRIMARY KEY, applied_at BIGINT NOT NULL)",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL)",
        "CREATE TABLE repositories (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        "INSERT INTO seaql_migrations (version, applied_at) VALUES ('m20260101_000001_init', 1)",
        "INSERT INTO users (id, username) VALUES (1, 'alice'), (2, 'bob'), (3, 'carol')",
        "INSERT INTO repositories (id, name) VALUES (1, 'forgekeep'), (2, 'notes')",
    ] {
        db.execute_unprepared(statement)
            .await
            .unwrap_or_else(|error| panic!("seed the source fixture with `{statement}`: {error}"));
    }
    db.close().await.expect("close the source fixture");
}

/// `(table count, users rows, repositories rows)` — the shape a backup has to
/// reproduce for the acceptance on card_8baddb74fa82.
async fn contents(path: &Path) -> (i64, i64, i64) {
    let database_url = format!("sqlite://{}?mode=rwc", path.display());
    let db = rg_db::connect_with_pool(
        &database_url,
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
        1,
    )
    .await
    .unwrap_or_else(|error| panic!("open {} for counting: {error}", path.display()));

    let mut counts = Vec::new();
    for query in [
        "SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'table'",
        "SELECT COUNT(*) AS n FROM users",
        "SELECT COUNT(*) AS n FROM repositories",
    ] {
        let row = db
            .query_one(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                query,
            ))
            .await
            .unwrap_or_else(|error| panic!("run `{query}` on {}: {error}", path.display()))
            .unwrap_or_else(|| panic!("`{query}` returned no row"));
        counts.push(row.try_get::<i64>("", "n").expect("decode the count"));
    }
    db.close().await.expect("close the counted database");
    (counts[0], counts[1], counts[2])
}

/// The reproduction from the live instance, with `docker exec`'s missing `-w`
/// standing in as a working directory that holds no database.
#[test]
fn backup_db_started_where_there_is_no_database_refuses_and_creates_nothing() {
    let cwd = tempfile::tempdir().expect("a working directory without a database");
    let backups = tempfile::tempdir().expect("a backup directory");
    let output = backups.path().join("pre-deploy.db");

    let result = run_in(
        cwd.path(),
        &["backup-db", output.to_str().expect("UTF-8 output path")],
    );
    let text = diagnostic(&result);

    assert!(
        !result.status.success(),
        "a backup of a database that is not there must fail:\n{text}"
    );
    assert!(
        !cwd.path().join("forgekeep.db").exists(),
        "the refusal must not leave a database behind:\n{text}"
    );
    assert!(!output.exists(), "no backup file may be written:\n{text}");
    assert!(text.contains("does not exist"), "{text}");
    assert!(
        text.contains(&cwd.path().join("forgekeep.db").display().to_string()),
        "the refusal must name the database it resolved:\n{text}"
    );
    assert!(
        !text.contains("Backup written"),
        "success must not be reported:\n{text}"
    );
}

/// The second run of the same mistake. The first one left an empty
/// `forgekeep.db` in the wrong directory, so presence alone would now say yes
/// and the same nothing would be copied out under the right name.
#[test]
fn backup_db_refuses_a_source_that_holds_no_tables() {
    let cwd = tempfile::tempdir().expect("a working directory");
    std::fs::write(cwd.path().join("forgekeep.db"), b"")
        .expect("leave an empty database behind, as the first bad run did");
    let backups = tempfile::tempdir().expect("a backup directory");
    let output = backups.path().join("pre-deploy.db");

    let result = run_in(
        cwd.path(),
        &["backup-db", output.to_str().expect("UTF-8 output path")],
    );
    let text = diagnostic(&result);

    assert!(
        !result.status.success(),
        "a source with no tables is not a ForgeKeep database:\n{text}"
    );
    assert!(text.contains("no tables"), "{text}");
    assert!(!output.exists(), "no backup file may be written:\n{text}");
}

/// The positive half: pointed at a real database, the command still produces a
/// backup carrying the same tables and the same rows.
#[tokio::test]
async fn a_backup_reproduces_the_database_it_was_taken_from() {
    let dir = tempfile::tempdir().expect("a data directory");
    let source = dir.path().join("forgekeep.db");
    seed_instance(&source).await;
    let output = dir.path().join("backup.db");

    let result = run_in(
        dir.path(),
        &[
            "backup-db",
            output.to_str().expect("UTF-8 output path"),
            "--db-url",
            &format!("sqlite://{}?mode=rwc", source.display()),
        ],
    );
    let text = diagnostic(&result);
    assert!(result.status.success(), "the backup must succeed:\n{text}");
    assert!(text.contains("Backup written"), "{text}");

    assert_eq!(
        contents(&output).await,
        contents(&source).await,
        "the backup must carry the source's tables and rows:\n{text}"
    );
}

/// The sweep the card asks for, on a second subcommand: the class is the
/// resolution of a relative URL, not anything specific to `backup-db`.
#[test]
fn a_maintenance_command_started_where_there_is_no_database_refuses() {
    let cwd = tempfile::tempdir().expect("a working directory without a database");

    let result = run_in(cwd.path(), &["rebuild-fts"]);
    let text = diagnostic(&result);

    assert!(
        !result.status.success(),
        "rebuilding indexes of a database that is not there must fail:\n{text}"
    );
    assert!(
        !cwd.path().join("forgekeep.db").exists(),
        "the refusal must not leave a database behind:\n{text}"
    );
    assert!(text.contains("does not exist"), "{text}");
}

/// `migrate` keeps its licence to create one — it is how an instance is
/// installed — but it no longer does it quietly, because "installed a second,
/// empty database" and "upgraded the real one" otherwise print the same
/// success line.
#[test]
fn migrate_still_creates_a_database_and_says_which_one() {
    let cwd = tempfile::tempdir().expect("a working directory without a database");

    let result = run_in(cwd.path(), &["migrate"]);
    let text = diagnostic(&result);

    assert!(result.status.success(), "migrate must install:\n{text}");
    assert!(
        cwd.path().join("forgekeep.db").exists(),
        "migrate must create the database it was pointed at:\n{text}"
    );
    assert!(
        text.contains("creating a NEW"),
        "the creation must be announced:\n{text}"
    );
    assert!(
        text.contains(&cwd.path().join("forgekeep.db").display().to_string()),
        "the announcement must name the database:\n{text}"
    );
}
