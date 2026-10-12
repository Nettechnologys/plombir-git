//! Columns widened for values the API accepts keep the rows they already hold
//! when the widening runs, and hold the widened values afterwards.
//!
//! - `webhooks.url` and `webhooks.events` were `VARCHAR(255)` on MySQL, shorter
//!   than the full event canon and than an ordinary long HTTPS URL
//!   (card_c16a9cc05033, card_535dbac9dcec).
//! - `sso_providers.client_secret_enc` and `ldap_bind_password_enc` were
//!   `VARCHAR(1024)` on MySQL and PostgreSQL, shorter than the sealed form of a
//!   secret the admin API accepted (card_b363c2b4d07d).
//!
//! The widening is rerun over a live row, the same `ALTER` an upgrade applies
//! to an instance that already stores hooks and providers. This file honours
//! `PLOMBIR_GIT_TEST_DATABASE_URL`; without it the rows go to a throwaway
//! SQLite, where the migrations are no-ops and only the round trip is proved.

use rg_db::entities::{sso_provider, webhook};
use rg_db::sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, EntityTrait, NotSet,
    Set, Statement,
};
use sea_orm_migration::{MigrationTrait, SchemaManager};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-column-widening-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
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

async fn setup() -> (DatabaseConnection, Option<TempDb>) {
    let (url, temp) = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => (url, None),
        _ => {
            let temp = TempDb::new();
            (
                format!("sqlite://{}?mode=rwc", temp.path.display()),
                Some(temp),
            )
        }
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// The declared type of `table.column` and its length ceiling, on the two
/// backends that enforce one. PostgreSQL's unsized `character varying` is as
/// unbounded as `text`; MySQL reports `TEXT` with its fixed 65 535 bytes.
async fn column_type(
    db: &DatabaseConnection,
    table: &str,
    column: &str,
) -> Option<(String, Option<i64>)> {
    let sql = match db.get_database_backend() {
        DatabaseBackend::MySql => {
            "SELECT DATA_TYPE, CAST(CHARACTER_MAXIMUM_LENGTH AS SIGNED) FROM information_schema.COLUMNS \
             WHERE TABLE_SCHEMA = DATABASE() AND TABLE_NAME = ? AND COLUMN_NAME = ?"
        }
        DatabaseBackend::Postgres => {
            "SELECT data_type::text, character_maximum_length::bigint FROM information_schema.columns \
             WHERE table_schema = current_schema() AND table_name = $1 AND column_name = $2"
        }
        DatabaseBackend::Sqlite => return None,
    };
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            sql,
            [table.into(), column.into()],
        ))
        .await
        .expect("read the column type")
        .expect("the column exists");
    Some((
        row.try_get_by_index::<String>(0)
            .expect("decode the column type")
            .to_ascii_lowercase(),
        row.try_get_by_index::<Option<i64>>(1)
            .expect("decode the column length"),
    ))
}

#[tokio::test]
async fn widened_columns_keep_live_rows_and_hold_accepted_values() {
    let (db, _temp) = setup().await;
    let manager = SchemaManager::new(&db);
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let now = chrono::Utc::now();

    // First a row that fits the old widths — what an instance being upgraded
    // already stores — then the widening, then the values the API now accepts.
    let old_url = format!("https://hooks.example.invalid/{suffix}/{}", "o".repeat(150));
    let old_events = "push,release.created,pull_request.opened".to_string();
    let old_sealed = "e".repeat(1_000);
    let url = format!(
        "https://hooks.example.invalid/{suffix}/{}",
        "p".repeat(8_000)
    );
    let events = "pull_request.review_requested,".repeat(12) + "push";
    let sealed = "s".repeat(6_000);

    let hook = webhook::ActiveModel {
        id: NotSet,
        repo_id: Set(0),
        scope_kind: Set("system".to_string()),
        scope_id: Set(0),
        url: Set(old_url.clone()),
        content_type: Set("json".to_string()),
        secret_encrypted: Set(None),
        active: Set(false),
        events: Set(old_events.clone()),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&db)
    .await
    .expect("store a webhook that fits the old columns");
    let provider = sso_provider::ActiveModel {
        id: NotSet,
        name: Set("Widening".to_string()),
        slug: Set(format!("widen-{}", &suffix[..10])),
        provider_type: Set("oauth2".to_string()),
        client_secret_enc: Set(Some(old_sealed.clone())),
        ldap_bind_password_enc: Set(Some(old_sealed.clone())),
        enabled: Set(false),
        auto_provision: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("store an SSO provider that fits the old columns");

    rg_db::migrations::m20261012_000003_webhook_url_events_mysql_text::Migration
        .up(&manager)
        .await
        .expect("rerun the webhook column widening over a live row");
    rg_db::migrations::m20261012_000004_sso_secret_columns_text::Migration
        .up(&manager)
        .await
        .expect("rerun the SSO secret column widening over a live row");

    let kept_hook = webhook::Entity::find_by_id(hook.id)
        .one(&db)
        .await
        .unwrap()
        .expect("the hook survived the widening");
    assert_eq!((kept_hook.url, kept_hook.events), (old_url, old_events));
    let kept_provider = sso_provider::Entity::find_by_id(provider.id)
        .one(&db)
        .await
        .unwrap()
        .expect("the provider survived the widening");
    assert_eq!(
        kept_provider.client_secret_enc.as_deref(),
        Some(old_sealed.as_str())
    );
    assert_eq!(
        kept_provider.ldap_bind_password_enc.as_deref(),
        Some(old_sealed.as_str())
    );

    let mut widened_hook: webhook::ActiveModel = hook.into();
    widened_hook.url = Set(url.clone());
    widened_hook.events = Set(events.clone());
    widened_hook
        .update(&db)
        .await
        .expect("store the widest accepted webhook values");
    let mut widened_provider: sso_provider::ActiveModel = provider.into();
    widened_provider.client_secret_enc = Set(Some(sealed.clone()));
    widened_provider.ldap_bind_password_enc = Set(Some(sealed.clone()));
    let provider = widened_provider
        .update(&db)
        .await
        .expect("store sealed secrets past VARCHAR(1024)");
    let hook_id = kept_hook.id;

    let stored_hook = webhook::Entity::find_by_id(hook_id)
        .one(&db)
        .await
        .unwrap()
        .expect("the hook exists");
    assert_eq!(stored_hook.url, url);
    assert_eq!(stored_hook.events, events);
    let stored_provider = sso_provider::Entity::find_by_id(provider.id)
        .one(&db)
        .await
        .unwrap()
        .expect("the provider exists");
    assert_eq!(
        stored_provider.client_secret_enc.as_deref(),
        Some(sealed.as_str())
    );
    assert_eq!(
        stored_provider.ldap_bind_password_enc.as_deref(),
        Some(sealed.as_str())
    );

    for (table, column) in [
        ("webhooks", "url"),
        ("webhooks", "events"),
        ("sso_providers", "client_secret_enc"),
        ("sso_providers", "ldap_bind_password_enc"),
    ] {
        if let Some((declared, length)) = column_type(&db, table, column).await {
            let unbounded =
                declared == "text" || (declared == "character varying" && length.is_none());
            assert!(
                unbounded,
                "{table}.{column} is still {declared}({length:?})"
            );
        }
    }

    webhook::Entity::delete_by_id(hook_id)
        .exec(&db)
        .await
        .unwrap();
    sso_provider::Entity::delete_by_id(provider.id)
        .exec(&db)
        .await
        .unwrap();
}
