//! A repository-touching subcommand started from a directory that is not the
//! instance's data directory.
//!
//! `[server].repo_root` defaults to the relative `./repos`, and a missing
//! directory is created rather than reported — `create_dir_all` exists to do
//! that. So an operator who gets `--db-url` / `--config` right and forgets
//! `--repo-root` writes the repository's row into the real database and clones
//! the repository into a root beside whatever directory the command was started
//! from. `forgekeep serve` then lists a repository whose git directory nobody
//! can open, and no step on the way said anything (card_cc8259eba428).
//!
//! The sibling half of `sqlite_db_presence`, and tested the same way: the real
//! binary with a real working directory, because the defect lives in the
//! relationship between the two and nothing smaller can reproduce it.

use std::path::Path;
use std::process::{Command, Output};

use rg_db::sea_orm::ConnectionTrait;

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

/// A real, migrated instance database — built by the binary's own `migrate`, so
/// the schema the count runs against is the shipped one rather than a fixture's
/// idea of it.
fn migrated_database(dir: &Path) -> String {
    let path = dir.join("forgekeep.db");
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let migrated = run_in(dir, &["migrate", "--db-url", &url]);
    assert!(
        migrated.status.success(),
        "the fixture instance must migrate:\n{}",
        diagnostic(&migrated)
    );
    url
}

/// Give the instance a repository, which is what makes a missing root a
/// mistake about *which* root rather than a clean install.
async fn add_a_repository(url: &str) {
    let db = rg_db::connect_with_pool(
        url,
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
        1,
    )
    .await
    .expect("open the fixture instance");
    // The owner comes first: this pool enforces foreign keys, so a repository
    // with no account behind it is not a row this schema will hold.
    for statement in [
        "INSERT INTO users \
         (id, username, email, password_hash, is_admin, is_active, auth_provider, mfa_enabled, \
          login_attempts, session_version, created_at, updated_at) \
         VALUES (1, 'alice', 'alice@example.com', '', 0, 1, 'local', 0, 0, 0, \
                 '2026-01-01 00:00:00+00:00', '2026-01-01 00:00:00+00:00')",
        "INSERT INTO repositories \
         (id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, \
          updated_at) \
         VALUES (1, 1, 'site', 0, 'main', 0, 0, '2026-01-01 00:00:00+00:00', \
                 '2026-01-01 00:00:00+00:00')",
    ] {
        db.execute_unprepared(statement)
            .await
            .unwrap_or_else(|error| {
                panic!("seed the fixture instance with `{statement}`: {error}")
            });
    }
    db.close().await.expect("close the fixture instance");
}

/// The card's reproduction: the database is the right one, the repository root
/// is not, and the only thing standing between them is a `create_dir_all`.
#[tokio::test]
async fn import_started_where_there_is_no_repo_root_refuses_and_creates_nothing() {
    let data = tempfile::tempdir().expect("a data directory");
    let url = migrated_database(data.path());
    add_a_repository(&url).await;

    let cwd = tempfile::tempdir().expect("a working directory that is not the data directory");
    let result = run_in(
        cwd.path(),
        &[
            "import",
            "github",
            "https://github.com/alice/site",
            "--target-owner",
            "alice",
            "--db-url",
            &url,
        ],
    );
    let text = diagnostic(&result);

    assert!(
        !result.status.success(),
        "an import into a repository root that is not this instance's must fail:\n{text}"
    );
    assert!(
        !cwd.path().join("repos").exists(),
        "the refusal must not leave a repository root behind:\n{text}"
    );
    assert!(text.contains("does not exist"), "{text}");
    assert!(
        text.contains(&cwd.path().join("repos").display().to_string()),
        "the refusal must name the absolute root it resolved:\n{text}"
    );
    assert!(
        !text.contains("Starting import"),
        "the clone must not be started:\n{text}"
    );
}

/// The other half of the same decision. A root that is not there is the right
/// answer on a clean install, so the check must let that through — otherwise it
/// would trade one silent failure for a first-run that cannot succeed.
///
/// The import is then refused by `validate_repo_name` a step later, which is
/// what keeps this test off the network: the assertion is about the directory
/// the command left behind, and about the refusal it did *not* produce.
#[tokio::test]
async fn import_on_a_clean_install_still_creates_the_repository_root() {
    let data = tempfile::tempdir().expect("a data directory");
    let url = migrated_database(data.path());

    let cwd = tempfile::tempdir().expect("a working directory");
    let result = run_in(
        cwd.path(),
        &[
            "import",
            "github",
            "https://github.com/alice/site",
            "--target-owner",
            "alice",
            // Refused inside `start_import`, after the root has been created.
            "--target-name",
            "site.git",
            "--db-url",
            &url,
        ],
    );
    let text = diagnostic(&result);

    assert!(
        cwd.path().join("repos").is_dir(),
        "a clean install must still get its repository root:\n{text}"
    );
    assert!(
        !text.contains("was pointed at the repository storage root"),
        "an instance with no repositories must not be refused:\n{text}"
    );
    assert!(
        text.contains("site.git"),
        "the import must get as far as its own name check:\n{text}"
    );
}

/// `index-repo` only ever reads out of the root, so a missing one cannot become
/// the right one by being created. Before this it reported the relative path of
/// a repository directory, which reads the same on the machine where the root
/// is right and on the one where the whole root is wrong.
#[tokio::test]
async fn index_repo_started_where_there_is_no_repo_root_names_the_root_it_resolved() {
    let data = tempfile::tempdir().expect("a data directory");
    let url = migrated_database(data.path());
    add_a_repository(&url).await;

    let cwd = tempfile::tempdir().expect("a working directory");
    let result = run_in(cwd.path(), &["index-repo", "alice/site", "--db-url", &url]);
    let text = diagnostic(&result);

    assert!(
        !result.status.success(),
        "indexing out of a root that is not there must fail:\n{text}"
    );
    assert!(
        !cwd.path().join("repos").exists(),
        "a read-only command must not create a root:\n{text}"
    );
    assert!(
        text.contains(&cwd.path().join("repos").display().to_string()),
        "the refusal must name the absolute root it resolved:\n{text}"
    );
}

/// `create-repo` opens no database — it exists to make a bare repository with
/// no row — so it has nothing to ask the question the other two ask. What it
/// can stop doing is reporting the relative spelling back: `./repos` is
/// identical on the machine where the root is the server's and on the one where
/// it has just appeared next to the operator's shell.
#[test]
fn create_repo_names_the_absolute_root_it_wrote_into() {
    let cwd = tempfile::tempdir().expect("a working directory");
    let result = run_in(cwd.path(), &["create-repo", "alice", "site"]);
    let text = diagnostic(&result);

    assert!(
        result.status.success(),
        "creating a bare repository must still work:\n{text}"
    );
    assert!(
        cwd.path().join("repos/alice/site.git").is_dir(),
        "the bare repository must be there:\n{text}"
    );
    assert!(
        text.contains(&cwd.path().join("repos").display().to_string()),
        "the output must name the absolute root it wrote into:\n{text}"
    );
}
