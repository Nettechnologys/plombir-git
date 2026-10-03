//! card_0a4c00fd1b89: removing the second factor must take its recovery
//! material with it.
//!
//! `disable_mfa` cleared `mfa_enabled`, `mfa_type`, `totp_secret` and the legacy
//! `users.backup_codes` column — and left the `mfa_backup_codes` rows exactly
//! where they were. Each of those is a full bypass of the factor the owner just
//! asked to remove, stored as an unsalted SHA-256, and `GET /users/mfa/backup`
//! went on reporting them: ten live recovery codes on an account with no second
//! factor.
//!
//! There is no direct authentication bypass — spending a code needs an MFA
//! challenge cookie, and that is only issued to an account with `mfa_enabled` —
//! which is exactly why nobody would notice. The question this file answers is
//! the phase's fourth criterion: what happens to a long-lived credential when
//! the reason it existed goes away. The answer chosen here is "it goes too", and
//! these tests are what keep it answered.
//!
//! What they guard:
//!
//! * **The live set is gone** after the factor is removed.
//! * **Spent codes stay**, because they are history rather than credentials —
//!   the same line `set_codes` already draws.
//! * **Both halves land together**, so the flag and the revocation are never
//!   separately observable.
//! * **Not vacuous**: the codes really are there before the removal runs.

use rg_db::ops::{mfa_backup_code_ops, user_ops};
use rg_db::sea_orm::DatabaseConnection;

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-mfa-disable-revokes-{label}-{}.db",
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

/// A migrated database and one enrolled account.
///
/// `PLOMBIR_GIT_TEST_DATABASE_URL` points the file at PostgreSQL or MySQL instead,
/// the same switch the neighbouring backup-code tests use — the two writes land
/// in one transaction, and that is a claim about each backend.
async fn setup(label: &str) -> (DatabaseConnection, Option<TempDb>, i64) {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let (url, temp) = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => (url, None),
        _ => {
            let temp = TempDb::new(label);
            (temp.url(), Some(temp))
        }
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    let user = user_ops::create_user(
        &db,
        &format!("iris{label}{suffix}"),
        &format!("iris{label}{suffix}@example.invalid"),
        "",
        "Iris",
    )
    .await
    .expect("create the account the codes belong to");
    (db, temp, user.id)
}

/// `(total rows, unused rows)` — the two numbers `GET /users/mfa/backup`
/// reports, read from the same table it reads.
async fn counts(db: &DatabaseConnection, user_id: i64) -> (usize, usize) {
    let codes = mfa_backup_code_ops::list_codes(db, user_id)
        .await
        .expect("list the backup codes");
    let unused = codes.iter().filter(|code| !code.used).count();
    (codes.len(), unused)
}

const CODES: [&str; 3] = ["alpha-one", "beta-two", "gamma-three"];

#[tokio::test]
async fn disabling_the_second_factor_revokes_its_unused_backup_codes() {
    let (db, _temp, user_id) = setup("live").await;
    let codes: Vec<String> = CODES.iter().map(|code| (*code).to_string()).collect();
    user_ops::enable_mfa_with_backup_codes(&db, user_id, &codes)
        .await
        .expect("enrol the second factor");

    // Not vacuous: the codes this test is about really are enrolled.
    assert_eq!(
        counts(&db, user_id).await,
        (3, 3),
        "the fixture did not enrol the set the rest of this test removes"
    );

    let user = user_ops::disable_mfa(&db, user_id)
        .await
        .expect("remove the second factor")
        .expect("the account remains open");
    assert!(!user.mfa_enabled, "the factor itself must be off");

    let (total, unused) = counts(&db, user_id).await;
    assert_eq!(
        unused, 0,
        "unused recovery codes outlived the factor they recover"
    );
    assert_eq!(
        total, 0,
        "an account that never spent a code should be left with no rows at all"
    );

    // The verifier agrees with the count: no plaintext still opens the door.
    for code in CODES {
        assert!(
            !mfa_backup_code_ops::verify_and_consume(&db, user_id, code)
                .await
                .expect("verify a revoked code"),
            "revoked backup code {code} still passes the second factor"
        );
    }
}

/// A code that was spent is a record of something that happened, not a way in.
/// `set_codes` keeps such rows on re-issue for that reason, and removal draws
/// the same line — otherwise turning MFA off quietly erases the audit trail of
/// every recovery that ever used it.
#[tokio::test]
async fn a_spent_code_survives_the_removal_as_history() {
    let (db, _temp, user_id) = setup("spent").await;
    let codes: Vec<String> = CODES.iter().map(|code| (*code).to_string()).collect();
    user_ops::enable_mfa_with_backup_codes(&db, user_id, &codes)
        .await
        .expect("enrol the second factor");
    assert!(
        mfa_backup_code_ops::verify_and_consume(&db, user_id, CODES[0])
            .await
            .expect("spend one code"),
        "the fixture must actually spend a code"
    );

    user_ops::disable_mfa(&db, user_id)
        .await
        .expect("remove the second factor");

    let (total, unused) = counts(&db, user_id).await;
    assert_eq!(unused, 0, "the two live codes had to go");
    assert_eq!(total, 1, "the spent code is history and stays");
    assert!(
        !mfa_backup_code_ops::verify_and_consume(&db, user_id, CODES[0])
            .await
            .expect("verify the spent code"),
        "a spent code kept as history must still not open anything"
    );
}

/// Re-enrolling issues a fresh set rather than reviving the old one — the
/// property that makes revocation on removal safe to do at all.
#[tokio::test]
async fn re_enrolling_after_a_removal_publishes_a_new_set() {
    let (db, _temp, user_id) = setup("reenrol").await;
    let first: Vec<String> = CODES.iter().map(|code| (*code).to_string()).collect();
    user_ops::enable_mfa_with_backup_codes(&db, user_id, &first)
        .await
        .expect("enrol the second factor");
    user_ops::disable_mfa(&db, user_id)
        .await
        .expect("remove the second factor");

    let second = vec!["delta-four".to_string(), "epsilon-five".to_string()];
    user_ops::enable_mfa_with_backup_codes(&db, user_id, &second)
        .await
        .expect("re-enrol the second factor");

    assert_eq!(
        counts(&db, user_id).await,
        (2, 2),
        "re-enrolment must publish exactly the new set"
    );
    assert!(
        !mfa_backup_code_ops::verify_and_consume(&db, user_id, CODES[0])
            .await
            .expect("verify an old code"),
        "a code from the set that was revoked must not come back with the factor"
    );
    assert!(
        mfa_backup_code_ops::verify_and_consume(&db, user_id, &second[0])
            .await
            .expect("verify a new code"),
        "the freshly published set must work"
    );
}
