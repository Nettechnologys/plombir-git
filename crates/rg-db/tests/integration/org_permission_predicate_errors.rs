//! card_8e618566ea32: `is_member_of_write_team` / `is_member_of_admin_team` used
//! to end in
//!
//! ```ignore
//! result.and_then(|row| row.try_get::<i64>("", "cnt").ok()).unwrap_or(0)
//! ```
//!
//! so an absent aggregate row and a `cnt` that would not decode collapsed into
//! the same `0` an honest "you are in no such team" produces. Both predicates
//! sit under `can_write_repo` / `can_admin_repo`, which serve HTTP, OCI,
//! Git-HTTP and SSH: on a schema drift or a backend type mismatch the server
//! answered *access denied* for a check that never ran, and the caller went off
//! to re-issue credentials that were never the problem.
//!
//! What these tests guard:
//!
//! * **A real answer stays an answer** — no membership is `Ok(false)`, a write
//!   or admin team membership is `Ok(true)`.
//! * **A check that could not run is an error**, not a denial: with the tables
//!   the query joins gone, both predicates return `Err`.

use rg_db::sea_orm::{ConnectionTrait, DatabaseConnection, Statement};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-org-predicate-{label}-{}.db",
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

async fn setup(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// An organization plus a member who belongs to no team at all.
async fn org_with_outsider(db: &DatabaseConnection, label: &str) -> (i64, i64) {
    let owner = rg_db::ops::user_ops::create_user(
        db,
        &format!("owner-{label}"),
        &format!("owner-{label}@example.com"),
        "",
        "Owner",
    )
    .await
    .expect("create the org owner");
    let member = rg_db::ops::user_ops::create_user(
        db,
        &format!("member-{label}"),
        &format!("member-{label}@example.com"),
        "",
        "Member",
    )
    .await
    .expect("create the account under test");
    let org = rg_db::ops::org_ops::create_org(
        db,
        &format!("acme-{label}"),
        None,
        None,
        owner.id,
        "public",
    )
    .await
    .expect("create the organization");
    rg_db::ops::org_ops::add_org_member(db, org.id, member.id, "member")
        .await
        .expect("add the account to the org");
    (org.id, member.id)
}

async fn join_team(db: &DatabaseConnection, org_id: i64, user_id: i64, permission: &str) {
    let team = rg_db::ops::org_ops::create_team(db, org_id, permission, None, permission)
        .await
        .expect("create the team");
    rg_db::ops::org_ops::add_team_member(db, team.id, user_id, "member")
        .await
        .expect("add the account to the team");
}

#[tokio::test]
async fn no_team_membership_is_a_decided_no() {
    let (db, _temp) = setup("no-membership").await;
    let (org_id, user_id) = org_with_outsider(&db, "no-membership").await;

    assert!(
        !rg_db::ops::org_ops::is_member_of_write_team(&db, org_id, user_id)
            .await
            .expect("the check runs"),
        "an org member in no team is not a writer"
    );
    assert!(
        !rg_db::ops::org_ops::is_member_of_admin_team(&db, org_id, user_id)
            .await
            .expect("the check runs"),
        "an org member in no team is not an admin"
    );
}

#[tokio::test]
async fn a_write_team_grants_write_but_not_admin() {
    let (db, _temp) = setup("write-team").await;
    let (org_id, user_id) = org_with_outsider(&db, "write-team").await;
    join_team(&db, org_id, user_id, "write").await;

    assert!(
        rg_db::ops::org_ops::is_member_of_write_team(&db, org_id, user_id)
            .await
            .expect("the check runs"),
        "a member of a write team is a writer"
    );
    assert!(
        !rg_db::ops::org_ops::is_member_of_admin_team(&db, org_id, user_id)
            .await
            .expect("the check runs"),
        "a write team is not an admin team"
    );
}

#[tokio::test]
async fn an_admin_team_grants_both() {
    let (db, _temp) = setup("admin-team").await;
    let (org_id, user_id) = org_with_outsider(&db, "admin-team").await;
    join_team(&db, org_id, user_id, "admin").await;

    assert!(
        rg_db::ops::org_ops::is_member_of_write_team(&db, org_id, user_id)
            .await
            .expect("the check runs"),
        "an admin team carries write"
    );
    assert!(
        rg_db::ops::org_ops::is_member_of_admin_team(&db, org_id, user_id)
            .await
            .expect("the check runs"),
        "a member of an admin team is an admin"
    );
}

/// The regression proper: with the joined tables gone the predicates must fail
/// loudly. Before the fix the execution error was propagated but its *result*
/// was not the only silent path — this pins the whole function to `Err`, so a
/// revert to `unwrap_or(0)` cannot pass by re-answering "not a member".
#[tokio::test]
async fn a_check_that_cannot_run_is_an_error_not_a_denial() {
    let (db, _temp) = setup("broken-schema").await;
    let (org_id, user_id) = org_with_outsider(&db, "broken-schema").await;

    for table in ["team_members", "teams"] {
        db.execute(Statement::from_string(
            db.get_database_backend(),
            format!("DROP TABLE {table}"),
        ))
        .await
        .expect("drop the table the predicate joins");
    }

    let write = rg_db::ops::org_ops::is_member_of_write_team(&db, org_id, user_id).await;
    assert!(
        write.is_err(),
        "a write-membership check against a missing table must not answer `false`, got {write:?}"
    );

    let admin = rg_db::ops::org_ops::is_member_of_admin_team(&db, org_id, user_id).await;
    assert!(
        admin.is_err(),
        "an admin-membership check against a missing table must not answer `false`, got {admin:?}"
    );
}
