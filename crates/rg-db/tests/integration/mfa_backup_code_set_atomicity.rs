//! card_3c33caaf7402: a backup-code set is published as one operation.
//!
//! `set_codes` used to delete the live set and then insert the replacement one
//! row at a time, on the pool. Every gap between those statements was a state a
//! failure could stop in — and the set is handed to its owner exactly once, in
//! the response, so a half-written set means an account with a second factor and
//! nobody holding its recovery codes.
//!
//! The fault here is aimed at the *middle* of the replacement, not at the whole
//! table: a trigger that aborts only the sixth code proves the other nine and
//! the delete that preceded them are undone with it.

use rg_db::ops::mfa_backup_code_ops::{self, hash_code, BACKUP_CODE_COUNT};
use rg_db::sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-mfa-backup-set-{label}-{}.db",
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

async fn setup(label: &str) -> (TempDb, DatabaseConnection, i64) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    let user = rg_db::ops::user_ops::create_user(
        &db,
        label,
        &format!("{label}@example.invalid"),
        "",
        label,
    )
    .await
    .expect("create the account the codes belong to");
    (temp, db, user.id)
}

/// `codes[n]` for a deterministic, distinguishable set.
fn codes(prefix: &str) -> Vec<String> {
    (0..BACKUP_CODE_COUNT)
        .map(|index| format!("{prefix}CODE{index}"))
        .collect()
}

/// Every stored row for the user, as `(code_hash, used)`, ordered by id.
async fn stored(db: &DatabaseConnection, user_id: i64) -> Vec<(String, bool)> {
    mfa_backup_code_ops::Entity::find()
        .filter(rg_db::entities::mfa_backup_code::Column::UserId.eq(user_id))
        .all(db)
        .await
        .expect("read stored backup codes")
        .into_iter()
        .map(|row| (row.code_hash, row.used))
        .collect()
}

/// Abort any INSERT carrying `code_hash`, and nothing else.
///
/// Narrower than failing the table: the replacement has to be stopped *inside*
/// the set for the rollback of the preceding delete to mean anything.
async fn fail_insert_of(db: &DatabaseConnection, code_hash: &str) {
    db.execute_unprepared(&format!(
        "CREATE TRIGGER fk_fault_mfa_backup_codes BEFORE INSERT ON mfa_backup_codes \
         WHEN NEW.code_hash = '{code_hash}' \
         BEGIN SELECT RAISE(ABORT, 'injected failure: one code of the set'); END;"
    ))
    .await
    .expect("arm the mid-set insert fault");
}

async fn clear_fault(db: &DatabaseConnection) {
    db.execute_unprepared("DROP TRIGGER IF EXISTS fk_fault_mfa_backup_codes")
        .await
        .expect("disarm the mid-set insert fault");
}

/// A failure in the middle of the replacement leaves the old set in place.
#[tokio::test]
async fn a_failure_mid_set_keeps_the_previous_codes_and_publishes_none_of_the_new_ones() {
    let (_temp, db, user_id) = setup("midset").await;

    let old = codes("OLD");
    mfa_backup_code_ops::set_codes(&db, user_id, &old)
        .await
        .expect("publish the first set");
    let before = stored(&db, user_id).await;
    assert_eq!(
        before,
        old.iter()
            .map(|code| (hash_code(code), false))
            .collect::<Vec<_>>(),
        "the first set was not stored as given"
    );

    let new = codes("NEW");
    fail_insert_of(&db, &hash_code(&new[5])).await;
    let error = mfa_backup_code_ops::set_codes(&db, user_id, &new)
        .await
        .expect_err("the injected failure must escape as an error");
    assert!(
        error.to_string().contains("injected failure"),
        "the failure that surfaced is not the injected one: {error}"
    );
    clear_fault(&db).await;

    assert_eq!(
        stored(&db, user_id).await,
        before,
        "the failed replacement did not leave the account's live codes alone"
    );
    for code in &new {
        assert!(
            mfa_backup_code_ops::verify_and_consume(&db, user_id, code)
                .await
                .map(|accepted| !accepted)
                .expect("verify a code of the failed set"),
            "a code from the failed replacement is live even though it was never handed out"
        );
    }
    for code in &old {
        assert!(
            mfa_backup_code_ops::verify_and_consume(&db, user_id, code)
                .await
                .expect("verify a code of the surviving set"),
            "the account lost a recovery code it was actually holding"
        );
    }
}

/// A successful replacement retires the previous live set and keeps the spent
/// ones — those are history the audit view reads, not credentials.
#[tokio::test]
async fn a_replacement_retires_the_live_set_and_keeps_the_spent_ones() {
    let (_temp, db, user_id) = setup("replace").await;

    let old = codes("OLD");
    mfa_backup_code_ops::set_codes(&db, user_id, &old)
        .await
        .expect("publish the first set");
    assert!(
        mfa_backup_code_ops::verify_and_consume(&db, user_id, &old[0])
            .await
            .expect("spend one code"),
        "the first code of a fresh set was refused"
    );

    let new = codes("NEW");
    mfa_backup_code_ops::set_codes(&db, user_id, &new)
        .await
        .expect("publish the replacement");

    let rows = stored(&db, user_id).await;
    assert_eq!(
        rows.iter().filter(|(_, used)| *used).count(),
        1,
        "the spent code did not survive the replacement"
    );
    assert_eq!(
        rows.iter().filter(|(_, used)| !*used).count(),
        BACKUP_CODE_COUNT,
        "the live set after a replacement is not exactly the new set"
    );
    for code in old.iter().skip(1) {
        assert!(
            !mfa_backup_code_ops::verify_and_consume(&db, user_id, code)
                .await
                .expect("verify a retired code"),
            "a code from the replaced set is still accepted"
        );
    }
}

/// An empty set is a request, not a malformed statement: it revokes every live
/// code. The batched insert has to say so explicitly, or SQL with no `VALUES`
/// would turn "revoke everything" into an error.
#[tokio::test]
async fn an_empty_set_revokes_every_live_code() {
    let (_temp, db, user_id) = setup("empty").await;

    let old = codes("OLD");
    mfa_backup_code_ops::set_codes(&db, user_id, &old)
        .await
        .expect("publish the first set");
    mfa_backup_code_ops::set_codes(&db, user_id, &[])
        .await
        .expect("an empty set must be accepted");

    assert!(
        stored(&db, user_id).await.is_empty(),
        "an empty replacement left live codes behind"
    );
}
