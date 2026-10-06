//! Regression coverage for card_90232d56a1ca: merging a pull request from a
//! fork must leave the base repository able to serve every LFS object the
//! merged commits point at.
//!
//! The author uploads their LFS objects to the fork, so the objects are rows
//! of the fork's `repo_id` and bytes under the fork's `<owner>/<name>`. A merge
//! used to move only Git objects, which put the pointers on the base branch
//! with nothing behind them: the next `git lfs pull` of the base repository
//! got `404 object not found`, and neither the author nor the reviewer ever
//! saw it.
//!
//! Every strategy is merged, because squash and rebase write new commits and
//! the objects must follow the blobs, not the commit ids. An object only the
//! base repository has proves the merge does not demand what is already
//! there, and an object nobody has proves the merge refuses, by name, instead
//! of publishing a pointer to nothing.

use sha2::{Digest, Sha256};

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

const OWNER: &str = "lfs_merge_owner";
const FORKER: &str = "lfs_merge_forker";
const REPO: &str = "lfs-merge-assets";

fn oid_of(payload: &[u8]) -> String {
    hex::encode(Sha256::digest(payload))
}

fn pointer_for(payload: &[u8]) -> String {
    format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n",
        oid_of(payload),
        payload.len()
    )
}

fn git(args: &[&str], cwd: Option<&std::path::Path>) -> String {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(args, cwd)
        .expect("run git");
    output.ensure_success().expect("git command succeeds");
    output.stdout_str().trim().to_string()
}

/// Commit `files` on a new `branch` of the bare repository at `bare_path`,
/// branching from its `main`.
fn push_branch(bare_path: &std::path::Path, branch: &str, files: &[(&str, String)]) {
    let worktree = tempfile::tempdir().expect("create branch worktree");
    let path = worktree.path();
    let path_arg = path.to_str().expect("UTF-8 worktree path");
    let bare_arg = bare_path.to_str().expect("UTF-8 bare repository path");

    git(&["clone", "-q", "-b", "main", bare_arg, path_arg], None);
    git(&["config", "user.name", "LFS merge test"], Some(path));
    git(
        &["config", "user.email", "lfs-merge@example.invalid"],
        Some(path),
    );
    git(&["checkout", "-q", "-b", branch], Some(path));
    for (name, content) in files {
        std::fs::write(path.join(name), content).expect("write branch file");
    }
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", branch], Some(path));
    git(&["push", "-q", "origin", branch], Some(path));
}

/// Store an object in `owner/REPO` the way `git lfs push` does: announce it
/// through the batch API, then `PUT` the bytes to the signed href.
async fn upload_object(base: &str, token: &str, owner: &str, payload: &[u8]) {
    let client = reqwest::Client::new();
    let batch = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{REPO}/lfs/objects/batch"
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
    assert_eq!(batch.status(), 200, "LFS upload batch failed");
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
    assert_eq!(stored.status(), 200, "LFS object upload failed");
}

/// What `owner/REPO` answers to a `git lfs pull` of `payload`: the bytes, or
/// the per-object error.
async fn download(
    base: &str,
    token: &str,
    owner: &str,
    payload: &[u8],
) -> Result<Vec<u8>, serde_json::Value> {
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
    assert_eq!(response.status(), 200, "LFS download batch failed");
    let body = response.json::<serde_json::Value>().await.unwrap();
    let object = &body["objects"][0];
    let Some(href) = object["actions"]["download"]["href"].as_str() else {
        return Err(object.clone());
    };
    let bytes = reqwest::get(href).await.unwrap();
    assert_eq!(bytes.status(), 200, "download href {href} failed");
    Ok(bytes.bytes().await.unwrap().to_vec())
}

struct Fixture {
    base: String,
    repo_root: std::path::PathBuf,
    owner_token: String,
    forker_token: String,
}

impl Fixture {
    async fn new() -> Self {
        let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
        let (owner_token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.com")).await;
        let (forker_token, _) =
            register_full(&base, FORKER, &format!("{FORKER}@example.com")).await;

        let created = reqwest::Client::new()
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({
                "name": REPO,
                "is_private": false,
                "auto_init": true,
                "readme": "default",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), 201, "seeding the base repository failed");
        let fork = reqwest::Client::new()
            .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/fork"))
            .bearer_auth(&forker_token)
            .send()
            .await
            .unwrap();
        assert_eq!(fork.status(), 201, "fork failed");

        Self {
            base,
            repo_root,
            owner_token,
            forker_token,
        }
    }

    fn fork_path(&self) -> std::path::PathBuf {
        self.repo_root.join(format!("{FORKER}/{REPO}.git"))
    }

    fn base_tip(&self) -> String {
        git(
            &["rev-parse", "refs/heads/main"],
            Some(&self.repo_root.join(format!("{OWNER}/{REPO}.git"))),
        )
    }

    async fn open_pr(&self, branch: &str) -> i64 {
        let created = reqwest::Client::new()
            .post(format!("{}/api/v1/repos/{OWNER}/{REPO}/pulls", self.base))
            .bearer_auth(&self.forker_token)
            .json(&serde_json::json!({
                "title": branch,
                "head": format!("{FORKER}:{branch}"),
                "base": "main",
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), 201, "opening the fork PR failed");
        created.json::<serde_json::Value>().await.unwrap()["number"]
            .as_i64()
            .expect("PR number")
    }

    async fn merge(&self, number: i64, strategy: &str) -> (reqwest::StatusCode, serde_json::Value) {
        let response = reqwest::Client::new()
            .post(format!(
                "{}/api/v1/repos/{OWNER}/{REPO}/pulls/{number}/merge",
                self.base
            ))
            .bearer_auth(&self.owner_token)
            .json(&serde_json::json!({"strategy": strategy}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.json().await.unwrap_or_default())
    }
}

#[tokio::test]
async fn every_merge_strategy_gives_the_base_repository_the_forks_lfs_objects() {
    let fixture = Fixture::new().await;
    let base = fixture.base.as_str();

    // Already in the base repository and never uploaded to the fork: the merge
    // must not demand it from the fork.
    let base_only = b"an LFS object the base repository already has".to_vec();
    upload_object(base, &fixture.owner_token, OWNER, &base_only).await;

    for strategy in ["merge", "squash", "rebase"] {
        let branch = format!("lfs-{strategy}");
        let payload = format!("an LFS object the fork brings with a {strategy} merge").into_bytes();
        upload_object(base, &fixture.forker_token, FORKER, &payload).await;
        push_branch(
            &fixture.fork_path(),
            &branch,
            &[
                (&format!("{strategy}.bin"), pointer_for(&payload)),
                (&format!("{strategy}-shared.bin"), pointer_for(&base_only)),
            ],
        );

        let before = download(base, &fixture.owner_token, OWNER, &payload).await;
        assert!(
            before.is_err(),
            "the base repository served the fork's {strategy} object before any merge"
        );

        let number = fixture.open_pr(&branch).await;
        let (status, body) = fixture.merge(number, strategy).await;
        assert_eq!(status, 200, "the {strategy} merge failed: {body}");

        match download(base, &fixture.owner_token, OWNER, &payload).await {
            Ok(bytes) => assert_eq!(
                bytes, payload,
                "the base repository returned different bytes after a {strategy} merge"
            ),
            Err(error) => panic!(
                "after a {strategy} merge the base repository cannot serve the object the \
                 merged pointer names: {error}"
            ),
        }
    }

    // The fork keeps its own objects: the base repository got links, not the
    // originals.
    let kept = b"an LFS object the fork brings with a merge merge".to_vec();
    assert_eq!(
        download(base, &fixture.forker_token, FORKER, &kept).await,
        Ok(kept),
        "the fork lost its own object to the merge"
    );
}

#[tokio::test]
async fn a_fork_pull_request_pointing_at_an_object_nobody_stores_is_refused_until_it_is_uploaded() {
    let fixture = Fixture::new().await;
    let base = fixture.base.as_str();

    // Committed as a pointer, never `git lfs push`ed.
    let never_uploaded = b"an LFS object the author forgot to push".to_vec();
    let oid = oid_of(&never_uploaded);
    push_branch(
        &fixture.fork_path(),
        "forgot-lfs-push",
        &[("forgot.bin", pointer_for(&never_uploaded))],
    );
    let number = fixture.open_pr("forgot-lfs-push").await;
    let tip_before = fixture.base_tip();

    let (status, body) = fixture.merge(number, "merge").await;
    assert_eq!(
        status, 409,
        "a merge that would publish a pointer to nothing must be refused as the \
         author's to fix: {body}"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&oid) && message.contains("git lfs push"),
        "the refusal must name the missing object and how to supply it, got: {body}"
    );
    assert_eq!(
        fixture.base_tip(),
        tip_before,
        "the base branch moved although the merge was refused"
    );

    // The author pushes the object to the fork; the same pull request now
    // merges, and the base repository serves it.
    upload_object(base, &fixture.forker_token, FORKER, &never_uploaded).await;
    let (status, body) = fixture.merge(number, "merge").await;
    assert_eq!(
        status, 200,
        "the merge must go through once the object is uploaded: {body}"
    );
    assert_eq!(
        download(base, &fixture.owner_token, OWNER, &never_uploaded).await,
        Ok(never_uploaded),
        "the base repository cannot serve the object after the retried merge"
    );
}
