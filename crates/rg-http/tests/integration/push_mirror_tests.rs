//! Push mirrors over the API and through a real push (card_5d25bc3276a3).
//!
//! Three promises are held here: the settings are repository administration,
//! the remote's credential never comes back — not in a response and not in the
//! audit journal — and a push into this server is accepted while its mirror is
//! unreachable, leaving the mirror marked behind for the background pass.

use std::path::Path;

use base64::Engine as _;
use sea_orm::EntityTrait;

use crate::common::{
    create_repo, register_full, spawn_test_app_with_db_and_repo_root, TEST_ENCRYPTION_KEY,
};

const SECRET: &str = "pm-s3cret-token";

fn git(args: &[&str], cwd: Option<&Path>) -> rg_git::cli_gateway::GitOutput {
    rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize")
        .run(args, cwd)
        .expect("git invocation must start")
}

fn git_ok(args: &[&str], cwd: Option<&Path>) {
    let output = git(args, cwd);
    assert!(
        output.success(),
        "git {args:?} failed: {}",
        output.stderr_str().trim()
    );
}

async fn add_collaborator(base: &str, token: &str, repo: &str, username: &str, permission: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/pm-owner/{repo}/collaborators"))
        .bearer_auth(token)
        .json(&serde_json::json!({"username": username, "permission": permission}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "add_collaborator '{username}' failed");
}

async fn create_pat(base: &str, token: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": "push-mirror-git" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn push_mirrors_are_administration_and_never_hand_back_their_credential() {
    let (base, db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, "pm-owner", "pm-owner@example.com").await;
    let (writer, _) = register_full(&base, "pm-writer", "pm-writer@example.com").await;
    create_repo(&base, &owner, "mirrored").await;
    create_repo(&base, &owner, "other").await;
    add_collaborator(&base, &owner, "mirrored", "pm-writer", "write").await;
    let client = reqwest::Client::new();
    let list = format!("{base}/api/v1/repos/pm-owner/mirrored/push_mirrors");
    let body = serde_json::json!({
        "url": format!("https://mirror-bot:{SECRET}@example.com/org/mirrored.git"),
        "sync_on_commit": true,
    });

    // Write access is not enough: the mirror hands the repository and a
    // credential for another forge to an address the caller picks.
    let refused = client
        .post(&list)
        .bearer_auth(&writer)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);
    let refused = client.get(&list).bearer_auth(&writer).send().await.unwrap();
    assert_eq!(refused.status(), 403);

    let created = client
        .post(&list)
        .bearer_auth(&owner)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let text = created.text().await.unwrap();
    assert!(!text.contains(SECRET), "the credential came back: {text}");
    let mirror: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(mirror["url"], "https://example.com/org/mirrored.git");
    assert_eq!(mirror["username"], "mirror-bot");
    assert_eq!(mirror["has_credentials"], true);
    assert_eq!(
        mirror["force"], false,
        "overwriting the remote is never the default"
    );
    let id = mirror["id"].as_i64().unwrap();

    let listed = client.get(&list).bearer_auth(&owner).send().await.unwrap();
    assert_eq!(listed.status(), 200);
    let text = listed.text().await.unwrap();
    assert!(!text.contains(SECRET), "the credential came back: {text}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text)
            .unwrap()
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // The id is only addressable under its own repository.
    let elsewhere = client
        .patch(format!(
            "{base}/api/v1/repos/pm-owner/other/push_mirrors/{id}"
        ))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"force": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(elsewhere.status(), 404);

    let updated = client
        .patch(format!("{list}/{id}"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"force": true, "password": "replacement-token"}))
        .send()
        .await
        .unwrap();
    assert_eq!(updated.status(), 200);
    let text = updated.text().await.unwrap();
    assert!(
        !text.contains("replacement-token"),
        "the credential came back: {text}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&text).unwrap()["force"],
        true
    );

    let queued = client
        .post(format!("{list}/{id}/sync"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), 202);

    let deleted = client
        .delete(format!("{list}/{id}"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), 204);
    let gone = client
        .post(format!("{list}/{id}/sync"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 404);

    // Every write was journalled, and none of the entries holds a secret.
    let journal = rg_db::entities::audit_log::Entity::find()
        .all(&db)
        .await
        .unwrap();
    let entries: Vec<_> = journal
        .iter()
        .filter(|entry| entry.action.starts_with("repo.push_mirror_"))
        .collect();
    assert_eq!(
        entries.len(),
        3,
        "create, update and delete each leave an entry"
    );
    for entry in entries {
        let details = entry.details.clone().unwrap_or_default();
        assert!(
            !details.contains(SECRET) && !details.contains("replacement-token"),
            "{} journalled the credential: {details}",
            entry.action
        );
    }
}

/// The acceptance's other half: the mirror being down is the mirror's
/// problem. The push into this server succeeds, the mirror is marked behind
/// by the push hooks, and the background pass records why it could not
/// deliver — without the credential in the message.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_is_accepted_while_its_mirror_is_unreachable() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, "pm-owner", "pm-owner@example.com").await;
    let repo_id = create_repo(&base, &owner, "pushed").await;
    let pat = create_pat(&base, &owner).await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/pm-owner/pushed/push_mirrors"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({
            // `.invalid` never resolves, so the pass fails at the SSRF guard's
            // DNS step — an unreachable remote without a network.
            "url": "https://mirror.example.invalid/org/pushed.git",
            "username": "mirror-bot",
            "password": SECRET,
            "sync_interval_seconds": 0,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let revision_before = rg_db::ops::push_mirror_ops::find_for_repo(&db, repo_id, id)
        .await
        .unwrap()
        .unwrap()
        .pending_revision;

    let seed = tempfile::tempdir().unwrap();
    git_ok(&["init", "--initial-branch=main"], Some(seed.path()));
    git_ok(&["config", "user.name", "Push Mirror"], Some(seed.path()));
    git_ok(
        &["config", "user.email", "pm@example.com"],
        Some(seed.path()),
    );
    std::fs::write(seed.path().join("README.md"), "mirrored\n").unwrap();
    git_ok(&["add", "README.md"], Some(seed.path()));
    git_ok(&["commit", "-m", "first"], Some(seed.path()));
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("pm-owner:{pat}"));
    let header = format!("http.extraHeader=Authorization: Basic {basic}");
    let url = format!("{base}/pm-owner/pushed.git");
    git_ok(&["-c", &header, "push", &url, "main"], Some(seed.path()));

    let marked = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            let row = rg_db::ops::push_mirror_ops::find_for_repo(&db, repo_id, id)
                .await
                .unwrap()
                .unwrap();
            if row.pending_revision > revision_before {
                return row;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the push hooks never marked the push mirror behind");
    assert!(marked.pending_since.is_some());
    assert!(
        marked
            .next_sync_at
            .is_some_and(|next| next <= chrono::Utc::now()),
        "a sync-on-commit mirror is not due after a push: {marked:?}"
    );

    let ran = rg_core::mirror::push::push_due_mirrors(
        &db,
        &repo_root,
        10,
        Default::default(),
        TEST_ENCRYPTION_KEY,
    )
    .await
    .expect("an unreachable mirror is not the pass's error");
    assert_eq!(ran, 1);
    let failed = rg_db::ops::push_mirror_ops::find_for_repo(&db, repo_id, id)
        .await
        .unwrap()
        .unwrap();
    let error = failed
        .last_sync_error
        .clone()
        .expect("the failure is recorded");
    assert!(
        !error.contains(SECRET),
        "the credential reached the error: {error}"
    );
    assert!(
        failed.pending_since.is_some(),
        "a failed pass cleared the lag"
    );
    assert!(failed
        .next_sync_at
        .is_some_and(|next| next > chrono::Utc::now()));

    // The API shows the lag and the reason to whoever administers the repo.
    let listed: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/pm-owner/pushed/push_mirrors"))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed[0]["pending_since"].is_string(), "{listed}");
    assert!(listed[0]["last_sync_error"].is_string(), "{listed}");
}
