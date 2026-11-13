//! Integration tests for opt-in release-asset provenance attestation.
//!
//! Guards:
//!   POST /repos/:o/:r/releases/assets/:id/attestation         — sign
//!   GET  /repos/:o/:r/releases/assets/:id/attestation         — fetch envelope
//!   POST /repos/:o/:r/releases/assets/:id/attestation/verify  — verify

use crate::common::{
    build_test_app_state, create_initialised_repo, register_user, setup_test_db, spawn_test_app,
    spawn_test_app_over_db_with, StateOverrides, TEST_ENCRYPTION_KEY,
};
use sea_orm::{ConnectionTrait, Statement};

const PW: &str = "Qz7$wRtm";

/// Database-backed keys, established as the server does on start.
async fn instance_keys(
    db: &sea_orm::DatabaseConnection,
    jwt_secret: &str,
) -> std::sync::Arc<rg_core::auth::instance_key::InstanceKeys> {
    let ring = rg_core::auth::instance_key::establish(db, jwt_secret, TEST_ENCRYPTION_KEY)
        .await
        .expect("establish instance keys");
    std::sync::Arc::new(rg_core::auth::instance_key::InstanceKeys::new(
        db.clone(),
        TEST_ENCRYPTION_KEY,
        ring,
    ))
}

async fn create_release(base: &str, token: &str, owner: &str, repo: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/releases"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": "Rel" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let rel: serde_json::Value = resp.json().await.unwrap();
    rel["id"].as_i64().unwrap()
}

async fn upload_asset(base: &str, token: &str, owner: &str, repo: &str, release_id: i64) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets"
        ))
        .bearer_auth(token)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=notes.txt")
        .body("release asset")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let asset: serde_json::Value = resp.json().await.unwrap();
    asset["id"].as_i64().unwrap()
}

#[tokio::test]
async fn sign_get_verify_round_trip() {
    let base = spawn_test_app().await;
    let owner = "attuser".to_string();
    let token = register_user(&base, &owner, "attuser@example.com", PW).await;
    let repo = "attrepo".to_string();
    create_initialised_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;
    let client = reqwest::Client::new();

    // Sign → 201 DSSE envelope.
    let signed = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(signed.status(), 201);
    let envelope: serde_json::Value = signed.json().await.unwrap();
    assert_eq!(envelope["payloadType"], "application/vnd.in-toto+json");
    assert!(!envelope["payload"].as_str().unwrap().is_empty());
    assert_eq!(envelope["signatures"].as_array().unwrap().len(), 1);

    // Fetch stored envelope → identical.
    let fetched = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), 200);
    let fetched_env: serde_json::Value = fetched.json().await.unwrap();
    assert_eq!(fetched_env, envelope);

    // Verify → verified against the asset digest and the instance key.
    let verified = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation/verify"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(verified.status(), 200);
    let report: serde_json::Value = verified.json().await.unwrap();
    assert_eq!(report["status"], "verified", "report: {report}");
    assert_eq!(
        report["predicate_type"], "https://plombir.com/git/provenance/v1",
        "new attestations are issued under the Plombir type, not the pre-rename one: {report}"
    );
    // "release asset" → known SHA-256 (matches the digest step's vector).
    assert_eq!(
        report["asset_sha256"],
        "e6abe9df7db8513616674b02b5edb26c37bf3b2f81daeec1e3c6fc8c9a802850"
    );
}

#[tokio::test]
async fn signing_an_asset_deleted_after_the_scoped_read_is_404() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    let base = spawn_test_app_over_db_with(db.clone(), repo_root, StateOverrides::default()).await;
    let owner = "attrace".to_string();
    let token = register_user(&base, &owner, "attrace@example.com", PW).await;
    let repo = "attracerepo".to_string();
    create_initialised_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;

    // The trigger runs in the actual attestation UPDATE, after the route has
    // already scoped the asset and the service has read the digest to sign.
    // This fixes the interleaving without timing sleeps or a production hook.
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_release_asset_before_attestation_update \
             BEFORE UPDATE OF attestation ON release_assets WHEN OLD.id = {asset_id} \
             BEGIN DELETE FROM release_assets WHERE id = OLD.id; END"
        ),
    ))
    .await
    .expect("install the competing release asset delete");

    let raced = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("race attestation signing against asset deletion");
    assert_eq!(
        raced.status(),
        404,
        "a DELETE after the scoped read must stay a typed missing asset"
    );
    assert!(
        rg_db::ops::release_ops::find_asset_by_id(&db, asset_id)
            .await
            .expect("look for the release asset after the race")
            .is_none(),
        "the losing attestation write must not recreate the deleted asset"
    );
}

/// The card's acceptance, end to end: rotate `jwt_secret`, restart, and the
/// envelope this instance issued before the rotation still verifies.
///
/// Before card_3aecf3708ebe the provenance key was derived from `jwt_secret`,
/// so a rotation the security guide tells operators to perform silently
/// replaced the instance's identity — and `POST .../attestation/verify` then
/// answered "invalid" about a signature this very server had produced. What
/// changes across the restart here is exactly that input — the secret the key
/// used to be derived from. The identity, and every signature under it, must
/// not.
#[tokio::test]
async fn a_rotated_jwt_secret_leaves_earlier_attestations_verifiable() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");

    // First boot: the instance establishes and stores its identity while one
    // signing secret is in force.
    let keys_before = instance_keys(&db, "the-original-jwt-secret").await;
    let kid_before = keys_before
        .current()
        .await
        .unwrap()
        .active()
        .kid()
        .to_owned();
    let base = spawn_test_app_over_db_with(
        db.clone(),
        repo_root.clone(),
        StateOverrides {
            instance_keys: Some(keys_before),
            ..Default::default()
        },
    )
    .await;

    let owner = "rotuser".to_string();
    let token = register_user(&base, &owner, "rotuser@example.com", PW).await;
    let repo = "rotrepo".to_string();
    create_initialised_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;

    let client = reqwest::Client::new();
    let signed = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(signed.status(), 201);
    let envelope: serde_json::Value = signed.json().await.unwrap();

    // The operator rotates the signing secret and restarts. Same database, same
    // repo root, cold state — only the secret the key would have been derived
    // from differs.
    let keys_after = instance_keys(&db, "the-rotated-jwt-secret").await;
    assert_eq!(
        keys_after.current().await.unwrap().active().kid(),
        kid_before,
        "the published kid must survive a rotated signing secret"
    );

    let base_after = spawn_test_app_over_db_with(
        db.clone(),
        repo_root,
        StateOverrides {
            instance_keys: Some(keys_after),
            ..Default::default()
        },
    )
    .await;

    let fetched: serde_json::Value = client
        .get(format!(
            "{base_after}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(fetched, envelope, "the stored envelope must be untouched");

    let report: serde_json::Value = client
        .post(format!(
            "{base_after}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation/verify"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        report["status"], "verified",
        "an envelope this instance signed must not be called invalid after a jwt_secret \
         rotation: {report}"
    );
    drop(dir);
}

/// The card's acceptance, over HTTP: the report has three answers, and the two
/// that are not "verified" must not be the same answer.
///
/// `verify` used to hand the client a single boolean, so *every* refusal of
/// `verify_envelope` arrived as `verified: false` — which the release page
/// renders as "Provenance check failed", a claim that the asset's bytes are no
/// longer the signed ones. A predicate type this build has no verifier for is
/// not that claim: the signature holds and the subject digest binds these exact
/// bytes. Only the digest comparison earns the loud verdict
/// (card_4579598691ce).
#[tokio::test]
async fn an_uncheckable_envelope_is_undeterminable_and_a_wrong_digest_is_a_mismatch() {
    use sea_orm::ConnectionTrait as _;

    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    let keys = instance_keys(&db, "attestation-secret").await;
    let key = keys.current().await.unwrap().active().clone();
    let base = spawn_test_app_over_db_with(
        db.clone(),
        repo_root,
        StateOverrides {
            instance_keys: Some(keys),
            ..Default::default()
        },
    )
    .await;

    let owner = "preduser".to_string();
    let token = register_user(&base, &owner, "preduser@example.com", PW).await;
    let repo = "predrepo".to_string();
    create_initialised_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;
    // SHA-256 of the uploaded body, shared with the round-trip test's vector.
    let asset_sha = "e6abe9df7db8513616674b02b5edb26c37bf3b2f81daeec1e3c6fc8c9a802850";
    let client = reqwest::Client::new();

    // Store an envelope this instance signed itself, binding the asset's real
    // digest, under a predicate type no registered verifier handles. Written
    // straight to the row because the signing endpoint deliberately only ever
    // issues Plombir Git's own predicate type.
    let store_envelope = |statement: rg_core::attestation::Statement| {
        let envelope = rg_core::attestation::sign_statement(&key, &statement)
            .expect("sign the envelope under the instance key");
        let json = serde_json::to_string(&envelope).expect("serialize the envelope");
        let db = db.clone();
        async move {
            db.execute(Statement::from_sql_and_values(
                db.get_database_backend(),
                "UPDATE release_assets SET attestation = ? WHERE id = ?",
                [json.into(), asset_id.into()],
            ))
            .await
            .expect("store the crafted attestation");
        }
    };

    store_envelope(rg_core::attestation::Statement::new(
        "notes.txt",
        asset_sha,
        "https://someone-elses.example/attestation/v9".to_string(),
        serde_json::json!({ "whatever": true }),
    ))
    .await;

    let report: serde_json::Value = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation/verify"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        report["status"], "undeterminable",
        "an unregistered predicate type is this instance's gap, not an accusation against the \
         asset: {report}"
    );
    assert_ne!(
        report["status"], "mismatch",
        "and it must never be reported as the tampering verdict: {report}"
    );
    assert!(
        report["reason"]
            .as_str()
            .is_some_and(|r| r.contains("no verifier registered")),
        "the reason still has to name what could not be checked: {report}"
    );

    // The regression half: a statement bound to bytes that are not this asset's
    // still gets the loud verdict.
    store_envelope(rg_core::attestation::Statement::new(
        "notes.txt",
        "0".repeat(64),
        rg_core::attestation::PLOMBIR_GIT_PROVENANCE_TYPE.to_string(),
        serde_json::json!({ "builder": { "id": "https://forge.example" } }),
    ))
    .await;

    let report: serde_json::Value = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation/verify"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        report["status"], "mismatch",
        "a subject digest that is not the asset's is exactly what this feature exists to \
         shout about: {report}"
    );
    drop(dir);
}

/// An attestation issued before the rename sits in `release_assets.attestation`
/// under the former predicate type, and the verify endpoint must still call it
/// verified (card_f835b97ca951). Stored straight to the row, as a pre-rename
/// build left it: the signing endpoint only issues the current type now.
#[tokio::test]
async fn an_attestation_stored_under_the_former_type_still_verifies() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    let keys = instance_keys(&db, "attestation-secret").await;
    let key = keys.current().await.unwrap().active().clone();
    let base = spawn_test_app_over_db_with(
        db.clone(),
        repo_root,
        StateOverrides {
            instance_keys: Some(keys),
            ..Default::default()
        },
    )
    .await;

    let owner = "legacyatt".to_string();
    let token = register_user(&base, &owner, "legacyatt@example.com", PW).await;
    let repo = "legacyattrepo".to_string();
    create_initialised_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;

    let envelope = rg_core::attestation::sign_statement(
        &key,
        &rg_core::attestation::Statement::new(
            "notes.txt",
            "e6abe9df7db8513616674b02b5edb26c37bf3b2f81daeec1e3c6fc8c9a802850",
            "https://forgekeep.dev/provenance/v1".to_string(),
            serde_json::json!({ "builder": { "id": "https://forge.example" } }),
        ),
    )
    .expect("sign the pre-rename envelope");
    db.execute(Statement::from_sql_and_values(
        db.get_database_backend(),
        "UPDATE release_assets SET attestation = ? WHERE id = ?",
        [
            serde_json::to_string(&envelope).unwrap().into(),
            asset_id.into(),
        ],
    ))
    .await
    .expect("store the pre-rename attestation");

    let report: serde_json::Value = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation/verify"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(report["status"], "verified", "report: {report}");
    assert_eq!(
        report["predicate_type"], "https://forgekeep.dev/provenance/v1",
        "report: {report}"
    );
    drop(dir);
}

#[tokio::test]
async fn verify_without_attestation_is_404() {
    let base = spawn_test_app().await;
    let owner = "attuser2".to_string();
    let token = register_user(&base, &owner, "attuser2@example.com", PW).await;
    let repo = "attrepo2".to_string();
    create_initialised_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// `GET /instance` says whether this forge does provenance at all
/// (card_5e52392a0274).
///
/// Both attestation endpoints answer `404` when the feature is off *and* when
/// an asset was simply never signed. For an operator those are the same status
/// code; for a reader they are opposite facts — "this forge does not do
/// provenance" versus "this file was never signed" — and the release page has
/// to render them differently. Without a capability to ask, the only way to
/// tell them apart is the wording of an error body.
///
/// Asserted at both settings, because a field hard-coded to `true` would
/// satisfy the enabled half on its own.
#[tokio::test]
async fn the_instance_announces_whether_it_does_provenance() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    // Anonymous: this is the browser of somebody reading a public release page
    // before logging in, and it is the reader the badge is for.
    let enabled = client
        .get(format!("{base}/api/v1/instance"))
        .send()
        .await
        .unwrap();
    assert_eq!(enabled.status(), 200);
    let enabled: serde_json::Value = enabled.json().await.unwrap();
    assert_eq!(
        enabled["attestation_enabled"], true,
        "the test harness enables attestation, so the instance must say so: {enabled}"
    );

    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let mut state = build_test_app_state(db, repo_root);
    state.attestation_enabled = false;
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let off_base = format!("http://{addr}");
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let disabled = client
        .get(format!("{off_base}/api/v1/instance"))
        .send()
        .await
        .unwrap();
    assert_eq!(disabled.status(), 200);
    let disabled: serde_json::Value = disabled.json().await.unwrap();
    assert_eq!(
        disabled["attestation_enabled"], false,
        "an instance with the feature off must say so rather than leaving the page to guess \
         from a 404: {disabled}"
    );
}

/// With attestation disabled (production default), every endpoint 404s.
#[tokio::test]
async fn disabled_endpoints_return_404() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let mut state = build_test_app_state(db, repo_root);
    state.attestation_enabled = false; // opt-in: off
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let owner = "attuser3".to_string();
    let token = register_user(&base, &owner, "attuser3@example.com", PW).await;
    let repo = "attrepo3".to_string();
    create_initialised_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;
    let client = reqwest::Client::new();

    let sign = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(sign.status(), 404, "sign must 404 when feature disabled");

    let verify = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation/verify"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        verify.status(),
        404,
        "verify must 404 when feature disabled"
    );
}

async fn published_kids(base: &str) -> (Vec<String>, String) {
    let response = reqwest::get(format!("{base}/api/v1/ci/oidc/jwks"))
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let cache = response
        .headers()
        .get(reqwest::header::CACHE_CONTROL)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let document: serde_json::Value = response.json().await.unwrap();
    let kids = document["keys"]
        .as_array()
        .unwrap()
        .iter()
        .map(|key| key["kid"].as_str().unwrap().to_owned())
        .collect();
    (kids, cache)
}

async fn post_json(url: String, token: &str) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(url)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

/// S11 / R1: `rotate-instance-key` runs against the database of a running
/// server. The server must stop publishing and signing with the old key once
/// its cache expires — here the TTL is zero — without a restart, and a
/// compromise rotation must turn the old key's attestations into `revoked`.
#[tokio::test]
async fn a_rotation_reaches_the_running_server_without_a_restart() {
    use rg_core::auth::instance_key::{revoke, rotate, InstanceKeys, RotationReason};

    let (db, dir) = setup_test_db().await;
    let ring = rg_core::auth::instance_key::establish(&db, "s11-secret", TEST_ENCRYPTION_KEY)
        .await
        .unwrap();
    let first = ring.active().kid().to_owned();
    let keys = InstanceKeys::new(db.clone(), TEST_ENCRYPTION_KEY, ring)
        .with_ttl(std::time::Duration::ZERO);
    let base = spawn_test_app_over_db_with(
        db.clone(),
        dir.path().join("repos"),
        StateOverrides {
            instance_keys: Some(std::sync::Arc::new(keys)),
            ..Default::default()
        },
    )
    .await;
    let owner = "s11user".to_string();
    let token = register_user(&base, &owner, "s11user@example.com", PW).await;
    let repo = "s11repo".to_string();
    create_initialised_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;
    let attestation =
        format!("{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation");

    let (kids, cache) = published_kids(&base).await;
    assert_eq!(kids, vec![first.clone()]);
    assert_eq!(cache, "public, max-age=300");
    let (status, _) = post_json(attestation.clone(), &token).await;
    assert_eq!(status, 201);

    // Routine: the old key verifies its attestation and stays published for
    // the overlap; new signatures use the new key.
    let second = rotate(&db, TEST_ENCRYPTION_KEY, RotationReason::Routine)
        .await
        .unwrap()
        .key
        .kid()
        .to_owned();
    let (kids, _) = published_kids(&base).await;
    assert!(kids.contains(&first) && kids.contains(&second), "{kids:?}");
    let (_, report) = post_json(format!("{attestation}/verify"), &token).await;
    assert_eq!(report["status"], "verified", "{report}");
    assert_eq!(report["keyid"], first.as_str(), "{report}");
    assert_eq!(report["key_status"], "retired", "{report}");
    assert_eq!(report["key_origin"], "generated", "{report}");
    let (status, envelope) = post_json(attestation.clone(), &token).await;
    assert_eq!(status, 201);
    assert_eq!(envelope["signatures"][0]["keyid"], second.as_str());

    // Compromised: the key leaves the JWKS at once and what it signed is
    // refused by name.
    let third = rotate(&db, TEST_ENCRYPTION_KEY, RotationReason::Compromised)
        .await
        .unwrap()
        .key
        .kid()
        .to_owned();
    let (kids, _) = published_kids(&base).await;
    assert!(!kids.contains(&second) && kids.contains(&third), "{kids:?}");
    let (_, report) = post_json(format!("{attestation}/verify"), &token).await;
    assert_eq!(report["status"], "revoked", "{report}");
    assert_eq!(report["keyid"], second.as_str(), "{report}");
    assert_eq!(report["key_status"], "revoked", "{report}");

    // A retired key revoked later leaves the JWKS within the TTL as well.
    revoke(&db, TEST_ENCRYPTION_KEY, &first).await.unwrap();
    let (kids, _) = published_kids(&base).await;
    assert_eq!(kids, vec![third]);
    drop(dir);
}
