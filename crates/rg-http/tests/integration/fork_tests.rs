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

async fn create_org(base: &str, token: &str, name: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "visibility": "public" }))
        .send()
        .await
        .expect("create organization");
    assert_eq!(
        response.status(),
        201,
        "baseline: destination organization creation failed: {}",
        response.text().await.expect("organization response body")
    );
}

async fn add_org_member(base: &str, token: &str, org: &str, user_id: i64) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs/{org}/members"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "user_id": user_id, "role": "member" }))
        .send()
        .await
        .expect("add organization member");
    assert_eq!(
        response.status(),
        201,
        "baseline: adding the destination member failed: {}",
        response.text().await.expect("membership response body")
    );
}

/// The commit `HEAD` resolves to in a bare repository on disk.
fn head_commit(bare: &Path) -> String {
    let repo = gix::open(bare).unwrap_or_else(|error| panic!("gix cannot open {bare:?}: {error}"));
    repo.head_id()
        .unwrap_or_else(|error| panic!("{bare:?} has no resolvable HEAD: {error}"))
        .to_string()
}

/// The fork destination is a second authorization subject, independent of the
/// readable source. A member may fork into their organization, a stranger may
/// not, and the historical request with no body still means the caller's own
/// account.
#[tokio::test]
async fn organization_destination_is_gated_and_no_body_stays_personal() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (source_token, _) =
        register_full(&base, "fork-org-source", "fork-org-source@example.com").await;
    create_seeded_repo(&base, &source_token, "share-me", false).await;

    let (org_owner_token, _) =
        register_full(&base, "fork-org-owner", "fork-org-owner@example.com").await;
    let (member_token, member_id) =
        register_full(&base, "fork-org-member", "fork-org-member@example.com").await;
    let (outsider_token, _) =
        register_full(&base, "fork-org-outsider", "fork-org-outsider@example.com").await;
    create_org(&base, &org_owner_token, "fork-org-target").await;
    add_org_member(&base, &org_owner_token, "fork-org-target", member_id).await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/fork-org-source/share-me/fork");

    let denied = client
        .post(&url)
        .bearer_auth(&outsider_token)
        .json(&serde_json::json!({ "org": "fork-org-target" }))
        .send()
        .await
        .expect("denied fork request");
    assert_eq!(
        denied.status(),
        403,
        "a non-member forked into someone else's organization: {}",
        denied.text().await.expect("denial body")
    );
    assert!(
        !repo_root.join("fork-org-target/share-me.git").exists(),
        "the denied fork left organization storage behind"
    );

    // The compatibility branch is only for zero bytes. A non-empty body that
    // forgot its JSON content type must not be discarded and silently become a
    // personal fork — the exact dead-wiring failure this route used to have.
    let untyped = client
        .post(&url)
        .bearer_auth(&member_token)
        .body(r#"{"org":"fork-org-target"}"#)
        .send()
        .await
        .expect("untyped fork request");
    assert_eq!(
        untyped.status(),
        400,
        "a non-empty untyped body was silently ignored: {}",
        untyped.text().await.expect("untyped rejection body")
    );
    assert!(
        !repo_root.join("fork-org-member/share-me.git").exists(),
        "the rejected untyped body produced a personal fork"
    );

    let accepted = client
        .post(&url)
        .bearer_auth(&member_token)
        .json(&serde_json::json!({ "org": "fork-org-target" }))
        .send()
        .await
        .expect("organization fork request");
    let status = accepted.status();
    let body: serde_json::Value = accepted.json().await.expect("organization fork body");
    assert_eq!(
        status, 201,
        "an organization member could not fork into it: {body}"
    );
    assert!(
        body["org_id"].as_i64().is_some(),
        "fork has no org id: {body}"
    );
    assert!(
        repo_root.join("fork-org-target/share-me.git").is_dir(),
        "the accepted fork is not stored under the organization"
    );
    let visible = client
        .get(format!("{base}/api/v1/repos/fork-org-target/share-me"))
        .bearer_auth(&member_token)
        .send()
        .await
        .expect("read organization fork");
    assert_eq!(
        visible.status(),
        200,
        "the accepted fork is not addressable through the organization namespace"
    );
    assert!(
        !repo_root.join("fork-org-member/share-me.git").exists(),
        "the organization fork also appeared in the member's account"
    );

    // Baseline and backward compatibility: no JSON body at all is still a
    // personal fork, even though the same user just forked the same source into
    // an organization. The namespace-unique key must distinguish the two.
    let personal = client
        .post(&url)
        .bearer_auth(&member_token)
        .send()
        .await
        .expect("personal fork request");
    let status = personal.status();
    let body: serde_json::Value = personal.json().await.expect("personal fork body");
    assert_eq!(status, 201, "no-body personal fork failed: {body}");
    assert!(
        body["org_id"].is_null(),
        "personal fork gained an org: {body}"
    );
    assert!(
        repo_root.join("fork-org-member/share-me.git").is_dir(),
        "the no-body fork did not land in the member's account"
    );

    // A fork listing is a navigation response, not an inventory of database
    // ids. In particular `owner_id` cannot name the organization namespace:
    // organization repositories retain the owner's user id in that column.
    let response = client
        .get(format!(
            "{base}/api/v1/repos/fork-org-source/share-me/forks?per_page=20"
        ))
        .bearer_auth(&member_token)
        .send()
        .await
        .expect("fork listing request");
    let status = response.status();
    let listing: serde_json::Value = response.json().await.expect("fork listing body");
    assert_eq!(status, 200, "fork listing failed: {listing}");
    let forks = listing["data"].as_array().expect("paginated fork data");
    for fork in forks {
        for field in ["fork_id", "org_id", "deleted_at", "origin_repo_id"] {
            assert!(
                fork.get(field).is_some(),
                "fork response dropped the existing `{field}` field: {fork}"
            );
        }
    }
    let mut owner_names: Vec<String> = forks
        .iter()
        .map(|fork| {
            fork["owner_name"]
                .as_str()
                .expect("fork owner namespace")
                .to_string()
        })
        .collect();
    owner_names.sort();
    assert_eq!(
        owner_names,
        vec!["fork-org-member".to_string(), "fork-org-target".to_string()],
        "the web must be able to link both personal and organization forks: {listing}"
    );
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

/// The fork button in the browser sends the HttpOnly `plombir_git_token` cookie
/// and no `Authorization` header — the web client keeps its token in memory
/// only, so after a page reload the cookie is the whole session.
///
/// The handler used to read the caller with `extract_bearer_claims`, which
/// accepts the header and nothing else, so every fork from a reloaded tab was a
/// `401` while the same account forked fine from `curl`. Anonymous is still a
/// `401`: taking the cookie must not mean taking nobody.
#[tokio::test]
async fn a_cookie_session_may_fork_and_an_anonymous_caller_may_not() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (owner_token, _) = register_full(&base, "cookieowner", "cookieowner@example.com").await;
    create_seeded_repo(&base, &owner_token, "cookieme", false).await;
    let (outsider_token, outsider_id) =
        register_full(&base, "cookieoutsider", "cookieoutsider@example.com").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/cookieowner/cookieme/fork");

    let anonymous = client.post(&url).send().await.expect("request");
    assert_eq!(
        anonymous.status(),
        401,
        "a fork with no session at all must stay a 401"
    );

    let resp = client
        .post(&url)
        .header("cookie", format!("plombir_git_token={outsider_token}"))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 201,
        "the cookie session is the same session as the bearer token, got {status} (body: {body})"
    );
    assert_eq!(
        body["owner_id"].as_i64(),
        Some(outsider_id),
        "the fork belongs to the account the cookie names: {body}"
    );
    assert!(
        repo_root.join("cookieoutsider/cookieme.git").is_dir(),
        "the cookie-session fork is missing on disk"
    );
}

/// A repository the forker already owns under that name is a `409`, not a `500`
/// and not a silent second clone over the first one's directory.
///
/// `409` and not `400` since card_cfba32a77acd: the fork request is correct and
/// an existing repository refuses it — the forker renames or deletes that one,
/// there is nothing in the request to fix.
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
        status, 409,
        "a second fork under the same name is refused by the name that is taken, got {status} (body: {body})"
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
    let (outsider_token, _) =
        register_full(&base, "privoutsider", "privoutsider@example.com").await;

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
