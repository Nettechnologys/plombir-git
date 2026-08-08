//! Acceptance for card_bee1b7f88d0e: the AI code-search route must not erase
//! the type of a database failure on its way out.
//!
//! `GET /api/v1/ai/repos/{owner}/{name}/search/code` wrapped the result of
//! `CodeIndexer::search_code` in `AppError::internal(format!("Search error: {e}"))`.
//! That call is three database round-trips behind an `anyhow::Error`, and
//! formatting it into a string destroyed the `DbErr` the shared classifier
//! downcasts to — so a connection-level outage answered a flat 500 ("we broke,
//! permanently") where every other database boundary in the server answers the
//! retryable 503.
//!
//! The three tests below split the claim into the parts each one can actually
//! prove, because no single request can prove all of it:
//!
//! * the live route serves its index and stays 503 under an outage — end to end,
//!   through the real handler with a real `code_fts` index behind it;
//! * the error `search_code` produces on a closed pool classifies as a retryable
//!   503 when it is handed to `AppError::from` — the conversion the handler now
//!   performs, asserted on the exact error value the handler receives;
//! * no handler in `api/ai.rs` stringifies an error into `AppError::internal`
//!   again — the census guard, and the only one of the three whose failing half
//!   was red before the fix (both wrappers this file used to hold trip it).
//!
//! The middle two exist because the handler makes an equivalent database call
//! (`SELECT COUNT(*) … FROM code_fts`, the index-status probe) twenty lines
//! *ahead* of the search, and that probe was already routed through
//! `AppError::from`. A closed pool therefore short-circuits into the probe, so
//! the end-to-end 503 assertion is a route guard rather than a regression test
//! for the search boundary itself. The narrower fault that would break the
//! search alone does not exist here: both statements read the same virtual
//! table over the same pool, and SQLite has no read triggers.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use sea_orm::{ConnectionTrait, Statement, Value};

use crate::common::source_scan;
use crate::common::{build_test_app_state, setup_test_db};

const OWNER_ID: i64 = 900_401;
const REPO_ID: i64 = 900_402;
const OWNER: &str = "ai-search-owner";
const REPO: &str = "ai-search-repo";
/// A token that appears in the indexed file and nowhere else, so a hit proves
/// the FTS query ran rather than that some default came back.
const NEEDLE: &str = "zqxjvfindme";
const INDEXED_PATH: &str = "src/lib.rs";

/// Seed the owner and the repository the `RepoRead` gate resolves to.
async fn seed_repo(db: &rg_db::DatabaseConnection) -> rg_db::entities::repository::Model {
    use sea_orm::ActiveModelTrait;
    use sea_orm::ActiveValue::Set;

    let now = chrono::Utc::now();
    rg_db::entities::user::ActiveModel {
        id: Set(OWNER_ID),
        username: Set(OWNER.to_string()),
        email: Set(format!("{OWNER}@example.test")),
        password_hash: Set(String::new()),
        is_admin: Set(false),
        is_active: Set(true),
        auth_provider: Set("local".to_string()),
        mfa_enabled: Set(false),
        login_attempts: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert AI search fixture owner");

    rg_db::entities::repository::ActiveModel {
        id: Set(REPO_ID),
        owner_id: Set(OWNER_ID),
        name: Set(REPO.to_string()),
        is_private: Set(false),
        default_branch: Set("main".to_string()),
        stars_count: Set(0),
        forks_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("insert AI search fixture repository")
}

/// Write one row into `code_fts` exactly the way `CodeIndexer::batch_insert_fts`
/// does, so the handler's index-status probe and its search see a real index
/// without this test needing a git repository to walk.
async fn index_one_file(db: &rg_db::DatabaseConnection) {
    let backend = db.get_database_backend();
    let sql = rg_db::prepare_sql(
        backend,
        "INSERT INTO code_fts(repo_id, file_path, file_name, content, language) \
         VALUES (?, ?, ?, ?, ?)",
    );
    db.execute(Statement::from_sql_and_values(
        backend,
        &sql,
        [
            Value::from(REPO_ID),
            Value::from(INDEXED_PATH.to_string()),
            Value::from("lib.rs".to_string()),
            Value::from(format!("pub fn {NEEDLE}() {{}}\n")),
            Value::from("Rust".to_string()),
        ],
    ))
    .await
    .expect("seed the code_fts index");
}

fn search_query(q: &str) -> Query<rg_http::api::ai::SearchCodeQuery> {
    Query(
        serde_json::from_value::<rg_http::api::ai::SearchCodeQuery>(serde_json::json!({ "q": q }))
            .expect("build code-search query"),
    )
}

/// Invoke the production handler behind an already-resolved `RepoRead`.
///
/// The gate is deliberately pre-satisfied: driving this through the router with
/// a closed pool would let the session middleware or `RepoRead` answer 503 on
/// their own lookups, and the assertion would hold with the handler untouched.
async fn search_response(
    state: rg_http::AppState,
    repo: rg_db::entities::repository::Model,
    q: &str,
) -> axum::response::Response {
    rg_http::api::ai::ai_search_code(
        State(state),
        rg_http::api::repo_access::RepoRead { repo },
        Path((OWNER.to_string(), REPO.to_string())),
        search_query(q),
    )
    .await
    .into_response()
}

/// The live route: a prepared index answers with the indexed file, and the same
/// call under a database outage answers the retryable 503 rather than a 500.
#[tokio::test]
async fn ai_code_search_serves_its_index_and_reports_an_outage_as_503() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let repo = seed_repo(&db).await;
    index_one_file(&db).await;
    let state = build_test_app_state(db.clone(), repo_root);

    let healthy = search_response(state.clone(), repo.clone(), NEEDLE).await;
    assert_eq!(
        healthy.status(),
        StatusCode::OK,
        "healthy baseline: a prepared index must be searchable"
    );
    let body = axum::body::to_bytes(healthy.into_body(), usize::MAX)
        .await
        .expect("read healthy code-search body");
    let body: serde_json::Value =
        serde_json::from_slice(&body).expect("healthy code-search response is JSON");
    assert_eq!(
        body[0]["file_path"], INDEXED_PATH,
        "healthy baseline must return the indexed file, got: {body}"
    );

    db.close().await.expect("close AI search fixture pool");

    let outage = search_response(state, repo, NEEDLE).await;
    assert_eq!(
        outage.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a code search that could not reach the database must be retryable, not a flat 500"
    );
}

/// The boundary the handler now converts through, asserted on the exact error
/// value it receives: `CodeIndexer::search_code` on a closed pool must classify
/// as a retryable 503 once handed to `AppError::from`.
///
/// The pre-fix `AppError::internal(format!("Search error: {e}"))` turned this
/// same error into a 500 — that is the difference this test pins down, on the
/// one call the end-to-end test above cannot reach in isolation.
#[tokio::test]
async fn a_failed_code_search_classifies_as_a_retryable_outage() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    seed_repo(&db).await;
    index_one_file(&db).await;

    let indexer = rg_core::search::code_indexer::CodeIndexer::new(db.clone());
    let (results, total) = indexer
        .search_code(NEEDLE, Some(REPO_ID), 20, 0)
        .await
        .expect("healthy baseline: the seeded index must answer");
    assert_eq!(total, 1);
    assert_eq!(results[0].file_path, INDEXED_PATH);

    db.close().await.expect("close code-search fixture pool");

    let error = indexer
        .search_code(NEEDLE, Some(REPO_ID), 20, 0)
        .await
        .expect_err("a closed pool must fail the search");
    assert_eq!(
        rg_http::error::AppError::from(error).status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "the search error must survive as a typed database outage"
    );
}

/// The census over `api/ai.rs`: every handler that can fail has to reach the
/// shared classifier, not rebuild a status from a formatted message.
///
/// This is the guard that was red before the fix, and the one that stays useful
/// afterwards — the next handler added to this file cannot quietly reintroduce
/// the same wrapper. `ai_index_repository` is held to it for the same reason and
/// now actually serves traffic: its route was mounted with card_928d72df493a.
#[test]
fn ai_handlers_reach_the_shared_error_classifier() {
    let source = include_str!("../../src/api/ai.rs");

    let functions = source_scan::functions(source);

    for handler in ["ai_search_code", "ai_index_repository"] {
        let body = functions
            .iter()
            .find(|f| f.name == handler)
            .unwrap_or_else(|| panic!("{handler} is not declared in api/ai.rs at all"));
        // `contains`, not `source_scan::calls`: the conversion is written both
        // as a call (`AppError::from(e)`) and as a function reference handed to
        // `map_err`, and only the first form has a `(` after the name.
        assert!(
            body.body.contains("AppError::from"),
            "{handler} can fail without reaching the shared error classifier"
        );
    }

    for function in functions {
        assert!(
            !function.body.contains("AppError::internal(format!")
                && !function.body.contains("AppError::internal(e.to_string()"),
            "{} rebuilds an error status from a formatted message instead of \
             classifying it — a database outage there answers 500, not 503",
            function.name
        );
    }
}
