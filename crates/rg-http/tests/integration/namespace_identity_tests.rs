//! card_92019cc97dcd: an organization's repository also answered at its
//! owner's personal path.
//!
//! A repository belongs to exactly one namespace, but the row hangs off a user
//! either way: `resolve_owner` stores an organization's repository under
//! `owner_id = org.owner_id`, and every lookup filtered on `owner_id` alone.
//! Verified against `HEAD` before the fix — a repository created as
//! `POST /repos {"name":"shared","org":"prcorp"}` answered `200` at both
//! `GET /repos/prcorp/shared` and `GET /repos/pr-owner/shared`, and appeared in
//! the owner's `GET /repos/pr-owner` listing as if it were their own.
//!
//! There was no privilege escalation — the personal namespace leaked only to
//! the organization's own owner, and access was still decided off the same row
//! — but the identity of the two namespaces was one, and the consequences were
//! real: the "is this name taken" checks in `create_repo_with_opts`,
//! `fork_repo` and `transfer_repo` reached across the boundary, and a
//! repository that entered an organization found *itself* as its own
//! destination collision and could never leave (see
//! `transfer_namespace_tests`).
//!
//! Each test below pairs the assertion under test with a baseline in the same
//! test: a `404` proves a namespace boundary only if the path that is supposed
//! to work still answers `200` next to it.

use crate::common::{register_full, spawn_test_app, spawn_test_app_with_repo_root};

async fn create_repo_in(base: &str, token: &str, name: &str, org: Option<&str>) -> (u16, String) {
    let mut body = serde_json::json!({ "name": name });
    if let Some(org) = org {
        body["org"] = serde_json::json!(org);
    }
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("request");
    let status = resp.status().as_u16();
    (status, resp.text().await.expect("response body"))
}

async fn create_org(base: &str, token: &str, name: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "visibility": "public" }))
        .send()
        .await
        .expect("create org")
        .status();
    assert_eq!(status, 201, "baseline: the organization exists");
}

async fn get_repo_status(base: &str, token: &str, owner: &str, name: &str) -> u16 {
    reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{name}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

/// The repository names listed under `GET /repos/{owner}`.
async fn listed_repo_names(base: &str, token: &str, owner: &str) -> Vec<String> {
    let body: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .json()
        .await
        .expect("listing body");
    body["data"]
        .as_array()
        .expect("listing carries a data array")
        .iter()
        .map(|repo| {
            repo["name"]
                .as_str()
                .expect("every listed repo has a name")
                .to_string()
        })
        .collect()
}

/// The reported symptom: one row, two URLs.
#[tokio::test]
async fn an_organization_repository_does_not_answer_at_its_owners_personal_path() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "ni-owner", "ni-owner@example.com").await;

    create_org(&base, &token, "nicorp").await;
    let (status, body) = create_repo_in(&base, &token, "shared", Some("nicorp")).await;
    assert_eq!(status, 201, "baseline: the org repository exists ({body})");

    assert_eq!(
        get_repo_status(&base, &token, "nicorp", "shared").await,
        200,
        "the organization's repository stopped answering at its own path"
    );
    assert_eq!(
        get_repo_status(&base, &token, "ni-owner", "shared").await,
        404,
        "the organization's repository also answers in its owner's personal namespace"
    );
}

/// The same boundary on the listing axis, which is where an owner would
/// actually notice it: `GET /repos/{owner}` advertised the organization's
/// repositories as the owner's own.
#[tokio::test]
async fn an_owners_repository_listing_excludes_the_organizations_repositories() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "ni2-owner", "ni2-owner@example.com").await;

    create_org(&base, &token, "ni2corp").await;
    assert_eq!(
        create_repo_in(&base, &token, "mine", None).await.0,
        201,
        "baseline: the owner has a repository of their own"
    );
    assert_eq!(
        create_repo_in(&base, &token, "theirs", Some("ni2corp"))
            .await
            .0,
        201,
        "baseline: the organization has one too"
    );

    let personal = listed_repo_names(&base, &token, "ni2-owner").await;
    assert_eq!(
        personal,
        vec!["mine".to_string()],
        "the owner's listing carries the organization's repositories as well"
    );

    let org = listed_repo_names(&base, &token, "ni2corp").await;
    assert_eq!(
        org,
        vec!["theirs".to_string()],
        "the organization's own listing lost its repository"
    );
}

/// The name-collision half, and the last piece of the boundary to land.
///
/// `create_repo_with_opts` used to ask `find_by_owner_and_name(owner_id, name)`,
/// which for an organization is *its owner's* account, so an organization
/// repository blocked the owner's personal one. card_92019cc97dcd fixed the
/// query; the *table* still could not hold both, because `repositories` was born
/// with `UNIQUE (owner_id, name)` and an organization's row hangs off its
/// owner's account id. Until card_615e00843297 replaced that constraint with
/// `UNIQUE (namespace_key, name)`, this test asserted the `400` the schema
/// forced and named it as such.
///
/// Now the two namespaces genuinely coexist, and both halves are asserted here:
/// the same name in the other namespace is accepted *and* both repositories
/// answer at their own paths afterwards — a `201` that leaves one of them
/// unreachable would be a worse outcome than the refusal it replaced.
#[tokio::test]
async fn the_same_name_lives_in_a_personal_account_and_in_an_organization_it_owns() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "ni3-owner", "ni3-owner@example.com").await;

    create_org(&base, &token, "ni3corp").await;
    assert_eq!(
        create_repo_in(&base, &token, "twin", Some("ni3corp"))
            .await
            .0,
        201,
        "baseline: the organization's repository exists"
    );

    let (status, body) = create_repo_in(&base, &token, "twin", None).await;
    assert_eq!(
        status, 201,
        "the owner's personal namespace still cannot hold a name their organization uses ({body})"
    );

    assert_eq!(
        get_repo_status(&base, &token, "ni3corp", "twin").await,
        200,
        "the organization's repository stopped answering once its personal twin existed"
    );
    assert_eq!(
        get_repo_status(&base, &token, "ni3-owner", "twin").await,
        200,
        "the personal repository was reported created but does not answer"
    );

    let personal = listed_repo_names(&base, &token, "ni3-owner").await;
    assert_eq!(
        personal,
        vec!["twin".to_string()],
        "the owner's listing does not show exactly their own `twin`"
    );
    let org = listed_repo_names(&base, &token, "ni3corp").await;
    assert_eq!(
        org,
        vec!["twin".to_string()],
        "the organization's listing does not show exactly its own `twin`"
    );
}

/// The other half of the same constraint: within *one* namespace the name is
/// still taken, and the refusal is the namespace check's, not the table's.
#[tokio::test]
async fn a_name_already_used_in_the_same_namespace_is_still_refused() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "ni5-owner", "ni5-owner@example.com").await;

    assert_eq!(
        create_repo_in(&base, &token, "solo", None).await.0,
        201,
        "baseline: the personal repository exists"
    );
    assert_eq!(
        create_repo_in(&base, &token, "solo", None).await.0,
        409,
        "a second personal repository of the same name was accepted"
    );

    create_org(&base, &token, "ni5corp").await;
    assert_eq!(
        create_repo_in(&base, &token, "solo", Some("ni5corp"))
            .await
            .0,
        201,
        "baseline: the organization may take the name too"
    );
    assert_eq!(
        create_repo_in(&base, &token, "solo", Some("ni5corp"))
            .await
            .0,
        409,
        "a second repository of the same name in one organization was accepted"
    );
}

/// A same-named repository in an organization the caller does *not* own must not
/// interfere at all — the boundary is not only between an account and the
/// organizations it owns, it is between every pair of namespaces.
#[tokio::test]
async fn a_foreign_organizations_repository_does_not_block_the_same_name() {
    let base = spawn_test_app().await;
    let (theirs, _) = register_full(&base, "ni4-them", "ni4-them@example.com").await;
    let (mine, _) = register_full(&base, "ni4-me", "ni4-me@example.com").await;

    create_org(&base, &theirs, "ni4corp").await;
    assert_eq!(
        create_repo_in(&base, &theirs, "common", Some("ni4corp"))
            .await
            .0,
        201,
        "baseline: their organization has the name"
    );

    let (status, body) = create_repo_in(&base, &mine, "common", None).await;
    assert_eq!(
        status, 201,
        "someone else's organization reserved the name across accounts ({body})"
    );
    assert_eq!(
        get_repo_status(&base, &mine, "ni4-me", "common").await,
        200,
        "the repository was reported created but does not answer"
    );
}

/// card_9b6fa1a621a0: deleting only the row left the bare repository at its
/// canonical path. The schema then allowed the same name again, but gix found
/// the old non-empty directory and creation ended as an anonymous 500.
#[tokio::test]
async fn a_deleted_repository_name_can_be_recreated_without_inheriting_history() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "ni6-owner", "ni6-owner@example.com").await;
    let client = reqwest::Client::new();

    let first = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "name": "recycled",
            "auto_init": true,
            "readme": "default"
        }))
        .send()
        .await
        .expect("create first repository");
    assert_eq!(
        first.status(),
        201,
        "baseline: first repository creation failed: {}",
        first.text().await.unwrap_or_default()
    );

    let bare = repo_root.join("ni6-owner/recycled.git");
    let first_head = gix::open(&bare)
        .expect("open first repository")
        .head_id()
        .expect("auto-init must create a commit")
        .detach();

    let deleted = client
        .delete(format!("{base}/api/v1/repos/ni6-owner/recycled"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete repository");
    assert_eq!(
        deleted.status(),
        200,
        "delete failed: {}",
        deleted.text().await.unwrap_or_default()
    );
    assert!(
        !bare.exists(),
        "DELETE succeeded but left the canonical bare repository behind"
    );
    let deleted_trees: Vec<_> = std::fs::read_dir(bare.parent().unwrap())
        .expect("read repository namespace")
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().starts_with("recycled.git.deleted-"))
        .collect();
    assert!(
        deleted_trees.is_empty(),
        "DELETE moved the old repository aside but did not remove it: {deleted_trees:?}"
    );

    let (status, body) = create_repo_in(&base, &token, "recycled", None).await;
    assert_eq!(
        status, 201,
        "the deleted repository still blocks its name ({body})"
    );

    let recreated = gix::open(&bare).expect("open recreated repository");
    assert!(
        recreated.head_id().is_err(),
        "the empty recreation inherited commit {first_head} from the deleted repository"
    );
}

/// A path can be occupied without a live database row (manual copy, failed old
/// create, stale pre-fix deletion). That is a state conflict, not an internal
/// error, and the server must not overwrite or remove the foreign bytes.
#[tokio::test]
async fn an_occupied_repository_directory_is_a_safe_conflict() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "ni7-owner", "ni7-owner@example.com").await;
    let occupied = repo_root.join("ni7-owner/occupied.git");
    std::fs::create_dir_all(&occupied).expect("create occupied repository path");
    let marker = occupied.join("do-not-overwrite");
    std::fs::write(&marker, b"foreign data").expect("write marker");

    let (status, body) = create_repo_in(&base, &token, "occupied", None).await;
    assert_eq!(status, 409, "occupied storage was not a conflict ({body})");
    assert!(
        body.contains("repository storage for 'ni7-owner/occupied' is already occupied"),
        "the conflict does not explain the occupied storage: {body}"
    );
    assert!(
        !body.contains(&repo_root.display().to_string()),
        "the client-visible conflict leaked the server's repository root: {body}"
    );
    assert_eq!(
        std::fs::read(&marker).expect("occupied path must survive"),
        b"foreign data"
    );
    assert_eq!(
        get_repo_status(&base, &token, "ni7-owner", "occupied").await,
        404,
        "a failed create still inserted a repository row"
    );
}
