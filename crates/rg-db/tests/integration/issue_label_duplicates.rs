//! card_85e5dda6b3b1, junction-table half: `issue_label_ops::set_labels`
//! inserted its `label_ids` one by one inside a transaction, so the same id
//! twice aborted the whole call against `idx_issue_labels_issue_label_unique`.
//!
//! It is tested here rather than through an HTTP route because there is no HTTP
//! route to test it through: issue bodies carry label *names*, and the
//! name-to-id resolution in `issue::service` filters the repository's label list
//! by membership, which collapses repeats before they get this far. That makes
//! this a latent trap on a public `rg_db::ops` entry point rather than a live
//! 500 — and the only honest place to pin it is the op itself.
//!
//! Asking for the same label twice is not a failure: the state the caller asked
//! for is reached either way. So the duplicate is dropped, and a UNIQUE
//! violation surviving that is no longer the caller's repeated id.

use rg_db::entities::{issue, label, repository};
use rg_db::sea_orm::{DatabaseConnection, NotSet, Set};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-issue-labels-{label}-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        Self { path }
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

/// An account, a repository, an issue and two labels to hang the rows off.
async fn setup(name: &str) -> (DatabaseConnection, TempDb, i64, Vec<i64>) {
    let temp = TempDb::new(name);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let user = rg_db::ops::user_ops::create_user(&db, "dana", "dana@example.com", "", "Dana")
        .await
        .expect("create the account the rows hang off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set("forge".to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        },
    )
    .await
    .expect("create the repository the rows hang off");

    let issue = rg_db::ops::issue_ops::create(
        &db,
        issue::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("an issue to label".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            author_id: Set(user.id),
            assignee_id: Set(None),
            milestone_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            deleted_at: Set(None),
        },
    )
    .await
    .expect("create the issue the labels hang off");

    let mut label_ids = Vec::new();
    for label_name in ["bug", "urgent"] {
        let created = rg_db::ops::label_ops::create(
            &db,
            label::ActiveModel {
                id: NotSet,
                repo_id: Set(repo.id),
                name: Set(label_name.to_string()),
                color: Set("#ff0000".to_string()),
                description: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
            },
        )
        .await
        .expect("create label");
        label_ids.push(created.id);
    }

    (db, temp, issue.id, label_ids)
}

/// The defect: a repeated id used to abort the transaction, so *none* of the
/// labels were applied and the caller was handed a database failure.
#[tokio::test]
async fn a_repeated_label_id_is_applied_once_rather_than_failing_the_write() {
    let (db, _temp, issue_id, labels) = setup("repeat").await;
    let (bug, urgent) = (labels[0], labels[1]);

    rg_db::ops::issue_label_ops::set_labels(&db, issue_id, vec![bug, urgent, bug])
        .await
        .expect("a label named twice is not a failed write");

    let stored = rg_db::ops::issue_label_ops::get_label_ids(&db, issue_id)
        .await
        .expect("read back the applied labels");
    assert_eq!(
        stored,
        vec![bug, urgent],
        "the duplicate is dropped, both labels are applied, and the caller's \
         order is kept"
    );
}

/// The replace half still replaces: the op's contract is "these labels and no
/// others", and deduplicating the input must not turn it into a merge.
#[tokio::test]
async fn setting_labels_again_replaces_rather_than_accumulates() {
    let (db, _temp, issue_id, labels) = setup("replace").await;
    let (bug, urgent) = (labels[0], labels[1]);

    rg_db::ops::issue_label_ops::set_labels(&db, issue_id, vec![bug, urgent])
        .await
        .expect("first set");
    rg_db::ops::issue_label_ops::set_labels(&db, issue_id, vec![urgent, urgent])
        .await
        .expect("second set");

    let stored = rg_db::ops::issue_label_ops::get_label_ids(&db, issue_id)
        .await
        .expect("read back the applied labels");
    assert_eq!(
        stored,
        vec![urgent],
        "the second call replaces the first, and its own repeat still collapses"
    );
}

/// An empty list clears the labels rather than leaving the old ones in place —
/// the delete runs before the (now empty) insert loop, and the dedup must not
/// have changed that.
#[tokio::test]
async fn an_empty_list_clears_the_labels() {
    let (db, _temp, issue_id, labels) = setup("clear").await;

    rg_db::ops::issue_label_ops::set_labels(&db, issue_id, labels)
        .await
        .expect("first set");
    rg_db::ops::issue_label_ops::set_labels(&db, issue_id, Vec::new())
        .await
        .expect("clear");

    assert!(
        rg_db::ops::issue_label_ops::get_label_ids(&db, issue_id)
            .await
            .expect("read back the applied labels")
            .is_empty(),
        "an empty list means no labels, not 'leave them alone'"
    );
}
