//! What the database does with the statements the hot paths send
//! (card_f2ca9c2005f2, card_6eba06ebef97).
//!
//! A missing index is invisible to every functional test: the answer is the
//! same, only slower, and only once a table has grown. These tests read the
//! plan instead. Each one explains the selection the server itself builds —
//! the ops expose it as a `pub(crate)` builder for that reason — against the
//! schema the migrations produce, so a reordered `ORDER BY`, a new filter or a
//! dropped index turns red here rather than in production.
//!
//! The foreign-key check covers what no listing names: deleting a parent row
//! looks up its children by the referencing column, and without an index that
//! is a full scan of the child table per deleted parent.

use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseBackend, DatabaseConnection, QuerySelect,
    QueryTrait, Statement,
};

use crate::entities::user;
use crate::ops::{
    issue_ops, notification_ops, pipeline_ops, pull_request_ops, release_ops, repo_ops,
    repo_star_ops, webhook_ops,
};

async fn migrated() -> DatabaseConnection {
    let mut options = ConnectOptions::new("sqlite::memory:");
    options.max_connections(1);
    let db = Database::connect(options)
        .await
        .expect("connect to in-memory db");
    crate::run_migrations(&db).await.expect("run migrations");
    db
}

/// The `detail` lines of `EXPLAIN QUERY PLAN` for `query`, in plan order.
async fn plan<Q: QueryTrait>(db: &DatabaseConnection, query: Q) -> Vec<String> {
    let statement = query.build(DatabaseBackend::Sqlite);
    let explain = Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        format!("EXPLAIN QUERY PLAN {}", statement.sql),
        statement.values.map(|values| values.0).unwrap_or_default(),
    );
    db.query_all(explain)
        .await
        .expect("explain query plan")
        .iter()
        .map(|row| row.try_get::<String>("", "detail").expect("plan detail"))
        .collect()
}

/// A page is read off an index in order: no step sorts, and none walks the
/// whole table.
fn assert_ordered_by_index(what: &str, plan: &[String], index: &str) {
    assert!(
        !plan.iter().any(|step| step.contains("TEMP B-TREE")),
        "{what}: the engine sorts every matching row to return one page — {plan:#?}"
    );
    assert!(
        !plan
            .iter()
            .any(|step| step.starts_with("SCAN ") && !step.contains(" INDEX ")),
        "{what}: the engine scans the whole table — {plan:#?}"
    );
    assert!(
        plan.iter().any(|step| step.contains(index)),
        "{what}: expected the plan to use `{index}` — {plan:#?}"
    );
}

#[tokio::test]
async fn paginated_listings_read_their_page_off_an_index() {
    let db = migrated().await;

    let cases = [
        (
            "issues, one state",
            plan(&db, issue_ops::page_query(1, Some("open")).limit(20)).await,
            "idx_issues_repo_state_created",
        ),
        (
            "issues, every state",
            plan(&db, issue_ops::page_query(1, None).limit(20)).await,
            "idx_issues_repo_created",
        ),
        (
            "pull requests, one state",
            plan(&db, pull_request_ops::page_query(1, Some("open")).limit(20)).await,
            "idx_pull_requests_repo_state_created",
        ),
        (
            "pull requests, every state",
            plan(&db, pull_request_ops::page_query(1, None).limit(20)).await,
            "idx_pull_requests_repo_created",
        ),
        (
            "pipelines",
            plan(&db, pipeline_ops::pipelines_page_query(1).limit(20)).await,
            "idx_pipelines_repo_created",
        ),
        (
            "notifications, all",
            plan(&db, notification_ops::page_query(1, false).limit(20)).await,
            "idx_notifications_user_created",
        ),
        (
            "notifications, unread",
            plan(&db, notification_ops::page_query(1, true).limit(20)).await,
            "idx_notifications_user_read_created",
        ),
        (
            "explore (anonymous)",
            plan(&db, repo_ops::public_page_query().limit(20)).await,
            "idx_repositories_public_updated",
        ),
        (
            "webhook deliveries",
            plan(&db, webhook_ops::deliveries_query(1)).await,
            "idx_webhook_deliveries_webhook_created",
        ),
        (
            "releases",
            plan(&db, release_ops::page_query(1).limit(20)).await,
            "idx_releases_repo_created",
        ),
        (
            "stargazers",
            plan(
                &db,
                repo_star_ops::stargazers_query(1)
                    .find_also_related(user::Entity)
                    .limit(20),
            )
            .await,
            "idx_repo_stars_repo_created",
        ),
    ];

    for (what, plan, index) in &cases {
        assert_ordered_by_index(what, plan, index);
    }
}

/// The runner poll runs every three seconds per runner. It must start from the
/// pending jobs, not from the repository's pipelines — those grow with every
/// push for as long as the repository exists.
#[tokio::test]
async fn runner_poll_starts_from_the_pending_jobs() {
    let db = migrated().await;
    let plan = plan(&db, pipeline_ops::pending_jobs_query(1)).await;

    let first = plan.first().expect("a plan has at least one step");
    assert!(
        first.contains("pipeline_jobs") && first.contains("idx_pipeline_jobs_status_runner"),
        "the poll does not start from the pending jobs — {plan:#?}"
    );
    assert!(
        !plan.iter().any(|step| step.starts_with("SCAN ")),
        "the poll scans a table — {plan:#?}"
    );
}

/// Every foreign-key column is the leading column of some index, so deleting
/// the parent — and any lookup by the parent — does not scan the child table.
#[tokio::test]
async fn every_foreign_key_column_leads_an_index() {
    let db = migrated().await;

    let tables: Vec<String> = db
        .query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' ORDER BY name",
        ))
        .await
        .expect("list tables")
        .iter()
        .map(|row| row.try_get::<String>("", "name").expect("table name"))
        .collect();

    let mut unindexed = Vec::new();
    for table in &tables {
        let leading: Vec<String> = db
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!(
                    "SELECT ii.name AS name FROM pragma_index_list('{table}') AS il \
                     JOIN pragma_index_info(il.name) AS ii ON ii.seqno = 0 \
                     UNION SELECT name FROM pragma_table_info('{table}') WHERE pk = 1"
                ),
            ))
            .await
            .expect("list leading index columns")
            .iter()
            .map(|row| row.try_get::<String>("", "name").expect("column name"))
            .collect();

        for row in db
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                format!("SELECT \"from\" AS col, \"table\" AS parent FROM pragma_foreign_key_list('{table}')"),
            ))
            .await
            .expect("list foreign keys")
        {
            let column: String = row.try_get("", "col").expect("fk column");
            let parent: String = row.try_get("", "parent").expect("fk parent");
            if !leading.contains(&column) {
                unindexed.push(format!("{table}.{column} -> {parent}"));
            }
        }
    }

    assert!(
        unindexed.is_empty(),
        "foreign-key columns that lead no index; deleting the parent scans the \
         child table once per row — {unindexed:#?}"
    );
}

/// Repository deletion inventories a repository's CI jobs and package files
/// through the ownership chain. Those walks used to carry each level's ids to
/// the next as bound `IN (?, ?, …)` parameters, so a repository with a long
/// history outgrew PostgreSQL's 65 535-parameter ceiling and could not be
/// deleted at all (card_f25c98fdf3ee). Each walk is now one statement that
/// binds the repository id and nothing else, however long the history is.
#[tokio::test]
async fn repository_inventory_walks_bind_one_value_however_long_the_history() {
    use std::sync::{Arc, Mutex};

    let mut db = migrated().await;
    // A real chain, so a walk that carried ids from level to level would have
    // something to carry: two pipelines, a stage each, two jobs each.
    let now = chrono::Utc::now();
    let owner = crate::ops::user_ops::create_user(&db, "walker", "walker@example.test", "", "")
        .await
        .expect("create owner");
    let repo = repo_ops::create(
        &db,
        crate::entities::repository::ActiveModel {
            owner_id: sea_orm::Set(owner.id),
            name: sea_orm::Set("history".to_string()),
            is_private: sea_orm::Set(false),
            default_branch: sea_orm::Set("main".to_string()),
            stars_count: sea_orm::Set(0),
            forks_count: sea_orm::Set(0),
            created_at: sea_orm::Set(now),
            updated_at: sea_orm::Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("create repository");
    let mut expected_jobs = Vec::new();
    for run in 0..2 {
        let pipeline = pipeline_ops::create_pipeline(
            &db,
            repo.id,
            &format!("{run:040}"),
            "main",
            "push",
            None,
        )
        .await
        .expect("create pipeline");
        let stage = pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
            .await
            .expect("create stage");
        for job in 0..2 {
            expected_jobs.push(
                pipeline_ops::create_job(
                    &db,
                    stage.id,
                    &format!("job-{job}"),
                    "true",
                    None,
                    None,
                    None,
                    None,
                    None,
                    None,
                    false,
                    None,
                    None,
                    None,
                )
                .await
                .expect("create job")
                .id,
            );
        }
    }

    // And one package file down the registry → package → version chain.
    use sea_orm::{ActiveModelTrait, Set};
    let registry = crate::entities::package_registry::ActiveModel {
        repo_id: Set(repo.id),
        package_type: Set("npm".to_string()),
        enabled: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create registry");
    let package = crate::entities::package::ActiveModel {
        package_registry_id: Set(registry.id),
        owner_id: Set(owner.id),
        name: Set("walked".to_string()),
        is_public: Set(true),
        download_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create package");
    let version = crate::entities::package_version::ActiveModel {
        package_id: Set(package.id),
        version: Set("1.0.0".to_string()),
        protocol_variant_key: Set(String::new()),
        size: Set(1),
        is_yanked: Set(false),
        download_count: Set(0),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create version");
    crate::entities::package_file::ActiveModel {
        version_id: Set(version.id),
        filename: Set("walked-1.0.0.tgz".to_string()),
        size: Set(1),
        storage_path: Set("packages/walker/history/walked-1.0.0.tgz".to_string()),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create package file");

    let seen: Arc<Mutex<Vec<usize>>> = Arc::default();
    let sink = Arc::clone(&seen);
    db.set_metric_callback(move |info| {
        let values = info
            .statement
            .values
            .as_ref()
            .map_or(0, |values| values.0.len());
        sink.lock().unwrap().push(values);
    });

    let mut jobs = pipeline_ops::list_job_ids_by_repo(&db, repo.id)
        .await
        .expect("list job ids");
    jobs.sort_unstable();
    assert_eq!(jobs, expected_jobs, "the walk must still find every job");
    assert_eq!(
        std::mem::take(&mut *seen.lock().unwrap()),
        vec![1],
        "the CI inventory must be one statement binding only the repository id"
    );

    let paths = crate::ops::package_file_ops::list_storage_paths_by_repo(&db, repo.id)
        .await
        .expect("list package storage paths");
    assert_eq!(
        paths,
        vec!["packages/walker/history/walked-1.0.0.tgz".to_string()]
    );
    assert_eq!(
        std::mem::take(&mut *seen.lock().unwrap()),
        vec![1],
        "the package inventory must be one statement binding only the repository id"
    );
}
