//! Fixtures shared by the integration tests in this binary.

use std::path::Path;

/// A migrated SQLite database at `db_path`, without running the migration chain
/// for it.
///
/// Every test in this binary used to run all 104 migrations against its own
/// fresh file to arrive at the identical schema. Measured on the sibling
/// `rg-http` suite, that is where most of a run's wall clock went: 761 tests
/// took 597 s with a migration each and 112 s starting from a copy. So the chain
/// runs once per test binary ([`migrated_template`]) and each test starts from a
/// copy of its result.
///
/// Not a hand-written schema dump — the template is produced by
/// `run_migrations` itself, so a migration added tomorrow is in it the moment it
/// is in the chain, with nothing to keep in sync. The bookkeeping table travels
/// with the copy, so a test that runs the migrations again gets the no-op it
/// always got.
#[allow(dead_code)]
pub async fn migrated_sqlite(db_path: &Path, max_connections: u32) -> rg_db::DatabaseConnection {
    copy_migrated_template(db_path).await;
    rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        max_connections,
    )
    .await
    .expect("connect sqlite")
}

/// The migrated schema, built once per test binary.
///
/// Held in a `static`, so the directory outlives every test and is never
/// dropped; it is a `tempfile` directory, which the OS reclaims.
async fn migrated_template() -> &'static Path {
    static TEMPLATE: tokio::sync::OnceCell<tempfile::TempDir> = tokio::sync::OnceCell::const_new();
    TEMPLATE
        .get_or_init(|| async {
            let dir = tempfile::tempdir().expect("create the template directory");
            let path = dir.path().join("template.db");
            let db = rg_db::connect_with_pool(
                &format!("sqlite://{}?mode=rwc", path.display()),
                rg_db::TEST_CONNECT_TIMEOUT_SECS,
                60,
                1,
            )
            .await
            .expect("connect to the template database");
            rg_db::run_migrations(&db)
                .await
                .expect("migrate the template database");
            // Closing the pool is what checkpoints the WAL into the file being
            // copied. Without it a test would start from a database missing
            // every table the last checkpoint did not cover.
            db.close().await.expect("close the template database");
            dir
        })
        .await
        .path()
}

async fn copy_migrated_template(db_path: &Path) {
    let template = migrated_template().await.join("template.db");
    std::fs::copy(&template, db_path).unwrap_or_else(|error| {
        panic!(
            "copy the migrated template {} -> {}: {error}",
            template.display(),
            db_path.display()
        )
    });
    // Belt and braces: the close above normally removes these, but a WAL left
    // behind belongs to the copy as much as the pages do — half of it would be
    // a database missing its most recent commits.
    for suffix in ["-wal", "-shm"] {
        let sidecar = template.with_file_name(format!("template.db{suffix}"));
        if sidecar.exists() {
            let target = db_path.with_file_name(format!(
                "{}{suffix}",
                db_path.file_name().expect("db file name").to_string_lossy()
            ));
            std::fs::copy(&sidecar, &target).expect("copy the template WAL sidecar");
        }
    }
}
