//! LFS locks from a clone made from the SSH address (card_8062fa65ca75).
//!
//! Such a clone reaches the locking API with the credential
//! `git-lfs-authenticate` minted on the SSH port, not with a session or a PAT:
//! git-lfs asks for an `upload` grant before it locks, unlocks or verifies, and
//! for a `download` grant before it lists. The grants are minted here directly
//! — the SSH half, that one is minted only past the SSH gate, lives in
//! `rg-ssh`'s `ssh_lfs_authenticate_tests` — so what is pinned is the locking
//! API's side: which grant opens which call, that a lock taken this way is the
//! key owner's, and that a deploy key can see locks but never hold one.

use rg_core::lfs::service::{
    sign_ssh_grant, LfsActionKind, LfsActor, LfsCredential, SSH_GRANT_AUTH_SCHEME,
};
use sea_orm::{ActiveValue::Set, NotSet};

use crate::common::{register_full, spawn_test_app_with_db};

/// The signing secret the test app is started with (`common::spawn_*`).
const TEST_JWT_SECRET: &[u8] = b"test-secret-key";
const OWNER: &str = "lock_ssh_owner";
const REPO: &str = "assets";

fn grant(action: LfsActionKind, repo_id: i64, actor: LfsActor) -> String {
    let expires_at = chrono::Utc::now().timestamp() + 600;
    let token = sign_ssh_grant(TEST_JWT_SECRET, action, repo_id, expires_at, actor).unwrap();
    format!("{SSH_GRANT_AUTH_SCHEME} {token}")
}

async fn call(
    method: reqwest::Method,
    url: &str,
    authorization: &str,
    body: Option<serde_json::Value>,
) -> (reqwest::StatusCode, serde_json::Value) {
    let mut request = reqwest::Client::new()
        .request(method, url)
        .header("Accept", "application/vnd.git-lfs+json")
        .header("Authorization", authorization);
    if let Some(body) = body {
        request = request
            .header("Content-Type", "application/vnd.git-lfs+json")
            .body(body.to_string());
    }
    let response = request.send().await.unwrap();
    let status = response.status();
    (
        status,
        response.json().await.unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn an_ssh_clone_locks_with_its_key_and_a_deploy_key_only_looks() {
    let (base, db) = spawn_test_app_with_db().await;
    let (session, owner_id) = register_full(&base, OWNER, "lock_ssh_owner@example.com").await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&session)
        .json(&serde_json::json!({ "name": REPO, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let key_id = rg_db::ops::ssh_key_ops::create(
        &db,
        rg_db::entities::ssh_key::ActiveModel {
            id: NotSet,
            user_id: Set(owner_id),
            title: Set("laptop".to_string()),
            public_key: Set("ssh-ed25519 AAAAlockssh test".to_string()),
            fingerprint: Set("SHA256:lfs-lock-ssh-test".to_string()),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap()
    .id;
    let deploy_key = |read_only: bool| {
        let db = db.clone();
        async move {
            rg_db::ops::deploy_key_ops::create(
                &db,
                rg_db::entities::deploy_key::ActiveModel {
                    id: NotSet,
                    repo_id: Set(repo_id),
                    created_by_id: Set(None),
                    title: Set("ci".to_string()),
                    public_key: Set(format!("ssh-ed25519 CCCC{read_only} test")),
                    fingerprint: Set(format!("SHA256:lfs-lock-deploy-{read_only}")),
                    read_only: Set(read_only),
                    created_at: Set(chrono::Utc::now()),
                    last_used_at: Set(None),
                },
            )
            .await
            .unwrap()
            .id
        }
    };
    let read_only_key = deploy_key(true).await;
    let writable_key = deploy_key(false).await;

    let user = LfsActor::User {
        user_id: owner_id,
        credential: LfsCredential::SshKey { id: key_id },
    };
    let locks = format!("{base}/api/v1/repos/{OWNER}/{REPO}/lfs/locks");
    let lock_body =
        serde_json::json!({"path": "maps/castle.level", "ref": {"name": "refs/heads/main"}});

    // `git lfs lock` from the SSH clone: an upload grant, and the lock is the
    // key owner's.
    let upload = grant(LfsActionKind::Upload, repo_id, user);
    let (status, body) = call(
        reqwest::Method::POST,
        &locks,
        &upload,
        Some(lock_body.clone()),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    assert_eq!(body["lock"]["owner"]["name"], OWNER);
    let lock_id = body["lock"]["id"].as_str().unwrap().to_string();

    // `git lfs locks`: a download grant lists. It does not lock.
    let download = grant(LfsActionKind::Download, repo_id, user);
    let (status, body) = call(reqwest::Method::GET, &locks, &download, None).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["locks"][0]["path"], "maps/castle.level");
    let other = serde_json::json!({"path": "maps/forest.level"});
    let (status, _) = call(
        reqwest::Method::POST,
        &locks,
        &download,
        Some(other.clone()),
    )
    .await;
    assert_eq!(status, 403, "a download grant took a lock");

    // A grant for another repository opens nothing here.
    let elsewhere = grant(LfsActionKind::Upload, repo_id + 1000, user);
    let (status, _) = call(reqwest::Method::GET, &locks, &elsewhere, None).await;
    assert_eq!(status, 403);

    // A read-only deploy key sees the locks and cannot take one — not even
    // with an upload grant, which the SSH gate would never mint for it.
    let ro = LfsActor::DeployKey {
        key_id: read_only_key,
    };
    let (status, body) = call(
        reqwest::Method::GET,
        &locks,
        &grant(LfsActionKind::Download, repo_id, ro),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["locks"][0]["path"], "maps/castle.level");
    let (status, _) = call(
        reqwest::Method::POST,
        &locks,
        &grant(LfsActionKind::Upload, repo_id, ro),
        Some(other.clone()),
    )
    .await;
    assert_eq!(status, 403, "a read-only deploy key took a lock");

    // A deploy key that may push holds no lock: it cannot take one, and every
    // lock is someone else's when it verifies before a push.
    let rw = grant(
        LfsActionKind::Upload,
        repo_id,
        LfsActor::DeployKey {
            key_id: writable_key,
        },
    );
    let (status, body) = call(reqwest::Method::POST, &locks, &rw, Some(other)).await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body["message"].as_str().unwrap().contains("deploy key"),
        "{body}"
    );
    let (status, body) = call(
        reqwest::Method::POST,
        &format!("{locks}/verify"),
        &rw,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["ours"], serde_json::json!([]));
    assert_eq!(body["theirs"][0]["path"], "maps/castle.level");

    // The key owner unlocks with the upload grant; deleting the key revokes
    // every grant minted for it.
    let (status, body) = call(
        reqwest::Method::POST,
        &format!("{locks}/{lock_id}/unlock"),
        &upload,
        Some(serde_json::json!({})),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(rg_db::ops::ssh_key_ops::delete_by_id(&db, key_id)
        .await
        .unwrap());
    let (status, _) = call(reqwest::Method::GET, &locks, &download, None).await;
    assert_eq!(status, 401, "a grant outlived the key it was minted for");
}
