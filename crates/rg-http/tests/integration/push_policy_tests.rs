//! What a push over HTTP is held to beyond branch and tag protection, driven by
//! the stock `git` client against the clone URL the UI shows.
//!
//! * The server's own namespaces refuse a client's write, ref by ref
//!   (card_e62ac71c4768).
//! * A pull mirror is read-only while it is enabled — for a push, a web edit
//!   and a merge alike — and writable again once it is switched off
//!   (card_97a2c0209056).
//! * A path someone else has locked with `git lfs lock` cannot be changed by a
//!   push, whether or not the client checks locks itself; the lock holder's
//!   own push goes through (card_4a40b70a6796). The same lock holds a web
//!   edit and the merge of a fork pull request (card_e486e8e09406).
//!
//! The SSH twin of each lives in `rg-ssh`'s `ssh_push_policy_tests`.

use std::path::{Path, PathBuf};

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

async fn pat_for(base: &str, session: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(session)
        .json(&serde_json::json!({ "name": "git", "scopes": "repo" }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201);
    response.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn create_initialised_repo(base: &str, session: &str, name: &str) -> i64 {
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(session)
        .json(&serde_json::json!({ "name": name, "is_private": true, "auto_init": true, "readme": "default" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    created.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

struct Outcome {
    success: bool,
    output: String,
}

/// git through the same gateway the server uses, as one named person.
fn git(cwd: &Path, who: &str, args: &[&str]) -> Outcome {
    let email = format!("{who}@example.com");
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run_with_env(
            args,
            Some(cwd),
            &[
                ("GIT_AUTHOR_NAME", who),
                ("GIT_AUTHOR_EMAIL", &email),
                ("GIT_COMMITTER_NAME", who),
                ("GIT_COMMITTER_EMAIL", &email),
            ],
        )
        .expect("run git");
    Outcome {
        success: output.success(),
        output: format!("{}{}", output.stdout_str(), output.stderr_str()),
    }
}

fn git_ok(cwd: &Path, who: &str, args: &[&str]) -> String {
    let outcome = git(cwd, who, args);
    assert!(
        outcome.success,
        "{who} ran git {args:?} and it failed:\n{}",
        outcome.output
    );
    outcome.output.trim().to_string()
}

/// A working copy of `owner/repo` for `who`, cloned over HTTP with a PAT.
fn checkout(root: &Path, base: &str, who: &str, pat: &str, owner: &str, repo: &str) -> PathBuf {
    let address = base.trim_start_matches("http://");
    let url = format!("http://{who}:{pat}@{address}/git/{owner}/{repo}");
    let dir = format!("{who}-{repo}");
    git_ok(root, who, &["clone", "-q", &url, &dir]);
    root.join(dir)
}

fn commit(work: &Path, who: &str, file: &str, contents: &str) -> String {
    std::fs::write(work.join(file), contents).unwrap();
    git_ok(work, who, &["add", file]);
    git_ok(work, who, &["commit", "-q", "-m", file]);
    git_ok(work, who, &["rev-parse", "HEAD"])
}

/// Run blocking git on the blocking pool: the server under test shares this
/// runtime.
async fn on_machine<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    tokio::task::spawn_blocking(work).await.expect("git task")
}

fn refs_in(bare: &Path, prefixes: &[&str]) -> String {
    let mut args = vec!["for-each-ref", "--format=%(refname)"];
    args.extend_from_slice(prefixes);
    git_ok(bare, "server", &args)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_push_into_a_server_namespace_is_refused_ref_by_ref() {
    const OWNER: &str = "ns_push_owner";
    const REPO: &str = "namespaces";
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (session, _) = register_full(&base, OWNER, "ns_push_owner@example.com").await;
    let pat = pat_for(&base, &session).await;
    create_initialised_repo(&base, &session, REPO).await;
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_path_buf();
    let base_for_git = base.clone();

    let (pushed, head) = on_machine(move || {
        let work = checkout(&root_path, &base_for_git, OWNER, &pat, OWNER, REPO);
        let head = commit(&work, OWNER, "next.txt", "next\n");
        let pushed = git(
            &work,
            OWNER,
            &[
                "push",
                "origin",
                "HEAD:refs/heads/main",
                "HEAD:refs/merge-queue/1",
                "HEAD:refs/forks/x",
            ],
        );
        (pushed, head)
    })
    .await;

    assert!(
        !pushed.success,
        "a push into the server's namespaces was reported as a success:\n{}",
        pushed.output
    );
    for refname in ["refs/merge-queue/1", "refs/forks/x"] {
        assert!(
            pushed
                .output
                .lines()
                .any(|line| line.contains(refname) && line.contains("server-owned namespace")),
            "{refname} was not refused as a server namespace:\n{}",
            pushed.output
        );
    }
    let bare = repo_root.join(format!("{OWNER}/{REPO}.git"));
    assert_eq!(
        git_ok(&bare, "server", &["rev-parse", "refs/heads/main"]),
        head,
        "the allowed half of the push did not land"
    );
    assert_eq!(
        refs_in(&bare, &["refs/merge-queue/", "refs/forks/"]),
        "",
        "a refused ref was written anyway"
    );
    drop(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_pull_mirror_refuses_pushes_edits_and_merges_until_it_is_switched_off() {
    const OWNER: &str = "mirror_ro_owner";
    const REPO: &str = "mirrored";
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (session, _) = register_full(&base, OWNER, "mirror_ro_owner@example.com").await;
    let pat = pat_for(&base, &session).await;
    let repo_id = create_initialised_repo(&base, &session, REPO).await;
    let root = tempfile::tempdir().unwrap();
    let client = reqwest::Client::new();

    // A pull request between two branches, opened before the mirror existed.
    let work = {
        let (root_path, base, pat) = (root.path().to_path_buf(), base.clone(), pat.clone());
        on_machine(move || {
            let work = checkout(&root_path, &base, OWNER, &pat, OWNER, REPO);
            git_ok(&work, OWNER, &["checkout", "-q", "-b", "feature"]);
            commit(&work, OWNER, "feature.txt", "feature\n");
            git_ok(&work, OWNER, &["push", "-q", "origin", "feature"]);
            work
        })
        .await
    };
    let pr = client
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/pulls"))
        .bearer_auth(&session)
        .json(&serde_json::json!({ "title": "feature", "head": "feature", "base": "main" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pr.status(), 201, "{}", pr.text().await.unwrap());

    let now = chrono::Utc::now();
    let mirror = rg_db::ops::mirror_ops::create(
        &db,
        rg_db::entities::mirror::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: sea_orm::Set(repo_id),
            url: sea_orm::Set("https://example.com/upstream.git".to_string()),
            username: sea_orm::Set(None),
            password_encrypted: sea_orm::Set(None),
            sync_interval_seconds: sea_orm::Set(3600),
            next_sync_at: sea_orm::Set(None),
            last_sync_at: sea_orm::Set(None),
            last_sync_error: sea_orm::Set(None),
            status: sea_orm::Set(rg_db::entities::mirror::STATUS_ACTIVE.to_string()),
            created_at: sea_orm::Set(now),
            updated_at: sea_orm::Set(now),
        },
    )
    .await
    .unwrap();
    let bare = repo_root.join(format!("{OWNER}/{REPO}.git"));
    let main_before = git_ok(&bare, "server", &["rev-parse", "refs/heads/main"]);

    // A push: refused, and the branch stays where the upstream put it.
    let refused = {
        let work = work.clone();
        on_machine(move || {
            commit(&work, OWNER, "local.txt", "a local change\n");
            git(&work, OWNER, &["push", "origin", "feature"])
        })
        .await
    };
    assert!(
        !refused.success,
        "a push into an enabled pull mirror went through:\n{}",
        refused.output
    );
    assert!(
        refused.output.contains("pull mirror"),
        "the refusal does not say why:\n{}",
        refused.output
    );

    // A web edit and a merge: `409` each, and `main` does not move.
    let edit = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/contents/edited.txt"
        ))
        .bearer_auth(&session)
        .json(&serde_json::json!({ "content": "edited\n", "message": "edit in the browser" }))
        .send()
        .await
        .unwrap();
    assert_eq!(edit.status(), 409, "{}", edit.text().await.unwrap());
    let merge = client
        .post(format!("{base}/api/v1/repos/{OWNER}/{REPO}/pulls/1/merge"))
        .bearer_auth(&session)
        .json(&serde_json::json!({ "strategy": "merge" }))
        .send()
        .await
        .unwrap();
    let merge_status = merge.status();
    let merge_body = merge.text().await.unwrap();
    assert_eq!(merge_status, 409, "{merge_body}");
    assert!(merge_body.contains("pull mirror"), "{merge_body}");
    assert_eq!(
        git_ok(&bare, "server", &["rev-parse", "refs/heads/main"]),
        main_before
    );

    // Switched off, the repository is an ordinary one again.
    let mut switched_off: rg_db::entities::mirror::ActiveModel = mirror.into();
    switched_off.status = sea_orm::Set(rg_db::entities::mirror::STATUS_INACTIVE.to_string());
    rg_db::ops::mirror_ops::update(&db, switched_off)
        .await
        .unwrap();
    let accepted = {
        let work = work.clone();
        on_machine(move || git(&work, OWNER, &["push", "origin", "feature"])).await
    };
    assert!(
        accepted.success,
        "a switched-off mirror still refused the push:\n{}",
        accepted.output
    );
    drop(root);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_path_someone_else_has_locked_cannot_be_changed_by_a_push() {
    const ALICE: &str = "lock_push_alice";
    const BOB: &str = "lock_push_bob";
    const REPO: &str = "levels";
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (alice_session, _) = register_full(&base, ALICE, "lock_push_alice@example.com").await;
    let (bob_session, _) = register_full(&base, BOB, "lock_push_bob@example.com").await;
    let alice_pat = pat_for(&base, &alice_session).await;
    let bob_pat = pat_for(&base, &bob_session).await;
    create_initialised_repo(&base, &alice_session, REPO).await;
    let added = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{ALICE}/{REPO}/collaborators"))
        .bearer_auth(&alice_session)
        .json(&serde_json::json!({"username": BOB, "permission": "write"}))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), 201);
    let root = tempfile::tempdir().unwrap();

    let alice_work = {
        let (root_path, base, pat) = (root.path().to_path_buf(), base.clone(), alice_pat.clone());
        on_machine(move || {
            let work = checkout(&root_path, &base, ALICE, &pat, ALICE, REPO);
            commit(&work, ALICE, "castle.level", "castle v1\n");
            git_ok(&work, ALICE, &["push", "-q", "origin", "HEAD"]);
            work
        })
        .await
    };
    // Alice locks the file. Bob's client is never asked to check locks: no
    // `git lfs`, no `lfs.locksverify`.
    let locked = reqwest::Client::new()
        .post(format!("{base}/git/{ALICE}/{REPO}.git/info/lfs/locks"))
        .header("Accept", "application/vnd.git-lfs+json")
        .header("Content-Type", "application/vnd.git-lfs+json")
        .basic_auth(ALICE, Some(&alice_pat))
        .body(serde_json::json!({ "path": "castle.level" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(locked.status(), 201, "{}", locked.text().await.unwrap());

    let bare = repo_root.join(format!("{ALICE}/{REPO}.git"));
    let main_before = git_ok(&bare, "server", &["rev-parse", "refs/heads/main"]);
    let refused = {
        let (root_path, base) = (root.path().to_path_buf(), base.clone());
        on_machine(move || {
            let work = checkout(&root_path, &base, BOB, &bob_pat, ALICE, REPO);
            commit(&work, BOB, "castle.level", "castle v2 by bob\n");
            git(&work, BOB, &["push", "origin", "HEAD"])
        })
        .await
    };
    assert!(
        !refused.success,
        "bob's push of alice's locked file went through:\n{}",
        refused.output
    );
    assert!(
        refused
            .output
            .contains(&format!("path 'castle.level' is locked by {ALICE}")),
        "the refusal does not name the path and its holder:\n{}",
        refused.output
    );
    assert_eq!(
        git_ok(&bare, "server", &["rev-parse", "refs/heads/main"]),
        main_before
    );

    // The lock holder's own change is hers to push.
    let pushed = on_machine(move || {
        commit(&alice_work, ALICE, "castle.level", "castle v2 by alice\n");
        git(&alice_work, ALICE, &["push", "origin", "HEAD"])
    })
    .await;
    assert!(
        pushed.success,
        "the lock holder's push was refused:\n{}",
        pushed.output
    );
    drop(root);
}

/// The lock holds every way into a branch, not only `git push`: a web edit
/// and the merge of a fork pull request are refused with the path and its
/// holder, and the lock holder does both (card_e486e8e09406).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_path_someone_else_has_locked_cannot_be_changed_by_a_web_edit_or_a_fork_merge() {
    const ALICE: &str = "lock_edit_alice";
    const BOB: &str = "lock_edit_bob";
    const REPO: &str = "levels";
    let (base, _db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (alice_session, _) = register_full(&base, ALICE, "lock_edit_alice@example.com").await;
    let (bob_session, _) = register_full(&base, BOB, "lock_edit_bob@example.com").await;
    let alice_pat = pat_for(&base, &alice_session).await;
    let bob_pat = pat_for(&base, &bob_session).await;
    create_initialised_repo(&base, &alice_session, REPO).await;
    let client = reqwest::Client::new();
    let added = client
        .post(format!("{base}/api/v1/repos/{ALICE}/{REPO}/collaborators"))
        .bearer_auth(&alice_session)
        .json(&serde_json::json!({"username": BOB, "permission": "write"}))
        .send()
        .await
        .unwrap();
    assert_eq!(added.status(), 201);
    let root = tempfile::tempdir().unwrap();

    {
        let (root_path, base, pat) = (root.path().to_path_buf(), base.clone(), alice_pat.clone());
        on_machine(move || {
            let work = checkout(&root_path, &base, ALICE, &pat, ALICE, REPO);
            commit(&work, ALICE, "castle.level", "castle v1\n");
            git_ok(&work, ALICE, &["push", "-q", "origin", "HEAD"]);
        })
        .await;
    }
    let locked = client
        .post(format!("{base}/git/{ALICE}/{REPO}.git/info/lfs/locks"))
        .header("Accept", "application/vnd.git-lfs+json")
        .header("Content-Type", "application/vnd.git-lfs+json")
        .basic_auth(ALICE, Some(&alice_pat))
        .body(serde_json::json!({ "path": "castle.level" }).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(locked.status(), 201, "{}", locked.text().await.unwrap());

    let bare = repo_root.join(format!("{ALICE}/{REPO}.git"));
    let main = |bare: &Path| git_ok(bare, "server", &["rev-parse", "refs/heads/main"]);
    let refusal = format!("path 'castle.level' is locked by {ALICE}");
    let edit = |session: &str, content: &str| {
        let blob = git_ok(&bare, "server", &["rev-parse", "main:castle.level"]);
        client
            .post(format!(
                "{base}/api/v1/repos/{ALICE}/{REPO}/contents/castle.level"
            ))
            .bearer_auth(session)
            .json(&serde_json::json!({
                "content": content,
                "message": "edit in the browser",
                "sha": blob,
            }))
            .send()
    };

    // A web edit by someone else: refused, naming the path and the holder.
    let main_before = main(&bare);
    let refused = edit(&bob_session, "castle v2 by bob in the browser\n")
        .await
        .unwrap();
    let status = refused.status();
    let body = refused.text().await.unwrap();
    assert_eq!(status, 409, "{body}");
    assert!(body.contains(&refusal), "{body}");
    assert_eq!(main(&bare), main_before);

    // The holder edits her own locked file.
    let accepted = edit(&alice_session, "castle v2 by alice in the browser\n")
        .await
        .unwrap();
    let status = accepted.status();
    assert!(
        status.is_success(),
        "the lock holder's web edit was refused: {status} {}",
        accepted.text().await.unwrap()
    );

    // Bob's fork has no locks of its own, so the push into it lands; the merge
    // into Alice's repository is where her lock applies.
    let fork = client
        .post(format!("{base}/api/v1/repos/{ALICE}/{REPO}/fork"))
        .bearer_auth(&bob_session)
        .send()
        .await
        .unwrap();
    assert_eq!(fork.status(), 201, "{}", fork.text().await.unwrap());
    {
        let (root_path, base) = (root.path().to_path_buf(), base.clone());
        on_machine(move || {
            let work = checkout(&root_path, &base, BOB, &bob_pat, BOB, REPO);
            git_ok(&work, BOB, &["checkout", "-q", "-b", "bob-castle"]);
            commit(&work, BOB, "castle.level", "castle v3 by bob\n");
            git_ok(&work, BOB, &["push", "-q", "origin", "bob-castle"]);
        })
        .await;
    }
    let pr = client
        .post(format!("{base}/api/v1/repos/{ALICE}/{REPO}/pulls"))
        .bearer_auth(&bob_session)
        .json(&serde_json::json!({
            "title": "bob's castle",
            "head": format!("{BOB}:bob-castle"),
            "base": "main",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pr.status(), 201, "{}", pr.text().await.unwrap());
    let number = pr.json::<serde_json::Value>().await.unwrap()["number"]
        .as_i64()
        .unwrap();
    let merge = |session: &str| {
        client
            .post(format!(
                "{base}/api/v1/repos/{ALICE}/{REPO}/pulls/{number}/merge"
            ))
            .bearer_auth(session)
            .json(&serde_json::json!({ "strategy": "squash" }))
            .send()
    };

    let main_before = main(&bare);
    let refused = merge(&bob_session).await.unwrap();
    let status = refused.status();
    let body = refused.text().await.unwrap();
    assert_eq!(status, 409, "{body}");
    assert!(body.contains(&refusal), "{body}");
    assert_eq!(main(&bare), main_before);
    assert_eq!(
        refs_in(&bare, &["refs/forks/"]),
        "",
        "the refused merge left its scratch fork ref behind"
    );

    let merged = merge(&alice_session).await.unwrap();
    let status = merged.status();
    assert!(
        status.is_success(),
        "the lock holder's merge was refused: {status} {}",
        merged.text().await.unwrap()
    );
    assert_ne!(main(&bare), main_before);
    drop(root);
}
