//! card_7559ec4ef7fc: merge requests imported from GitLab lose the foreign
//! `head_repo_id` the importer wrote, and nothing else does — a fork pull
//! request whose fork was deleted keeps its dangling head.
//!
//! Every clause of the byline-less criterion has a row here that only it
//! protects, so dropping any one of them turns this test red.

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TryGetable};
use sea_orm_migration::{MigrationName, MigratorTrait};

use super::{repair_imported_heads, Repaired};

/// Ids above anything the shared exclusive database holds: on the server jobs
/// the issue-comment and agent-run migration tests seeded it first.
const BASE: i64 = 9_200;
/// Head repository ids no `repositories` row carries.
const GONE: i64 = 987_600;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-gitlab-heads-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// Apply every migration before this one that the database does not have yet.
async fn migrate_to_just_before(db: &DatabaseConnection) {
    let name = super::Migration.name().to_string();
    let position = crate::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == name)
        .unwrap_or_else(|| panic!("{name} must be part of the migration list"));
    let applied = crate::migrations::Migrator::get_applied_migrations(db)
        .await
        .expect("read the applied migrations")
        .len();
    assert!(
        applied <= position,
        "the database already has {name}: an upgrade test needs one that does not"
    );
    let steps = u32::try_from(position - applied).expect("migration count fits in u32");
    if steps > 0 {
        crate::migrations::Migrator::up(db, Some(steps))
            .await
            .expect("migrate to the schema immediately before this migration");
    }
}

/// The pull requests, by what the migration must do with each.
struct Rows {
    /// GitLab byline, head an unrelated local repository.
    byline_unrelated: i64,
    /// GitLab byline on a self-hosted instance, head a repository that never existed.
    byline_gone: i64,
    /// No byline: merge request without an author, head gone.
    bare_gone: i64,
    /// No byline, head an unrelated local repository.
    bare_unrelated: i64,
    /// Head a live fork of the target: only `create_pr` writes that.
    live_fork: i64,
    /// A fork pull request opened here whose fork was deleted afterwards.
    deleted_fork: i64,
    /// Opened by someone other than the account that ran the import.
    other_author: i64,
    /// Imported from GitHub, whose importer always wrote `NULL`.
    github: i64,
    /// In a repository that was never imported.
    not_imported: i64,
}

const IMPORTER: i64 = BASE;
const OTHER: i64 = BASE + 1;
const TARGET: i64 = BASE;
const FORK: i64 = BASE + 1;
const UNRELATED: i64 = BASE + 2;
const PLAIN: i64 = BASE + 3;

async fn seed(db: &DatabaseConnection) -> Rows {
    let mut script = format!(
        r#"
        INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
        VALUES ({IMPORTER}, 'importer{IMPORTER}', 'importer{IMPORTER}@example.invalid', '',
                CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
        VALUES ({OTHER}, 'other{OTHER}', 'other{OTHER}@example.invalid', '',
                CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES ({TARGET}, {IMPORTER}, 'imported{TARGET}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, origin_repo_id, created_at, updated_at)
        VALUES ({FORK}, {OTHER}, 'imported{TARGET}', {TARGET}, CURRENT_TIMESTAMP,
                CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES ({UNRELATED}, {OTHER}, 'unrelated{UNRELATED}', CURRENT_TIMESTAMP,
                CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES ({PLAIN}, {IMPORTER}, 'plain{PLAIN}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO import_tasks (id, user_id, repo_id, platform, source_url, target_owner,
                                  target_name, status, progress, import_repo, import_issues,
                                  import_pull_requests, import_wiki, import_releases,
                                  import_labels, import_milestones, created_at, updated_at)
        VALUES ({BASE}, {IMPORTER}, {TARGET}, 'gitlab', 'https://gitlab.com/acme/widgets',
                'importer{IMPORTER}', 'imported{TARGET}', 'completed', 100, TRUE, TRUE, TRUE,
                FALSE, FALSE, TRUE, TRUE, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        "#
    );
    let rows = Rows {
        byline_unrelated: BASE,
        byline_gone: BASE + 1,
        bare_gone: BASE + 2,
        bare_unrelated: BASE + 3,
        live_fork: BASE + 4,
        deleted_fork: BASE + 5,
        other_author: BASE + 6,
        github: BASE + 7,
        not_imported: BASE + 8,
    };
    for (id, repo, author, head, service) in [
        (
            rows.byline_unrelated,
            TARGET,
            IMPORTER,
            UNRELATED,
            Some("gitlab"),
        ),
        (
            rows.byline_gone,
            TARGET,
            IMPORTER,
            GONE,
            Some("gitlab:git.example.com"),
        ),
        (rows.bare_gone, TARGET, IMPORTER, GONE + 1, None),
        (rows.bare_unrelated, TARGET, IMPORTER, UNRELATED, None),
        (rows.live_fork, TARGET, IMPORTER, FORK, None),
        (rows.deleted_fork, TARGET, IMPORTER, GONE + 2, None),
        (rows.other_author, TARGET, OTHER, GONE + 3, None),
        (rows.github, TARGET, IMPORTER, GONE + 4, Some("github")),
        (rows.not_imported, PLAIN, IMPORTER, GONE + 5, None),
    ] {
        let service = service.map_or_else(|| "NULL".to_string(), |s| format!("'{s}'"));
        script.push_str(&format!(
            "INSERT INTO pull_requests (id, repo_id, number, title, state, is_draft, \
                                        auto_merge_enabled, author_id, head_branch, base_branch, \
                                        head_repo_id, original_service, created_at, updated_at) \
             VALUES ({id}, {repo}, {id}, 'pr {id}', 'open', FALSE, FALSE, {author}, 'feature', \
                     'main', {head}, {service}, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);"
        ));
    }
    script.push_str(&format!(
        "INSERT INTO pr_events (id, repo_id, pr_id, actor_id, event_type, metadata, created_at) \
         VALUES ({BASE}, {TARGET}, {}, {IMPORTER}, 'pull_request_opened', '{{}}', \
                 CURRENT_TIMESTAMP);",
        rows.deleted_fork
    ));
    for statement in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        db.execute_unprepared(statement)
            .await
            .unwrap_or_else(|error| panic!("seed `{statement}`: {error}"));
    }
    rows
}

async fn scalar<T: TryGetable>(db: &DatabaseConnection, sql: &str) -> T {
    let row = db
        .query_one(Statement::from_string(
            db.get_database_backend(),
            sql.to_owned(),
        ))
        .await
        .unwrap_or_else(|error| panic!("`{sql}`: {error}"))
        .unwrap_or_else(|| panic!("`{sql}` returned no row"));
    row.try_get_by_index(0)
        .unwrap_or_else(|error| panic!("`{sql}`: {error}"))
}

async fn head(db: &DatabaseConnection, pr: i64) -> Option<i64> {
    scalar(
        db,
        &format!("SELECT head_repo_id FROM pull_requests WHERE id = {pr}"),
    )
    .await
}

async fn exercise(db: &DatabaseConnection, rows: &Rows) {
    for (label, pr) in [
        ("GitLab byline, unrelated head", rows.byline_unrelated),
        ("GitLab byline, missing head", rows.byline_gone),
        ("byline-less import, missing head", rows.bare_gone),
        ("byline-less import, unrelated head", rows.bare_unrelated),
    ] {
        assert_eq!(head(db, pr).await, None, "{label}: kept a foreign head");
    }
    for (label, pr, expected) in [
        ("head is a live fork of the target", rows.live_fork, FORK),
        (
            "fork pull request whose fork was deleted",
            rows.deleted_fork,
            GONE + 2,
        ),
        ("opened by another account", rows.other_author, GONE + 3),
        ("GitHub import", rows.github, GONE + 4),
        ("repository never imported", rows.not_imported, GONE + 5),
    ] {
        assert_eq!(
            head(db, pr).await,
            Some(expected),
            "{label}: lost a head it may own"
        );
    }
    let left_with_gitlab_byline: i64 = scalar(
        db,
        "SELECT COUNT(*) FROM pull_requests \
         WHERE original_service LIKE 'gitlab%' AND head_repo_id IS NOT NULL",
    )
    .await;
    assert_eq!(left_with_gitlab_byline, 0);

    let again = repair_imported_heads(db).await.expect("second run");
    assert_eq!(again.with_byline, 0, "a second run changes nothing");
    assert_eq!(again.without_byline, 0, "a second run changes nothing");
    assert!(
        again.undecidable >= 2,
        "the other author's and the never-imported repository's rows are reported \
         as undecidable: {again:?}"
    );
}

#[tokio::test]
async fn only_imported_merge_requests_lose_their_head_repository() {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway SQLite database");
    migrate_to_just_before(&db).await;
    let rows = seed(&db).await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("apply the migration over imported rows");
    exercise(&db, &rows).await;

    // On a fresh database the counts are exact.
    let undecidable = repair_imported_heads(&db).await.unwrap().undecidable;
    assert_eq!(
        Repaired {
            with_byline: 0,
            without_byline: 0,
            undecidable,
        },
        Repaired {
            with_byline: 0,
            without_byline: 0,
            undecidable: 2,
        }
    );
}

/// The same repair on the server database `PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL`
/// names. The CI jobs hand it over after the agent-run migration test, which
/// leaves it just before this migration.
#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL: a PostgreSQL or MySQL database without this migration"]
async fn only_imported_merge_requests_lose_their_head_repository_on_a_server_database() {
    let url = std::env::var("PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL must be set");
    assert!(!url.starts_with("sqlite"), "SQLite is covered above");
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to the exclusive test database");
    migrate_to_just_before(&db).await;
    let rows = seed(&db).await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("apply the migration over imported rows");
    exercise(&db, &rows).await;
}
