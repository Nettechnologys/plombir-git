//! Regression: a valid session/PAT is not fresh proof for access administration.
use crate::common;
use rg_db::entities::notification_delivery;
use sea_orm::EntityTrait;
use serde_json::{json, Value};
const SECRET: &str = "test-secret-key";
async fn call(base: &str, method: &str, path: &str, token: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("{base}/api/v1{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    (
        response.status().as_u16(),
        response.json().await.unwrap_or_default(),
    )
}
fn encode(claims: &rg_core::auth::jwt::Claims) -> String {
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        claims,
        &jsonwebtoken::EncodingKey::from_secret(SECRET.as_bytes()),
    )
    .unwrap()
}
#[tokio::test]
async fn sensitive_administration_requires_current_factor_and_refuses_pat_without_side_effects() {
    let (base, db) = common::spawn_test_app_with_db().await;
    let (password_only, owner_id) =
        common::register_full(&base, "access_owner", "owner@example.invalid").await;
    let (_, member_id) =
        common::register_full(&base, "access_member", "member@example.invalid").await;
    rg_db::ops::user_ops::update_by_id(&db, owner_id, None, None, Some(true), None)
        .await
        .unwrap();
    common::create_repo(&base, &password_only, "private").await;
    assert_eq!(
        call(
            &base,
            "PATCH",
            "/repos/access_owner/private",
            &password_only,
            json!({"is_private":true})
        )
        .await
        .0,
        200
    );
    assert_eq!(
        call(
            &base,
            "POST",
            "/orgs",
            &password_only,
            json!({"name":"access_org"})
        )
        .await
        .0,
        201
    );
    let token = call(
        &base,
        "POST",
        "/users/tokens",
        &password_only,
        json!({"name":"automation","scopes":"repo,user,admin"}),
    )
    .await;
    assert_eq!(token.0, 201, "{token:?}");
    let pat = token.1["token"].as_str().unwrap();
    let setup = call(&base, "POST", "/users/mfa/setup", &password_only, json!({})).await;
    assert_eq!(setup.0, 200, "{setup:?}");
    let code = totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        totp_rs::Secret::Encoded(setup.1["secret"].as_str().unwrap().into())
            .to_bytes()
            .unwrap(),
        None,
        String::new(),
    )
    .unwrap()
    .generate_current()
    .unwrap();
    let enabled = call(
        &base,
        "POST",
        "/users/mfa/enable",
        &password_only,
        json!({"password":"Qz7$wRtm","code":code}),
    )
    .await;
    assert_eq!(enabled.0, 200, "{enabled:?}");
    let proof = call(
        &base,
        "POST",
        "/users/me/sudo",
        &password_only,
        json!({"password":"Qz7$wRtm","backup_code":enabled.1["backup_codes"][0]}),
    )
    .await;
    assert_eq!(proof.0, 200, "{proof:?}");
    let fresh = proof.1["token"].as_str().unwrap();
    let mut claims = rg_core::auth::jwt::validate_token(fresh, SECRET).unwrap();
    assert!(claims.sudo_mfa.is_some());
    claims.sudo_exp = Some(chrono::Utc::now().timestamp() - 60);
    let expired = encode(&claims);
    let mut routes: Vec<(String, String, Value)> = vec![];
    let repo = "/repos/access_owner/private";
    for (collection, update) in [
        ("collaborators", "PATCH"),
        ("hooks", "PATCH"),
        ("push_mirrors", "PATCH"),
        ("branches/protection", "PATCH"),
        ("tags/protection", "PATCH"),
        ("actions/environments", "PUT"),
    ] {
        routes.push(("POST".into(), format!("{repo}/{collection}"), json!({})));
        routes.push((
            update.into(),
            format!("{repo}/{collection}/999999"),
            json!({}),
        ));
        routes.push((
            "DELETE".into(),
            format!("{repo}/{collection}/999999"),
            json!({}),
        ));
    }
    for method in ["POST", "PATCH", "DELETE"] {
        routes.push((method.into(), format!("{repo}/mirror"), json!({})));
    }
    for method in ["PUT", "DELETE"] {
        routes.push((
            method.into(),
            format!("{repo}/actions/secrets/SECRET"),
            json!({}),
        ));
    }
    routes.extend([
        ("DELETE".into(), repo.into(), json!({})),
        (
            "POST".into(),
            format!("{repo}/transfer"),
            json!({"new_owner":"access_org"}),
        ),
        (
            "PATCH".into(),
            repo.into(),
            json!({"is_private":true,"description":"must not change"}),
        ),
        (
            "PATCH".into(),
            "/orgs/access_org".into(),
            json!({"visibility":"private","description":"must not change"}),
        ),
        ("DELETE".into(), "/orgs/access_org".into(), json!({})),
        (
            "POST".into(),
            "/orgs/access_org/members".into(),
            json!({"user_id":member_id}),
        ),
        (
            "DELETE".into(),
            format!("/orgs/access_org/members/{member_id}"),
            json!({}),
        ),
        ("POST".into(), "/orgs/access_org/teams".into(), json!({})),
        (
            "DELETE".into(),
            "/orgs/access_org/teams/999999".into(),
            json!({}),
        ),
        (
            "POST".into(),
            "/orgs/access_org/teams/999999/members".into(),
            json!({}),
        ),
        (
            "DELETE".into(),
            format!("/orgs/access_org/teams/999999/members/{member_id}"),
            json!({}),
        ),
        (
            "POST".into(),
            format!("/admin/users/{member_id}/unlock"),
            json!({}),
        ),
        (
            "DELETE".into(),
            format!("/admin/users/{member_id}"),
            json!({}),
        ),
        ("DELETE".into(), "/admin/orgs/access_org".into(), json!({})),
        (
            "DELETE".into(),
            "/admin/sso/providers/999999".into(),
            json!({}),
        ),
        ("DELETE".into(), "/admin/runners/999999".into(), json!({})),
    ]);
    for scope in ["/orgs/access_org/hooks", "/admin/hooks"] {
        routes.push(("POST".into(), scope.into(), json!({})));
        for method in ["PATCH", "DELETE"] {
            routes.push((method.into(), format!("{scope}/999999"), json!({})));
        }
    }
    routes.push(("DELETE".into(), "/users/bots/missing-bot".into(), json!({})));
    let before = notification_delivery::Entity::find()
        .all(&db)
        .await
        .unwrap();
    for (kind, token) in [
        ("expired", expired.as_str()),
        ("password-only", password_only.as_str()),
        ("PAT", pat),
    ] {
        for (method, path, body) in &routes {
            let answer = call(&base, method, path, token, body.clone()).await;
            assert_eq!(answer.0, 403, "{kind}: {method} {path}: {answer:?}");
            if kind != "PAT" {
                assert_eq!(
                    answer.1["error"]["reason"], "sudo_required",
                    "{method} {path}: {answer:?}"
                );
            }
        }
    }
    assert_eq!(
        before,
        notification_delivery::Entity::find()
            .all(&db)
            .await
            .unwrap()
    );
    assert!(rg_db::ops::repo_collaborator_ops::list_by_repo(&db, 1)
        .await
        .unwrap()
        .is_empty());
    let cosmetic = call(
        &base,
        "PATCH",
        repo,
        &expired,
        json!({"description":"ordinary edit"}),
    )
    .await;
    assert_eq!(cosmetic.0, 200, "{cosmetic:?}");
    let cosmetic = call(
        &base,
        "PATCH",
        "/orgs/access_org",
        &expired,
        json!({"description":"ordinary edit"}),
    )
    .await;
    assert_eq!(cosmetic.0, 200, "{cosmetic:?}");
    // The current factor binding and account generation are independently checked.
    let mut stale_factor = rg_core::auth::jwt::validate_token(fresh, SECRET).unwrap();
    stale_factor.sudo_mfa = Some("previous-factor".into());
    let refused = call(
        &base,
        "POST",
        &format!("{repo}/collaborators"),
        &encode(&stale_factor),
        json!({"user_id":member_id,"permission":"admin"}),
    )
    .await;
    assert_eq!(refused.1["error"]["reason"], "sudo_required");
    let mut revoked = rg_core::auth::jwt::validate_token(fresh, SECRET).unwrap();
    revoked.session_version += 1;
    let refused = call(
        &base,
        "POST",
        &format!("{repo}/collaborators"),
        &encode(&revoked),
        json!({"user_id":member_id,"permission":"admin"}),
    )
    .await;
    assert_eq!(refused.0, 401);
    assert_eq!(refused.1["error"]["reason"], "session_revoked");
    let (outsider, _) = common::register_full(&base, "outsider", "outsider@example.invalid").await;
    let hidden = call(
        &base,
        "POST",
        &format!("{repo}/collaborators"),
        &outsider,
        json!({}),
    )
    .await;
    let missing = call(
        &base,
        "POST",
        "/repos/access_owner/missing/collaborators",
        &outsider,
        json!({}),
    )
    .await;
    assert_eq!(hidden.0, 404);
    assert_eq!(missing.0, 404);
    assert_eq!(hidden.1["error"]["message"], missing.1["error"]["message"]);
    let added = call(
        &base,
        "POST",
        &format!("{repo}/collaborators"),
        fresh,
        json!({"user_id":member_id,"permission":"read"}),
    )
    .await;
    assert_eq!(added.0, 201, "{added:?}");
    let changed = call(
        &base,
        "PATCH",
        &format!("{repo}/collaborators/{}", added.1["id"]),
        fresh,
        json!({"permission":"admin"}),
    )
    .await;
    assert_eq!(changed.0, 200, "{changed:?}");
    let removed = call(
        &base,
        "DELETE",
        &format!("{repo}/collaborators/{member_id}"),
        fresh,
        json!({}),
    )
    .await;
    assert_eq!(removed.0, 204, "{removed:?}");
    let rows = notification_delivery::Entity::find()
        .all(&db)
        .await
        .unwrap();
    for id in [owner_id, member_id] {
        assert_eq!(
            rows.iter()
                .filter(|r| r.user_id == id && r.event_key == "repository_access_changed")
                .count(),
            3
        );
    }
}

/// Keep all sensitive HTTP doors, including nested org/system helpers, tied
/// to full session proof. Resource authorization precedes proof to preserve
/// the private/nonexistent repository envelope.
#[test]
fn sensitive_administration_doors_keep_their_session_proof_extractor() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/api");
    for (file, handlers) in [
        ("collaborators", "add_collaborator update_permission remove_collaborator"),
        ("orgs", "delete_org add_org_member remove_org_member create_team delete_team add_team_member remove_team_member update_org"),
        ("repos", "delete_repo_handler transfer_repo_handler update_repo_handler"),
        ("webhooks", "create_webhook update_webhook delete_webhook org_create org_update org_delete system_create system_update system_delete"),
        ("mirrors", "create_mirror update_mirror delete_mirror"),
        ("push_mirrors", "create_push_mirror update_push_mirror delete_push_mirror"),
        ("branch_protection", "create_protection update_protection delete_protection"),
        ("tag_protection", "create update delete"),
        ("ci_environments", "create update delete"),
        ("ci_secrets", "put delete"),
        ("admin", "unlock_user delete_user delete_org delete_sso_provider"),
        ("runners", "delete_runner_admin"),
        ("bots", "delete_bot"),
    ] {
        let source = std::fs::read_to_string(root.join(format!("{file}.rs"))).unwrap();
        for name in handlers.split_whitespace() {
            let params = common::source_scan::signature_params(&source, name).unwrap();
            let sudo = params.iter().position(|param| param.contains("SudoUser"))
                .unwrap_or_else(|| panic!("{file}::{name} lost the full proof guard"));
            if let Some(role) = params.iter().position(|param| ["RepoAdmin", "RepoOwner", "OrgAdmin", "OrgOwner", "InstanceAdmin"].iter().any(|role| param.contains(role))) {
                assert!(role < sudo, "{file}::{name} must resolve access before requesting proof");
            }
        }
    }
}
