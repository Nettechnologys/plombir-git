//! A repository's LFS store: its size, and removing objects nothing needs
//! (card_9e4dd3f8330c).
//!
//! An object used to live until its repository was deleted, so a force-push
//! left bytes in the store that nobody could see or remove. What is pinned:
//! an object a branch points at and a freshly uploaded one are never offered
//! for removal, one only a force-pushed-away commit pointed at is; removal
//! re-checks and keeps what it must, with the reason; and an object a fork
//! shares stays downloadable from the fork after the source removes it.

use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sha2::{Digest, Sha256};

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

const OWNER: &str = "lfs_gc_owner";
const WRITER: &str = "lfs_gc_writer";
const FORKER: &str = "lfs_gc_forker";
const REPO: &str = "assets";

fn oid_of(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

fn pointer_text(payload: &[u8]) -> String {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        oid_of(payload),
        payload.len()
    )
}

async fn upload_object(base: &str, token: &str, payload: &[u8]) {
    let client = reqwest::Client::new();
    let batch = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid_of(payload), "size": payload.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(batch.status(), 200);
    let batch = batch.json::<serde_json::Value>().await.unwrap();
    let href = batch["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .expect("upload href")
        .to_string();
    let stored = client
        .put(href)
        .body(payload.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(stored.status(), 200);
}

async fn commit_file(base: &str, token: &str, path: &str, content: &str) {
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/contents/{path}"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"content": content, "message": format!("add {path}")}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
}

/// What `git lfs pull` from `owner` would get for `payload`.
async fn download(base: &str, token: &str, owner: &str, payload: &[u8]) -> Option<Vec<u8>> {
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{REPO}/lfs/objects/batch"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "operation": "download",
            "objects": [{"oid": oid_of(payload), "size": payload.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.json::<serde_json::Value>().await.unwrap();
    let href = body["objects"][0]["actions"]["download"]["href"].as_str()?;
    let bytes = reqwest::Client::new()
        .get(href)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    (bytes.status() == 200).then_some(bytes.bytes().await.unwrap().to_vec())
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run(args, Some(repo))
        .unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

async fn get_json(url: &str, token: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let response = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap_or_default())
}

fn oids(objects: &serde_json::Value) -> Vec<String> {
    let mut oids: Vec<String> = objects
        .as_array()
        .unwrap()
        .iter()
        .map(|object| object["oid"].as_str().unwrap().to_string())
        .collect();
    oids.sort();
    oids
}

#[tokio::test]
async fn only_old_objects_no_ref_points_at_are_removed_and_a_fork_keeps_its_copy() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, OWNER, "lfs_gc_owner@example.com").await;
    let (writer, _) = register_full(&base, WRITER, "lfs_gc_writer@example.com").await;
    let (forker, _) = register_full(&base, FORKER, "lfs_gc_forker@example.com").await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"name": REPO, "is_private": false, "auto_init": true, "readme": "default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let collaborator = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/collaborators"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"username": WRITER, "permission": "write"}))
        .send()
        .await
        .unwrap();
    assert_eq!(collaborator.status(), 201);

    let on_branch = b"texture the main branch still uses".to_vec();
    let force_pushed = b"a model only a force-pushed-away commit used".to_vec();
    let fresh = b"uploaded a moment ago, its push not finished yet".to_vec();
    for payload in [&on_branch, &force_pushed, &fresh] {
        upload_object(&base, &owner, payload).await;
    }
    let bare = repo_root.join(format!("{OWNER}/{REPO}.git"));
    commit_file(
        &base,
        &owner,
        "textures/wall.png",
        &pointer_text(&on_branch),
    )
    .await;
    let before_force_push = git(&bare, &["rev-parse", "refs/heads/main"]);
    commit_file(
        &base,
        &owner,
        "models/ship.obj",
        &pointer_text(&force_pushed),
    )
    .await;

    // A fork taken now shares all three objects, and its history still
    // points at the model.
    let fork = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/fork"))
        .bearer_auth(&forker)
        .send()
        .await
        .unwrap();
    assert_eq!(fork.status(), 201);

    // The force-push: `main` moves back past the commit that added the model.
    git(
        &bare,
        &["update-ref", "refs/heads/main", &before_force_push],
    );
    // Two of the source's objects are two days old; the third was just uploaded.
    for payload in [&on_branch, &force_pushed] {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            format!(
                "UPDATE lfs_objects SET created_at = '2020-01-01T00:00:00Z' \
                 WHERE repo_id = {repo_id} AND oid = '{}'",
                oid_of(payload)
            ),
        ))
        .await
        .unwrap();
    }

    let api = format!("{base}/api/v1/repos/{OWNER}/{REPO}/lfs");
    let (status, usage) = get_json(&format!("{api}/usage"), &owner).await;
    assert_eq!(status, 200, "{usage}");
    assert_eq!(usage["object_count"], 3);
    assert_eq!(
        usage["total_bytes"],
        (on_branch.len() + force_pushed.len() + fresh.len()) as i64
    );
    let (status, listed) = get_json(&format!("{api}/objects"), &owner).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["objects"].as_array().unwrap().len(), 3);

    // Only the object the force-push left behind is offered.
    let (status, orphans) = get_json(&format!("{api}/orphans"), &owner).await;
    assert_eq!(status, 200, "{orphans}");
    assert_eq!(oids(&orphans["objects"]), vec![oid_of(&force_pushed)]);
    assert_eq!(orphans["grace_hours"], 24);

    // A writer is not an administrator.
    let (status, _) = get_json(&format!("{api}/orphans"), &writer).await;
    assert_eq!(status, 403);
    let refused = reqwest::Client::new()
        .post(format!("{api}/orphans/prune"))
        .bearer_auth(&writer)
        .json(&serde_json::json!({"oids": [oid_of(&force_pushed)]}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 403);

    // Asked to remove all three, the server removes the orphan and keeps the
    // other two, each with its reason.
    let pruned = reqwest::Client::new()
        .post(format!("{api}/orphans/prune"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"oids": [oid_of(&on_branch), oid_of(&force_pushed), oid_of(&fresh)]}))
        .send()
        .await
        .unwrap();
    assert_eq!(pruned.status(), 200);
    let pruned = pruned.json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        pruned["deleted"],
        serde_json::json!([oid_of(&force_pushed)])
    );
    let kept: std::collections::BTreeMap<String, String> = pruned["kept"]
        .as_array()
        .unwrap()
        .iter()
        .map(|kept| {
            (
                kept["oid"].as_str().unwrap().to_string(),
                kept["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert!(
        kept[&oid_of(&on_branch)].contains("points at it"),
        "{kept:?}"
    );
    assert!(kept[&oid_of(&fresh)].contains("too recently"), "{kept:?}");

    assert_eq!(download(&base, &owner, OWNER, &force_pushed).await, None);
    assert_eq!(
        download(&base, &owner, OWNER, &on_branch).await.as_deref(),
        Some(on_branch.as_slice())
    );
    assert_eq!(
        download(&base, &owner, OWNER, &fresh).await.as_deref(),
        Some(fresh.as_slice())
    );
    let (_, usage) = get_json(&format!("{api}/usage"), &owner).await;
    assert_eq!(usage["object_count"], 2);

    // The fork's copy is the fork's.
    assert_eq!(
        download(&base, &forker, FORKER, &force_pushed)
            .await
            .as_deref(),
        Some(force_pushed.as_slice()),
        "removing the source's object took the fork's copy with it"
    );
}

/// card_9907a20218b0: a push that the batch API told "already stored" sends no
/// bytes and moves its ref afterwards. Until the ref moves, an old object looks
/// exactly like an orphan, and removal used to take it — leaving the pushed
/// history pointing at an object the server no longer has.
#[tokio::test]
async fn an_object_a_push_was_told_is_stored_survives_removal_until_the_push_lands() {
    let (base, db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, OWNER, "lfs_gc_owner@example.com").await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"name": REPO, "is_private": false, "auto_init": true, "readme": "default"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let repo_id = created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    // An object uploaded two days ago that no ref points at any more.
    let reverted = b"an asset a revert is about to bring back".to_vec();
    upload_object(&base, &owner, &reverted).await;
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        format!(
            "UPDATE lfs_objects SET created_at = '2020-01-01T00:00:00Z' \
             WHERE repo_id = {repo_id} AND oid = '{}'",
            oid_of(&reverted)
        ),
    ))
    .await
    .unwrap();
    let api = format!("{base}/api/v1/repos/{OWNER}/{REPO}/lfs");
    let (_, orphans) = get_json(&format!("{api}/orphans"), &owner).await;
    assert_eq!(oids(&orphans["objects"]), vec![oid_of(&reverted)]);

    // The revert's `git lfs push`: the batch says the server has it.
    let batch = reqwest::Client::new()
        .post(format!("{api}/objects/batch"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({
            "operation": "upload",
            "objects": [{"oid": oid_of(&reverted), "size": reverted.len()}],
            "transfers": ["basic"]
        }))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(
        batch["objects"][0]["actions"].is_null(),
        "the server already stores the object, so the client sends nothing: {batch}"
    );

    // An administrator removes unused objects before the ref update arrives.
    let (_, orphans) = get_json(&format!("{api}/orphans"), &owner).await;
    assert_eq!(
        oids(&orphans["objects"]),
        Vec::<String>::new(),
        "a claimed object is not offered for removal"
    );
    let pruned = reqwest::Client::new()
        .post(format!("{api}/orphans/prune"))
        .bearer_auth(&owner)
        .json(&serde_json::json!({"oids": [oid_of(&reverted)]}))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(pruned["deleted"], serde_json::json!([]));
    assert_eq!(
        pruned["kept"][0]["reason"],
        "a push was recently told it is stored and may still point a ref at it"
    );

    // The push lands, and what it points at is still there.
    commit_file(&base, &owner, "assets/back.bin", &pointer_text(&reverted)).await;
    assert_eq!(
        download(&base, &owner, OWNER, &reverted).await.as_deref(),
        Some(reverted.as_slice())
    );
}
