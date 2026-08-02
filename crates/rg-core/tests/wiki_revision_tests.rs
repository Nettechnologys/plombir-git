//! Wiki revision numbering (card_e087c0f31f2e).
//!
//! `update_page` snapshots the pre-edit content under the next free version
//! number. That number came from `latest_version(...).unwrap_or(0)`, and since
//! "no revisions yet" is already `Ok(0)`, the fallback caught exactly one
//! thing: the query failing. The snapshot was then filed as version 1 on top of
//! the version 1 already there — and `wiki_revisions` has no unique key on
//! (wiki_page_id, version) to stop it, so the history list and "restore this
//! version" pick between the duplicates arbitrarily.
//!
//! These two pin both halves: the numbers a healthy database hands out, and
//! what happens when the revision table cannot be read at all.

use sea_orm::ConnectionTrait;

async fn fresh_db(dir: &std::path::Path) -> sea_orm::DatabaseConnection {
    let db_path = dir.join("test.db");
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", db_path.display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        2,
    )
    .await
    .expect("connect sqlite");
    rg_db::run_migrations(&db).await.expect("run migrations");
    db
}

/// A repository with one wiki page, ready to be edited.
async fn wiki_fixture(
    dir: &std::path::Path,
    name: &str,
) -> (
    sea_orm::DatabaseConnection,
    rg_db::entities::repository::Model,
) {
    let db = fresh_db(dir).await;
    let owner =
        rg_db::ops::user_ops::create_user(&db, name, &format!("{name}@example.invalid"), "", name)
            .await
            .unwrap_or_else(|error| panic!("create user {name}: {error:#}"));

    let repo = rg_core::repo::service::create_repo(
        &db,
        owner.id,
        "wikirepo",
        None,
        false,
        &dir.join("repos"),
        None,
    )
    .await
    .expect("create repo");

    rg_core::wiki::service::create_page(&db, repo.id, "Home", "first", None, Some(owner.id))
        .await
        .expect("create wiki page");

    (db, repo)
}

#[tokio::test]
async fn every_edit_files_its_snapshot_under_the_next_free_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (db, repo) = wiki_fixture(dir.path(), "wikiversions").await;

    for content in ["second", "third", "fourth"] {
        rg_core::wiki::service::update_page(&db, repo.id, "Home", content, None, None)
            .await
            .unwrap_or_else(|error| panic!("update to {content}: {error:#}"));
    }

    let revisions = rg_core::wiki::service::list_revisions(&db, repo.id, "Home")
        .await
        .expect("list revisions");

    // Newest first, one per edit, and each snapshot carries the content that
    // was on the page before that edit.
    assert_eq!(
        revisions
            .iter()
            .map(|revision| (revision.version, revision.content.as_str()))
            .collect::<Vec<_>>(),
        vec![(3, "third"), (2, "second"), (1, "first")],
    );
}

#[tokio::test]
async fn an_unreadable_revision_table_fails_the_edit_instead_of_reusing_a_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (db, repo) = wiki_fixture(dir.path(), "wikibrokenhistory").await;

    rg_core::wiki::service::update_page(&db, repo.id, "Home", "second", None, None)
        .await
        .expect("first edit");

    // The lookup the next edit needs can no longer be answered. It used to read
    // as "no history", which handed out version 1 a second time.
    db.execute_unprepared("DROP TABLE wiki_revisions")
        .await
        .expect("take the revision table away");

    let error = rg_core::wiki::service::update_page(&db, repo.id, "Home", "third", None, None)
        .await
        .expect_err("an unanswerable revision lookup is a failed edit, not version 1 again");
    let chain = format!("{error:#}");
    assert!(
        chain.contains("find latest wiki revision version"),
        "the failure should name the lookup that could not be answered, got: {chain}"
    );

    // And the page still holds what it held before the refused edit.
    let page = rg_core::wiki::service::get_page(&db, repo.id, "Home")
        .await
        .expect("read wiki page")
        .expect("wiki page still exists");
    assert_eq!(page.content, "second");
}
