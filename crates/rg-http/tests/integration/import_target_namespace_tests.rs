//! card_e736b5186281: `POST /api/v1/imports` took the target namespace out of
//! the request body and asked no one about it.
//!
//! `target_owner` went straight from the JSON payload to the import service,
//! which either found the named repository and poured the import into it —
//! issues, pull requests, releases, labels, milestones, branches — or resolved
//! the name to a user/organization and *created* a repository under that
//! account. The caller's own id was recorded on the task row and nowhere else,
//! so "may this user write into `owner/name`" was never a question anybody
//! asked.
//!
//! The route sweep could not see it: it drives `POST /imports` with an empty
//! body and gets a correct `401`. The hole was in the body's *content*.

use crate::common::{register_full, spawn_test_app};

/// `.invalid` never resolves (RFC 6761), so an accepted import fails in its
/// background worker instead of reaching a real host. These tests only care
/// about the answer to the POST.
const SOURCE_URL: &str = "https://example.invalid/octo/widgets.git";

async fn start_import(
    base: &str,
    token: &str,
    target_owner: &str,
    target_name: &str,
) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "platform": "git",
            "source_url": SOURCE_URL,
            "target_owner": target_owner,
            "target_name": target_name,
        }))
        .send()
        .await
        .expect("request")
        .status()
}

async fn create_repo(base: &str, token: &str, name: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name }))
        .send()
        .await
        .expect("request")
        .status()
}

async fn repo_exists(base: &str, token: &str, owner: &str, name: &str) -> bool {
    reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{name}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .status()
        .is_success()
}

/// The target repository already exists: the import would write into a
/// repository the caller has no write access to.
#[tokio::test]
async fn an_outsider_cannot_import_into_someone_elses_existing_repository() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "ns-owner", "ns-owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "ns-outsider", "ns-outsider@example.com").await;

    assert_eq!(
        create_repo(&base, &owner_token, "widgets").await,
        201,
        "baseline: the owner has a repository to be targeted"
    );

    assert_eq!(
        start_import(&base, &outsider_token, "ns-owner", "widgets").await,
        403,
        "an outsider filled someone else's existing repository from a remote"
    );
}

/// The target repository does not exist: the import would have one *created*
/// under the named account.
#[tokio::test]
async fn an_outsider_cannot_have_a_repository_created_under_someone_elses_account() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "ns2-owner", "ns2-owner@example.com").await;
    let (outsider_token, _) =
        register_full(&base, "ns2-outsider", "ns2-outsider@example.com").await;

    assert_eq!(
        start_import(&base, &outsider_token, "ns2-owner", "squatted").await,
        403,
        "an outsider had a repository created under another account"
    );
    assert!(
        !repo_exists(&base, &owner_token, "ns2-owner", "squatted").await,
        "the denied import still created the repository"
    );
}

/// A name that is neither a user nor an organization is a denial, not a `404`:
/// the gate is not an account-existence oracle for a caller with no business in
/// that namespace either way.
#[tokio::test]
async fn an_unknown_target_owner_is_denied_rather_than_reported_missing() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "ns3-user", "ns3-user@example.com").await;

    assert_eq!(
        start_import(&base, &token, "nobody-by-that-name", "widgets").await,
        403,
    );
}

/// The other half of the gate: the people who *may* import still do.
#[tokio::test]
async fn the_owner_imports_into_their_own_namespace() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "ns4-owner", "ns4-owner@example.com").await;

    assert_eq!(
        start_import(&base, &token, "ns4-owner", "fresh-import").await,
        201,
        "the owner may import into a name of their own that is still free"
    );

    assert_eq!(
        create_repo(&base, &token, "existing").await,
        201,
        "baseline: an existing repository of the caller's own"
    );
    assert_eq!(
        start_import(&base, &token, "ns4-owner", "existing").await,
        201,
        "the owner may import into their own existing repository"
    );
}

/// Organization membership is the same rule `create_repo` applies to its `org`
/// field — a member may import into the organization, a stranger may not.
#[tokio::test]
async fn an_org_member_imports_into_the_org_and_a_stranger_does_not() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "ns5-owner", "ns5-owner@example.com").await;
    let (member_token, member_id) = register_full(&base, "ns5-member", "ns5-mem@example.com").await;
    let (stranger_token, _) = register_full(&base, "ns5-stranger", "ns5-str@example.com").await;

    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "name": "ns5corp", "visibility": "public" }))
        .send()
        .await
        .expect("create org");
    assert_eq!(created.status(), 201, "baseline: the organization exists");

    let added = client
        .post(format!("{base}/api/v1/orgs/ns5corp/members"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({ "user_id": member_id, "role": "member" }))
        .send()
        .await
        .expect("add org member");
    assert_eq!(added.status(), 201, "baseline: the member is in the org");

    assert_eq!(
        start_import(&base, &member_token, "ns5corp", "org-import").await,
        201,
        "an organization member may import into the organization"
    );
    assert_eq!(
        start_import(&base, &stranger_token, "ns5corp", "sneaked").await,
        403,
        "a stranger imported into an organization they do not belong to"
    );
}

/// The gate runs before the payload's other checks, and after authentication:
/// an anonymous caller is still `401` (the route sweep's contract), and a
/// caller who *is* authorized still gets the ordinary `400` for a bad payload.
#[tokio::test]
async fn the_namespace_gate_does_not_displace_the_other_answers() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "ns6-user", "ns6-user@example.com").await;
    let client = reqwest::Client::new();

    let anonymous = client
        .post(format!("{base}/api/v1/imports"))
        .json(&serde_json::json!({
            "platform": "git",
            "source_url": SOURCE_URL,
            "target_owner": "ns6-user",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(anonymous.status(), 401, "no session is still 401");

    let bad_platform = client
        .post(format!("{base}/api/v1/imports"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "platform": "bitbucket",
            "source_url": SOURCE_URL,
            "target_owner": "ns6-user",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        bad_platform.status(),
        400,
        "an unknown platform in an authorized namespace is still a 400"
    );
}
