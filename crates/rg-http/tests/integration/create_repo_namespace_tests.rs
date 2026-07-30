//! card_1e1ed1ee06f1: `POST /repos` decided "may I create a repository in this
//! organization" with its own copy of the rule.
//!
//! The copy differed from `require_namespace_create` in both directions and both
//! were wrong. Its `Ok(true) => Some(org.id), _ => forbidden(...)` arm swallowed
//! the `Err`, so a failed membership lookup answered `403 you are not a member of
//! this organization` — a permission we never managed to read reported as one the
//! caller lacks. And an organization that does not exist answered
//! `404 organization not found`, while the gate deliberately answers `403`: a
//! caller with no business in that namespace should not learn from the reply
//! whether the account is real.
//!
//! There was no open hole — membership *was* checked — which is exactly why this
//! is worth a test: the rule had gone back to being a convention, and the next
//! copy would have drifted silently. The handler now takes `NamespaceCreate`,
//! the same body extractor the transfer destination and the import target take,
//! and the gate hands back the organization id it resolved.
//!
//! The denials and the baseline live in the same test on purpose: a `403` proves
//! a refusal only if the legitimate call next to it still answers `201`.
//!
//! card_2179245d41db: the second round is about *bodies*. The gate answered one
//! status to every refusal and two different messages underneath it, so the
//! oracle this file was written to close had simply moved one level down — out
//! of reach of a test that reads `.status()` and of `route_access_sweep_tests`,
//! which judges every persona on its status code alone. Every assertion below
//! that claims two callers cannot be told apart now compares the response body
//! as well.

use crate::common::{register_full, spawn_test_app};

/// The full reply, not just its code — the refusals in this file are supposed
/// to be indistinguishable, and a status-only assertion cannot see the half of
/// the reply where the difference actually lived.
///
/// `request_id` is dropped before comparing: the tracing middleware stamps a
/// fresh uuid on every response, so it differs between any two requests and
/// carries nothing about which branch answered. Everything else — the status,
/// the error `code`, the `message` — is compared verbatim.
async fn create_in_full(
    base: &str,
    token: &str,
    name: &str,
    org: Option<&str>,
) -> (u16, serde_json::Value) {
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
    let text = resp.text().await.expect("response body");
    let mut json: serde_json::Value =
        serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({ "raw": text }));
    if let Some(error) = json.get_mut("error").and_then(|e| e.as_object_mut()) {
        error.remove("request_id");
    }
    (status, json)
}

async fn create_in(base: &str, token: &str, name: &str, org: Option<&str>) -> u16 {
    create_in_full(base, token, name, org).await.0
}

async fn create_org_with(base: &str, token: &str, name: &str, visibility: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "visibility": visibility }))
        .send()
        .await
        .expect("create org")
        .status();
    assert_eq!(status, 201, "baseline: the organization exists");
}

async fn create_org(base: &str, token: &str, name: &str) {
    create_org_with(base, token, name, "public").await;
}

async fn add_org_member(base: &str, token: &str, org: &str, user_id: i64) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs/{org}/members"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "user_id": user_id, "role": "member" }))
        .send()
        .await
        .expect("add org member")
        .status();
    assert_eq!(status, 201, "baseline: the member is in the org");
}

async fn repo_exists_at(base: &str, token: &str, owner: &str, name: &str) -> bool {
    reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{name}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .status()
        .is_success()
}

/// Membership decides, and the refusal is a refusal — with the two calls that
/// *are* legitimate in the same test to prove the fixture is not simply broken.
#[tokio::test]
async fn creating_in_an_organization_needs_membership() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "cn-owner", "cn-owner@example.com").await;
    let (member_token, member_id) = register_full(&base, "cn-mem", "cn-mem@example.com").await;
    let (outsider_token, _) = register_full(&base, "cn-out", "cn-out@example.com").await;

    create_org(&base, &owner_token, "cncorp").await;
    add_org_member(&base, &owner_token, "cncorp", member_id).await;

    assert_eq!(
        create_in(&base, &outsider_token, "squatted", Some("cncorp")).await,
        403,
        "a non-member created a repository inside someone else's organization"
    );
    assert!(
        !repo_exists_at(&base, &owner_token, "cncorp", "squatted").await,
        "the denied create left a repository in the organization anyway"
    );

    // Baseline, same test: a member may, and so may their own account.
    assert_eq!(
        create_in(&base, &member_token, "widgets", Some("cncorp")).await,
        201,
        "a member of the organization was refused"
    );
    assert!(
        repo_exists_at(&base, &member_token, "cncorp", "widgets").await,
        "the accepted create did not actually produce the repository"
    );
    assert_eq!(
        create_in(&base, &member_token, "personal", None).await,
        201,
        "creating under one's own account was refused"
    );
}

/// An organization nobody has heard of is a denial, not a `404` — and the
/// denial reads the same as the one an existing organization produces, all the
/// way down to the message.
///
/// The old handler answered `404 organization not found`, which is a free
/// existence check on every namespace name for any account that can log in: a
/// `403` and a `404` told apart mean "this org exists but you are not in it" and
/// "this name is unused". The gate gives one answer to both.
///
/// card_2179245d41db: it gave one *status* to both and two messages —
/// `you are not a member of this organization` against an organization that
/// exists, `you may not create a repository under this owner` against a free
/// name — so the split this test was written to catch survived it untouched.
/// Comparing the bodies is what makes the assertion mean what its name says;
/// re-splitting the messages has to fail here.
#[tokio::test]
async fn an_unknown_organization_is_denied_rather_than_reported_missing() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "cn2-owner", "cn2-owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "cn2-out", "cn2-out@example.com").await;

    create_org(&base, &owner_token, "cn2corp").await;
    // The private one is the case the card measured: masked everywhere else on
    // the instance, so if any reply is going to single it out it is this one.
    create_org_with(&base, &owner_token, "cn2secret", "private").await;

    let unknown = create_in_full(&base, &outsider_token, "ghost", Some("no-such-org-here")).await;
    let public = create_in_full(&base, &outsider_token, "ghost", Some("cn2corp")).await;
    let private = create_in_full(&base, &outsider_token, "ghost", Some("cn2secret")).await;

    assert_eq!(
        unknown.0, 403,
        "an unknown namespace was reported missing instead of refused"
    );
    assert_eq!(
        unknown, public,
        "the reply tells an outsider apart an organization that exists ({public:?}) from a name \
         that is free ({unknown:?}) — that is an account-existence oracle"
    );
    assert_eq!(
        unknown, private,
        "the reply tells an outsider apart a *private* organization ({private:?}) from a name \
         that is free ({unknown:?}) — every other route on the instance masks that org behind a \
         404, and this one hands it over in the body of a 403"
    );
}

/// A namespace naming somebody else's *account* is refused for the same reason,
/// and by the same resolution order the write path uses (username first, then
/// organization) — so `org` cannot be used to smuggle a repository under another
/// user either.
///
/// Refused the same way, too: the third branch of the gate is the one that ran
/// here, and it has to be unreadable apart from the other two for the same
/// reason they are unreadable apart from each other.
#[tokio::test]
async fn creating_under_another_users_account_is_refused() {
    let base = spawn_test_app().await;
    let (victim_token, _) = register_full(&base, "cn3-victim", "cn3-victim@example.com").await;
    let (token, _) = register_full(&base, "cn3-user", "cn3-user@example.com").await;

    let stranger = create_in_full(&base, &token, "planted", Some("cn3-victim")).await;
    let free_name = create_in_full(&base, &token, "planted", Some("cn3-nobody")).await;

    assert_eq!(
        stranger.0, 403,
        "a repository was created under a stranger's account"
    );
    assert_eq!(
        stranger, free_name,
        "the reply tells an existing account ({stranger:?}) apart from a name nobody holds \
         ({free_name:?}) — that is a username-existence oracle on a route that needs no such \
         answer"
    );
    assert!(
        !repo_exists_at(&base, &victim_token, "cn3-victim", "planted").await,
        "the denied create planted the repository anyway"
    );
}

/// The gate authenticates before it parses, so an anonymous caller is still
/// `401` and does not get to find out whether the namespace they named exists.
#[tokio::test]
async fn anonymous_creation_is_still_unauthorized() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "cn4-owner", "cn4-owner@example.com").await;
    create_org(&base, &owner_token, "cn4corp").await;

    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .json(&serde_json::json!({ "name": "anon", "org": "cn4corp" }))
        .send()
        .await
        .expect("request")
        .status();

    assert_eq!(status, 401, "no session is still 401");
}
