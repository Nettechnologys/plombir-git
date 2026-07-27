//! Regression coverage for card_3b3525983401: `POST /repos/{owner}/{name}/fork`
//! answered `500` to every caller on every repository, and nothing noticed
//! because the feature had no test of its own at all.
//!
//! Two defects sat on top of each other, and only the first one was reachable:
//!
//! - `fork_repo` ran the *target* path through `path_to_git_url`, whose first
//!   act is `fs::canonicalize`. The target is precisely what the clone brings
//!   into existence, so it never exists yet — ENOENT, every time.
//! - Behind that, the target was handed to `git clone` as a `file://` URL. Git
//!   reads a clone *destination* as a plain filesystem path, never as a URL, so
//!   a fork that got past the canonicalize would have written a literal `file:`
//!   directory under the server's own working directory instead of into
//!   `repo_root` — a `201` pointing at a repository that is not there.
//!
//! Hence the assertions below are about the repository *on disk*, not only
//! about the status code: a `201` whose bare repo landed somewhere else is the
//! second defect passing itself off as a fix.

use std::path::Path;

use crate::common::{register_full, spawn_test_app_with_repo_root};

/// Create a repository with `auto_init`, so the fork has an actual commit to
/// carry over — an empty source would let a bare `mkdir` pass as a clone.
async fn create_seeded_repo(base: &str, token: &str, name: &str, is_private: bool) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "is_private": is_private,
            "auto_init": true,
            "readme": "default",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "seeding repository '{name}' failed");
    resp.json::<serde_json::Value>().await.expect("json body")["id"]
        .as_i64()
        .expect("repository id")
}

/// The commit `HEAD` resolves to in a bare repository on disk.
fn head_commit(bare: &Path) -> String {
    let repo = gix::open(bare).unwrap_or_else(|error| panic!("gix cannot open {bare:?}: {error}"));
    repo.head_id()
        .unwrap_or_else(|error| panic!("{bare:?} has no resolvable HEAD: {error}"))
        .to_string()
}

/// The acceptance the card asked for: an outsider forks a public repository,
/// gets a `201`, and the fork is really there — in the API *and* on disk.
#[tokio::test]
async fn outsider_forks_public_repository_and_the_clone_lands_in_repo_root() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (owner_token, _) = register_full(&base, "forkowner", "forkowner@example.com").await;
    let source_id = create_seeded_repo(&base, &owner_token, "forkme", false).await;
    let (outsider_token, outsider_id) =
        register_full(&base, "forkoutsider", "forkoutsider@example.com").await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/v1/repos/forkowner/forkme/fork"))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 201,
        "forking a public repository must succeed, got {status} (body: {body})"
    );
    assert_eq!(body["name"], "forkme", "fork keeps the source name: {body}");
    assert_eq!(
        body["owner_id"].as_i64(),
        Some(outsider_id),
        "the fork belongs to the forker, not the source owner: {body}"
    );

    // The fork is a repository of the forker's, reachable under their name.
    let resp = client
        .get(format!("{base}/api/v1/repos/forkoutsider/forkme"))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "the fork must be readable at /repos/forkoutsider/forkme"
    );

    // …and the bare repository is where the server says it is, with the source
    // history in it. This is the assertion the `file://` destination fails.
    let source_bare = repo_root.join("forkowner/forkme.git");
    let fork_bare = repo_root.join("forkoutsider/forkme.git");
    assert!(
        fork_bare.is_dir(),
        "no bare repository at {fork_bare:?} — the clone wrote somewhere else"
    );
    assert_eq!(
        head_commit(&fork_bare),
        head_commit(&source_bare),
        "the fork does not carry the source history — it is not a clone of it"
    );

    // The source counts the fork it just gained.
    let resp = client
        .get(format!("{base}/api/v1/repos/forkowner/forkme"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["forks_count"].as_i64(),
        Some(1),
        "the source repository must count its fork: {body}"
    );

    let resp = client
        .get(format!("{base}/api/v1/repos/forkowner/forkme/forks"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["data"][0]["owner_id"].as_i64(),
        Some(outsider_id),
        "the fork must be listed under the source's forks (source id {source_id}): {body}"
    );
}

/// A repository the forker already owns under that name is a `400`, not a `500`
/// and not a silent second clone over the first one's directory.
#[tokio::test]
async fn forking_twice_is_refused_without_touching_the_first_fork() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (owner_token, _) = register_full(&base, "twiceowner", "twiceowner@example.com").await;
    create_seeded_repo(&base, &owner_token, "twiceme", false).await;
    let (outsider_token, _) =
        register_full(&base, "twiceoutsider", "twiceoutsider@example.com").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/twiceowner/twiceme/fork");
    let resp = client
        .post(&url)
        .bearer_auth(&outsider_token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 201, "the first fork must succeed");
    let fork_head = head_commit(&repo_root.join("twiceoutsider/twiceme.git"));

    let resp = client
        .post(&url)
        .bearer_auth(&outsider_token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 400,
        "a second fork under the same name is the caller's mistake, got {status} (body: {body})"
    );
    assert_eq!(
        head_commit(&repo_root.join("twiceoutsider/twiceme.git")),
        fork_head,
        "the refused second fork must leave the first one alone"
    );
}

/// The read gate still holds now that the handler gets far enough to have one:
/// a private source is a denial, and the forker's directory stays empty.
#[tokio::test]
async fn outsider_cannot_fork_a_private_repository() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (owner_token, _) = register_full(&base, "privowner", "privowner@example.com").await;
    create_seeded_repo(&base, &owner_token, "privme", true).await;
    create_seeded_repo(&base, &owner_token, "publicme", false).await;
    let (outsider_token, _) = register_full(&base, "privoutsider", "privoutsider@example.com").await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/v1/repos/privowner/privme/fork"))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        matches!(status.as_u16(), 403 | 404),
        "an outsider must not fork a private repository, got {status} (body: {body})"
    );
    assert!(
        !repo_root.join("privoutsider/privme.git").exists(),
        "a denied fork must not leave a repository behind"
    );

    // The live baseline, in the same test and with the same persona: this
    // account *can* fork a public repository of the same owner. Without it, a
    // fork endpoint broken all over again would read as a passing security
    // test — which is exactly how this defect survived for so long.
    let resp = client
        .post(format!("{base}/api/v1/repos/privowner/publicme/fork"))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 201,
        "baseline: the same outsider must be able to fork the public repository, got \
         {status} (body: {body}) — the denial above proves nothing if forking is broken"
    );
    assert!(
        repo_root.join("privoutsider/publicme.git").is_dir(),
        "baseline fork is missing on disk"
    );
}
