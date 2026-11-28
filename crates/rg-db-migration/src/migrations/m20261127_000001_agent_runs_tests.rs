//! The agent-run schema arrives over a live database (card_621be18a01e5):
//! tokens and agent profiles written before it keep every value, gain the
//! defaults that mean "what this token could do before", and the new rows
//! hold their uniqueness and cascades on every backend.

use sea_orm::{ConnectionTrait, DatabaseConnection, Statement, TryGetable};
use sea_orm_migration::{MigrationName, MigratorTrait};

/// Ids far above anything the shared exclusive database already holds: on the
/// server jobs it is the one the issue-comment migration test seeded first.
const BASE: i64 = 9_100;

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-agent-runs-{}.db",
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

/// What a database holds before this migration: an owner, their bot, a
/// repository, the bot's narrowed token with its repository list, a person's
/// plain token, and the bot's agent profile with its repository list.
async fn seed(db: &DatabaseConnection) {
    let owner = BASE;
    let bot = BASE + 1;
    let repo = BASE;
    let script = format!(
        r#"
        INSERT INTO users (id, username, email, password_hash, created_at, updated_at)
        VALUES ({owner}, 'runowner{owner}', 'runowner{owner}@example.invalid', '',
                CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO users (id, username, email, password_hash, bot_owner_id, created_at, updated_at)
        VALUES ({bot}, 'runbot{bot}', 'runbot{bot}@example.invalid', '', {owner},
                CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO repositories (id, owner_id, name, created_at, updated_at)
        VALUES ({repo}, {owner}, 'runrepo{repo}', CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO access_tokens (id, user_id, name, token_hash, scopes, created_at,
                                   repo_restricted, mcp_tools, deny_protected_merge)
        VALUES ({bot_token}, {bot}, 'agent', 'hash-{bot_token}', 'repo', CURRENT_TIMESTAMP,
                TRUE, 'create_pr,read_file', TRUE);
        INSERT INTO access_token_repositories (token_id, repository_id)
        VALUES ({bot_token}, {repo});
        INSERT INTO access_tokens (id, user_id, name, token_hash, scopes, created_at,
                                   repo_restricted, mcp_tools, deny_protected_merge)
        VALUES ({plain_token}, {owner}, 'laptop', 'hash-{plain_token}', 'repo,user',
                CURRENT_TIMESTAMP, FALSE, NULL, FALSE);
        INSERT INTO agent_profiles (id, bot_id, enabled, preset, image, command, runner_tags,
                                    model_key_encrypted, timeout_secs, max_runs_per_day,
                                    max_iterations, created_at, updated_at)
        VALUES ({profile}, {bot}, TRUE, 'custom', 'ghcr.io/acme/agent:1', 'run-agent', 'docker',
                'sealed', 1800, 20, 5, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);
        INSERT INTO agent_profile_repositories (profile_id, repository_id)
        VALUES ({profile}, {repo});
        "#,
        bot_token = BASE,
        plain_token = BASE + 1,
        profile = BASE,
    );
    for statement in script.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        db.execute_unprepared(statement)
            .await
            .unwrap_or_else(|error| panic!("seed `{statement}`: {error}"));
    }
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

async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
    scalar::<i64>(db, sql).await
}

/// Everything the upgrade must keep, and what the new schema must hold.
async fn exercise(db: &DatabaseConnection) {
    let (owner, bot, repo) = (BASE, BASE + 1, BASE);
    let (bot_token, plain_token) = (BASE, BASE + 1);

    // The narrowing an existing token carried is untouched, and the new
    // columns say "what it could do before".
    assert_eq!(
        count(
            db,
            &format!(
                "SELECT COUNT(*) FROM access_tokens WHERE id = {bot_token} \
                 AND repo_restricted = TRUE AND mcp_tools = 'create_pr,read_file' \
                 AND deny_protected_merge = TRUE AND write_ref_prefix IS NULL \
                 AND allow_git = FALSE AND agent_run_attempt_id IS NULL"
            )
        )
        .await,
        1,
        "the bot's narrowed token keeps its narrowing and gains the defaults"
    );
    assert_eq!(
        count(
            db,
            &format!(
                "SELECT COUNT(*) FROM access_tokens WHERE id = {plain_token} \
                 AND repo_restricted = FALSE AND mcp_tools IS NULL \
                 AND write_ref_prefix IS NULL AND allow_git = FALSE"
            )
        )
        .await,
        1,
        "a person's plain token stays plain"
    );
    assert_eq!(
        count(
            db,
            &format!("SELECT COUNT(*) FROM access_token_repositories WHERE token_id = {bot_token}")
        )
        .await,
        1
    );
    assert_eq!(
        count(
            db,
            &format!(
                "SELECT COUNT(*) FROM agent_profiles WHERE bot_id = {bot} \
                 AND model_key_encrypted = 'sealed' AND command = 'run-agent'"
            )
        )
        .await,
        1,
        "the agent profile is untouched"
    );
    assert_eq!(
        count(
            db,
            &format!("SELECT COUNT(*) FROM agent_profile_repositories WHERE profile_id = {BASE}")
        )
        .await,
        1
    );

    // A run, its attempt and the attempt's token.
    let run = |id: i64, key: &str, active: &str| {
        format!(
            "INSERT INTO agent_runs (id, bot_id, repository_id, iteration, triggered_by, \
             trigger_key, status, active, created_at) VALUES ({id}, {bot}, {repo}, 1, {owner}, \
             '{key}', 'running', {active}, CURRENT_TIMESTAMP)"
        )
    };
    db.execute_unprepared(&run(BASE, "assigned:1", "TRUE"))
        .await
        .expect("the first active run");
    assert!(
        db.execute_unprepared(&run(BASE + 1, "assigned:1", "NULL"))
            .await
            .is_err(),
        "a second delivery of one trigger inserted a second run"
    );
    assert!(
        db.execute_unprepared(&run(BASE + 1, "assigned:2", "TRUE"))
            .await
            .is_err(),
        "a second active run of one bot in one repository"
    );
    db.execute_unprepared(&run(BASE + 1, "assigned:2", "NULL"))
        .await
        .expect("a finished run beside the active one");
    db.execute_unprepared(&run(BASE + 2, "assigned:3", "NULL"))
        .await
        .expect("any number of finished runs");

    let attempt = |id: i64, number: i64| {
        format!(
            "INSERT INTO agent_run_attempts (id, run_id, attempt, created_at) \
             VALUES ({id}, {BASE}, {number}, CURRENT_TIMESTAMP)"
        )
    };
    db.execute_unprepared(&attempt(BASE, 1))
        .await
        .expect("the first attempt");
    assert!(
        db.execute_unprepared(&attempt(BASE + 1, 1)).await.is_err(),
        "two attempts with one number"
    );
    db.execute_unprepared(&attempt(BASE + 1, 2))
        .await
        .expect("a retry is a new row");

    db.execute_unprepared(&format!(
        "UPDATE access_tokens SET agent_run_attempt_id = {BASE}, allow_git = TRUE, \
         write_ref_prefix = 'refs/heads/agent/runbot/' WHERE id = {bot_token}"
    ))
    .await
    .expect("tie the token to the attempt");
    assert!(
        db.execute_unprepared(&format!(
            "UPDATE access_tokens SET agent_run_attempt_id = {BASE} WHERE id = {plain_token}"
        ))
        .await
        .is_err(),
        "one attempt held two tokens"
    );

    // Deleting the run takes its attempts and their token, and nothing else.
    db.execute_unprepared(&format!("DELETE FROM agent_runs WHERE id = {BASE}"))
        .await
        .expect("delete the run");
    assert_eq!(
        count(
            db,
            &format!("SELECT COUNT(*) FROM agent_run_attempts WHERE run_id = {BASE}")
        )
        .await,
        0
    );
    assert_eq!(
        count(
            db,
            &format!("SELECT COUNT(*) FROM access_tokens WHERE id = {bot_token}")
        )
        .await,
        0,
        "the run's token outlived the run"
    );
    assert_eq!(
        count(
            db,
            &format!("SELECT COUNT(*) FROM access_tokens WHERE id = {plain_token}")
        )
        .await,
        1
    );

    // The bot's runs outlive the bot: they record what it wrote.
    db.execute_unprepared(&format!("DELETE FROM agent_profiles WHERE bot_id = {bot}"))
        .await
        .expect("delete the profile");
    db.execute_unprepared(&format!("DELETE FROM users WHERE id = {bot}"))
        .await
        .expect("delete the bot");
    assert_eq!(
        count(
            db,
            &format!("SELECT COUNT(*) FROM agent_runs WHERE bot_id = {bot}")
        )
        .await,
        2
    );
    // The repository owns them.
    db.execute_unprepared(&format!("DELETE FROM repositories WHERE id = {repo}"))
        .await
        .expect("delete the repository");
    assert_eq!(
        count(
            db,
            &format!("SELECT COUNT(*) FROM agent_runs WHERE repository_id = {repo}")
        )
        .await,
        0
    );
}

#[tokio::test]
async fn the_schema_arrives_over_live_tokens_and_profiles() {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway SQLite database");
    migrate_to_just_before(&db).await;
    seed(&db).await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("apply the migration over live rows");
    exercise(&db).await;
}

/// The same upgrade on the server database `PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL`
/// names — not the URL the shared `multi_backend_smoke` tests migrate
/// concurrently. The CI jobs hand it over after the issue-comment migration
/// test, which leaves it at an older schema; this test brings it the rest of
/// the way to just before this migration.
#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL: a PostgreSQL or MySQL database without this migration"]
async fn the_schema_arrives_over_live_tokens_and_profiles_on_a_server_database() {
    let url = std::env::var("PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_EXCLUSIVE_DATABASE_URL must be set");
    assert!(!url.starts_with("sqlite"), "SQLite is covered above");
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to the exclusive test database");
    migrate_to_just_before(&db).await;
    seed(&db).await;
    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("apply the migration over live rows");
    exercise(&db).await;
}
