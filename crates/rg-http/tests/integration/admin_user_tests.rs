use crate::common::{register_full, spawn_test_app_with_db};
use rg_db::sea_orm::{ConnectionTrait, Statement};

async fn install_retirement_before_user_update(
    db: &rg_db::DatabaseConnection,
    user_id: i64,
    trigger_name: &str,
    columns: &str,
) {
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER {trigger_name} BEFORE UPDATE OF {columns} ON users \
             WHEN OLD.id = {user_id} \
             BEGIN \
                 UPDATE users \
                 SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
                 WHERE id = OLD.id; \
                 SELECT RAISE(IGNORE); \
             END"
        ),
    ))
    .await
    .expect("install the competing account retirement");
}

async fn install_delete_before_user_update(
    db: &rg_db::DatabaseConnection,
    user_id: i64,
    trigger_name: &str,
    columns: &str,
) {
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER {trigger_name} BEFORE UPDATE OF {columns} ON users \
             WHEN OLD.id = {user_id} \
             BEGIN \
                 DELETE FROM users WHERE id = OLD.id; \
                 SELECT RAISE(IGNORE); \
             END"
        ),
    ))
    .await
    .expect("install the competing account delete");
}

async fn assert_typed_user_absence(response: reqwest::Response, operation: &str) {
    assert_eq!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND,
        "{operation} did not classify a winning account retirement as typed absence"
    );
    let body = response.text().await.expect("read typed-absence response");
    assert!(
        !body.contains("RecordNotUpdated") && !body.contains("db:"),
        "{operation} leaked a backend-shaped failure: {body}"
    );
}

async fn audit_total(base: &str, admin_token: &str, action: &str) -> i64 {
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/admin/audit/logs?action={action}"))
        .bearer_auth(admin_token)
        .send()
        .await
        .expect("list filtered audit events");
    assert_eq!(response.status(), 200);
    response
        .json::<serde_json::Value>()
        .await
        .expect("decode filtered audit events")["total"]
        .as_i64()
        .expect("audit total")
}

async fn promote_user_to_admin(db: &rg_db::DatabaseConnection, user_id: i64) {
    rg_db::ops::user_ops::update_by_id(db, user_id, None, None, Some(true), None)
        .await
        .expect("promote user to admin")
        .expect("registered user must exist");
}

/// A query string the caller was never going to be allowed to send must not be
/// parsed before the caller is turned away.
///
/// `Query<_>` is a `FromRequestParts` and axum runs a handler's arguments left
/// to right, so a gate written as the first statement of the function *body*
/// still runs after the query is deserialized. `?per_page=abc` therefore
/// answered an anonymous caller `400` with serde's complaint — which names the
/// parameter and the type it wanted — instead of `401`.
///
/// The four probes are one argument, not four assertions. The admin's `400` is
/// what makes the anonymous `401` mean something: it shows the deserializer
/// really does reject `abc`, so the denial is the gate having run first rather
/// than a parser that happened to be lenient. The admin's `200` is the live
/// baseline — without it every line here would still pass against a route that
/// had simply stopped working.
#[tokio::test]
async fn admin_users_denies_before_it_parses_the_query() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/admin/users?per_page=abc");

    let anon = client.get(&url).send().await.unwrap();
    assert_eq!(
        anon.status(),
        401,
        "an anonymous caller was answered about the query instead of about itself"
    );
    let body = anon.text().await.unwrap();
    assert!(
        !body.contains("per_page") && !body.contains("u64"),
        "the denial handed out the parameter's name or type: {body}"
    );

    let (admin_token, _) = register_full(&base, "founder", "founder@example.com").await;
    let (user_token, _) = register_full(&base, "queryuser", "queryuser@example.com").await;
    let outsider = client
        .get(&url)
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        outsider.status(),
        403,
        "a signed-in non-admin was answered about the query instead of about itself"
    );

    let admin = client
        .get(&url)
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        admin.status(),
        400,
        "`per_page=abc` is supposed to be a bad request once you are past the gate — if it \
         is not, the two denials above prove nothing about ordering"
    );

    let alive = client
        .get(format!("{base}/api/v1/admin/users?per_page=5"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        alive.status(),
        200,
        "the route is dead, so every denial above is meaningless"
    );
}

#[tokio::test]
async fn admin_users_list_requires_auth() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/admin/users", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn admin_users_list_requires_admin() {
    let (base, _db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, _) = register_full(&base, "founder", "founder@example.com").await;
    let (user_token, _) = register_full(&base, "nona", "nona@example.com").await;
    let user_resp = client
        .get(format!("{}/api/v1/admin/users", base))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(user_resp.status(), 403);

    let admin_resp = client
        .get(format!("{}/api/v1/admin/users", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(admin_resp.status(), 200);
    let body: serde_json::Value = admin_resp.json().await.unwrap();
    assert_eq!(body["pagination"]["total"], 2);
}

#[tokio::test]
async fn admin_users_get_and_update() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(&base, "adminx", "adminx@example.com").await;
    let (_, target_id) = register_full(&base, "target", "target@example.com").await;
    promote_user_to_admin(&db, admin_id).await;

    let get_resp = client
        .get(format!("{}/api/v1/admin/users/{}", base, target_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let target_body: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(target_body["id"], target_id);

    let update_resp = client
        .patch(format!("{}/api/v1/admin/users/{}", base, target_id))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({
            "display_name": "Target User",
            "bio": "Updated for admin test",
            "is_admin": true,
            "is_active": false,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(update_resp.status(), 200);
    let updated: serde_json::Value = update_resp.json().await.unwrap();
    assert_eq!(updated["id"], target_id);
    assert_eq!(updated["display_name"], "Target User");
    assert_eq!(updated["is_admin"], true);
    assert_eq!(updated["is_active"], false);

    let missing = client
        .patch(format!("{}/api/v1/admin/users/999999", base))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({ "display_name": "Missing" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}

/// Both triggers run inside the real conditional UPDATE. `RAISE(IGNORE)` keeps
/// the retirement marker (or DELETE) but prevents the outer PATCH assignment,
/// making the lifecycle winner deterministic without a timing race.
#[tokio::test]
async fn admin_user_patch_losing_to_retirement_or_delete_is_404_without_audit() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "patch_race_admin", "patch_race_admin@example.com").await;
    let (_, retiring_id) = register_full(
        &base,
        "patch_retiring_target",
        "patch_retiring_target@example.com",
    )
    .await;
    let (_, deleted_id) = register_full(
        &base,
        "patch_deleted_target",
        "patch_deleted_target@example.com",
    )
    .await;
    promote_user_to_admin(&db, admin_id).await;

    install_retirement_before_user_update(
        &db,
        retiring_id,
        "retire_user_inside_admin_patch",
        "display_name",
    )
    .await;
    let retirement = client
        .patch(format!("{base}/api/v1/admin/users/{retiring_id}"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"display_name": "must not land"}))
        .send()
        .await
        .expect("race admin PATCH against retirement");
    assert_typed_user_absence(retirement, "admin PATCH vs retirement").await;

    let retiring = rg_db::ops::user_ops::find_by_id(&db, retiring_id)
        .await
        .expect("read the retiring target")
        .expect("retirement keeps the row until its storage is retired");
    assert!(retiring.deleted_at.is_some());
    assert_ne!(retiring.display_name.as_deref(), Some("must not land"));

    install_delete_before_user_update(
        &db,
        deleted_id,
        "delete_user_inside_admin_patch",
        "display_name",
    )
    .await;
    let deletion = client
        .patch(format!("{base}/api/v1/admin/users/{deleted_id}"))
        .bearer_auth(&admin_token)
        .json(&serde_json::json!({"display_name": "must not resurrect"}))
        .send()
        .await
        .expect("race admin PATCH against physical delete");
    assert_typed_user_absence(deletion, "admin PATCH vs physical delete").await;
    assert!(
        rg_db::ops::user_ops::find_by_id(&db, deleted_id)
            .await
            .expect("look for the deleted target")
            .is_none(),
        "the losing PATCH resurrected the deleted account"
    );
    assert_eq!(
        audit_total(&base, &admin_token, "admin.update_user").await,
        0,
        "a losing admin PATCH published a success audit event"
    );
}

#[tokio::test]
async fn admin_users_delete_block_self() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) =
        register_full(&base, "admin_delete_self", "admin_delete_self@example.com").await;
    promote_user_to_admin(&db, admin_id).await;

    let del_resp = client
        .delete(format!("{}/api/v1/admin/users/{}", base, admin_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del_resp.status(), 400);
}

#[tokio::test]
async fn admin_users_delete_target() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();

    let (admin_token, admin_id) = register_full(&base, "admin_del", "admin_del@example.com").await;
    let (_, target_id) = register_full(&base, "victim", "victim@example.com").await;
    promote_user_to_admin(&db, admin_id).await;

    let del_resp = client
        .delete(format!("{}/api/v1/admin/users/{}", base, target_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(del_resp.status(), 200);
    let del_body: serde_json::Value = del_resp.json().await.unwrap();
    assert_eq!(del_body["deleted"], true);

    let missing = client
        .delete(format!("{}/api/v1/admin/users/{}", base, target_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        missing.status(),
        404,
        "deleting a user that no longer exists must not look like a database failure"
    );
    let missing_body: serde_json::Value = missing.json().await.unwrap();
    assert_eq!(missing_body["error"]["message"], "user not found");

    let get_after = client
        .get(format!("{}/api/v1/admin/users/{}", base, target_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_after.status(), 404);
}

async fn unused_backup_codes(db: &rg_db::DatabaseConnection, user_id: i64) -> usize {
    rg_db::ops::mfa_backup_code_ops::list_codes(db, user_id)
        .await
        .expect("list backup codes")
        .into_iter()
        .filter(|code| !code.used)
        .count()
}

/// `POST /admin/users/{id}/mfa/reset`: the administrator's way back into an
/// account whose second factor its owner can no longer pass (security audit
/// finding #6). Admin-only like its siblings; `404` for an account that is not
/// there; `409` for one with no factor to reset; `400` for the administrator's
/// own account; and on success the factor, its backup codes and every session
/// the account had are gone, with one journal row saying who did it.
#[tokio::test]
async fn admin_can_reset_a_users_mfa_and_the_action_is_audited() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "mfa_reset_admin", "mfa_reset_admin@example.com").await;
    let (target_token, target_id) =
        register_full(&base, "mfa_reset_target", "mfa_reset_target@example.com").await;
    let (bystander_token, bystander_id) = register_full(
        &base,
        "mfa_reset_bystander",
        "mfa_reset_bystander@example.com",
    )
    .await;
    promote_user_to_admin(&db, admin_id).await;

    // The target stands on a second factor with a full recovery set — the
    // state a stolen session leaves an account in.
    rg_db::ops::user_ops::enable_mfa(&db, target_id)
        .await
        .expect("enable mfa");
    let codes = rg_db::ops::mfa_backup_code_ops::generate_codes(
        rg_db::ops::mfa_backup_code_ops::BACKUP_CODE_COUNT,
    );
    rg_db::ops::mfa_backup_code_ops::reissue_codes(&db, target_id, &codes)
        .await
        .expect("issue backup codes");
    assert_eq!(unused_backup_codes(&db, target_id).await, codes.len());

    let reset_url = |id: i64| format!("{base}/api/v1/admin/users/{id}/mfa/reset");

    let forbidden = client
        .post(reset_url(target_id))
        .bearer_auth(&bystander_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403, "a non-admin reset somebody's MFA");

    let missing = client
        .post(reset_url(999_999))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);

    let own = client
        .post(reset_url(admin_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        own.status(),
        400,
        "an administrator's own factor comes off with a password, not from here"
    );

    let nothing_to_reset = client
        .post(reset_url(bystander_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        nothing_to_reset.status(),
        409,
        "an account with no second factor was 'reset' — and its sessions revoked for nothing"
    );

    // Nothing above touched the target.
    let untouched = rg_db::ops::user_ops::find_by_id(&db, target_id)
        .await
        .unwrap()
        .unwrap();
    assert!(untouched.mfa_enabled);
    assert_eq!(unused_backup_codes(&db, target_id).await, codes.len());

    let reset = client
        .post(reset_url(target_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(reset.status(), 200);
    let body: serde_json::Value = reset.json().await.unwrap();
    assert_eq!(body["id"], target_id);
    assert_eq!(body["mfa_enabled"], false);

    let after = rg_db::ops::user_ops::find_by_id(&db, target_id)
        .await
        .unwrap()
        .unwrap();
    assert!(!after.mfa_enabled, "the factor is still on");
    assert!(
        after.totp_secret.is_none(),
        "the secret outlived the factor"
    );
    assert_eq!(
        unused_backup_codes(&db, target_id).await,
        0,
        "backup codes outlived the factor"
    );

    // The session that enrolled the wrong authenticator is the one that must
    // not survive its removal.
    let old_session = client
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(&target_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        old_session.status(),
        401,
        "a session the account held before the reset still works"
    );

    let audit = client
        .get(format!(
            "{base}/api/v1/admin/audit/logs?action=admin.reset_mfa"
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(audit.status(), 200);
    let audit_body: serde_json::Value = audit.json().await.unwrap();
    assert_eq!(audit_body["total"], 1, "exactly one reset is journalled");
    assert_eq!(audit_body["logs"][0]["resource_id"], target_id);
    assert_eq!(audit_body["logs"][0]["user_id"], admin_id);
    assert_eq!(audit_body["logs"][0]["username"], "mfa_reset_admin");

    let again = client
        .post(reset_url(target_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 409, "a second reset has nothing to reset");
}

#[tokio::test]
async fn admin_can_unlock_user_and_action_is_audited() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "unlock_admin", "unlock_admin@example.com").await;
    let (user_token, target_id) =
        register_full(&base, "locked_target", "locked_target@example.com").await;
    promote_user_to_admin(&db, admin_id).await;

    let (first, second, third, fourth, fifth) = tokio::join!(
        rg_db::ops::user_ops::record_failed_login(&db, target_id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, target_id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, target_id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, target_id, 5),
        rg_db::ops::user_ops::record_failed_login(&db, target_id, 5),
    );
    for result in [first, second, third, fourth, fifth] {
        result.unwrap();
    }
    let locked = rg_db::ops::user_ops::find_by_id(&db, target_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(locked.login_attempts, 5);
    assert!(locked.locked_until.is_some());

    let forbidden = client
        .post(format!("{}/api/v1/admin/users/{}/unlock", base, target_id))
        .bearer_auth(&user_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), 403);

    let unlocked = client
        .post(format!("{}/api/v1/admin/users/{}/unlock", base, target_id))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(unlocked.status(), 200);
    let body: serde_json::Value = unlocked.json().await.unwrap();
    assert_eq!(body["login_attempts"], 0);
    assert!(body["locked_until"].is_null());
    let target = rg_db::ops::user_ops::find_by_id(&db, target_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(target.login_attempts, 0);
    assert!(target.locked_until.is_none());

    let audit = client
        .get(format!(
            "{}/api/v1/admin/audit/logs?action=admin.unlock_user",
            base
        ))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(audit.status(), 200);
    let audit_body: serde_json::Value = audit.json().await.unwrap();
    assert_eq!(audit_body["total"], 1);
    assert_eq!(audit_body["logs"][0]["resource_id"], target_id);

    let missing = client
        .post(format!("{}/api/v1/admin/users/999999/unlock", base))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}

#[tokio::test]
async fn admin_unlock_losing_to_retirement_or_delete_is_404_without_audit() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (admin_token, admin_id) =
        register_full(&base, "unlock_race_admin", "unlock_race_admin@example.com").await;
    let (_, retiring_id) = register_full(
        &base,
        "unlock_retiring_target",
        "unlock_retiring_target@example.com",
    )
    .await;
    let (_, deleted_id) = register_full(
        &base,
        "unlock_deleted_target",
        "unlock_deleted_target@example.com",
    )
    .await;
    promote_user_to_admin(&db, admin_id).await;
    for user_id in [retiring_id, deleted_id] {
        rg_db::ops::user_ops::record_failed_login(&db, user_id, 5)
            .await
            .expect("seed a failed login");
    }

    install_retirement_before_user_update(
        &db,
        retiring_id,
        "retire_user_inside_admin_unlock",
        "login_attempts, locked_until",
    )
    .await;
    let retirement = client
        .post(format!("{base}/api/v1/admin/users/{retiring_id}/unlock"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("race admin unlock against retirement");
    assert_typed_user_absence(retirement, "admin unlock vs retirement").await;

    let retiring = rg_db::ops::user_ops::find_by_id(&db, retiring_id)
        .await
        .expect("read the retiring target")
        .expect("retirement keeps the row until its storage is retired");
    assert!(retiring.deleted_at.is_some());
    assert_eq!(retiring.login_attempts, 1);

    install_delete_before_user_update(
        &db,
        deleted_id,
        "delete_user_inside_admin_unlock",
        "login_attempts, locked_until",
    )
    .await;
    let deletion = client
        .post(format!("{base}/api/v1/admin/users/{deleted_id}/unlock"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .expect("race admin unlock against physical delete");
    assert_typed_user_absence(deletion, "admin unlock vs physical delete").await;
    assert!(
        rg_db::ops::user_ops::find_by_id(&db, deleted_id)
            .await
            .expect("look for the deleted target")
            .is_none(),
        "the losing unlock resurrected the deleted account"
    );
    assert_eq!(
        audit_total(&base, &admin_token, "admin.unlock_user").await,
        0,
        "a losing admin unlock published a success audit event"
    );
}
