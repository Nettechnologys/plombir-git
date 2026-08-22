//! Wiki service — CRUD operations for repository wiki pages.
//!
//! Each repository can have an associated wiki. Wiki pages are stored in the
//! database for fast querying and optionally mirrored to a `.wiki.git` bare
//! repository on disk for version control.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{DatabaseConnection, TransactionTrait};

use crate::db_retry::{classify, classify_anyhow};
use rg_db::entities::wiki_page;
use rg_db::entities::wiki_revision;
use rg_db::ops::wiki_page_ops;
use rg_db::ops::wiki_revision_ops;

/// Create a new wiki page.
pub async fn create_page(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
) -> Result<wiki_page::Model> {
    create_page_with_post_commit(db, repo_id, title, content, message, author_id, || {
        std::future::ready(())
    })
    .await
}

/// Create the source row, then expose the historical post-commit window to a
/// deterministic regression test. Production has no work in that window: the
/// source-table trigger is the sole writer of `wiki_pages_fts`.
async fn create_page_with_post_commit<F, Fut>(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
    after_source_commit: F,
) -> Result<wiki_page::Model>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // The one answer both the pre-read and a losing insert give, so a caller
    // cannot tell which of the two noticed — and so no constraint text leaks.
    // `Conflict`, not `InvalidRequest`: an existing page refuses the title, and
    // the caller either picks another one or edits the page that holds it.
    let already_exists = || {
        crate::error::conflict(format!(
            "wiki page '{title}' already exists in this repository"
        ))
    };

    // Check for duplicate title. This read is the fast path only — the row can
    // still appear between here and the insert below, which is why the insert
    // classifies its own failure rather than trusting this answer.
    if wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("check existing wiki page")?
        .is_some()
    {
        return Err(already_exists());
    }

    let now = Utc::now();
    let model = wiki_page::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: sea_orm::Set(repo_id),
        title: sea_orm::Set(title.to_string()),
        content: sea_orm::Set(content.to_string()),
        message: sea_orm::Set(message.map(|s| s.to_string())),
        author_id: sea_orm::Set(author_id),
        sha: sea_orm::Set(None),
        edit_version: sea_orm::Set(0),
        created_at: sea_orm::Set(now),
        updated_at: sea_orm::Set(now),
    };

    // Losing the `idx_wiki_repo_title` race is the same outcome the read above
    // reports, reached a moment later: someone else created the page first.
    // Only that one loss is folded — any other write failure stays an error.
    let page = match wiki_page_ops::create(db, model).await {
        Ok(page) => page,
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => return Err(already_exists()),
        Err(error) => return Err(error),
    };

    after_source_commit().await;

    Ok(page)
}

/// Get a wiki page by repo and title.
pub async fn get_page(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
) -> Result<Option<wiki_page::Model>> {
    wiki_page_ops::find_by_repo_and_title(db, repo_id, title).await
}

/// List all wiki pages for a repo (title + updated_at only for index).
pub async fn list_pages(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<wiki_page::Model>> {
    wiki_page_ops::list_by_repo(db, repo_id).await
}

/// Update a wiki page.
pub async fn update_page(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
) -> Result<wiki_page::Model> {
    update_page_with_post_commit(db, repo_id, title, content, message, author_id, || {
        std::future::ready(())
    })
    .await
}

/// Commit the source-row update before exposing the old explicit-index-writer
/// window to a test callback. Nothing may write metadata FTS after this point:
/// doing so could replay this call's snapshot over a newer trigger result.
async fn update_page_with_post_commit<F, Fut>(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
    after_source_commit: F,
) -> Result<wiki_page::Model>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let updated =
        update_page_transactionally(db, repo_id, title, content, message, author_id, |_| {
            std::future::ready(())
        })
        .await?;

    after_source_commit().await;

    Ok(updated)
}

/// Serialize `read page -> snapshot -> overwrite` through the page's monotonic
/// edit token. The callback is a private test seam: production supplies a
/// ready future, while the regression test can stop both first attempts after
/// they have read the same token without depending on scheduler timing.
///
/// # Why the budget is a deadline and not an attempt count
///
/// This transaction reads before it writes, which on SQLite means every refusal
/// arrives *instantly* — thirty-two attempts plus their jittered waits came to
/// about a third of a second whatever was holding the lock. The holder is
/// frequently the code-index refresh, which keeps SQLite's database-wide writer
/// lock for the whole publication of a repository snapshot, so an edit landing
/// during a push into a large repository lost by construction rather than
/// occasionally (card_d5612b049af6).
///
/// [`rg_db::contention::ContentionBudget::for_request_write`] is the deadline
/// the other request-bound loops use, measured against how long a holder can
/// plausibly keep the lock. The waits stay
/// `rg_db::contention::contention_backoff`.
async fn update_page_transactionally<F, Fut>(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
    after_read: F,
) -> Result<wiki_page::Model>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let mut budget = rg_db::contention::ContentionBudget::for_request_write();

    loop {
        let attempt = budget.begin_attempt();

        let transaction = match db.begin().await {
            Ok(transaction) => transaction,
            Err(error) if budget.may_retry() && classify(&error).is_worthwhile() => {
                classify(&error).wait(attempt).await;
                continue;
            }
            Err(error) => return Err(error).context("begin wiki page update transaction"),
        };

        let write_result: Result<Option<wiki_page::Model>> = async {
            let existing = wiki_page_ops::find_by_repo_and_title(&transaction, repo_id, title)
                .await
                .context("find wiki page for update")?
                .ok_or_else(|| crate::error::not_found("wiki page"))?;
            after_read(attempt).await;

            let next_version = existing
                .edit_version
                .checked_add(1)
                .context("wiki edit version exhausted")?;
            let revision = wiki_revision::ActiveModel {
                id: sea_orm::NotSet,
                wiki_page_id: sea_orm::Set(existing.id),
                content: sea_orm::Set(existing.content.clone()),
                message: sea_orm::Set(existing.message.clone()),
                author_id: sea_orm::Set(existing.author_id),
                version: sea_orm::Set(next_version),
                created_at: sea_orm::Set(Utc::now()),
            };
            wiki_revision_ops::create(&transaction, revision)
                .await
                .context("snapshot wiki page before update")?;

            wiki_page_ops::update_if_version(
                &transaction,
                &existing,
                content,
                message,
                author_id.or(existing.author_id),
                next_version,
                Utc::now(),
            )
            .await
        }
        .await;

        let updated = match write_result {
            Ok(Some(updated)) => updated,
            Ok(None) => {
                transaction
                    .rollback()
                    .await
                    .context("rollback stale wiki page update")?;
                if !budget.may_retry() {
                    anyhow::bail!("{}", budget.exhausted("serialize a wiki page update"));
                }
                continue;
            }
            Err(error) => {
                let retry = classify_anyhow(&error);
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!(
                        "wiki page update failed and its transaction could not be rolled back: \
                         {rollback_error}"
                    ));
                }
                if retry.is_worthwhile() && budget.may_retry() {
                    retry.wait(attempt).await;
                    continue;
                }
                if retry.is_worthwhile() {
                    return Err(error).context(budget.exhausted("serialize a wiki page update"));
                }
                return Err(error);
            }
        };

        match transaction.commit().await {
            Ok(()) => return Ok(updated),
            Err(error) if budget.may_retry() && classify(&error).is_worthwhile() => {
                classify(&error).wait(attempt).await;
                continue;
            }
            Err(error) if rg_db::is_retryable_transaction_error(&error) => {
                return Err(error).context(budget.exhausted("commit a wiki page update"));
            }
            Err(error) => return Err(error).context("commit wiki page update transaction"),
        }
    }
}

/// List all revisions for a wiki page (newest first).
pub async fn list_revisions(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
) -> Result<Vec<wiki_revision::Model>> {
    let page = wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("find wiki page for revisions")?
        .ok_or_else(|| crate::error::not_found("wiki page"))?;
    wiki_revision_ops::list_by_page(db, page.id).await
}

/// Get a specific revision's content, scoped to the page that owns it.
///
/// `revision_id` is global, so the repository and title from the route decide
/// which revisions are addressable at all. Taking them here rather than trusting
/// the caller to compare afterwards is what keeps one repository's history out
/// of a URL that points at another one; a revision of a different page is
/// `Ok(None)`, indistinguishable from a revision that does not exist.
pub async fn get_revision(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    revision_id: i64,
) -> Result<Option<wiki_revision::Model>> {
    let page = wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("find wiki page for revision")?
        .ok_or_else(|| crate::error::not_found("wiki page"))?;

    let Some(revision) = wiki_revision_ops::find_by_id(db, revision_id)
        .await
        .context("find wiki revision")?
    else {
        return Ok(None);
    };

    if revision.wiki_page_id != page.id {
        return Ok(None);
    }

    Ok(Some(revision))
}

/// Delete a wiki page.
pub async fn delete_page(db: &DatabaseConnection, repo_id: i64, title: &str) -> Result<()> {
    let existing = wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("find wiki page for delete")?
        .ok_or_else(|| crate::error::not_found("wiki page"))?;

    let page_id = existing.id;

    // The lookup above and this `DELETE` are two statements, so a concurrent
    // delete can empty the row out from under it; zero rows reports `not_found`
    // rather than confirming a deletion this call did not perform.
    if wiki_page_ops::delete_by_id(db, page_id).await? {
        Ok(())
    } else {
        Err(crate::error::not_found("wiki page"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};

    async fn wiki_repo_fixture(name: &str) -> (tempfile::TempDir, DatabaseConnection, i64, i64) {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join(format!("{name}.db"));
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", db_path.display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            2,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let user = rg_db::ops::user_ops::create_user(
            &db,
            name,
            &format!("{name}@example.invalid"),
            "",
            name,
        )
        .await
        .expect("create user");
        let repo = crate::repo::service::create_repo(
            &db,
            user.id,
            name,
            None,
            false,
            &dir.path().join("repos"),
            None,
        )
        .await
        .expect("create repo");

        (dir, db, user.id, repo.id)
    }

    async fn wiki_fts_content(db: &DatabaseConnection, page_id: i64) -> Option<String> {
        db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            "SELECT content FROM wiki_pages_fts WHERE rowid = ?",
            [page_id.into()],
        ))
        .await
        .expect("read wiki FTS row")
        .map(|row| {
            row.try_get::<String>("", "content")
                .expect("decode wiki FTS content")
        })
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn same_snapshot_edits_form_a_complete_history_chain() {
        let (_dir, db, user_id, repo_id) = wiki_repo_fixture("wikicas").await;
        create_page(&db, repo_id, "Home", "v0", None, Some(user_id))
            .await
            .expect("create page");

        let after_same_read = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let first_gate = after_same_read.clone();
        let first = update_page_transactionally(
            &db,
            repo_id,
            "Home",
            "A",
            None,
            Some(user_id),
            move |attempt| {
                let gate = first_gate.clone();
                async move {
                    if attempt == 1 {
                        gate.wait().await;
                    }
                }
            },
        );
        let second_gate = after_same_read.clone();
        let second = update_page_transactionally(
            &db,
            repo_id,
            "Home",
            "B",
            None,
            Some(user_id),
            move |attempt| {
                let gate = second_gate.clone();
                async move {
                    if attempt == 1 {
                        gate.wait().await;
                    }
                }
            },
        );

        let (first, second) = tokio::join!(first, second);
        first.expect("first same-snapshot edit succeeds");
        second.expect("second same-snapshot edit succeeds");

        let current = get_page(&db, repo_id, "Home")
            .await
            .expect("read current page")
            .expect("page exists");
        let revisions = list_revisions(&db, repo_id, "Home")
            .await
            .expect("read revisions");
        assert_eq!(
            revisions
                .iter()
                .map(|revision| revision.version)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(current.edit_version, 2);

        let mut preserved_states = revisions
            .iter()
            .map(|revision| revision.content.as_str())
            .chain(std::iter::once(current.content.as_str()))
            .collect::<Vec<_>>();
        preserved_states.sort_unstable();
        assert_eq!(preserved_states, vec!["A", "B", "v0"]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn late_update_completion_cannot_overwrite_newer_fts_content() {
        let (_dir, db, user_id, repo_id) = wiki_repo_fixture("wikiupdaterace").await;
        let page = create_page(
            &db,
            repo_id,
            "Home",
            "initial wiki payload",
            None,
            Some(user_id),
        )
        .await
        .expect("create page");

        let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first_db = db.clone();
        let first = tokio::spawn(async move {
            update_page_with_post_commit(
                &first_db,
                repo_id,
                "Home",
                "stale alpha payload",
                None,
                Some(user_id),
                move || async move {
                    committed_tx.send(()).expect("announce source commit");
                    release_rx.await.expect("release late update");
                },
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), committed_rx)
            .await
            .expect("first update did not commit in time")
            .expect("first update dropped its commit signal");
        let second = update_page(
            &db,
            repo_id,
            "Home",
            "fresh beta payload",
            None,
            Some(user_id),
        )
        .await;
        let indexed_before_release = wiki_fts_content(&db, page.id).await;
        release_tx.send(()).expect("release first update");
        first
            .await
            .expect("first update task panicked")
            .expect("first update succeeds");
        second.expect("newer update succeeds");

        assert_eq!(
            indexed_before_release.as_deref(),
            Some("fresh beta payload")
        );
        assert_eq!(
            wiki_fts_content(&db, page.id).await.as_deref(),
            Some("fresh beta payload"),
            "a caller returning late replayed its stale snapshot into FTS"
        );
        assert_eq!(
            get_page(&db, repo_id, "Home")
                .await
                .expect("read current page")
                .expect("page exists")
                .content,
            "fresh beta payload"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn late_update_completion_cannot_resurrect_deleted_fts_row() {
        let (_dir, db, user_id, repo_id) = wiki_repo_fixture("wikiupdatedelete").await;
        let page = create_page(
            &db,
            repo_id,
            "Home",
            "initial wiki payload",
            None,
            Some(user_id),
        )
        .await
        .expect("create page");

        let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first_db = db.clone();
        let first = tokio::spawn(async move {
            update_page_with_post_commit(
                &first_db,
                repo_id,
                "Home",
                "stale deleted payload",
                None,
                Some(user_id),
                move || async move {
                    committed_tx.send(()).expect("announce source commit");
                    release_rx.await.expect("release late update");
                },
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), committed_rx)
            .await
            .expect("update did not commit in time")
            .expect("update dropped its commit signal");
        let deleted = delete_page(&db, repo_id, "Home").await;
        release_tx.send(()).expect("release late update");
        first
            .await
            .expect("update task panicked")
            .expect("update succeeds");
        deleted.expect("delete succeeds");

        assert!(
            wiki_fts_content(&db, page.id).await.is_none(),
            "a late update resurrected the deleted FTS row"
        );
        assert!(get_page(&db, repo_id, "Home")
            .await
            .expect("read deleted page")
            .is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn late_create_completion_cannot_resurrect_deleted_fts_row() {
        let (_dir, db, user_id, repo_id) = wiki_repo_fixture("wikicreatedelete").await;
        let (committed_tx, committed_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let first_db = db.clone();
        let first = tokio::spawn(async move {
            create_page_with_post_commit(
                &first_db,
                repo_id,
                "Home",
                "created then deleted payload",
                None,
                Some(user_id),
                move || async move {
                    committed_tx.send(()).expect("announce source commit");
                    release_rx.await.expect("release late create");
                },
            )
            .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(10), committed_rx)
            .await
            .expect("create did not commit in time")
            .expect("create dropped its commit signal");
        let page = get_page(&db, repo_id, "Home")
            .await
            .expect("read committed page")
            .expect("page exists before delete");
        let deleted = delete_page(&db, repo_id, "Home").await;
        release_tx.send(()).expect("release late create");
        first
            .await
            .expect("create task panicked")
            .expect("create succeeds");
        deleted.expect("delete succeeds");

        assert!(
            wiki_fts_content(&db, page.id).await.is_none(),
            "a late create resurrected the deleted FTS row"
        );
    }
}
