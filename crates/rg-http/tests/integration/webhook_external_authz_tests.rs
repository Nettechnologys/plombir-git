//! Authorization regressions for the external CI/CD webhook receiver.
//!
//! `POST /api/v1/repos/{owner}/{name}/webhooks/external/ci` used to stop at
//! `extract_user_id(...)` and then resolve the repository straight out of the
//! path. That is authentication, not authorization: any account with a valid
//! token could write a row into the `commit_statuses` of any repository —
//! private ones included — painting somebody else's commits `success` or
//! `failure`, and confirming the private repository exists on the way.
//!
//! The optional HMAC secret was never a substitute for the gate. It is
//! instance-wide, so every CI system wired to the server holds the same one;
//! knowing it says "I am some configured CI here", not "I may write to this
//! repository". The tests below drive both halves: no signature, and a
//! perfectly valid signature.
//!
//! Writing a commit status is a repository write, so the gate is `RepoWrite` —
//! the same one `POST /repos/{owner}/{name}/statuses/{sha}` carries, which is
//! the endpoint this one duplicates.

use sea_orm::EntityTrait;

use crate::common::{
    create_repo, register_full, spawn_test_app_with_db, spawn_test_app_with_webhook_secret,
};

const BODY: &str = r#"{"context":"jenkins/pipe","state":"success"}"#;

async fn create_private_repo(base: &str, token: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_private_repo '{name}' failed");
}

async fn add_collaborator(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    username: &str,
    permission: &str,
) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(token)
        .json(&serde_json::json!({"username": username, "permission": permission}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "add_collaborator '{username}' failed");
}

/// Post to the endpoint, optionally signed, optionally authenticated.
async fn post_status(
    base: &str,
    owner: &str,
    repo: &str,
    token: Option<&str>,
    signature: Option<String>,
) -> reqwest::StatusCode {
    let mut req = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/webhooks/external/ci"
        ))
        .header("content-type", "application/json")
        .body(BODY.to_string());
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    if let Some(signature) = signature {
        req = req.header("X-Hub-Signature-256", signature);
    }
    req.send().await.unwrap().status()
}

/// Rows the endpoint is supposed to produce — the only thing that proves a
/// refusal was a refusal and not just a status code.
async fn commit_status_count(db: &rg_db::DatabaseConnection) -> usize {
    rg_db::entities::commit_status::Entity::find()
        .all(db)
        .await
        .expect("read commit_statuses")
        .len()
}

/// The leak in its shortest form, plus the baseline that makes it mean
/// something: the owner's identical request goes through.
#[tokio::test]
async fn outsider_cannot_write_a_commit_status_to_a_private_repo() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _owner_id) = register_full(&base, "xci_owner", "xci_o@e.com").await;
    let (outsider_token, _outsider_id) = register_full(&base, "xci_outsider", "xci_x@e.com").await;
    create_private_repo(&base, &owner_token, "secret").await;

    let status = post_status(&base, "xci_owner", "secret", Some(&outsider_token), None).await;
    assert_eq!(
        status, 403,
        "a logged-in outsider wrote a commit status into a private repository"
    );
    assert_eq!(
        commit_status_count(&db).await,
        0,
        "the refusal was only a status code — the row was written anyway"
    );

    // Baseline in the same test: the gate is closed, not the endpoint.
    let status = post_status(&base, "xci_owner", "secret", Some(&owner_token), None).await;
    assert_eq!(
        status, 200,
        "the owner cannot post to their own repository — the fixture is broken, \
         so the denial above proves nothing"
    );
    assert_eq!(commit_status_count(&db).await, 1);
}

#[tokio::test]
async fn anonymous_cannot_write_a_commit_status() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _owner_id) = register_full(&base, "xcia_owner", "xcia_o@e.com").await;
    create_private_repo(&base, &owner_token, "secret").await;

    let status = post_status(&base, "xcia_owner", "secret", None, None).await;
    assert_eq!(status, 401, "an anonymous caller reached the handler");
    assert_eq!(commit_status_count(&db).await, 0);

    // A repository that does not exist answers the same 401, so the endpoint is
    // not an existence oracle for anonymous callers either.
    let status = post_status(&base, "xcia_owner", "nosuchrepo", None, None).await;
    assert_eq!(
        status, 401,
        "an anonymous caller was told whether the repository exists"
    );
}

/// A public repository is not an exception: reading it is open, writing a
/// status onto its commits is not.
#[tokio::test]
async fn outsider_cannot_write_a_commit_status_to_a_public_repo() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _owner_id) = register_full(&base, "xcip_owner", "xcip_o@e.com").await;
    let (outsider_token, _outsider_id) =
        register_full(&base, "xcip_outsider", "xcip_x@e.com").await;
    create_repo(&base, &owner_token, "open").await;

    let status = post_status(&base, "xcip_owner", "open", Some(&outsider_token), None).await;
    assert_eq!(
        status, 403,
        "any logged-in user painted the commits of a public repository"
    );
    assert_eq!(commit_status_count(&db).await, 0);
}

/// The level is *write*, pinned from both sides: a read collaborator is turned
/// away, a write collaborator is not.
#[tokio::test]
async fn read_access_is_not_enough_but_write_access_is() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner_token, _owner_id) = register_full(&base, "xcic_owner", "xcic_o@e.com").await;
    let (reader_token, _reader_id) = register_full(&base, "xcic_reader", "xcic_r@e.com").await;
    let (writer_token, _writer_id) = register_full(&base, "xcic_writer", "xcic_w@e.com").await;
    create_private_repo(&base, &owner_token, "secret").await;
    add_collaborator(
        &base,
        &owner_token,
        "xcic_owner",
        "secret",
        "xcic_reader",
        "read",
    )
    .await;
    add_collaborator(
        &base,
        &owner_token,
        "xcic_owner",
        "secret",
        "xcic_writer",
        "write",
    )
    .await;

    let status = post_status(&base, "xcic_owner", "secret", Some(&reader_token), None).await;
    assert_eq!(
        status, 403,
        "a read-only collaborator wrote a commit status"
    );

    let status = post_status(&base, "xcic_owner", "secret", Some(&writer_token), None).await;
    assert_eq!(
        status, 200,
        "a write collaborator was refused — the gate is above the level it should be"
    );
}

/// The signature is defense-in-depth, not the gate. The secret is one value for
/// the whole instance, so holding it is exactly as common as being wired to
/// this server at all — and it must not open somebody else's repository.
#[tokio::test]
async fn a_valid_signature_does_not_authorize_an_outsider() {
    let secret = "shared-webhook-secret-1234567890";
    let (base, db) = spawn_test_app_with_webhook_secret(secret).await;
    let (owner_token, _owner_id) = register_full(&base, "xcis_owner", "xcis_o@e.com").await;
    let (outsider_token, _outsider_id) =
        register_full(&base, "xcis_outsider", "xcis_x@e.com").await;
    create_private_repo(&base, &owner_token, "secret").await;

    let signature = {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(BODY.as_bytes());
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    };

    let status = post_status(
        &base,
        "xcis_owner",
        "secret",
        Some(&outsider_token),
        Some(signature.clone()),
    )
    .await;
    assert_eq!(
        status, 403,
        "the instance-wide webhook secret was accepted as authorization"
    );
    assert_eq!(commit_status_count(&db).await, 0);

    // And the signed path still works for somebody who may write, so the 403
    // above is the gate talking and not a broken signature.
    let status = post_status(
        &base,
        "xcis_owner",
        "secret",
        Some(&owner_token),
        Some(signature),
    )
    .await;
    assert_eq!(status, 200, "a correctly signed owner request was refused");
    assert_eq!(commit_status_count(&db).await, 1);
}
