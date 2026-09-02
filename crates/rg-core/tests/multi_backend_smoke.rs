//! Runtime smoke for PostgreSQL/MySQL CI service containers.
//!
//! Run with:
//! `FORGEKEEP_TEST_DATABASE_URL=... cargo test -p rg-core --test multi_backend_smoke -- --ignored`

use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection,
    EntityTrait, NotSet, QueryFilter, Set, Statement, TransactionTrait,
};

/// Assert a spawned probe is still parked behind the boundary under test — and
/// say what it actually did when it is not.
///
/// `assert!(timeout(window, &mut probe).await.is_err())` reads `Err` as "still
/// parked". A probe that returned an error, and a probe that panicked, both
/// resolve the future *immediately* — so `is_err()` is false and the assertion
/// reports the opposite of the truth: "it crossed the boundary" when in fact it
/// never got in, with the real failure left in the probe's own output.
async fn assert_probe_stays_blocked<T>(probe: &mut tokio::task::JoinHandle<T>, crossed: &str)
where
    T: std::fmt::Debug,
{
    match tokio::time::timeout(std::time::Duration::from_millis(200), probe).await {
        Err(_still_parked) => {}
        Ok(Ok(outcome)) => {
            panic!("{crossed} — the probe finished with {outcome:?} instead of waiting")
        }
        Ok(Err(panic)) => {
            panic!("{crossed} — the probe panicked instead of waiting: {panic}")
        }
    }
}

/// A repository row in the namespace given by `org_id` (`None` = personal).
fn namespace_repo(
    owner_id: i64,
    org_id: Option<i64>,
    name: &str,
) -> rg_db::entities::repository::ActiveModel {
    let now = chrono::Utc::now();
    rg_db::entities::repository::ActiveModel {
        id: NotSet,
        owner_id: Set(owner_id),
        name: Set(name.to_string()),
        description: Set(None),
        is_private: Set(false),
        default_branch: Set("main".to_string()),
        fork_id: Set(None),
        stars_count: Set(0),
        forks_count: Set(0),
        org_id: Set(org_id),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        origin_repo_id: Set(None),
    }
}

/// The hashes of a user's *live* backup codes, oldest first.
async fn live_backup_hashes<C: ConnectionTrait>(db: &C, user_id: i64) -> Vec<String> {
    let mut rows = rg_db::ops::mfa_backup_code_ops::list_codes(db, user_id)
        .await
        .expect("read stored backup codes");
    rows.sort_by_key(|row| row.id);
    rows.into_iter()
        .filter(|row| !row.used)
        .map(|row| row.code_hash)
        .collect()
}

fn backup_hashes(codes: &[String]) -> Vec<String> {
    codes
        .iter()
        .map(|code| rg_db::ops::mfa_backup_code_ops::hash_code(code))
        .collect()
}

async fn repo_fts_snapshot(db: &DatabaseConnection, repo_id: i64) -> Option<(String, String)> {
    let backend = db.get_database_backend();
    db.query_one(Statement::from_sql_and_values(
        backend,
        rg_db::prepare_sql(
            backend,
            "SELECT name, description FROM repos_fts WHERE rowid = ?",
        ),
        [repo_id.into()],
    ))
    .await
    .expect("read repository FTS row")
    .map(|row| {
        (
            row.try_get("", "name").expect("decode FTS name"),
            row.try_get("", "description")
                .expect("decode FTS description"),
        )
    })
}

async fn exercise_organization_update_contract(
    db: &DatabaseConnection,
    org: rg_db::entities::organization::Model,
    owner_id: i64,
    suffix: &str,
) -> rg_db::entities::organization::Model {
    let org = rg_db::ops::org_ops::update_org(
        db,
        org.id,
        Some("Cross-backend organization"),
        Some("ordinary organization update"),
        Some("private"),
    )
    .await
    .expect("update smoke-test organization")
    .expect("smoke-test organization still exists");
    assert_eq!(
        org.display_name.as_deref(),
        Some("Cross-backend organization")
    );
    assert_eq!(
        org.description.as_deref(),
        Some("ordinary organization update")
    );
    assert_eq!(org.visibility, "private");

    let retiring_org = rg_db::ops::org_ops::create_org(
        db,
        &format!("retiring{suffix}"),
        None,
        None,
        owner_id,
        "public",
    )
    .await
    .expect("create organization for the absent update outcome");
    assert!(
        rg_db::ops::org_ops::begin_org_retirement(db, retiring_org.id)
            .await
            .expect("claim organization for retirement")
    );
    assert!(
        rg_db::ops::org_ops::update_org(
            db,
            retiring_org.id,
            Some("Too late"),
            Some("must not overwrite a closing namespace"),
            Some("private"),
        )
        .await
        .expect("a retiring organization is an outcome, not a database error")
        .is_none(),
        "a production DELETE claim must make the organization absent to PATCH"
    );
    let retiring_row = rg_db::ops::org_ops::get_org(db, retiring_org.id)
        .await
        .expect("read the claimed organization")
        .expect("retirement keeps the row until storage is retired");
    assert!(retiring_row.deleted_at.is_some());
    assert_eq!(retiring_row.display_name, None);
    assert!(rg_db::ops::org_ops::delete_org(db, retiring_org.id)
        .await
        .expect("finish deleting the smoke-test organization"));
    assert!(rg_db::ops::org_ops::update_org(
        db,
        retiring_org.id,
        Some("Still too late"),
        None,
        None,
    )
    .await
    .expect("a deleted organization is an outcome, not a database error")
    .is_none());

    org
}

async fn exercise_admin_user_mutation_contract(db: &DatabaseConnection, suffix: &str) {
    let ordinary = rg_db::ops::user_ops::create_user(
        db,
        &format!("adminmutation{suffix}"),
        &format!("adminmutation{suffix}@example.invalid"),
        "unused",
        "Admin Mutation Smoke",
    )
    .await
    .expect("create the ordinary admin-mutation account");
    let updated = rg_db::ops::user_ops::update_by_id(
        db,
        ordinary.id,
        Some(Some("Portable Admin Update".to_string())),
        Some(Some("portable admin bio".to_string())),
        Some(true),
        None,
    )
    .await
    .expect("update an open account")
    .expect("the ordinary admin-mutation account remains open");
    assert_eq!(
        updated.display_name.as_deref(),
        Some("Portable Admin Update")
    );
    assert_eq!(updated.bio.as_deref(), Some("portable admin bio"));
    assert!(updated.is_admin);

    let unchanged = rg_db::ops::user_ops::update_by_id(
        db,
        ordinary.id,
        Some(updated.display_name.clone()),
        Some(updated.bio.clone()),
        Some(updated.is_admin),
        Some(updated.is_active),
    )
    .await
    .expect("repeat an unchanged admin update")
    .expect("a MySQL zero-change result is not a missing account");
    assert_eq!(unchanged.id, ordinary.id);

    rg_db::ops::user_ops::record_failed_login(db, ordinary.id, 5)
        .await
        .expect("seed a failed login on the open account");
    let unlocked = rg_db::ops::user_ops::reset_login_failures_if_open(db, ordinary.id)
        .await
        .expect("reset failures on the open account")
        .expect("the ordinary unlock target remains open");
    assert_eq!(unlocked.login_attempts, 0);
    assert!(unlocked.locked_until.is_none());

    let retiring_patch = rg_db::ops::user_ops::create_user(
        db,
        &format!("adminpatchgone{suffix}"),
        &format!("adminpatchgone{suffix}@example.invalid"),
        "unused",
        "Retiring Admin Patch",
    )
    .await
    .expect("create the retiring admin-PATCH account");
    assert!(
        rg_db::ops::user_ops::begin_user_retirement(db, retiring_patch.id)
            .await
            .expect("claim the admin-PATCH account for retirement")
    );
    assert!(
        rg_db::ops::user_ops::update_by_id(
            db,
            retiring_patch.id,
            Some(Some("Too Late".to_string())),
            None,
            Some(true),
            None,
        )
        .await
        .expect("retirement is an outcome, not an admin-PATCH database error")
        .is_none(),
        "admin PATCH accepted an account already claimed for deletion"
    );
    let retiring_patch_row = rg_db::ops::user_ops::find_by_id(db, retiring_patch.id)
        .await
        .expect("read the retiring admin-PATCH account")
        .expect("retirement keeps the account row until storage is retired");
    assert_eq!(
        retiring_patch_row.display_name.as_deref(),
        Some("Retiring Admin Patch")
    );
    assert!(!retiring_patch_row.is_admin);
    assert!(rg_db::ops::user_ops::delete_by_id(db, retiring_patch.id)
        .await
        .expect("finish deleting the admin-PATCH account"));
    assert!(rg_db::ops::user_ops::update_by_id(
        db,
        retiring_patch.id,
        Some(Some("Still Too Late".to_string())),
        None,
        None,
        None,
    )
    .await
    .expect("physical deletion is an outcome, not an admin-PATCH database error")
    .is_none());

    let retiring_unlock = rg_db::ops::user_ops::create_user(
        db,
        &format!("adminunlockgone{suffix}"),
        &format!("adminunlockgone{suffix}@example.invalid"),
        "unused",
        "Retiring Admin Unlock",
    )
    .await
    .expect("create the retiring admin-unlock account");
    rg_db::ops::user_ops::record_failed_login(db, retiring_unlock.id, 5)
        .await
        .expect("seed a failure on the retiring unlock target");
    assert!(
        rg_db::ops::user_ops::begin_user_retirement(db, retiring_unlock.id)
            .await
            .expect("claim the admin-unlock account for retirement")
    );
    assert!(
        rg_db::ops::user_ops::reset_login_failures_if_open(db, retiring_unlock.id)
            .await
            .expect("retirement is an outcome, not an admin-unlock database error")
            .is_none(),
        "admin unlock accepted an account already claimed for deletion"
    );
    let retiring_unlock_row = rg_db::ops::user_ops::find_by_id(db, retiring_unlock.id)
        .await
        .expect("read the retiring admin-unlock account")
        .expect("retirement keeps the account row until storage is retired");
    assert_eq!(retiring_unlock_row.login_attempts, 1);
    assert!(rg_db::ops::user_ops::delete_by_id(db, retiring_unlock.id)
        .await
        .expect("finish deleting the admin-unlock account"));
    assert!(
        rg_db::ops::user_ops::reset_login_failures_if_open(db, retiring_unlock.id)
            .await
            .expect("physical deletion is an outcome, not an admin-unlock database error")
            .is_none()
    );
}

async fn exercise_login_finalization_contract(db: &DatabaseConnection, suffix: &str) {
    let plain = rg_db::ops::user_ops::create_user(
        db,
        &format!("loginfinalplain{suffix}"),
        &format!("loginfinalplain{suffix}@example.invalid"),
        "unused",
        "Plain Login Finalization",
    )
    .await
    .expect("create the plain login-finalization account");
    rg_db::ops::user_ops::record_failed_login(db, plain.id, 5)
        .await
        .expect("seed a failed primary-factor attempt");
    let plain = rg_db::ops::user_ops::finalize_primary_login(db, plain.id)
        .await
        .expect("finalize an open plain account")
        .expect("the plain account remains open");
    assert_eq!(plain.login_attempts, 0);
    assert!(plain.locked_until.is_none());
    assert!(
        plain.last_login_at.is_some(),
        "a primary factor completes login while MFA is off"
    );

    let mfa = rg_db::ops::user_ops::create_user(
        db,
        &format!("loginfinalmfa{suffix}"),
        &format!("loginfinalmfa{suffix}@example.invalid"),
        "unused",
        "MFA Login Finalization",
    )
    .await
    .expect("create the MFA login-finalization account");
    rg_db::ops::user_ops::enable_mfa(db, mfa.id)
        .await
        .expect("enable MFA for the login-finalization account");
    rg_db::ops::user_ops::record_failed_login(db, mfa.id, 5)
        .await
        .expect("seed a failed MFA attempt");
    let pending = rg_db::ops::user_ops::finalize_primary_login(db, mfa.id)
        .await
        .expect("finalize the MFA primary factor")
        .expect("the MFA account remains open");
    assert!(pending.mfa_enabled);
    assert_eq!(
        pending.login_attempts, 1,
        "a fresh primary-factor challenge erased the second-factor failure"
    );
    assert_eq!(pending.last_login_at, None);
    let completed = rg_db::ops::user_ops::record_successful_login(db, mfa.id)
        .await
        .expect("finalize the second factor")
        .expect("the MFA account remains open");
    assert!(completed.last_login_at.is_some());

    let retiring = rg_db::ops::user_ops::create_user(
        db,
        &format!("loginfinalretire{suffix}"),
        &format!("loginfinalretire{suffix}@example.invalid"),
        "unused",
        "Retiring Login Finalization",
    )
    .await
    .expect("create the retiring login-finalization account");
    assert!(rg_db::ops::user_ops::begin_user_retirement(db, retiring.id)
        .await
        .expect("claim the login-finalization account for retirement"));
    assert!(
        rg_db::ops::user_ops::finalize_primary_login(db, retiring.id)
            .await
            .expect("retirement is an outcome, not a database error")
            .is_none()
    );
    assert!(
        rg_db::ops::user_ops::record_successful_login(db, retiring.id)
            .await
            .expect("retirement is an outcome, not a database error")
            .is_none()
    );

    let deleted = rg_db::ops::user_ops::create_user(
        db,
        &format!("loginfinaldelete{suffix}"),
        &format!("loginfinaldelete{suffix}@example.invalid"),
        "unused",
        "Deleted Login Finalization",
    )
    .await
    .expect("create the deleted login-finalization account");
    assert!(rg_db::ops::user_ops::delete_by_id(db, deleted.id)
        .await
        .expect("delete the login-finalization account"));
    assert!(
        rg_db::ops::user_ops::record_successful_login(db, deleted.id)
            .await
            .expect("physical deletion is an outcome, not a database error")
            .is_none()
    );
}

async fn exercise_standing_credential_finalization_contract(db: &DatabaseConnection, suffix: &str) {
    let open = rg_db::ops::user_ops::create_user(
        db,
        &format!("credentialowner{suffix}"),
        &format!("credentialowner{suffix}@example.invalid"),
        "unused",
        "Standing Credential Owner",
    )
    .await
    .expect("create the open standing-credential owner");
    rg_db::ops::user_ops::record_failed_login(db, open.id, 5)
        .await
        .expect("seed account state the credential finalizer must preserve");
    let before = rg_db::ops::user_ops::find_by_id(db, open.id)
        .await
        .expect("read standing-credential owner before finalization")
        .expect("standing-credential owner exists");
    let finalized = rg_db::ops::user_ops::finalize_standing_credential_owner(db, open.id)
        .await
        .expect("finalize an open standing-credential owner")
        .expect("the standing-credential owner remains open");
    assert_eq!(finalized.login_attempts, before.login_attempts);
    assert_eq!(finalized.locked_until, before.locked_until);
    assert_eq!(finalized.last_login_at, before.last_login_at);
    assert_eq!(finalized.session_version, before.session_version);
    assert_eq!(
        finalized.updated_at, before.updated_at,
        "standing-credential proof claimed to edit the account"
    );

    let retiring = rg_db::ops::user_ops::create_user(
        db,
        &format!("credentialretire{suffix}"),
        &format!("credentialretire{suffix}@example.invalid"),
        "unused",
        "Retiring Credential Owner",
    )
    .await
    .expect("create the retiring standing-credential owner");
    assert!(rg_db::ops::user_ops::begin_user_retirement(db, retiring.id)
        .await
        .expect("claim standing-credential owner for retirement"));
    assert!(
        rg_db::ops::user_ops::finalize_standing_credential_owner(db, retiring.id)
            .await
            .expect("retirement is an outcome, not a database error")
            .is_none(),
        "standing credential accepted an owner already claimed for retirement"
    );

    let deleted = rg_db::ops::user_ops::create_user(
        db,
        &format!("credentialdelete{suffix}"),
        &format!("credentialdelete{suffix}@example.invalid"),
        "unused",
        "Deleted Credential Owner",
    )
    .await
    .expect("create the deleted standing-credential owner");
    assert!(rg_db::ops::user_ops::delete_by_id(db, deleted.id)
        .await
        .expect("delete standing-credential owner"));
    assert!(
        rg_db::ops::user_ops::finalize_standing_credential_owner(db, deleted.id)
            .await
            .expect("physical deletion is an outcome, not a database error")
            .is_none(),
        "standing credential accepted a physically deleted owner"
    );
}

async fn create_running_ci_job(
    db: &DatabaseConnection,
    stage_id: i64,
) -> rg_db::entities::pipeline_job::Model {
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage_id, "build", "true", None, None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create CI job-token job");
    rg_db::ops::pipeline_ops::update_job_result(db, job.id, "running", None, None, None, None)
        .await
        .expect("mark CI job-token job running");
    rg_db::ops::pipeline_ops::get_job(db, job.id)
        .await
        .expect("read running CI job-token job")
        .expect("running CI job-token job exists")
}

async fn assert_ci_entity_schema_types(db: &DatabaseConnection) {
    const BIGINT_COLUMNS: &[(&str, &str)] = &[
        ("pipelines", "id"),
        ("pipelines", "repo_id"),
        ("pipelines", "triggered_by"),
        ("pipeline_stages", "id"),
        ("pipeline_stages", "pipeline_id"),
        ("pipeline_jobs", "id"),
        ("pipeline_jobs", "stage_id"),
        ("pipeline_jobs", "runner_id"),
        ("pipeline_jobs", "timeout_seconds"),
        ("pipeline_jobs", "environment_id"),
        ("ci_environments", "id"),
        ("ci_environments", "repo_id"),
        ("ci_environment_approvals", "id"),
        ("ci_environment_approvals", "job_id"),
        ("ci_environment_approvals", "environment_id"),
        ("ci_environment_approvals", "approved_by"),
    ];
    const NAIVE_DATETIME_COLUMNS: &[(&str, &str)] = &[
        ("pipelines", "started_at"),
        ("pipelines", "finished_at"),
        ("pipelines", "created_at"),
        ("pipeline_stages", "started_at"),
        ("pipeline_stages", "finished_at"),
        ("pipeline_jobs", "started_at"),
        ("pipeline_jobs", "finished_at"),
        ("pipeline_jobs", "updated_at"),
    ];
    const UTC_DATETIME_COLUMNS: &[(&str, &str)] = &[
        ("ci_environments", "created_at"),
        ("ci_environments", "updated_at"),
        ("ci_environment_approvals", "created_at"),
    ];

    let (schema, bigint, naive_datetime, utc_datetime) = match db.get_database_backend() {
        DatabaseBackend::Postgres => (
            "current_schema()",
            "bigint",
            "timestamp without time zone",
            "timestamp with time zone",
        ),
        DatabaseBackend::MySql => ("DATABASE()", "bigint", "datetime", "timestamp"),
        DatabaseBackend::Sqlite => panic!("CI server-schema contract requires PostgreSQL/MySQL"),
    };
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            format!(
                "SELECT table_name AS ci_table_name, column_name AS ci_column_name, \
                        data_type AS ci_data_type FROM information_schema.columns \
                 WHERE table_schema = {schema} AND table_name IN \
                   ('pipelines', 'pipeline_stages', 'pipeline_jobs', \
                    'ci_environments', 'ci_environment_approvals')"
            ),
        ))
        .await
        .expect("read CI schema types");
    let actual = rows
        .into_iter()
        .map(|row| {
            (
                row.try_get::<String>("", "ci_table_name").unwrap(),
                row.try_get::<String>("", "ci_column_name").unwrap(),
                row.try_get::<String>("", "ci_data_type").unwrap(),
            )
        })
        .collect::<Vec<_>>();

    for (columns, expected) in [
        (BIGINT_COLUMNS, bigint),
        (NAIVE_DATETIME_COLUMNS, naive_datetime),
        (UTC_DATETIME_COLUMNS, utc_datetime),
    ] {
        for (table, column) in columns {
            let data_type = actual
                .iter()
                .find(|(actual_table, actual_column, _)| {
                    actual_table == table && actual_column == column
                })
                .map(|(_, _, data_type)| data_type.as_str());
            assert_eq!(
                data_type,
                Some(expected),
                "{table}.{column} drifted from its SeaORM entity type"
            );
        }
    }
}

async fn exercise_ci_job_token_finalization_contract(db: &DatabaseConnection, suffix: &str) {
    assert_ci_entity_schema_types(db).await;

    let owner = rg_db::ops::user_ops::create_user(
        db,
        &format!("cijobowner{suffix}"),
        &format!("cijobowner{suffix}@example.invalid"),
        "unused",
        "CI Job Token Owner",
    )
    .await
    .expect("create CI job-token owner");
    let repo = rg_db::ops::repo_ops::create(
        db,
        namespace_repo(owner.id, None, &format!("cijobrepo{suffix}")),
    )
    .await
    .expect("create CI job-token repository");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo.id,
        "0000000000000000000000000000000000000000",
        "refs/heads/main",
        "push",
        Some(owner.id),
    )
    .await
    .expect("create CI job-token pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "build", 0)
        .await
        .expect("create CI job-token stage");

    let running = create_running_ci_job(db, stage.id).await;
    let finalized = rg_db::ops::pipeline_ops::finalize_ci_job_token_job(db, running.id)
        .await
        .expect("finalize live CI job-token job")
        .expect("live CI job-token job remains usable");
    assert_eq!(finalized.status, running.status);
    assert_eq!(finalized.updated_at, running.updated_at);
    assert_eq!(finalized.stage_id, running.stage_id);

    rg_db::ops::pipeline_ops::update_job_result(db, running.id, "canceled", None, None, None, None)
        .await
        .expect("cancel CI job-token job");
    assert!(
        rg_db::ops::pipeline_ops::finalize_ci_job_token_job(db, running.id)
            .await
            .expect("cancellation is an outcome, not a database error")
            .is_none(),
        "canceled CI job remained usable by its token"
    );

    let deleted = create_running_ci_job(db, stage.id).await;
    rg_db::entities::pipeline_job::Entity::delete_by_id(deleted.id)
        .exec(db)
        .await
        .expect("delete CI job-token job");
    assert!(
        rg_db::ops::pipeline_ops::finalize_ci_job_token_job(db, deleted.id)
            .await
            .expect("physical deletion is an outcome, not a database error")
            .is_none(),
        "deleted CI job remained usable by its token"
    );
}

async fn exercise_ci_graph_transition_contract(db: &DatabaseConnection, suffix: &str) {
    let owner = rg_db::ops::user_ops::create_user(
        db,
        &format!("cigraphowner{suffix}"),
        &format!("cigraphowner{suffix}@example.invalid"),
        "unused",
        "CI Graph Owner",
    )
    .await
    .expect("create CI graph owner");
    let repo = rg_db::ops::repo_ops::create(
        db,
        namespace_repo(owner.id, None, &format!("cigraphrepo{suffix}")),
    )
    .await
    .expect("create CI graph repository");
    let (runner, _) = rg_db::ops::runner_ops::register_runner(
        db,
        repo.id,
        &format!("cigraphrunner{suffix}"),
        "[]",
        None,
        None,
        None,
    )
    .await
    .expect("create CI graph runner");

    let manual_pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo.id,
        "1111111111111111111111111111111111111111",
        "refs/heads/main",
        "manual",
        Some(owner.id),
    )
    .await
    .expect("create manual CI pipeline");
    let manual_stage = rg_db::ops::pipeline_ops::create_stage(db, manual_pipeline.id, "manual", 0)
        .await
        .expect("create manual CI stage");
    let manual_job = rg_db::ops::pipeline_ops::create_job(
        db,
        manual_stage.id,
        "manual",
        "true",
        None,
        None,
        None,
        None,
        None,
        None,
        false,
        None,
        Some("manual"),
        None,
    )
    .await
    .expect("create manual CI job");
    rg_db::ops::pipeline_ops::update_stage_status(db, manual_stage.id, "manual", None, None)
        .await
        .expect("pause manual CI stage");
    rg_db::ops::pipeline_ops::update_pipeline_status(db, manual_pipeline.id, "manual", None, None)
        .await
        .expect("pause manual CI pipeline");
    assert!(
        rg_db::ops::pipeline_ops::play_manual_job_and_resume_pipeline_chain(
            db,
            manual_pipeline.id,
            manual_stage.id,
            manual_job.id,
        )
        .await
        .expect("release manual CI graph")
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(db, manual_job.id)
            .await
            .expect("read released manual job")
            .expect("manual job exists")
            .status,
        "pending"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_stage_by_id(db, manual_stage.id)
            .await
            .expect("read released manual stage")
            .expect("manual stage exists")
            .status,
        "pending"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_pipeline(db, manual_pipeline.id)
            .await
            .expect("read released manual pipeline")
            .expect("manual pipeline exists")
            .status,
        "pending"
    );

    assert!(
        rg_db::ops::pipeline_ops::assign_job(db, manual_job.id, runner.id)
            .await
            .expect("assign manual job to the runner")
    );
    rg_db::ops::pipeline_ops::update_job_result(
        db,
        manual_job.id,
        "running",
        None,
        None,
        Some(chrono::Utc::now().naive_utc()),
        None,
    )
    .await
    .expect("start the assigned manual job");
    rg_db::ops::pipeline_ops::update_stage_status(db, manual_stage.id, "running", None, None)
        .await
        .expect("start manual stage");
    rg_db::ops::pipeline_ops::update_pipeline_status(db, manual_pipeline.id, "running", None, None)
        .await
        .expect("start manual pipeline");
    rg_db::ops::runner_ops::update_status(db, runner.id, "busy")
        .await
        .expect("mark CI graph runner busy");
    let finished = rg_db::ops::pipeline_ops::finish_runner_job(
        db,
        runner.id,
        manual_job.id,
        manual_stage.id,
        "success",
        Some(0),
        None,
        Some(chrono::Utc::now().naive_utc()),
    )
    .await
    .expect("finish the CI graph transaction");
    assert!(finished.job_settled);
    assert_eq!(
        finished
            .completed_pipeline
            .as_ref()
            .map(|pipeline| pipeline.status.as_str()),
        Some("success")
    );
    assert_eq!(
        rg_db::ops::runner_ops::find_by_id(db, runner.id)
            .await
            .expect("read finished CI graph runner")
            .expect("CI graph runner exists")
            .status,
        "online"
    );

    let approval_pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo.id,
        "2222222222222222222222222222222222222222",
        "refs/heads/main",
        "push",
        Some(owner.id),
    )
    .await
    .expect("create approval CI pipeline");
    let approval_stage =
        rg_db::ops::pipeline_ops::create_stage(db, approval_pipeline.id, "approval", 0)
            .await
            .expect("create approval CI stage");
    let approval_job = rg_db::ops::pipeline_ops::create_job(
        db,
        approval_stage.id,
        "approval",
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
    .expect("create approval CI job");
    rg_db::ops::pipeline_ops::update_job_result(
        db,
        approval_job.id,
        "waiting_approval",
        None,
        None,
        None,
        None,
    )
    .await
    .expect("gate approval job");
    rg_db::ops::pipeline_ops::update_stage_status(
        db,
        approval_stage.id,
        "waiting_approval",
        None,
        None,
    )
    .await
    .expect("gate approval stage");
    rg_db::ops::pipeline_ops::update_pipeline_status(
        db,
        approval_pipeline.id,
        "waiting_approval",
        None,
        None,
    )
    .await
    .expect("gate approval pipeline");
    let released = rg_db::ops::pipeline_ops::release_approved_job_and_resume_approval_chain(
        db,
        approval_pipeline.id,
        approval_stage.id,
        approval_job.id,
    )
    .await
    .expect("release approval CI graph");
    assert_eq!(
        released,
        rg_db::ops::pipeline_ops::ApprovalRelease {
            released: true,
            resumed_pipeline: true,
        }
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_job(db, approval_job.id)
            .await
            .expect("read released approval job")
            .expect("approval job exists")
            .status,
        "pending"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_stage_by_id(db, approval_stage.id)
            .await
            .expect("read released approval stage")
            .expect("approval stage exists")
            .status,
        "pending"
    );
    assert_eq!(
        rg_db::ops::pipeline_ops::get_pipeline(db, approval_pipeline.id)
            .await
            .expect("read released approval pipeline")
            .expect("approval pipeline exists")
            .status,
        "pending"
    );
}

async fn exercise_ci_secret_update_contract(db: &DatabaseConnection, repo_id: i64, actor_id: i64) {
    let created = rg_db::ops::ci_secret_ops::upsert(
        db,
        repo_id,
        "PORTABLE_DEPLOY_TOKEN",
        "ciphertext-initial",
        actor_id,
    )
    .await
    .expect("create the smoke-test CI secret")
    .expect("a first PUT creates the CI secret");
    let updated = rg_db::ops::ci_secret_ops::upsert(
        db,
        repo_id,
        "PORTABLE_DEPLOY_TOKEN",
        "ciphertext-rotated",
        actor_id,
    )
    .await
    .expect("update the smoke-test CI secret")
    .expect("the observed CI secret still exists");
    assert_eq!(updated.id, created.id);
    assert_eq!(updated.encrypted_value, "ciphertext-rotated");

    let unchanged = rg_db::ops::ci_secret_ops::update_existing(
        db,
        updated.id,
        repo_id,
        &updated.encrypted_value,
        updated.updated_at,
    )
    .await
    .expect("repeat an unchanged CI secret update")
    .expect("a MySQL zero-change result is not a missing row");
    assert_eq!(unchanged.id, updated.id);

    assert!(rg_db::ops::ci_secret_ops::delete_by_repo_and_name(
        db,
        repo_id,
        "PORTABLE_DEPLOY_TOKEN",
    )
    .await
    .expect("delete the smoke-test CI secret"));
    assert!(
        rg_db::ops::ci_secret_ops::update_existing(
            db,
            updated.id,
            repo_id,
            "ciphertext-too-late",
            chrono::Utc::now(),
        )
        .await
        .expect("a deleted CI secret is an outcome, not a database error")
        .is_none(),
        "the losing update must not recreate the deleted CI secret"
    );
}

async fn exercise_commit_status_parent_delete_contract(
    db: &DatabaseConnection,
    repo_id: i64,
    actor_id: i64,
) {
    let now = chrono::Utc::now();
    let created = rg_db::ops::commit_status_ops::create_or_update(
        db,
        repo_id,
        "portable-status-sha",
        "portable/status",
        rg_db::entities::commit_status::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            sha: Set("portable-status-sha".to_string()),
            state: Set("pending".to_string()),
            context: Set("portable/status".to_string()),
            description: Set(Some("portable initial report".to_string())),
            target_url: Set(None),
            creator_id: Set(Some(actor_id)),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("create the portable commit status")
    .expect("the portable repository exists");
    let updated = rg_db::ops::commit_status_ops::create_or_update(
        db,
        repo_id,
        "portable-status-sha",
        "portable/status",
        rg_db::entities::commit_status::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            sha: Set("portable-status-sha".to_string()),
            state: Set("success".to_string()),
            context: Set("portable/status".to_string()),
            description: Set(Some("portable updated report".to_string())),
            target_url: Set(None),
            creator_id: Set(Some(actor_id)),
            created_at: Set(now),
            updated_at: Set(chrono::Utc::now()),
        },
    )
    .await
    .expect("repeat the portable commit status")
    .expect("the observed status still exists");
    assert_eq!(updated.id, created.id);
    assert_eq!(updated.state, "success");

    let unchanged: rg_db::entities::commit_status::ActiveModel = updated.clone().into();
    let unchanged = rg_db::ops::commit_status_ops::update_existing(db, updated.clone(), unchanged)
        .await
        .expect("repeat an unchanged commit status update")
        .expect("a MySQL zero-change result is not a missing row");
    assert_eq!(unchanged.id, updated.id);

    rg_db::entities::repository::Entity::delete_by_id(repo_id)
        .exec(db)
        .await
        .expect("delete the portable commit-status repository");
    let mut too_late: rg_db::entities::commit_status::ActiveModel = updated.clone().into();
    too_late.state = Set("error".to_string());
    too_late.description = Set(Some("must not resurrect".to_string()));
    too_late.updated_at = Set(chrono::Utc::now());
    assert!(
        rg_db::ops::commit_status_ops::update_existing(db, updated, too_late)
            .await
            .expect("parent deletion is an outcome, not a database error")
            .is_none(),
        "the losing portable update claimed a status deleted by its parent"
    );
    assert!(
        rg_db::ops::commit_status_ops::list_by_sha(db, repo_id, "portable-status-sha")
            .await
            .expect("look for a resurrected portable status")
            .is_empty()
    );
}

async fn exercise_retention_watch_parent_delete_contract(
    db: &DatabaseConnection,
    deleted_repo_id: i64,
    surviving_repo_id: i64,
    watcher_id: i64,
) {
    let policy = rg_db::ops::ci_retention_ops::upsert_policy(db, deleted_repo_id, 30, 7)
        .await
        .expect("create the portable retention policy")
        .expect("the portable policy repository exists");
    let policy = rg_db::ops::ci_retention_ops::apply_policy(db, policy, 30, 7)
        .await
        .expect("repeat an unchanged portable policy update")
        .expect("a MySQL zero-change policy result is not a missing row");

    rg_db::ops::repo_watch_ops::set_watch_state(db, watcher_id, deleted_repo_id, "watching")
        .await
        .expect("create the portable repository-cascade watch")
        .expect("both watch parents exist");
    let repository_watch = rg_db::entities::repo_watch::Entity::find()
        .filter(rg_db::entities::repo_watch::Column::UserId.eq(watcher_id))
        .filter(rg_db::entities::repo_watch::Column::RepoId.eq(deleted_repo_id))
        .one(db)
        .await
        .expect("read the portable repository-cascade watch")
        .expect("the portable repository-cascade watch exists");
    let repository_watch = rg_db::ops::repo_watch_ops::apply_state(
        db,
        repository_watch,
        "watching",
        chrono::Utc::now(),
    )
    .await
    .expect("repeat an unchanged portable watch update")
    .expect("a MySQL zero-change watch result is not a missing row");

    rg_db::ops::repo_watch_ops::set_watch_state(db, watcher_id, surviving_repo_id, "watching")
        .await
        .expect("create the portable user-cascade watch")
        .expect("both watch parents exist");
    let user_watch = rg_db::entities::repo_watch::Entity::find()
        .filter(rg_db::entities::repo_watch::Column::UserId.eq(watcher_id))
        .filter(rg_db::entities::repo_watch::Column::RepoId.eq(surviving_repo_id))
        .one(db)
        .await
        .expect("read the portable user-cascade watch")
        .expect("the portable user-cascade watch exists");

    rg_db::entities::repository::Entity::delete_by_id(deleted_repo_id)
        .exec(db)
        .await
        .expect("delete the portable policy/watch repository");
    assert!(
        rg_db::ops::ci_retention_ops::apply_policy(db, policy, 90, 14)
            .await
            .expect("repository deletion is a policy outcome, not a database error")
            .is_none(),
        "the losing portable update claimed a policy deleted by its repository"
    );
    assert!(
        rg_db::ops::repo_watch_ops::apply_state(
            db,
            repository_watch,
            "ignoring",
            chrono::Utc::now(),
        )
        .await
        .expect("repository deletion is a watch outcome, not a database error")
        .is_none(),
        "the losing portable update claimed a watch deleted by its repository"
    );

    rg_db::entities::user::Entity::delete_by_id(watcher_id)
        .exec(db)
        .await
        .expect("delete the portable watcher");
    assert!(
        rg_db::ops::repo_watch_ops::apply_state(db, user_watch, "ignoring", chrono::Utc::now(),)
            .await
            .expect("user deletion is a watch outcome, not a database error")
            .is_none(),
        "the losing portable update claimed a watch deleted by its user"
    );
    assert!(
        rg_db::ops::repo_ops::find_by_id(db, surviving_repo_id)
            .await
            .expect("read the repository owned by somebody else")
            .is_some(),
        "deleting the watcher deleted somebody else's portable repository"
    );
    assert!(
        rg_db::ops::repo_watch_ops::get_watch_state(db, watcher_id, surviving_repo_id)
            .await
            .expect("look for a resurrected portable watch")
            .is_none()
    );
}

async fn portable_merge_queue_pr(
    db: &DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    number: i64,
) -> rg_db::entities::pull_request::Model {
    let now = chrono::Utc::now();
    rg_db::entities::pull_request::ActiveModel {
        repo_id: Set(repo_id),
        number: Set(number),
        title: Set(format!("portable merge queue PR {number}")),
        state: Set("open".to_string()),
        is_draft: Set(false),
        auto_merge_enabled: Set(false),
        author_id: Set(author_id),
        head_branch: Set(format!("portable-feature-{number}")),
        base_branch: Set("main".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(db)
    .await
    .expect("create portable merge-queue pull request")
}

/// Keep the existing-row enqueue contract honest on backends whose no-op
/// `UPDATE` counts differ. This covers queued/running idempotency, terminal row
/// recycling, stale-attempt fencing, and both parent-cascade outcomes without
/// relying on SQLite-only triggers.
async fn exercise_merge_queue_parent_delete_contract(
    db: &DatabaseConnection,
    pr_delete_repo_id: i64,
    repo_delete_repo_id: i64,
    actor_id: i64,
) {
    let pr = portable_merge_queue_pr(db, pr_delete_repo_id, actor_id, 1).await;
    let first =
        rg_db::ops::merge_queue_ops::enqueue(db, pr_delete_repo_id, pr.id, actor_id, "merge")
            .await
            .expect("enqueue the portable first attempt")
            .expect("the portable parents remain live");
    let queued =
        rg_db::ops::merge_queue_ops::enqueue(db, pr_delete_repo_id, pr.id, actor_id, "squash")
            .await
            .expect("repeat the portable queued enqueue")
            .expect("a backend no-op update is not absence");
    assert_eq!(queued.id, first.id);
    assert_eq!(queued.attempt_number, first.attempt_number);
    assert_eq!(
        queued.strategy, "merge",
        "a live enqueue keeps its strategy"
    );

    assert!(
        rg_db::ops::merge_queue_ops::claim(db, first.id, first.attempt_number)
            .await
            .expect("claim the portable queue attempt")
    );
    let running =
        rg_db::ops::merge_queue_ops::enqueue(db, pr_delete_repo_id, pr.id, actor_id, "rebase")
            .await
            .expect("repeat the portable running enqueue")
            .expect("a running no-op update is not absence");
    assert_eq!(running.status, "running");
    assert_eq!(running.attempt_number, first.attempt_number);

    assert!(rg_db::ops::merge_queue_ops::finish(
        db,
        first.id,
        first.attempt_number,
        "failed",
        Some("portable fixture failure".to_string()),
    )
    .await
    .expect("finish the portable first attempt"));
    let recycled =
        rg_db::ops::merge_queue_ops::enqueue(db, pr_delete_repo_id, pr.id, actor_id, "rebase")
            .await
            .expect("recycle the portable terminal attempt")
            .expect("the portable parents remain live");
    assert_eq!(recycled.id, first.id);
    assert_eq!(recycled.attempt_number, first.attempt_number + 1);
    assert_eq!(recycled.status, "queued");
    assert_eq!(recycled.strategy, "rebase");
    assert!(
        !rg_db::ops::merge_queue_ops::claim(db, first.id, first.attempt_number)
            .await
            .expect("refuse the stale portable attempt"),
        "recycling weakened the attempt fence"
    );

    rg_db::entities::pull_request::Entity::delete_by_id(pr.id)
        .exec(db)
        .await
        .expect("delete the portable pull request");
    assert!(
        rg_db::ops::merge_queue_ops::adopt_existing(
            db,
            recycled,
            actor_id,
            "merge",
            chrono::Utc::now(),
        )
        .await
        .expect("PR deletion is an enqueue outcome, not a database error")
        .is_none(),
        "a stale portable live snapshot survived its PR cascade"
    );

    let terminal_pr = portable_merge_queue_pr(db, repo_delete_repo_id, actor_id, 2).await;
    let terminal = rg_db::ops::merge_queue_ops::enqueue(
        db,
        repo_delete_repo_id,
        terminal_pr.id,
        actor_id,
        "merge",
    )
    .await
    .expect("enqueue the portable terminal fixture")
    .expect("the portable terminal parents remain live");
    assert!(rg_db::ops::merge_queue_ops::finish(
        db,
        terminal.id,
        terminal.attempt_number,
        "failed",
        Some("portable terminal fixture".to_string()),
    )
    .await
    .expect("finish the portable terminal fixture"));
    let terminal = rg_db::ops::merge_queue_ops::find_by_pr(db, terminal_pr.id)
        .await
        .expect("read the portable terminal snapshot")
        .expect("the portable terminal snapshot exists");
    rg_db::entities::repository::Entity::delete_by_id(repo_delete_repo_id)
        .exec(db)
        .await
        .expect("delete the portable queue repository");
    assert!(
        rg_db::ops::merge_queue_ops::adopt_existing(
            db,
            terminal,
            actor_id,
            "squash",
            chrono::Utc::now(),
        )
        .await
        .expect("repository deletion is an enqueue outcome, not a database error")
        .is_none(),
        "a stale portable terminal snapshot survived its repository cascade"
    );
    assert!(rg_db::ops::merge_queue_ops::find_by_pr(db, terminal_pr.id)
        .await
        .expect("look for a resurrected portable queue entry")
        .is_none());
}

/// The three backend-sensitive outcomes of the cache publication protocol:
/// conflict updates stay whole, stale readers cannot repoint/delete a newer
/// publication, and a real parent deletion is never retried into resurrection.
async fn exercise_ci_cache_eviction_contract(db: &DatabaseConnection, repo_id: i64) {
    let key = "portable-cache-eviction";
    let old = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        db,
        repo_id,
        key,
        "portable-old.tar",
        3,
        Some("portable-old-sha"),
        7,
    )
    .await
    .expect("publish the initial portable cache entry");
    let fresh = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        db,
        repo_id,
        key,
        "portable-fresh.tar",
        5,
        Some("portable-fresh-sha"),
        7,
    )
    .await
    .expect("replace the portable cache entry");
    assert_eq!(
        fresh.id, old.id,
        "the conflict update replaced the row identity"
    );
    assert_eq!(fresh.file_path, "portable-fresh.tar");
    assert_eq!(fresh.size, 5);
    assert_eq!(fresh.sha256.as_deref(), Some("portable-fresh-sha"));

    assert!(
        !rg_db::ops::ci_retention_ops::refresh_cache_entry(db, &old, 7)
            .await
            .expect("classify a stale portable cache refresh"),
        "a stale reader pointed the row back at the old publication",
    );

    // Prove that publication identity — not a currently-fresh expiry — keeps a
    // stale eviction from deleting the replacement on every supported backend.
    let mut expired_fresh: rg_db::entities::ci_cache_entry::ActiveModel = fresh.clone().into();
    expired_fresh.expires_at = Set(chrono::Utc::now() - chrono::Duration::days(1));
    let expired_fresh = expired_fresh
        .update(db)
        .await
        .expect("expire the fresh portable cache entry");
    assert!(
        !rg_db::ops::ci_retention_ops::delete_cache_entry_if_expired(db, &old)
            .await
            .expect("classify a stale portable cache eviction"),
        "a stale eviction deleted the fresh publication",
    );
    let current = rg_db::ops::ci_retention_ops::find_cache_entry(db, repo_id, key)
        .await
        .expect("read the fresh portable cache entry")
        .expect("the fresh portable cache entry survives");
    assert_eq!(current.file_path, "portable-fresh.tar");

    assert!(
        rg_db::ops::ci_retention_ops::refresh_cache_entry(db, &expired_fresh, 7)
            .await
            .expect("refresh the exact portable publication"),
        "the exact publication was not refreshed",
    );
    assert!(
        !rg_db::ops::ci_retention_ops::delete_cache_entry_if_expired(db, &expired_fresh)
            .await
            .expect("classify the stale post-refresh eviction"),
        "an expired snapshot deleted the same publication after it was refreshed",
    );

    let current = rg_db::ops::ci_retention_ops::find_cache_entry(db, repo_id, key)
        .await
        .expect("read the refreshed portable cache entry")
        .expect("the refreshed portable cache entry survives");
    let mut expired_again: rg_db::entities::ci_cache_entry::ActiveModel = current.into();
    expired_again.expires_at = Set(chrono::Utc::now() - chrono::Duration::days(1));
    let expired_again = expired_again
        .update(db)
        .await
        .expect("expire the portable cache entry again");
    assert!(
        rg_db::ops::ci_retention_ops::delete_cache_entry_if_expired(db, &expired_again)
            .await
            .expect("delete the exact expired portable publication"),
        "the exact expired publication was not deleted",
    );

    rg_db::ops::ci_retention_ops::upsert_cache_entry(
        db,
        repo_id,
        key,
        "portable-after-eviction.tar",
        7,
        Some("portable-after-eviction-sha"),
        7,
    )
    .await
    .expect("recreate the cache entry after ordinary eviction");
    rg_db::entities::repository::Entity::delete_by_id(repo_id)
        .exec(db)
        .await
        .expect("delete the portable cache repository");
    assert!(
        rg_db::ops::ci_retention_ops::upsert_cache_entry(
            db,
            repo_id,
            key,
            "portable-must-not-resurrect.tar",
            11,
            Some("portable-must-not-resurrect-sha"),
            7,
        )
        .await
        .is_err(),
        "cache upsert recreated a row after repository cascade",
    );
    assert!(
        rg_db::ops::ci_retention_ops::find_cache_entry(db, repo_id, key)
            .await
            .expect("look for a resurrected portable cache entry")
            .is_none(),
        "repository cascade left a cache entry behind",
    );
}

async fn exercise_oauth_account_touch_contract(
    db: &DatabaseConnection,
    user_id: i64,
    suffix: &str,
) {
    let provider = format!("oauth-touch-{suffix}");
    let created = rg_db::ops::oauth_account_ops::link(
        db,
        user_id,
        &provider,
        "portable-subject",
        "portable-user",
        "portable-user@example.invalid",
    )
    .await
    .expect("create the smoke-test OAuth account")
    .expect("the first link remains present");

    let repeated = rg_db::ops::oauth_account_ops::link(
        db,
        user_id,
        &provider,
        "portable-subject",
        "portable-user",
        "portable-user@example.invalid",
    )
    .await
    .expect("converge with the existing smoke-test OAuth account")
    .expect("the existing link remains present");
    assert_eq!(repeated.id, created.id);

    let touched = rg_db::ops::oauth_account_ops::touch_existing(db, created.id)
        .await
        .expect("touch the existing smoke-test OAuth account")
        .expect("the touched OAuth account remains present");
    assert_eq!(touched.id, created.id);

    assert!(
        rg_db::ops::oauth_account_ops::delete_by_id(db, created.id, user_id)
            .await
            .expect("delete the smoke-test OAuth account")
    );
    assert!(
        rg_db::ops::oauth_account_ops::touch_existing(db, created.id)
            .await
            .expect("a deleted OAuth account is an outcome, not a database error")
            .is_none(),
        "the losing touch must not recreate the deleted external identity"
    );
}

async fn exercise_notification_read_contract(db: &DatabaseConnection, user_id: i64) {
    let single = rg_db::ops::notification_ops::create_notification(
        db,
        user_id,
        "smoke",
        "portable single read",
        None,
        None,
    )
    .await
    .expect("create the single-read smoke notification");
    rg_db::ops::notification_ops::create_notification(
        db,
        user_id,
        "smoke",
        "portable batch read",
        None,
        None,
    )
    .await
    .expect("create the batch-read smoke notification");

    assert!(
        rg_db::ops::notification_ops::mark_notification_read_for_user(db, single.id, user_id)
            .await
            .expect("mark one notification read")
    );
    assert!(
        rg_db::ops::notification_ops::mark_notification_read_for_user(db, single.id, user_id)
            .await
            .expect("repeat an unchanged notification read update"),
        "a MySQL zero-change result is still a present notification"
    );
    assert_eq!(
        rg_db::ops::notification_ops::mark_all_read(db, user_id)
            .await
            .expect("mark the remaining unread notification"),
        1
    );
    assert_eq!(
        rg_db::ops::notification_ops::mark_all_read(db, user_id)
            .await
            .expect("repeat the idempotent batch update"),
        0
    );

    assert!(
        rg_db::ops::notification_ops::delete_notification_for_user(db, single.id, user_id)
            .await
            .expect("delete the single-read smoke notification")
    );
    assert!(
        !rg_db::ops::notification_ops::mark_notification_read_for_user(db, single.id, user_id)
            .await
            .expect("a deleted notification is an outcome, not a database error")
    );
}

async fn exercise_ldap_identity_sync_contract(db: &DatabaseConnection, suffix: &str) {
    let provider_name = format!("LDAP Sync Smoke {suffix}");
    let provider_slug = format!("ldap-sync-{suffix}");
    let provider = rg_db::ops::sso_provider_ops::create(
        db,
        rg_db::ops::sso_provider_ops::SsoProviderInput {
            name: &provider_name,
            slug: &provider_slug,
            provider_type: "ldap",
            enabled: true,
            auto_provision: true,
            ..Default::default()
        },
    )
    .await
    .expect("create the LDAP sync provider");
    let username = format!("ldapsync{suffix}");
    let email = format!("{username}@example.invalid");
    let user = rg_db::ops::user_ops::create_ldap_user(
        db,
        provider.id,
        &username,
        &email,
        Some("Original Directory Name"),
        Some(&username),
    )
    .await
    .expect("create the LDAP sync account");

    let synced = rg_db::ops::user_ops::sync_ldap_identity(
        db,
        user.id,
        provider.id,
        Some("Refreshed Directory Name"),
        Some(&username),
    )
    .await
    .expect("sync the live LDAP identity")
    .expect("the live LDAP identity remains present");
    assert_eq!(
        synced.display_name.as_deref(),
        Some("Refreshed Directory Name")
    );
    assert_eq!(synced.email, email, "LDAP sync rewrote authoritative email");

    assert!(rg_db::ops::user_ops::begin_user_retirement(db, user.id)
        .await
        .expect("claim the LDAP account for retirement"));
    assert!(
        rg_db::ops::user_ops::sync_ldap_identity(
            db,
            user.id,
            provider.id,
            Some("Too Late"),
            Some(&username),
        )
        .await
        .expect("retirement is an outcome, not an LDAP database error")
        .is_none(),
        "LDAP sync accepted an account already claimed for deletion"
    );
    let retiring = rg_db::ops::user_ops::find_by_id(db, user.id)
        .await
        .expect("read the retiring LDAP account")
        .expect("retirement keeps the account row until storage is retired");
    assert_eq!(
        retiring.display_name.as_deref(),
        Some("Refreshed Directory Name"),
        "the refused sync changed a retiring account"
    );

    assert!(rg_db::ops::user_ops::delete_by_id(db, user.id)
        .await
        .expect("finish deleting the LDAP sync account"));
    assert!(
        rg_db::ops::user_ops::sync_ldap_identity(
            db,
            user.id,
            provider.id,
            Some("Still Too Late"),
            Some(&username),
        )
        .await
        .expect("physical deletion is an outcome, not an LDAP database error")
        .is_none(),
        "LDAP sync recreated a physically deleted account"
    );
    assert!(rg_db::ops::sso_provider_ops::delete_by_id(db, provider.id)
        .await
        .expect("delete the LDAP sync provider"));
}

async fn exercise_mfa_account_delete_contract(db: &DatabaseConnection, suffix: &str) {
    let ordinary = rg_db::ops::user_ops::create_user(
        db,
        &format!("mfaordinary{suffix}"),
        &format!("mfaordinary{suffix}@example.invalid"),
        "unused",
        "Portable MFA Lifecycle",
    )
    .await
    .expect("create the ordinary MFA lifecycle account");
    let stored = rg_db::ops::user_ops::update_totp_secret(db, ordinary.id, "ciphertext")
        .await
        .expect("store the smoke-test TOTP secret")
        .expect("the ordinary MFA account remains open");
    assert_eq!(stored.totp_secret.as_deref(), Some("ciphertext"));
    let codes = vec!["portable-one".to_string(), "portable-two".to_string()];
    let enabled = rg_db::ops::user_ops::enable_mfa_with_backup_codes(db, ordinary.id, &codes)
        .await
        .expect("enable MFA and publish backup codes")
        .expect("the ordinary MFA account remains open");
    assert!(enabled.mfa_enabled);
    let disabled = rg_db::ops::user_ops::disable_mfa(db, ordinary.id)
        .await
        .expect("disable MFA and revoke backup codes")
        .expect("the ordinary MFA account remains open");
    assert!(!disabled.mfa_enabled);
    assert_eq!(disabled.totp_secret, None);
    assert!(rg_db::ops::mfa_backup_code_ops::list_codes(db, ordinary.id)
        .await
        .expect("list codes after ordinary MFA disable")
        .is_empty());

    let setup_target = rg_db::ops::user_ops::create_user(
        db,
        &format!("mfasetupgone{suffix}"),
        &format!("mfasetupgone{suffix}@example.invalid"),
        "unused",
        "Portable Retiring MFA Setup",
    )
    .await
    .expect("create the retiring MFA setup account");
    assert!(
        rg_db::ops::user_ops::begin_user_retirement(db, setup_target.id)
            .await
            .expect("claim the MFA setup account for retirement")
    );
    assert!(
        rg_db::ops::user_ops::update_totp_secret(db, setup_target.id, "too-late")
            .await
            .expect("retirement is an outcome, not a TOTP database error")
            .is_none(),
        "TOTP setup accepted an account already claimed for deletion"
    );
    assert!(rg_db::ops::user_ops::delete_by_id(db, setup_target.id)
        .await
        .expect("finish deleting the MFA setup account"));
    assert!(
        rg_db::ops::user_ops::update_totp_secret(db, setup_target.id, "still-too-late")
            .await
            .expect("physical deletion is an outcome, not a TOTP database error")
            .is_none()
    );

    let enable_target = rg_db::ops::user_ops::create_user(
        db,
        &format!("mfaenablegone{suffix}"),
        &format!("mfaenablegone{suffix}@example.invalid"),
        "unused",
        "Portable Retiring MFA Enable",
    )
    .await
    .expect("create the retiring MFA enable account");
    assert!(
        rg_db::ops::user_ops::begin_user_retirement(db, enable_target.id)
            .await
            .expect("claim the MFA enable account for retirement")
    );
    assert!(
        rg_db::ops::user_ops::enable_mfa_with_backup_codes(db, enable_target.id, &codes)
            .await
            .expect("retirement is an outcome, not an MFA enable database error")
            .is_none(),
        "MFA enable accepted an account already claimed for deletion"
    );
    assert!(
        rg_db::ops::mfa_backup_code_ops::list_codes(db, enable_target.id)
            .await
            .expect("list codes after refused MFA enable")
            .is_empty(),
        "the refused MFA enable published recovery credentials"
    );

    let disable_target = rg_db::ops::user_ops::create_user(
        db,
        &format!("mfadisablegone{suffix}"),
        &format!("mfadisablegone{suffix}@example.invalid"),
        "unused",
        "Portable Retiring MFA Disable",
    )
    .await
    .expect("create the retiring MFA disable account");
    rg_db::ops::user_ops::enable_mfa_with_backup_codes(db, disable_target.id, &codes)
        .await
        .expect("seed the MFA disable account")
        .expect("the MFA disable account is open before retirement");
    assert!(
        rg_db::ops::user_ops::begin_user_retirement(db, disable_target.id)
            .await
            .expect("claim the MFA disable account for retirement")
    );
    assert!(
        rg_db::ops::user_ops::disable_mfa(db, disable_target.id)
            .await
            .expect("retirement is an outcome, not an MFA disable database error")
            .is_none(),
        "MFA disable accepted an account already claimed for deletion"
    );
    let retiring = rg_db::ops::user_ops::find_by_id(db, disable_target.id)
        .await
        .expect("read the retiring MFA disable account")
        .expect("retirement keeps the account row until storage is retired");
    assert!(
        retiring.mfa_enabled,
        "the refused MFA disable changed the flag"
    );
    assert_eq!(
        rg_db::ops::mfa_backup_code_ops::list_codes(db, disable_target.id)
            .await
            .expect("list codes after refused MFA disable")
            .len(),
        codes.len(),
        "the refused MFA disable partially revoked recovery credentials"
    );
    assert!(rg_db::ops::user_ops::delete_by_id(db, disable_target.id)
        .await
        .expect("finish deleting the MFA disable account"));
    assert!(rg_db::ops::user_ops::disable_mfa(db, disable_target.id)
        .await
        .expect("physical deletion is an outcome, not an MFA disable database error")
        .is_none());
    assert!(
        rg_db::ops::mfa_backup_code_ops::list_codes(db, disable_target.id)
            .await
            .expect("list codes after account deletion")
            .is_empty(),
        "backup credentials outlived the deleted account"
    );
}

async fn exercise_password_reset_account_delete_contract(db: &DatabaseConnection, suffix: &str) {
    async fn issue(
        db: &DatabaseConnection,
        user_id: i64,
        raw: &str,
    ) -> rg_db::entities::password_reset_token::Model {
        use sha2::Digest;

        let hash = hex::encode(sha2::Sha256::digest(raw.as_bytes()));
        rg_db::ops::password_reset_token_ops::create(
            db,
            user_id,
            &hash,
            chrono::Utc::now() + chrono::Duration::minutes(15),
        )
        .await
        .expect("issue portable password-reset token")
    }

    let ordinary = rg_db::ops::user_ops::create_user(
        db,
        &format!("resetordinary{suffix}"),
        &format!("resetordinary{suffix}@example.invalid"),
        "old-portable-hash",
        "Portable Password Reset",
    )
    .await
    .expect("create ordinary password-reset account");
    let ordinary_token = issue(db, ordinary.id, &format!("ordinary-reset-{suffix}")).await;
    let completed = rg_db::ops::password_reset_token_ops::complete_password_reset(
        db,
        ordinary_token.id,
        ordinary.id,
        "new-portable-hash",
    )
    .await
    .expect("complete portable password reset")
    .expect("ordinary password reset was refused");
    assert_eq!(completed.password_hash, "new-portable-hash");
    assert_eq!(completed.session_version, ordinary.session_version + 1);
    assert!(
        rg_db::ops::password_reset_token_ops::find_by_hash(db, &ordinary_token.token_hash)
            .await
            .expect("read completed portable reset token")
            .is_none(),
        "completed reset left a sibling token alive"
    );

    let retiring = rg_db::ops::user_ops::create_user(
        db,
        &format!("resetretiring{suffix}"),
        &format!("resetretiring{suffix}@example.invalid"),
        "retiring-old-hash",
        "Portable Retiring Password Reset",
    )
    .await
    .expect("create retiring password-reset account");
    let retiring_token = issue(db, retiring.id, &format!("retiring-reset-{suffix}")).await;
    assert!(rg_db::ops::user_ops::begin_user_retirement(db, retiring.id)
        .await
        .expect("claim portable password-reset account for retirement"));
    assert!(
        rg_db::ops::password_reset_token_ops::complete_password_reset(
            db,
            retiring_token.id,
            retiring.id,
            "must-not-land",
        )
        .await
        .expect("retirement is an outcome, not a password-reset database error")
        .is_none(),
        "password reset accepted a retiring account"
    );
    let stored = rg_db::ops::user_ops::find_by_id(db, retiring.id)
        .await
        .expect("read portable retiring reset account")
        .expect("retirement keeps the account row until storage is retired");
    assert_eq!(stored.password_hash, "retiring-old-hash");
    assert_eq!(stored.session_version, retiring.session_version);
    let stored_token =
        rg_db::ops::password_reset_token_ops::find_by_hash(db, &retiring_token.token_hash)
            .await
            .expect("read refused portable reset token")
            .expect("refused portable reset deleted its token");
    assert!(!stored_token.used, "refused portable reset spent its token");

    assert!(rg_db::ops::user_ops::delete_by_id(db, retiring.id)
        .await
        .expect("finish portable password-reset account deletion"));
    assert!(
        rg_db::ops::password_reset_token_ops::complete_password_reset(
            db,
            retiring_token.id,
            retiring.id,
            "still-must-not-land",
        )
        .await
        .expect("physical deletion is an outcome, not a password-reset database error")
        .is_none(),
        "password reset accepted a physically deleted account"
    );
}

async fn exercise_package_version_yank_contract(
    db: &DatabaseConnection,
    repo_id: i64,
    actor_id: i64,
    suffix: &str,
) {
    let registry = rg_db::ops::package_registry_ops::find_or_create(db, repo_id, "cargo")
        .await
        .expect("create the smoke-test package registry");
    let package = rg_db::ops::package_ops::create(
        db,
        registry.id,
        actor_id,
        &format!("yank-smoke-{suffix}"),
        None,
        None,
        None,
    )
    .await
    .expect("create the smoke-test package");
    let version = rg_db::ops::package_version_ops::create(
        db,
        package.id,
        "1.0.0",
        None,
        Some("1.0.0"),
        None,
        0,
        None,
        Some(actor_id),
    )
    .await
    .expect("create the smoke-test package version");

    let yanked = rg_db::ops::package_version_ops::set_yanked(db, version.id, true)
        .await
        .expect("yank the smoke-test package version")
        .expect("the smoke-test package version still exists");
    assert!(yanked.is_yanked);
    let unchanged = rg_db::ops::package_version_ops::set_yanked(db, version.id, true)
        .await
        .expect("repeat an unchanged package version yank")
        .expect("a MySQL zero-change result is not a missing package version");
    assert!(unchanged.is_yanked);
    let unyanked = rg_db::ops::package_version_ops::set_yanked(db, version.id, false)
        .await
        .expect("unyank the smoke-test package version")
        .expect("the smoke-test package version still exists");
    assert!(!unyanked.is_yanked);

    assert_eq!(
        rg_db::ops::package_version_ops::delete_by_id(db, version.id)
            .await
            .expect("delete the smoke-test package version"),
        1
    );
    assert!(
        rg_db::ops::package_version_ops::set_yanked(db, version.id, true)
            .await
            .expect("a deleted package version is an outcome, not a database error")
            .is_none(),
        "the losing yank must not recreate the deleted package version"
    );
}

async fn exercise_release_asset_mutation_contract(
    db: &DatabaseConnection,
    repo_id: i64,
    actor_id: i64,
    suffix: &str,
) {
    let now = chrono::Utc::now();
    let release = rg_db::ops::release_ops::create(
        db,
        rg_db::entities::release::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            tag_name: Set(format!("asset-smoke-{suffix}")),
            target_commitish: Set("main".to_string()),
            title: Set("Release asset mutation smoke".to_string()),
            body: Set(None),
            is_draft: Set(false),
            is_prerelease: Set(false),
            author_id: Set(Some(actor_id)),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("create release for the asset mutation smoke");
    let asset = rg_db::ops::release_ops::create_asset(
        db,
        rg_db::entities::release_asset::ActiveModel {
            id: NotSet,
            release_id: Set(release.id),
            filename: Set("portable.bin".to_string()),
            size: Set(8),
            content_type: Set("application/octet-stream".to_string()),
            download_count: Set(0),
            uploader_id: Set(Some(actor_id)),
            created_at: Set(now),
            sha256: Set(None),
            attestation: Set(None),
        },
    )
    .await
    .expect("create release asset for the mutation smoke");

    let envelope = r#"{"payloadType":"application/vnd.in-toto+json"}"#.to_string();
    let signed =
        rg_db::ops::release_ops::set_asset_attestation(db, asset.id, Some(envelope.clone()))
            .await
            .expect("set the smoke-test asset attestation")
            .expect("the smoke-test release asset still exists");
    assert_eq!(signed.attestation.as_deref(), Some(envelope.as_str()));
    let unchanged =
        rg_db::ops::release_ops::set_asset_attestation(db, asset.id, Some(envelope.clone()))
            .await
            .expect("repeat an unchanged asset attestation update")
            .expect("a MySQL zero-change result is not a missing release asset");
    assert_eq!(unchanged.id, asset.id);

    let (first, second) = tokio::join!(
        rg_db::ops::release_ops::increment_download_count(db, asset.id),
        rg_db::ops::release_ops::increment_download_count(db, asset.id),
    );
    assert!(first.expect("first concurrent asset download increment"));
    assert!(second.expect("second concurrent asset download increment"));
    let counted = rg_db::ops::release_ops::find_asset_by_id(db, asset.id)
        .await
        .expect("read the counted release asset")
        .expect("the counted release asset still exists");
    assert_eq!(counted.download_count, 2);

    assert!(rg_db::ops::release_ops::delete_asset_by_id(db, asset.id)
        .await
        .expect("delete the smoke-test release asset"));
    assert!(
        rg_db::ops::release_ops::set_asset_attestation(db, asset.id, Some(envelope))
            .await
            .expect("a deleted asset is an outcome, not an attestation database error")
            .is_none()
    );
    assert!(
        !rg_db::ops::release_ops::increment_download_count(db, asset.id)
            .await
            .expect("a deleted asset is an outcome, not a counter database error")
    );
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn ci_secret_conditional_update_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("secretpatch{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "CI Secret Patch Smoke",
    )
    .await
    .expect("create CI secret update owner");
    let repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("secretpatchrepo{suffix}")),
    )
    .await
    .expect("create CI secret update repository");

    exercise_ci_secret_update_contract(&db, repo.id, owner.id).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn commit_status_parent_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("statuspatch{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "Commit Status Patch Smoke",
    )
    .await
    .expect("create commit-status update owner");
    let repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("statuspatchrepo{suffix}")),
    )
    .await
    .expect("create commit-status update repository");

    exercise_commit_status_parent_delete_contract(&db, repo.id, owner.id).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn retention_and_watch_parent_deletes_are_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let owner_name = format!("policywatchowner{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &owner_name,
        &format!("{owner_name}@example.invalid"),
        "unused",
        "Policy Watch Parent Owner",
    )
    .await
    .expect("create policy/watch repository owner");
    let watcher_name = format!("policywatcher{suffix}");
    let watcher = rg_db::ops::user_ops::create_user(
        &db,
        &watcher_name,
        &format!("{watcher_name}@example.invalid"),
        "unused",
        "Policy Watch Parent Watcher",
    )
    .await
    .expect("create portable watcher");
    let deleted_repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("policywatchgone{suffix}")),
    )
    .await
    .expect("create repository for the portable repository cascade");
    let surviving_repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("policywatchlive{suffix}")),
    )
    .await
    .expect("create repository for the portable user cascade");

    exercise_retention_watch_parent_delete_contract(
        &db,
        deleted_repo.id,
        surviving_repo.id,
        watcher.id,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn merge_queue_parent_deletes_are_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let owner_name = format!("queueparent{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &owner_name,
        &format!("{owner_name}@example.invalid"),
        "unused",
        "Merge Queue Parent Owner",
    )
    .await
    .expect("create portable merge-queue owner");
    let pr_delete_repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("queueprgone{suffix}")),
    )
    .await
    .expect("create repository for the portable PR cascade");
    let repo_delete_repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("queuerepogone{suffix}")),
    )
    .await
    .expect("create repository for the portable repository cascade");

    exercise_merge_queue_parent_delete_contract(
        &db,
        pr_delete_repo.id,
        repo_delete_repo.id,
        owner.id,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn ci_cache_eviction_upsert_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("cacheevict{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "CI Cache Eviction Smoke",
    )
    .await
    .expect("create portable cache owner");
    let repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("cacheevictrepo{suffix}")),
    )
    .await
    .expect("create portable cache repository");

    exercise_ci_cache_eviction_contract(&db, repo.id).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn mfa_lifecycle_account_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    exercise_mfa_account_delete_contract(&db, &suffix[..10]).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn admin_user_mutations_account_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    exercise_admin_user_mutation_contract(&db, &suffix[..10]).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn login_finalization_account_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    exercise_login_finalization_contract(&db, &suffix[..10]).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn standing_credential_finalization_account_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    exercise_standing_credential_finalization_contract(&db, &suffix[..10]).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn ci_job_token_finalization_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    exercise_ci_job_token_finalization_contract(&db, &suffix[..10]).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn ci_graph_transitions_are_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    exercise_ci_graph_transition_contract(&db, &suffix[..10]).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn password_reset_account_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    exercise_password_reset_account_delete_contract(&db, &suffix[..10]).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn ldap_identity_sync_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    exercise_ldap_identity_sync_contract(&db, &suffix[..10]).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn oauth_account_touch_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("oauthtouch{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "OAuth Touch Smoke",
    )
    .await
    .expect("create OAuth touch owner");

    exercise_oauth_account_touch_contract(&db, owner.id, suffix).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn notification_read_mutations_are_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("notificationread{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "Notification Read Smoke",
    )
    .await
    .expect("create notification read owner");

    exercise_notification_read_contract(&db, owner.id).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn organization_patch_concurrent_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("orgpatch{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "Organization Patch Smoke",
    )
    .await
    .expect("create organization update owner");
    let org = rg_db::ops::org_ops::create_org(
        &db,
        &format!("{username}org"),
        None,
        None,
        owner.id,
        "public",
    )
    .await
    .expect("create organization update fixture");

    exercise_organization_update_contract(&db, org, owner.id, suffix).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn package_version_yank_concurrent_delete_is_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("pkgyank{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "Package Yank Smoke",
    )
    .await
    .expect("create package yank owner");
    let repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("pkgyankrepo{suffix}")),
    )
    .await
    .expect("create package yank repository");

    exercise_package_version_yank_contract(&db, repo.id, owner.id, suffix).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn release_asset_mutations_are_portable() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("assetmutation{suffix}");
    let owner = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "Release Asset Mutation Smoke",
    )
    .await
    .expect("create release asset mutation owner");
    let repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(owner.id, None, &format!("assetmutationrepo{suffix}")),
    )
    .await
    .expect("create release asset mutation repository");

    exercise_release_asset_mutation_contract(&db, repo.id, owner.id, suffix).await;
}

#[tokio::test]
#[ignore = "requires FORGEKEEP_TEST_DATABASE_URL pointing at a disposable database"]
async fn migrations_crud_counters_and_fts_work_on_server_database() {
    let database_url = std::env::var("FORGEKEEP_TEST_DATABASE_URL")
        .expect("FORGEKEEP_TEST_DATABASE_URL must be set");
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to test database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let username = format!("dbsmoke{suffix}");
    let repo_name = format!("crossbackendrepo{suffix}");
    let repo_update_term = format!("repoupdatedneedle{suffix}");
    let repo_restore_term = format!("reporestoredneedle{suffix}");
    let wiki_term = format!("crossbackendneedle{suffix}");
    let first_wiki_edit_term = format!("firstwikiedit{suffix}");
    let second_wiki_edit_term = format!("secondwikiedit{suffix}");
    let first_wiki_edit_content = format!("first concurrent server edit {first_wiki_edit_term}");
    let second_wiki_edit_content = format!("second concurrent server edit {second_wiki_edit_term}");

    let user = rg_db::ops::user_ops::create_user(
        &db,
        &username,
        &format!("{username}@example.invalid"),
        "unused",
        "Database Smoke",
    )
    .await
    .expect("create user");

    let (first, second, third, fourth, fifth) = tokio::join!(
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, user.id, 5),
    );
    for result in [first, second, third, fourth, fifth] {
        result.expect("atomically record concurrent failed login");
    }
    let locked_user = rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .expect("read locked user")
        .expect("locked user exists");
    assert_eq!(locked_user.login_attempts, 5);
    assert!(locked_user.locked_until.is_some());
    rg_db::ops::user_ops::reset_login_failures_if_open(&db, user.id)
        .await
        .expect("reset failed logins")
        .expect("locked user remains open");

    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set(repo_name.clone()),
            description: Set(Some("cross backend repository search".to_string())),
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
    .expect("create repository");
    assert_eq!(
        repo_fts_snapshot(&db, repo.id).await,
        Some((
            repo_name.clone(),
            "cross backend repository search".to_string()
        )),
        "the repository INSERT trigger did not publish the source snapshot"
    );

    // card_61e4278d15e3: grouped CI publication is arbitrated by a durable
    // `(repo_id, group_name)` row. Prove the server backends lock that exact key
    // rather than the whole repository, the whole lock table, or this process.
    // SQLite has a single writer by design and is covered by rg-ci's full
    // concurrent trigger tests; PostgreSQL/MySQL are where row-level scope must
    // be demonstrated explicitly.
    let other_repo = rg_db::ops::repo_ops::create(
        &db,
        namespace_repo(user.id, None, &format!("lockscope{suffix}")),
    )
    .await
    .expect("create second repository for concurrency-lock scope");
    let held = db.begin().await.expect("begin held group transaction");
    rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(
        &held,
        repo.id,
        "deploy-production",
    )
    .await
    .expect("hold first CI concurrency group");

    let different_group_db = db.clone();
    let different_group_repo = repo.id;
    let different_group = tokio::spawn(async move {
        let tx = different_group_db
            .begin()
            .await
            .expect("begin different-group transaction");
        rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(
            &tx,
            different_group_repo,
            "nightly-audit",
        )
        .await
        .expect("different group must not wait for the held group");
        tx.rollback().await.expect("rollback different-group probe");
    });
    let different_repo_db = db.clone();
    let different_repo_id = other_repo.id;
    let different_repo = tokio::spawn(async move {
        let tx = different_repo_db
            .begin()
            .await
            .expect("begin different-repository transaction");
        rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(
            &tx,
            different_repo_id,
            "deploy-production",
        )
        .await
        .expect("same group name in another repository must not wait");
        tx.rollback()
            .await
            .expect("rollback different-repository probe");
    });
    let (different_group_result, different_repo_result) =
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(different_group, different_repo)
        })
        .await
        .expect("unrelated CI concurrency keys serialized globally");
    different_group_result.expect("different-group probe task panicked");
    different_repo_result.expect("different-repository probe task panicked");

    let same_group_db = db.clone();
    let same_group_repo = repo.id;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let mut same_group = tokio::spawn(async move {
        let tx = same_group_db
            .begin()
            .await
            .expect("begin same-group transaction");
        started_tx.send(()).expect("announce same-group attempt");
        rg_db::ops::pipeline_ops::acquire_pipeline_concurrency_lock(
            &tx,
            same_group_repo,
            "deploy-production",
        )
        .await
        .expect("same group acquires after its predecessor commits");
        tx.rollback().await.expect("rollback same-group probe");
    });
    started_rx.await.expect("same-group probe started");
    assert_probe_stays_blocked(
        &mut same_group,
        "the same repository/group key was not held until transaction commit",
    )
    .await;
    held.commit().await.expect("release held CI group");
    tokio::time::timeout(std::time::Duration::from_secs(3), same_group)
        .await
        .expect("same group did not resume after commit")
        .expect("same-group probe task panicked");

    // card_04db226ae9b3: the same normalized grant writer and lock sequence must
    // behave identically on PostgreSQL and MySQL. CI invokes this ignored smoke
    // once per disposable server database; SQLite has a dedicated deterministic
    // integration test in rg-db.
    let branch = rg_db::ops::protected_branch_ops::create_with_push_grants(
        &db,
        rg_db::entities::protected_branch::ActiveModel {
            repo_id: Set(repo.id),
            branch_name: Set(format!("grant-{suffix}")),
            require_pr: Set(true),
            require_status_check: Set(false),
            required_status_checks: Set(None),
            require_approval: Set(false),
            required_approvals: Set(None),
            allow_force_push: Set(false),
            require_signed_commits: Set(false),
            allowed_push_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user.id]),
    )
    .await
    .expect("create cross-backend branch grant");
    let tag = rg_db::ops::protected_tag_ops::create_with_push_grants(
        &db,
        rg_db::entities::protected_tag::ActiveModel {
            repo_id: Set(repo.id),
            pattern: Set(format!("grant-{suffix}-*")),
            allowed_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user.id]),
    )
    .await
    .expect("create cross-backend tag grant");

    let mut branch_update = branch.clone();
    branch_update.require_signed_commits = true;
    branch_update.updated_at = chrono::Utc::now();
    let branch = rg_db::ops::protected_branch_ops::update_with_push_grants(
        &db,
        branch_update,
        Some(vec![user.id]),
    )
    .await
    .expect("update cross-backend protected branch and grants")
    .expect("the cross-backend protected branch still exists");
    assert!(branch.require_signed_commits);

    let mut tag_update = tag.clone();
    tag_update.updated_at = chrono::Utc::now();
    let tag =
        rg_db::ops::protected_tag_ops::update_with_push_grants(&db, tag_update, vec![user.id])
            .await
            .expect("update cross-backend protected tag and grants")
            .expect("the cross-backend protected tag still exists");

    let doomed_branch = rg_db::ops::protected_branch_ops::create_with_push_grants(
        &db,
        rg_db::entities::protected_branch::ActiveModel {
            repo_id: Set(repo.id),
            branch_name: Set(format!("doomed-{suffix}")),
            require_pr: Set(true),
            require_status_check: Set(false),
            required_status_checks: Set(None),
            require_approval: Set(false),
            required_approvals: Set(None),
            allow_force_push: Set(false),
            require_signed_commits: Set(false),
            allowed_push_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user.id]),
    )
    .await
    .expect("create doomed cross-backend branch grant");
    assert!(
        rg_db::ops::protected_branch_ops::delete_by_id(&db, doomed_branch.id)
            .await
            .expect("delete doomed cross-backend branch")
    );
    let mut deleted_branch_update = doomed_branch;
    deleted_branch_update.updated_at = chrono::Utc::now();
    assert!(
        rg_db::ops::protected_branch_ops::update_with_push_grants(
            &db,
            deleted_branch_update,
            Some(vec![user.id]),
        )
        .await
        .expect("classify absent cross-backend protected branch")
        .is_none(),
        "an absent protected branch must be an ordinary outcome on every backend"
    );

    let doomed_tag = rg_db::ops::protected_tag_ops::create_with_push_grants(
        &db,
        rg_db::entities::protected_tag::ActiveModel {
            repo_id: Set(repo.id),
            pattern: Set(format!("doomed-{suffix}-*")),
            allowed_user_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        Some(vec![user.id]),
    )
    .await
    .expect("create doomed cross-backend tag grant");
    assert!(
        rg_db::ops::protected_tag_ops::delete_by_id(&db, doomed_tag.id)
            .await
            .expect("delete doomed cross-backend tag")
    );
    let mut deleted_tag_update = doomed_tag;
    deleted_tag_update.updated_at = chrono::Utc::now();
    assert!(
        rg_db::ops::protected_tag_ops::update_with_push_grants(
            &db,
            deleted_tag_update,
            vec![user.id],
        )
        .await
        .expect("classify absent cross-backend protected tag")
        .is_none(),
        "an absent protected tag must be an ordinary outcome on every backend"
    );

    let environment = rg_db::ops::ci_environment_ops::create_with_approvers(
        &db,
        rg_db::entities::ci_environment::ActiveModel {
            repo_id: Set(repo.id),
            name: Set(format!("grant-{suffix}")),
            protected: Set(true),
            required_approvals: Set(1),
            allowed_approver_ids: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        },
        vec![user.id],
    )
    .await
    .expect("create cross-backend environment grant");
    let environment = rg_db::ops::ci_environment_ops::update_with_approvers(
        &db,
        environment.id,
        repo.id,
        format!("grant-{suffix}-updated"),
        true,
        1,
        chrono::Utc::now(),
        vec![user.id],
    )
    .await
    .expect("update cross-backend environment and grants")
    .expect("the cross-backend environment still exists");
    assert_eq!(environment.name, format!("grant-{suffix}-updated"));
    let grant_targets = [
        rg_db::user_grants::Target::ProtectedBranch(branch.id),
        rg_db::user_grants::Target::ProtectedTag(tag.id),
        rg_db::user_grants::Target::CiEnvironment(environment.id),
    ];

    let inactive = rg_db::ops::user_ops::create_user(
        &db,
        &format!("inactive{suffix}"),
        &format!("inactive{suffix}@example.invalid"),
        "unused",
        "Inactive Grant Principal",
    )
    .await
    .expect("create inactive grant principal");
    rg_db::ops::user_ops::update_by_id(&db, inactive.id, None, None, None, Some(false))
        .await
        .expect("deactivate grant principal")
        .expect("inactive grant principal exists");
    let retiring = rg_db::ops::user_ops::create_user(
        &db,
        &format!("retiring{suffix}"),
        &format!("retiring{suffix}@example.invalid"),
        "unused",
        "Retiring Grant Principal",
    )
    .await
    .expect("create retiring grant principal");
    assert!(rg_db::ops::user_ops::find_by_id(&db, retiring.id)
        .await
        .expect("preflight retiring principal")
        .is_some());
    assert!(
        rg_db::ops::user_ops::begin_user_retirement(&db, retiring.id)
            .await
            .expect("begin grant principal retirement")
    );

    for (invalid_id, expected) in [
        (i64::MAX, format!("grant user {} does not exist", i64::MAX)),
        (
            inactive.id,
            format!("grant user {} is inactive", inactive.id),
        ),
        (
            retiring.id,
            format!("grant user {} is being retired", retiring.id),
        ),
    ] {
        for target in grant_targets {
            let transaction = db.begin().await.expect("begin invalid grant write");
            let error = rg_db::user_grants::replace(&transaction, target, Some(&[invalid_id]))
                .await
                .expect_err("cross-backend invalid principal must be rejected");
            assert_eq!(
                rg_db::user_grants::invalid_principal_message(&error).as_deref(),
                Some(expected.as_str()),
                "different cross-backend semantic for {target:?}: {error:#}"
            );
            transaction
                .rollback()
                .await
                .expect("roll back invalid cross-backend grant write");
        }
    }
    assert_eq!(
        rg_db::user_grants::load_verified(
            &db,
            grant_targets[0],
            branch.allowed_push_user_ids.as_deref(),
        )
        .await
        .expect("load cross-backend branch grants"),
        vec![user.id]
    );
    assert_eq!(
        rg_db::user_grants::load_verified(&db, grant_targets[1], tag.allowed_user_ids.as_deref(),)
            .await
            .expect("load cross-backend tag grants"),
        vec![user.id]
    );
    assert_eq!(
        rg_db::user_grants::load_verified(
            &db,
            grant_targets[2],
            environment.allowed_approver_ids.as_deref(),
        )
        .await
        .expect("load cross-backend environment grants"),
        vec![user.id]
    );

    let mut updated_repo: rg_db::entities::repository::ActiveModel = repo.clone().into();
    updated_repo.description = Set(Some(repo_update_term.clone()));
    updated_repo.updated_at = Set(chrono::Utc::now());
    let repo = updated_repo
        .update(&db)
        .await
        .expect("update repository metadata through the source row");
    assert_eq!(
        repo_fts_snapshot(&db, repo.id).await,
        Some((repo_name.clone(), repo_update_term.clone())),
        "the repository UPDATE trigger did not replace the FTS snapshot"
    );

    // card_9d3b68368396: open-PR head refresh uses the same snapshot CAS on
    // SQLite, PostgreSQL and MySQL, including the nullable state of a deleted
    // branch. The deterministic overtaken-writer interleaving lives beside the
    // primitive in rg-db; this server smoke proves the generated predicates and
    // NULL transition have the same external result on both server dialects.
    let pr_now = chrono::Utc::now();
    let initial_pr_head = "1111111111111111111111111111111111111111";
    let refreshed_pr_head = "2222222222222222222222222222222222222222";
    let recreated_pr_head = "3333333333333333333333333333333333333333";
    let head_pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            number: Set(1),
            title: Set("cross-backend head refresh".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(user.id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some(initial_pr_head.to_string())),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(pr_now),
            updated_at: Set(pr_now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("create cross-backend PR head fixture");
    let refreshed = rg_db::ops::pull_request_ops::update_open_head_sha(
        &db,
        repo.id,
        "feature",
        Some(refreshed_pr_head),
    )
    .await
    .expect("CAS-refresh the PR head");
    assert_eq!(refreshed.stale_rows, 0);
    assert_eq!(
        refreshed.open_prs[0].head_sha.as_deref(),
        Some(refreshed_pr_head)
    );
    let stale_swap = rg_db::ops::pull_request_ops::compare_and_swap_open_head_sha(
        &db,
        head_pr.id,
        Some(initial_pr_head),
        Some(recreated_pr_head),
    )
    .await
    .expect("run a stale PR-head compare-and-swap");
    assert!(
        !stale_swap,
        "a stale expected head must lose on every database backend"
    );
    let after_stale_swap = rg_db::ops::pull_request_ops::find_by_id(&db, head_pr.id)
        .await
        .expect("reload PR after stale compare-and-swap")
        .expect("cross-backend PR still exists");
    assert_eq!(
        after_stale_swap.head_sha.as_deref(),
        Some(refreshed_pr_head),
        "the losing writer must not roll the PR head back"
    );
    let deleted = rg_db::ops::pull_request_ops::update_open_head_sha(&db, repo.id, "feature", None)
        .await
        .expect("clear the PR head for a deleted branch");
    assert_eq!(deleted.stale_rows, 0);
    assert_eq!(deleted.open_prs[0].head_sha, None);
    let recreated = rg_db::ops::pull_request_ops::update_open_head_sha(
        &db,
        repo.id,
        "feature",
        Some(recreated_pr_head),
    )
    .await
    .expect("restore the PR head after branch recreation");
    assert_eq!(recreated.stale_rows, 0);
    assert_eq!(
        recreated.open_prs[0].head_sha.as_deref(),
        Some(recreated_pr_head)
    );
    assert_eq!(recreated.open_prs[0].id, head_pr.id);

    assert!(
        rg_db::ops::repo_star_ops::toggle_star(&db, user.id, repo.id)
            .await
            .expect("create star")
    );
    rg_db::ops::repo_ops::update_stars_count(&db, repo.id)
        .await
        .expect("update star counter with backend-specific placeholders");
    let counted_repo =
        rg_db::ops::repo_ops::find_personal_by_owner_and_name(&db, user.id, &repo_name)
            .await
            .expect("read repository")
            .expect("repository exists");
    assert_eq!(counted_repo.stars_count, 1);

    let mut live_fork = namespace_repo(user.id, None, &format!("forklive{suffix}"));
    live_fork.origin_repo_id = Set(Some(repo.id));
    let live_fork = rg_db::ops::repo_ops::create(&db, live_fork)
        .await
        .expect("create live fork row");

    let mut deleted_fork = namespace_repo(user.id, None, &format!("forkgone{suffix}"));
    deleted_fork.origin_repo_id = Set(Some(repo.id));
    let deleted_fork = rg_db::ops::repo_ops::create(&db, deleted_fork)
        .await
        .expect("create deleted fork row");
    rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
        &db,
        deleted_fork.id,
        chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
    )
    .await
    .expect("soft-delete one fork before refreshing the count");

    rg_db::ops::repo_ops::update_forks_count(&db, repo.id)
        .await
        .expect("refresh fork counter with backend-specific atomic SQL");
    let counted_repo = rg_db::ops::repo_ops::find_by_id(&db, repo.id)
        .await
        .expect("read source after fork count refresh")
        .expect("source repository exists");
    assert_eq!(
        counted_repo.forks_count, 1,
        "only the live fork row contributes to forks_count"
    );

    // card_957cc2683f70: deleting an account retracts its stars through
    // `repo_stars.user_id ON DELETE CASCADE`, and the repositories they sat on
    // belong to other people, so `user_ops::delete_by_id` refreshes a whole
    // inventory of counters at once. That statement carries a variadic `IN`
    // list and a correlated subquery over its own target table — MySQL rejects
    // a subquery that reads the target in its `FROM` (error 1093) — so the
    // multi-id form needs proving on every backend, not just the single-id one
    // above.
    rg_db::ops::repo_ops::refresh_stars_counts(&db, &[repo.id, live_fork.id])
        .await
        .expect("refresh several star counters in one statement on every backend");
    let counted_repo = rg_db::ops::repo_ops::find_by_id(&db, repo.id)
        .await
        .expect("read source after batched star count refresh")
        .expect("source repository exists");
    assert_eq!(
        counted_repo.stars_count, 1,
        "the batched refresh lost the star the single-id refresh had counted"
    );
    let counted_fork = rg_db::ops::repo_ops::find_by_id(&db, live_fork.id)
        .await
        .expect("read fork after batched star count refresh")
        .expect("fork repository exists");
    assert_eq!(
        counted_fork.stars_count, 0,
        "the batched refresh credited one repository's stars to another"
    );

    // card_615e00843297: `repositories` used to hold one name per *account*, so
    // a personal repository and one in an organization the same account owns
    // could not share a name. The replacement — a `namespace_key` generated
    // column plus `UNIQUE (namespace_key, name)` — is spelled differently on
    // each backend (`STORED` here, `VIRTUAL` on MySQL, a table rebuild on
    // SQLite), so "the migration applied" is not the same claim as "it
    // enforces the right thing". Assert the behaviour, on the real server.
    let org = rg_db::ops::org_ops::create_org(
        &db,
        &format!("{username}org"),
        None,
        None,
        user.id,
        "public",
    )
    .await
    .expect("create an organization owned by the same account");

    // card_5c878b2468a6: run the same successful-write, retirement-claim and
    // completed-delete assertions as the focused server-backend test above.
    let org = exercise_organization_update_contract(&db, org, user.id, suffix).await;

    let twin = format!("twin{suffix}");
    let personal_twin = rg_db::ops::repo_ops::create(&db, namespace_repo(user.id, None, &twin))
        .await
        .expect("create the personal repository");
    rg_db::ops::repo_ops::create(&db, namespace_repo(user.id, Some(org.id), &twin))
        .await
        .expect(
            "the two namespaces still cannot hold the same name — the account-wide \
             constraint is still on this backend",
        );
    assert!(
        rg_db::ops::repo_ops::create(&db, namespace_repo(user.id, None, &twin))
            .await
            .is_err(),
        "a duplicate name inside one namespace was accepted — uniqueness has to stay \
         enforced by the database, not only by the service layer"
    );

    rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
        &db,
        personal_twin.id,
        chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
    )
    .await
    .expect("soft-delete the personal repository");
    rg_db::ops::repo_ops::create(&db, namespace_repo(user.id, None, &twin))
        .await
        .expect(
            "a soft-deleted repository still reserves its name: every lookup filters \
             `deleted_at IS NULL`, so recreating it surfaced as an anonymous 5xx",
        );

    let initial_wiki_content = format!("This page contains {wiki_term} for full text search.");
    let page = rg_core::wiki::service::create_page(
        &db,
        repo.id,
        "Home",
        &initial_wiki_content,
        Some("initial page"),
        Some(user.id),
    )
    .await
    .expect("create wiki page and synchronize FTS");

    let (wiki_results, wiki_total) = rg_core::search::service::search(
        &db,
        &format!("{wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search wiki FTS with repository filter");
    assert_eq!(wiki_total, 1);
    assert_eq!(wiki_results.first().map(|result| result.id), Some(page.id));

    let (first_edit, second_edit) = tokio::join!(
        rg_core::wiki::service::update_page(
            &db,
            repo.id,
            "Home",
            &first_wiki_edit_content,
            None,
            Some(user.id),
        ),
        rg_core::wiki::service::update_page(
            &db,
            repo.id,
            "Home",
            &second_wiki_edit_content,
            None,
            Some(user.id),
        ),
    );
    first_edit.expect("store the first concurrent wiki edit");
    second_edit.expect("store the second concurrent wiki edit");

    let revisions = rg_core::wiki::service::list_revisions(&db, repo.id, "Home")
        .await
        .expect("read concurrent wiki revisions");
    assert_eq!(
        revisions
            .iter()
            .map(|revision| revision.version)
            .collect::<Vec<_>>(),
        vec![2, 1],
        "parallel edits must leave one uniquely numbered revision each"
    );
    let current = rg_core::wiki::service::get_page(&db, repo.id, "Home")
        .await
        .expect("read current wiki page after concurrent edits")
        .expect("wiki page still exists");
    let mut preserved_states = revisions
        .iter()
        .map(|revision| revision.content.as_str())
        .chain(std::iter::once(current.content.as_str()))
        .collect::<Vec<_>>();
    preserved_states.sort_unstable();
    let mut expected_states = vec![
        initial_wiki_content.as_str(),
        first_wiki_edit_content.as_str(),
        second_wiki_edit_content.as_str(),
    ];
    expected_states.sort_unstable();
    assert_eq!(
        preserved_states, expected_states,
        "both successful edit texts must survive in current state or history"
    );

    let (current_wiki_term, superseded_wiki_term) = if current.content == first_wiki_edit_content {
        (&first_wiki_edit_term, &second_wiki_edit_term)
    } else if current.content == second_wiki_edit_content {
        (&second_wiki_edit_term, &first_wiki_edit_term)
    } else {
        panic!("concurrent updates left an unexpected current wiki state")
    };
    let (current_wiki_results, current_wiki_total) = rg_core::search::service::search(
        &db,
        &format!("{current_wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search the current concurrent wiki edit");
    assert_eq!(current_wiki_total, 1);
    assert_eq!(
        current_wiki_results.first().map(|result| result.id),
        Some(page.id),
        "FTS does not reflect the current source row"
    );
    let (_, superseded_wiki_total) = rg_core::search::service::search(
        &db,
        &format!("{superseded_wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search the superseded concurrent wiki edit");
    assert_eq!(
        superseded_wiki_total, 0,
        "FTS retained the superseded concurrent wiki content"
    );

    // Repository-local issue numbers under the same treatment. On a server
    // backend the losing insert reaches the UNIQUE index and comes back as a
    // duplicate key — the primitive SQLite rarely produces here, because it
    // refuses the write on the snapshot first.
    let (first_issue, second_issue) = tokio::join!(
        rg_core::issue::service::create_issue(
            &db,
            repo.id,
            user.id,
            "first concurrent issue".to_string(),
            None,
            None,
            None,
        ),
        rg_core::issue::service::create_issue(
            &db,
            repo.id,
            user.id,
            "second concurrent issue".to_string(),
            None,
            None,
            None,
        )
    );
    let first_issue = first_issue.expect("store the first concurrent issue");
    let second_issue = second_issue.expect("store the second concurrent issue");
    let mut issue_numbers = [first_issue.number, second_issue.number];
    issue_numbers.sort_unstable();
    assert_eq!(
        issue_numbers,
        [1, 2],
        "parallel issue creates must each keep a distinct consecutive number"
    );

    let (repo_results, repo_total) = rg_core::search::service::search(
        &db,
        &format!("{repo_name} author:{username}"),
        "repos",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search repository FTS with owner filter");
    assert_eq!(repo_total, 1);
    assert_eq!(repo_results.first().map(|result| result.id), Some(repo.id));

    rg_core::wiki::service::delete_page(&db, repo.id, "Home")
        .await
        .expect("delete wiki page and FTS row");
    let (_, wiki_total_after_delete) = rg_core::search::service::search(
        &db,
        &format!("{current_wiki_term} repo:{username}/{repo_name}"),
        "wiki",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("verify wiki FTS deletion");
    assert_eq!(wiki_total_after_delete, 0);

    // card_04054a87159b: a board reorder is published as one serialized
    // transaction. The three backends refuse an overtaken writer in three
    // different ways — a lost WAL snapshot, a serialization failure, a deadlock
    // victim — so the external promise (both callers succeed, and the board
    // holds one submitted order in full) has to be checked on the server
    // databases too, not only on SQLite.
    let board_now = chrono::Utc::now();
    let board = rg_db::ops::board_ops::create_board(
        &db,
        rg_db::entities::board::ActiveModel {
            id: NotSet,
            repo_id: Set(Some(repo.id)),
            org_id: Set(None),
            name: Set("Smoke".to_string()),
            description: Set(None),
            created_by: Set(Some(user.id)),
            created_at: Set(board_now),
            updated_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test board");
    let board_column = rg_db::ops::board_ops::create_column(
        &db,
        rg_db::entities::board_column::ActiveModel {
            id: NotSet,
            board_id: Set(board.id),
            name: Set("Todo".to_string()),
            color: Set(None),
            position: Set(0),
            created_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test board column");

    // card_0ac3b290f3a9: single-row board/column updates use conditional
    // UPDATEs on every backend. A missing row is an ordinary typed outcome,
    // never SeaORM's backend-shaped RecordNotUpdated error.
    let board = rg_db::ops::board_ops::update_board(
        &db,
        board.id,
        Some("Smoke updated".to_string()),
        Some("cross-backend board update".to_string()),
        chrono::Utc::now(),
    )
    .await
    .expect("update smoke-test board")
    .expect("smoke-test board still exists");
    assert_eq!(board.name, "Smoke updated");
    assert_eq!(
        board.description.as_deref(),
        Some("cross-backend board update")
    );
    let board_column = rg_db::ops::board_ops::update_column(
        &db,
        board_column.id,
        Some("Ready".to_string()),
        Some("#123456".to_string()),
    )
    .await
    .expect("update smoke-test board column")
    .expect("smoke-test board column still exists");
    assert_eq!(board_column.name, "Ready");
    assert_eq!(board_column.color.as_deref(), Some("#123456"));

    // card_0a1f24172339: the three metadata/access PATCH helpers use the same
    // portable conditional-update contract as board/column. Exercise both the
    // successful write and the absent-row outcome on the server backends.
    let collaborator = rg_db::ops::repo_collaborator_ops::create(
        &db,
        rg_db::entities::repo_collaborator::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            user_id: Set(user.id),
            permission: Set("read".to_string()),
            created_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test collaborator");
    let collaborator = rg_db::ops::repo_collaborator_ops::update_permission(
        &db,
        collaborator.id,
        "write".to_string(),
    )
    .await
    .expect("update smoke-test collaborator")
    .expect("smoke-test collaborator still exists");
    assert!(collaborator.changed);
    assert_eq!(collaborator.collaborator.permission, "write");
    assert!(
        rg_db::ops::repo_collaborator_ops::delete_by_repo_and_user(&db, repo.id, user.id)
            .await
            .expect("delete smoke-test collaborator")
    );
    assert!(rg_db::ops::repo_collaborator_ops::update_permission(
        &db,
        collaborator.collaborator.id,
        "admin".to_string(),
    )
    .await
    .expect("an absent collaborator update is an outcome, not a database error")
    .is_none());

    let label = rg_db::ops::label_ops::create(
        &db,
        rg_db::entities::label::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            name: Set(format!("smoke-label-{suffix}")),
            color: Set("#112233".to_string()),
            description: Set(None),
            created_at: Set(board_now),
            updated_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test label");
    let label = rg_db::ops::label_ops::update(
        &db,
        label.id,
        Some(format!("smoke-label-updated-{suffix}")),
        Some("#445566".to_string()),
        Some(Some("cross-backend label update".to_string())),
        chrono::Utc::now(),
    )
    .await
    .expect("update smoke-test label")
    .expect("smoke-test label still exists");
    assert_eq!(label.color, "#445566");
    assert_eq!(
        label.description.as_deref(),
        Some("cross-backend label update")
    );
    assert!(rg_db::ops::label_ops::delete_by_id(&db, label.id)
        .await
        .expect("delete smoke-test label"));
    assert_eq!(
        rg_db::ops::label_ops::update(
            &db,
            label.id,
            Some("gone".to_string()),
            None,
            None,
            chrono::Utc::now(),
        )
        .await
        .expect("an absent label update is an outcome, not a database error"),
        None
    );

    let milestone = rg_db::ops::milestone_ops::create(
        &db,
        rg_db::entities::milestone::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            title: Set(format!("smoke milestone {suffix}")),
            description: Set(None),
            state: Set("open".to_string()),
            due_date: Set(None),
            created_at: Set(board_now),
            updated_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test milestone");
    let milestone = rg_db::ops::milestone_ops::update(
        &db,
        milestone.id,
        Some(format!("smoke milestone updated {suffix}")),
        Some(Some("cross-backend milestone update".to_string())),
        Some("closed".to_string()),
        None,
        chrono::Utc::now(),
    )
    .await
    .expect("update smoke-test milestone")
    .expect("smoke-test milestone still exists");
    assert_eq!(milestone.state, "closed");
    assert_eq!(
        milestone.description.as_deref(),
        Some("cross-backend milestone update")
    );
    assert!(rg_db::ops::milestone_ops::delete_by_id(&db, milestone.id)
        .await
        .expect("delete smoke-test milestone"));
    assert_eq!(
        rg_db::ops::milestone_ops::update(
            &db,
            milestone.id,
            Some("gone".to_string()),
            None,
            None,
            None,
            chrono::Utc::now(),
        )
        .await
        .expect("an absent milestone update is an outcome, not a database error"),
        None
    );

    // card_e41569944751: release, mirror and webhook PATCH writes must expose a
    // row that disappeared as an ordinary absent outcome on every backend,
    // while preserving every field the successful path owns.
    let release = rg_db::ops::release_ops::create(
        &db,
        rg_db::entities::release::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            tag_name: Set(format!("v-smoke-{suffix}")),
            target_commitish: Set("main".to_string()),
            title: Set("Smoke release".to_string()),
            body: Set(None),
            is_draft: Set(false),
            is_prerelease: Set(false),
            author_id: Set(Some(user.id)),
            created_at: Set(board_now),
            updated_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test release");
    let release = rg_db::ops::release_ops::update(
        &db,
        release.id,
        Some("Smoke release updated".to_string()),
        Some("cross-backend release update".to_string()),
        Some(true),
        Some(true),
        chrono::Utc::now(),
    )
    .await
    .expect("update smoke-test release")
    .expect("smoke-test release still exists");
    assert_eq!(release.title, "Smoke release updated");
    assert_eq!(
        release.body.as_deref(),
        Some("cross-backend release update")
    );
    assert!(release.is_draft);
    assert!(release.is_prerelease);
    assert!(rg_db::ops::release_ops::delete_by_id(&db, release.id)
        .await
        .expect("delete smoke-test release"));
    assert_eq!(
        rg_db::ops::release_ops::update(
            &db,
            release.id,
            Some("gone".to_string()),
            None,
            None,
            None,
            chrono::Utc::now(),
        )
        .await
        .expect("an absent release update is an outcome, not a database error"),
        None
    );

    let mirror = rg_db::ops::mirror_ops::create(
        &db,
        rg_db::entities::mirror::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            url: Set("https://example.com/original.git".to_string()),
            username: Set(None),
            password_encrypted: Set(None),
            sync_interval_seconds: Set(3600),
            next_sync_at: Set(None),
            last_sync_at: Set(None),
            last_sync_error: Set(None),
            status: Set(rg_db::entities::mirror::STATUS_ACTIVE.to_string()),
            created_at: Set(board_now),
            updated_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test mirror");
    let mirror = rg_db::ops::mirror_ops::update_settings(
        &db,
        mirror.id,
        Some("https://example.com/updated.git".to_string()),
        Some(Some("sync-bot".to_string())),
        Some(Some("sealed-placeholder".to_string())),
        Some(7200),
        Some(rg_db::entities::mirror::STATUS_INACTIVE.to_string()),
        chrono::Utc::now(),
    )
    .await
    .expect("update smoke-test mirror")
    .expect("smoke-test mirror still exists");
    assert_eq!(mirror.url, "https://example.com/updated.git");
    assert_eq!(mirror.username.as_deref(), Some("sync-bot"));
    assert_eq!(
        mirror.password_encrypted.as_deref(),
        Some("sealed-placeholder")
    );
    assert_eq!(mirror.sync_interval_seconds, 7200);
    assert_eq!(mirror.status, rg_db::entities::mirror::STATUS_INACTIVE);
    assert_eq!(
        rg_db::ops::mirror_ops::delete_by_id_unless_syncing(
            &db,
            mirror.id,
            repo.id,
            chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
        )
        .await
        .expect("delete smoke-test mirror"),
        rg_db::ops::mirror_ops::MirrorRetirement::Deleted
    );
    assert_eq!(
        rg_db::ops::mirror_ops::update_settings(
            &db,
            mirror.id,
            None,
            None,
            None,
            Some(3600),
            None,
            chrono::Utc::now(),
        )
        .await
        .expect("an absent mirror update is an outcome, not a database error"),
        None
    );

    let webhook = rg_db::ops::webhook_ops::create_webhook(
        &db,
        rg_db::entities::webhook::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            url: Set("https://example.com/original-hook".to_string()),
            content_type: Set("json".to_string()),
            secret_encrypted: Set(None),
            active: Set(true),
            events: Set("push".to_string()),
            created_at: Set(board_now),
            updated_at: Set(board_now),
        },
    )
    .await
    .expect("create smoke-test webhook");
    let webhook = rg_db::ops::webhook_ops::update_webhook(
        &db,
        webhook.id,
        "https://example.com/updated-hook".to_string(),
        "form".to_string(),
        Some("sealed-placeholder".to_string()),
        false,
        "release.created".to_string(),
        chrono::Utc::now(),
    )
    .await
    .expect("update smoke-test webhook")
    .expect("smoke-test webhook still exists");
    assert_eq!(webhook.url, "https://example.com/updated-hook");
    assert_eq!(webhook.content_type, "form");
    assert_eq!(
        webhook.secret_encrypted.as_deref(),
        Some("sealed-placeholder")
    );
    assert!(!webhook.active);
    assert_eq!(webhook.events, "release.created");
    assert!(
        rg_db::ops::webhook_ops::delete_webhook_by_id(&db, webhook.id)
            .await
            .expect("delete smoke-test webhook")
    );
    assert_eq!(
        rg_db::ops::webhook_ops::update_webhook(
            &db,
            webhook.id,
            "https://example.com/gone".to_string(),
            "json".to_string(),
            None,
            true,
            "push".to_string(),
            chrono::Utc::now(),
        )
        .await
        .expect("an absent webhook update is an outcome, not a database error"),
        None
    );

    let mut card_ids = Vec::with_capacity(3);
    for index in 0..3 {
        let card = rg_db::ops::board_ops::create_card(
            &db,
            rg_db::entities::board_card::ActiveModel {
                id: NotSet,
                column_id: Set(board_column.id),
                issue_id: Set(None),
                note: Set(Some(format!("smoke card {index}"))),
                position: Set(index),
                created_at: Set(board_now),
                updated_at: Set(board_now),
            },
        )
        .await
        .expect("create smoke-test board card");
        card_ids.push(card.id);
    }

    let reversed_order: Vec<(i64, i32)> =
        vec![(card_ids[0], 2), (card_ids[1], 1), (card_ids[2], 0)];
    let shifted_order: Vec<(i64, i32)> =
        vec![(card_ids[0], 10), (card_ids[1], 11), (card_ids[2], 12)];
    let (first_reorder, second_reorder) = tokio::join!(
        rg_db::ops::board_ops::update_card_positions(
            &db,
            board.id,
            Some(board_column.id),
            &reversed_order,
        ),
        rg_db::ops::board_ops::update_card_positions(
            &db,
            board.id,
            Some(board_column.id),
            &shifted_order,
        ),
    );
    assert_eq!(
        first_reorder.expect("store the first concurrent reorder"),
        rg_db::ops::board_ops::ReorderOutcome::Applied
    );
    assert_eq!(
        second_reorder.expect("store the second concurrent reorder"),
        rg_db::ops::board_ops::ReorderOutcome::Applied
    );

    let mut stored_positions = Vec::with_capacity(card_ids.len());
    for card_id in &card_ids {
        stored_positions.push(
            rg_db::ops::board_ops::find_card_by_id(&db, *card_id)
                .await
                .expect("read reordered board card")
                .expect("board card still exists")
                .position,
        );
    }
    assert!(
        stored_positions == vec![2, 1, 0] || stored_positions == vec![10, 11, 12],
        "concurrent reorders left a blended order: {stored_positions:?}"
    );

    let absent_card = card_ids.iter().copied().max().unwrap_or_default() + 10_000;
    let refused = rg_db::ops::board_ops::update_card_positions(
        &db,
        board.id,
        Some(board_column.id),
        &[(card_ids[0], 30), (absent_card, 31)],
    )
    .await
    .expect("a card this board does not own is an answer, not a failure");
    assert_eq!(
        refused,
        rg_db::ops::board_ops::ReorderOutcome::NotOnBoard(vec![absent_card])
    );
    let refused_positions = rg_db::ops::board_ops::find_card_by_id(&db, card_ids[0])
        .await
        .expect("read the in-scope card of a refused batch")
        .expect("board card still exists")
        .position;
    assert_eq!(
        refused_positions, stored_positions[0],
        "a refused batch wrote the card that was in scope"
    );

    assert!(
        rg_db::ops::board_ops::delete_board_by_id(&db, board.id)
            .await
            .expect("delete smoke-test board"),
        "deleting the smoke-test board removed no row"
    );
    assert_eq!(
        rg_db::ops::board_ops::update_board(
            &db,
            board.id,
            Some("gone".to_string()),
            None,
            chrono::Utc::now(),
        )
        .await
        .expect("an absent board update is an outcome, not a database error"),
        None
    );
    assert_eq!(
        rg_db::ops::board_ops::update_column(&db, board_column.id, Some("gone".to_string()), None,)
            .await
            .expect("an absent column update is an outcome, not a database error"),
        None
    );

    // ── MFA enrolment atomicity (card_3c33caaf7402) ──────────────────────────
    //
    // The precise halves of this live where a fault can be aimed at one row of
    // the set: `rg-db/tests/mfa_backup_code_set_atomicity` and `rg-http`'s
    // `mfa_enable_atomicity_tests`, both on a SQLite trigger. Neither seam is
    // portable, so what this backend has to answer for is the mechanism those
    // two rest on — the whole replacement joins the caller's unit of work (a
    // nested SAVEPOINT under an outer transaction), so a failure anywhere in that
    // unit takes the set with it instead of leaving the account with a second
    // factor and no codes.
    let first_codes = rg_db::ops::mfa_backup_code_ops::generate_codes(
        rg_db::ops::mfa_backup_code_ops::BACKUP_CODE_COUNT,
    );
    let enrolled = rg_db::ops::user_ops::enable_mfa_with_backup_codes(&db, user.id, &first_codes)
        .await
        .expect("enrol a second factor and its backup codes in one commit")
        .expect("the account remains open during MFA enrolment");
    assert!(
        enrolled.mfa_enabled,
        "the enrolment committed the codes without the flag"
    );
    assert_eq!(
        live_backup_hashes(&db, user.id).await,
        backup_hashes(&first_codes),
        "the codes handed to the owner are not the codes that were stored"
    );

    let second_codes = rg_db::ops::mfa_backup_code_ops::generate_codes(
        rg_db::ops::mfa_backup_code_ops::BACKUP_CODE_COUNT,
    );
    let doomed = db.begin().await.expect("begin a doomed re-issue");
    rg_db::ops::mfa_backup_code_ops::set_codes(&doomed, user.id, &second_codes)
        .await
        .expect("the replacement must nest inside the caller's transaction");
    assert_eq!(
        live_backup_hashes(&doomed, user.id).await,
        backup_hashes(&second_codes),
        "the nested replacement is not visible to the transaction that made it"
    );
    // A later step of the same enrolment failing is exactly the case the split
    // commits could not survive. `i64::MAX` is nobody's account.
    rg_db::ops::user_ops::enable_mfa(&doomed, i64::MAX)
        .await
        .expect_err("the doomed step must fail");
    doomed
        .rollback()
        .await
        .expect("roll the doomed re-issue back");
    assert_eq!(
        live_backup_hashes(&db, user.id).await,
        backup_hashes(&first_codes),
        "a re-issue that failed after writing its codes revoked the set the owner still holds"
    );

    // ── Repository transfer lease (card_507ff03ec043) ────────────────────────
    //
    // The lease is the boundary that keeps a transfer's storage move and the
    // deletion of the namespace it is moving out of from each other, and both of
    // its halves are backend-specific: the claim rides an upsert whose DO NOTHING
    // is a MySQL polyfill, and the source lifecycle is verified through
    // `lock_exclusive`, which only becomes `FOR UPDATE` on a server database. A
    // SQLite-only proof says nothing about either.
    let stale_before = || chrono::Utc::now() - rg_db::ops::repo_ops::TRANSFER_LEASE_STALE_AFTER;
    let first_holder = format!("holder-a-{suffix}");
    let second_holder = format!("holder-b-{suffix}");
    async fn lease_bid(
        db: &DatabaseConnection,
        repo_id: i64,
        owner_id: i64,
        source: &str,
        token: &str,
    ) -> rg_db::ops::repo_ops::TransferLeaseBid {
        rg_db::ops::repo_ops::bid_for_transfer_lease(
            db,
            repo_id,
            owner_id,
            None,
            source,
            "elsewhere",
            token,
            chrono::Utc::now() - rg_db::ops::repo_ops::TRANSFER_LEASE_STALE_AFTER,
        )
        .await
        .expect("bid for the repository transfer lease")
    }
    assert_eq!(
        lease_bid(&db, repo.id, user.id, &username, &first_holder).await,
        rg_db::ops::repo_ops::TransferLeaseBid::Granted,
        "the first bid on an untouched repository must win"
    );
    assert_eq!(
        lease_bid(&db, repo.id, user.id, &username, &second_holder).await,
        rg_db::ops::repo_ops::TransferLeaseBid::Busy,
        "two transfers hold the same repository at once on this backend"
    );
    assert!(
        rg_db::ops::repo_ops::transfer_lease_in_flight(&db, repo.id, stale_before())
            .await
            .expect("read the transfer lease")
            .is_some(),
        "the deletion path cannot see a lease this backend accepted"
    );
    // A losing bid must not release the winner's hold.
    assert!(
        !rg_db::ops::repo_ops::release_transfer_lease(&db, repo.id, &second_holder)
            .await
            .expect("attempt a release from the wrong holder"),
        "a token that never held the lease released it"
    );
    assert!(
        rg_db::ops::repo_ops::release_transfer_lease(&db, repo.id, &first_holder)
            .await
            .expect("release the transfer lease"),
        "the holder could not release its own lease"
    );
    assert!(
        rg_db::ops::repo_ops::transfer_lease_in_flight(&db, repo.id, stale_before())
            .await
            .expect("read the released transfer lease")
            .is_none(),
        "a released lease is still visible to the deletion path"
    );
    // And the source lifecycle half: a claimed namespace refuses the bid, under
    // the row lock a retirement claim contends for on this backend.
    assert!(rg_db::ops::user_ops::begin_user_retirement(&db, user.id)
        .await
        .expect("claim the source account"));
    assert_eq!(
        lease_bid(&db, repo.id, user.id, &username, &first_holder).await,
        rg_db::ops::repo_ops::TransferLeaseBid::SourceAccountClosed,
        "a transfer was admitted out of a namespace that is being retired"
    );
    assert!(
        rg_db::ops::repo_ops::transfer_lease_in_flight(&db, repo.id, stale_before())
            .await
            .expect("read the refused transfer lease")
            .is_none(),
        "a refused bid left its lease row behind"
    );
    rg_db::ops::user_ops::abort_user_retirement(&db, user.id)
        .await
        .expect("reopen the source account");

    // ── Mirror sync lease (card_a1f2a20281af) ────────────────────────────────
    //
    // Same protocol, same backend-specific halves — an upsert whose DO NOTHING is
    // a MySQL polyfill and a `lock_exclusive` that only becomes `FOR UPDATE` on a
    // server database — plus one this table has and the transfer lease does not:
    // the guarded soft-delete, whose whole correctness is that its UPDATE takes
    // the repository row *before* it reads this table, in one transaction.
    let mirror_now = chrono::Utc::now();
    let mirror = rg_db::ops::mirror_ops::create(
        &db,
        rg_db::entities::mirror::ActiveModel {
            id: NotSet,
            repo_id: Set(repo.id),
            url: Set(format!("https://example.invalid/upstream-{suffix}.git")),
            username: Set(None),
            password_encrypted: Set(None),
            sync_interval_seconds: Set(3600),
            next_sync_at: Set(None),
            last_sync_at: Set(None),
            last_sync_error: Set(None),
            status: Set(rg_db::entities::mirror::STATUS_ACTIVE.to_string()),
            created_at: Set(mirror_now),
            updated_at: Set(mirror_now),
        },
    )
    .await
    .expect("create the mirror whose sync lease is under test");
    let sync_stale_before = || chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER;
    let sync_holder = format!("sync-a-{suffix}");
    let rival_holder = format!("sync-b-{suffix}");
    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(&db, repo.id, &sync_holder, sync_stale_before())
            .await
            .expect("bid for the mirror sync lease"),
        rg_db::ops::mirror_ops::SyncLeaseBid::Granted,
        "the first bid on a configured mirror must win"
    );
    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(
            &db,
            repo.id,
            &rival_holder,
            sync_stale_before()
        )
        .await
        .expect("bid for a mirror sync lease somebody else holds"),
        rg_db::ops::mirror_ops::SyncLeaseBid::Busy,
        "two passes write the same mirror clone at once on this backend"
    );
    assert!(
        rg_db::ops::mirror_ops::sync_lease_in_flight(&db, repo.id, sync_stale_before())
            .await
            .expect("read the mirror sync lease")
            .is_some(),
        "the deletion path cannot see a lease this backend accepted"
    );
    // The half that has to hold inside a transaction: the row must survive a
    // refused retirement, not be soft-deleted and then reported as refused.
    assert_eq!(
        rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(&db, repo.id, sync_stale_before())
            .await
            .expect("run the guarded soft-delete under a held mirror sync lease"),
        rg_db::ops::repo_ops::RepositoryRetirement::MirrorSyncInFlight,
        "the guarded soft-delete cannot see a lease this backend accepted"
    );
    assert!(
        rg_db::ops::repo_ops::find_by_id(&db, repo.id)
            .await
            .expect("re-read the repository after a refused retirement")
            .is_some(),
        "the refused retirement rolled forward instead of back on this backend"
    );
    assert_eq!(
        rg_db::ops::mirror_ops::delete_by_id_unless_syncing(
            &db,
            mirror.id,
            repo.id,
            sync_stale_before()
        )
        .await
        .expect("run the guarded mirror retirement under its held lease"),
        rg_db::ops::mirror_ops::MirrorRetirement::MirrorSyncInFlight,
        "the guarded mirror retirement cannot see a lease this backend accepted"
    );
    assert!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo.id)
            .await
            .expect("re-read the mirror after a refused retirement")
            .is_some(),
        "the refused mirror retirement rolled forward instead of back on this backend"
    );
    assert!(
        !rg_db::ops::mirror_ops::release_sync_lease(&db, repo.id, &rival_holder)
            .await
            .expect("attempt a release from the wrong holder"),
        "a token that never held the mirror sync lease released it"
    );
    assert!(
        rg_db::ops::mirror_ops::release_sync_lease(&db, repo.id, &sync_holder)
            .await
            .expect("release the mirror sync lease"),
        "the holder could not release its own lease"
    );
    assert!(
        rg_db::ops::mirror_ops::sync_lease_in_flight(&db, repo.id, sync_stale_before())
            .await
            .expect("read the released mirror sync lease")
            .is_none(),
        "a released lease is still visible to the deletion path"
    );
    assert_eq!(
        rg_db::ops::mirror_ops::delete_by_id_unless_syncing(
            &db,
            mirror.id,
            repo.id,
            sync_stale_before()
        )
        .await
        .expect("retire the mirror once no pass holds it"),
        rg_db::ops::mirror_ops::MirrorRetirement::Deleted,
        "the guarded mirror retirement kept refusing after the pass had finished"
    );
    assert!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo.id)
            .await
            .expect("verify the retired mirror cleanup")
            .is_none(),
        "the successful mirror retirement left its row behind"
    );
    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(&db, repo.id, &sync_holder, sync_stale_before())
            .await
            .expect("bid for the lease of a retired mirror"),
        rg_db::ops::mirror_ops::SyncLeaseBid::MirrorGone,
        "a pass was admitted after the mirror row had been retired"
    );
    assert!(
        rg_db::ops::mirror_ops::sync_lease_in_flight(&db, repo.id, sync_stale_before())
            .await
            .expect("read the lease after the mirror was retired")
            .is_none(),
        "a refused bid for a retired mirror left its lease row behind"
    );
    assert_eq!(
        rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(&db, repo.id, sync_stale_before())
            .await
            .expect("retire the repository once no pass holds it"),
        rg_db::ops::repo_ops::RepositoryRetirement::Deleted,
        "the guarded soft-delete kept refusing after the pass had finished"
    );
    // …and the other direction: a bid against a repository that is already gone
    // must decline and leave no row behind to block anything.
    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(&db, repo.id, &sync_holder, sync_stale_before())
            .await
            .expect("bid for the mirror sync lease of a deleted repository"),
        rg_db::ops::mirror_ops::SyncLeaseBid::RepositoryGone,
        "a pass was admitted against a repository this backend had already retired"
    );
    assert!(
        rg_db::ops::mirror_ops::sync_lease_in_flight(&db, repo.id, sync_stale_before())
            .await
            .expect("read the refused mirror sync lease")
            .is_none(),
        "a refused bid left its lease row behind"
    );

    rg_db::ops::repo_ops::soft_delete_unless_mirror_syncing(
        &db,
        repo.id,
        chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER,
    )
    .await
    .expect("soft-delete the repository through the source row");
    assert_eq!(
        repo_fts_snapshot(&db, repo.id).await,
        None,
        "the repository UPDATE trigger left a soft-deleted row searchable"
    );

    let deleted_repo = rg_db::entities::repository::Entity::find_by_id(repo.id)
        .one(&db)
        .await
        .expect("read the raw soft-deleted repository")
        .expect("soft-deleted repository row still exists");
    let mut restored_repo: rg_db::entities::repository::ActiveModel = deleted_repo.into();
    restored_repo.deleted_at = Set(None);
    restored_repo.description = Set(Some(repo_restore_term.clone()));
    restored_repo.updated_at = Set(chrono::Utc::now());
    restored_repo
        .update(&db)
        .await
        .expect("restore repository through the source row");
    assert_eq!(
        repo_fts_snapshot(&db, repo.id).await,
        Some((repo_name.clone(), repo_restore_term.clone())),
        "restoring the repository did not recreate its current FTS snapshot"
    );
    let (restored_results, restored_total) = rg_core::search::service::search(
        &db,
        &format!("{repo_restore_term} author:{username}"),
        "repos",
        Some(user.id),
        1,
        20,
    )
    .await
    .expect("search the restored repository metadata");
    assert_eq!(restored_total, 1);
    assert_eq!(
        restored_results.first().map(|result| result.id),
        Some(repo.id)
    );

    assert!(
        !rg_db::ops::repo_star_ops::toggle_star(&db, user.id, repo.id)
            .await
            .expect("remove smoke-test star")
    );
    rg_db::ops::repo_ops::delete_by_id(&db, live_fork.id)
        .await
        .expect("delete live fork row");
    rg_db::ops::repo_ops::delete_by_id(&db, deleted_fork.id)
        .await
        .expect("delete soft-deleted fork row");
    rg_db::ops::repo_ops::delete_by_id(&db, repo.id)
        .await
        .expect("delete smoke-test repository");
    assert_eq!(repo_fts_snapshot(&db, repo.id).await, None);
    // `organizations.owner_id` carries no foreign key, so deleting the user
    // below would leave this row behind.
    assert!(
        rg_db::ops::org_ops::delete_org(&db, org.id)
            .await
            .expect("delete smoke-test organization"),
        "deleting the smoke-test organization removed no row"
    );
    rg_db::ops::user_ops::delete_by_id(&db, user.id)
        .await
        .expect("delete smoke-test user");
    rg_db::ops::user_ops::delete_by_id(&db, inactive.id)
        .await
        .expect("delete inactive smoke-test user");
    rg_db::ops::user_ops::delete_by_id(&db, retiring.id)
        .await
        .expect("delete retiring smoke-test user");
    assert!(rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .expect("verify smoke-test user cleanup")
        .is_none());
}
