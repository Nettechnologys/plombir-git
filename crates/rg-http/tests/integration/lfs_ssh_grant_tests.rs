//! The HTTP half of Git LFS for clones made from the SSH address
//! (card_d8d274ed134d).
//!
//! git-lfs asks the SSH remote, with `git-lfs-authenticate`, where the LFS
//! endpoint is and which `Authorization` to send it; `rg-ssh` answers with a
//! grant minted by `rg_core::lfs::service::sign_ssh_grant` after its own gate
//! said yes. These tests mint the same grant directly and present it the way
//! git-lfs does, so what they pin is the batch handler's side of the contract:
//! the grant opens exactly its repository and operation, the actions it hands
//! out are issued to the SSH credential, and revoking that credential revokes
//! both. The SSH half — that the grant is only minted past the SSH gate — lives
//! in `rg-ssh`'s `ssh_lfs_authenticate_tests`.

use rg_core::lfs::service::{
    sign_ssh_grant, LfsActionKind, LfsActor, LfsCredential, SSH_GRANT_AUTH_SCHEME,
};
use sea_orm::{ActiveValue::Set, NotSet};
use sha2::{Digest, Sha256};

use crate::common::{register_full, spawn_test_app_with_db};

/// The signing secret the test app is started with (`common::spawn_*`).
const TEST_JWT_SECRET: &[u8] = b"test-secret-key";

async fn create_repo(base: &str, token: &str, name: &str, is_private: bool) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": is_private}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// A batch request shaped like git-lfs 3.x sends it, authenticated with
/// `Authorization` exactly as `git-lfs-authenticate` told the client to.
async fn batch(
    base: &str,
    owner: &str,
    repo: &str,
    authorization: Option<&str>,
    operation: &str,
    oid: &str,
    size: usize,
) -> reqwest::Response {
    let mut request = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/lfs/objects/batch"
        ))
        .header("Accept", "application/vnd.git-lfs+json")
        .json(&serde_json::json!({
            "operation": operation,
            "objects": [{"oid": oid, "size": size}],
            "transfers": ["lfs-standalone-file", "basic", "ssh"],
        }));
    if let Some(authorization) = authorization {
        request = request.header("Authorization", authorization);
    }
    request.send().await.unwrap()
}

fn grant(action: LfsActionKind, repo_id: i64, actor: LfsActor) -> String {
    let expires_at = chrono::Utc::now().timestamp() + 600;
    let token = sign_ssh_grant(TEST_JWT_SECRET, action, repo_id, expires_at, actor).unwrap();
    format!("{SSH_GRANT_AUTH_SCHEME} {token}")
}

async fn add_ssh_key(db: &rg_db::DatabaseConnection, user_id: i64) -> i64 {
    rg_db::ops::ssh_key_ops::create(
        db,
        rg_db::entities::ssh_key::ActiveModel {
            id: NotSet,
            user_id: Set(user_id),
            title: Set("laptop".to_string()),
            public_key: Set(format!("ssh-ed25519 AAAA{user_id} test")),
            fingerprint: Set(format!("SHA256:lfs-grant-test-{user_id}")),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap()
    .id
}

async fn add_deploy_key(db: &rg_db::DatabaseConnection, repo_id: i64, read_only: bool) -> i64 {
    rg_db::ops::deploy_key_ops::create(
        db,
        rg_db::entities::deploy_key::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            created_by_id: Set(None),
            title: Set("ci".to_string()),
            public_key: Set(format!("ssh-ed25519 BBBB{repo_id}{read_only} test")),
            fingerprint: Set(format!("SHA256:lfs-deploy-test-{repo_id}-{read_only}")),
            read_only: Set(read_only),
            created_at: Set(chrono::Utc::now()),
            last_used_at: Set(None),
        },
    )
    .await
    .unwrap()
    .id
}

/// Store one object through the owner's ordinary session, so the SSH-side
/// downloads below have something to fetch.
async fn store_object(base: &str, owner: &str, repo: &str, token: &str, content: &[u8]) -> String {
    let oid = hex::encode(Sha256::digest(content));
    let response = batch(
        base,
        owner,
        repo,
        Some(&format!("Bearer {token}")),
        "upload",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(response.status(), 200);
    let body = response.json::<serde_json::Value>().await.unwrap();
    let href = body["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap()
        .to_string();
    let stored = reqwest::Client::new()
        .put(href)
        .body(content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200);
    oid
}

fn download_href(body: &serde_json::Value) -> String {
    body["objects"][0]["actions"]["download"]["href"]
        .as_str()
        .unwrap_or_else(|| panic!("no download action in {body}"))
        .to_string()
}

#[tokio::test]
async fn an_ssh_key_grant_downloads_from_a_private_repository_until_the_key_is_deleted() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "ssh_lfs_owner", "ssh_lfs_owner@example.com").await;
    let repo_id = create_repo(&base, &owner_token, "ssh-lfs", true).await;
    let content = b"object fetched by a clone made from the SSH address";
    let oid = store_object(&base, "ssh_lfs_owner", "ssh-lfs", &owner_token, content).await;
    let key_id = add_ssh_key(&db, owner_id).await;
    let actor = LfsActor::User {
        user_id: owner_id,
        credential: LfsCredential::SshKey { id: key_id },
    };
    let authorization = grant(LfsActionKind::Download, repo_id, actor);

    // Baseline: without the grant a private repository is closed.
    let anonymous = batch(&base, "ssh_lfs_owner", "ssh-lfs", None, "download", &oid, 1).await;
    assert_eq!(anonymous.status(), 401);

    let granted = batch(
        &base,
        "ssh_lfs_owner",
        "ssh-lfs",
        Some(&authorization),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(granted.status(), 200);
    let body = granted.json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["transfer"], "basic", "{body}");
    let href = download_href(&body);
    assert!(
        href.contains(&format!("&actor={owner_id}&ssh_key={key_id}")),
        "the action must be issued to the SSH key, so deleting the key revokes it: {href}"
    );

    let fetched = reqwest::Client::new().get(&href).send().await.unwrap();
    assert_eq!(fetched.status(), 200);
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), content);

    // Deleting the key is how an SSH credential is revoked. Both the grant and
    // the URL it produced must stop working at once, not when they run out.
    assert!(rg_db::ops::ssh_key_ops::delete_by_id(&db, key_id)
        .await
        .unwrap());
    let revoked_url = reqwest::Client::new().get(&href).send().await.unwrap();
    assert_eq!(revoked_url.status(), 401);
    let revoked_grant = batch(
        &base,
        "ssh_lfs_owner",
        "ssh-lfs",
        Some(&authorization),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(revoked_grant.status(), 401);
}

#[tokio::test]
async fn a_grant_opens_only_its_own_repository_and_operation_and_runs_out() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) =
        register_full(&base, "ssh_lfs_bound", "ssh_lfs_bound@example.com").await;
    let repo_id = create_repo(&base, &owner_token, "granted", true).await;
    create_repo(&base, &owner_token, "other", true).await;
    let content = b"bound grant";
    let oid = store_object(&base, "ssh_lfs_bound", "granted", &owner_token, content).await;
    let key_id = add_ssh_key(&db, owner_id).await;
    let actor = LfsActor::User {
        user_id: owner_id,
        credential: LfsCredential::SshKey { id: key_id },
    };
    let download = grant(LfsActionKind::Download, repo_id, actor);

    // The owner may write both repositories; the grant still opens one, for one
    // operation, because that is all the SSH gate was asked.
    let other_repo = batch(
        &base,
        "ssh_lfs_bound",
        "other",
        Some(&download),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(other_repo.status(), 403);
    let upload_with_download_grant = batch(
        &base,
        "ssh_lfs_bound",
        "granted",
        Some(&download),
        "upload",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(upload_with_download_grant.status(), 403);

    let expired = format!(
        "{SSH_GRANT_AUTH_SCHEME} {}",
        sign_ssh_grant(
            TEST_JWT_SECRET,
            LfsActionKind::Download,
            repo_id,
            chrono::Utc::now().timestamp() - 1,
            actor,
        )
        .unwrap()
    );
    let expired = batch(
        &base,
        "ssh_lfs_bound",
        "granted",
        Some(&expired),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(expired.status(), 401);

    let forged = format!(
        "{SSH_GRANT_AUTH_SCHEME} {}",
        sign_ssh_grant(
            b"not-the-server-secret",
            LfsActionKind::Download,
            repo_id,
            chrono::Utc::now().timestamp() + 600,
            actor,
        )
        .unwrap()
    );
    let forged = batch(
        &base,
        "ssh_lfs_bound",
        "granted",
        Some(&forged),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(forged.status(), 401);
}

#[tokio::test]
async fn a_grant_is_re_gated_at_the_batch_against_current_repository_access() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _) =
        register_full(&base, "ssh_lfs_regate", "ssh_lfs_regate@example.com").await;
    let (_, outsider_id) =
        register_full(&base, "ssh_lfs_outsider", "ssh_lfs_outsider@example.com").await;
    let repo_id = create_repo(&base, &owner_token, "private", true).await;
    let content = b"not for outsiders";
    let oid = store_object(&base, "ssh_lfs_regate", "private", &owner_token, content).await;
    let key_id = add_ssh_key(&db, outsider_id).await;

    // A grant that names an account with no access to the repository — the SSH
    // gate would never mint it, so it stands for one minted before the access
    // was taken away. The batch must ask the gate again rather than trust it.
    let authorization = grant(
        LfsActionKind::Download,
        repo_id,
        LfsActor::User {
            user_id: outsider_id,
            credential: LfsCredential::SshKey { id: key_id },
        },
    );
    let refused = batch(
        &base,
        "ssh_lfs_regate",
        "private",
        Some(&authorization),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(refused.status(), 403);
}

#[tokio::test]
async fn a_deploy_key_grant_reads_its_repository_and_writes_only_when_not_read_only() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, _) =
        register_full(&base, "ssh_lfs_deploy", "ssh_lfs_deploy@example.com").await;
    let repo_id = create_repo(&base, &owner_token, "deployed", true).await;
    let other_repo_id = create_repo(&base, &owner_token, "elsewhere", true).await;
    let content = b"fetched by CI with a deploy key";
    let oid = store_object(&base, "ssh_lfs_deploy", "deployed", &owner_token, content).await;
    let read_only = add_deploy_key(&db, repo_id, true).await;
    let writable = add_deploy_key(&db, repo_id, false).await;
    let foreign = add_deploy_key(&db, other_repo_id, false).await;

    let pull = batch(
        &base,
        "ssh_lfs_deploy",
        "deployed",
        Some(&grant(
            LfsActionKind::Download,
            repo_id,
            LfsActor::DeployKey { key_id: read_only },
        )),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(pull.status(), 200);
    let href = download_href(&pull.json::<serde_json::Value>().await.unwrap());
    assert!(
        href.contains(&format!("&deploy_key={read_only}")) && !href.contains("actor="),
        "a deploy key is not an account and must be echoed alone: {href}"
    );
    let fetched = reqwest::Client::new().get(&href).send().await.unwrap();
    assert_eq!(fetched.status(), 200);
    assert_eq!(fetched.bytes().await.unwrap().as_ref(), content);

    let new_content = b"pushed by a deploy key";
    let new_oid = hex::encode(Sha256::digest(new_content));
    let read_only_push = batch(
        &base,
        "ssh_lfs_deploy",
        "deployed",
        Some(&grant(
            LfsActionKind::Upload,
            repo_id,
            LfsActor::DeployKey { key_id: read_only },
        )),
        "upload",
        &new_oid,
        new_content.len(),
    )
    .await;
    assert_eq!(read_only_push.status(), 403);

    let foreign_pull = batch(
        &base,
        "ssh_lfs_deploy",
        "deployed",
        Some(&grant(
            LfsActionKind::Download,
            repo_id,
            LfsActor::DeployKey { key_id: foreign },
        )),
        "download",
        &oid,
        content.len(),
    )
    .await;
    assert_eq!(foreign_pull.status(), 403);

    let push = batch(
        &base,
        "ssh_lfs_deploy",
        "deployed",
        Some(&grant(
            LfsActionKind::Upload,
            repo_id,
            LfsActor::DeployKey { key_id: writable },
        )),
        "upload",
        &new_oid,
        new_content.len(),
    )
    .await;
    assert_eq!(push.status(), 200);
    let upload_href = push.json::<serde_json::Value>().await.unwrap()["objects"][0]["actions"]
        ["upload"]["href"]
        .as_str()
        .unwrap()
        .to_string();

    // Narrowing the key to read-only after the URL was issued takes effect on
    // the next request, not when the URL's six hours run out.
    sea_orm::ConnectionTrait::execute_unprepared(
        &db,
        &format!("UPDATE deploy_keys SET read_only = 1 WHERE id = {writable}"),
    )
    .await
    .unwrap();
    let narrowed = reqwest::Client::new()
        .put(&upload_href)
        .body(new_content.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(narrowed.status(), 403);

    assert!(rg_db::ops::deploy_key_ops::delete_by_id(&db, read_only)
        .await
        .unwrap());
    let removed = reqwest::Client::new().get(&href).send().await.unwrap();
    assert_eq!(removed.status(), 401);
}
