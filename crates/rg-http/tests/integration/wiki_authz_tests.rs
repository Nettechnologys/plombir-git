//! Authorization regressions for the Wiki endpoints.
//!
//! Two failures used to live in `api/wiki.rs`, and both are covered below:
//!
//!   1. No handler asked whether the caller had anything to do with the
//!      repository. The reads took `_headers` and never looked at them, so the
//!      wiki of a private repository was readable anonymously; the writes
//!      stopped at `extract_user_id`, which authenticates but authorizes
//!      nothing — any account could create, rewrite and delete the pages of
//!      every repository on the instance.
//!
//!   2. `GET .../wiki/{title}/revisions/{id}` discarded owner, name *and*
//!      title and fetched the revision by its global primary key, so one
//!      repository's history could be read through another repository's URL.

use crate::common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

// ── helpers ──────────────────────────────────────────────────────────────────

async fn create_private_repo(base: &str, token: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_private_repo '{name}' failed");
}

async fn add_collaborator(base: &str, token: &str, owner: &str, repo: &str, username: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/collaborators"))
        .bearer_auth(token)
        .json(&serde_json::json!({"username": username, "permission": "read"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "add_collaborator '{username}' failed");
}

async fn create_page(base: &str, token: &str, owner: &str, repo: &str, title: &str, body: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/wiki"))
        .bearer_auth(token)
        .json(&serde_json::json!({"title": title, "content": body}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_page '{title}' failed");
}

/// Edit a page once so it has a revision, and return that revision's id.
async fn make_revision(base: &str, token: &str, owner: &str, repo: &str, title: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .patch(format!("{base}/api/v1/repos/{owner}/{repo}/wiki/{title}"))
        .bearer_auth(token)
        .json(&serde_json::json!({"content": "second version"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "update of '{title}' failed");

    let revisions: serde_json::Value = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/wiki/{title}/history"
        ))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    revisions[0]["id"].as_i64().expect("revision id")
}

async fn call(
    method: reqwest::Method,
    url: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> reqwest::StatusCode {
    let mut req = reqwest::Client::new().request(method, url);
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    if let Some(body) = body {
        req = req.json(&body);
    }
    req.send().await.unwrap().status()
}

/// Every wiki endpoint of one page, as `(method, url, body)` triples.
fn all_endpoints(
    base: &str,
    owner: &str,
    repo: &str,
    title: &str,
    rev_id: i64,
) -> Vec<(reqwest::Method, String, Option<serde_json::Value>)> {
    use reqwest::Method;
    let root = format!("{base}/api/v1/repos/{owner}/{repo}/wiki");
    vec![
        (Method::GET, root.clone(), None),
        (
            Method::POST,
            root.clone(),
            Some(serde_json::json!({"title": "Intruder", "content": "x"})),
        ),
        (Method::GET, format!("{root}/{title}"), None),
        (
            Method::PATCH,
            format!("{root}/{title}"),
            Some(serde_json::json!({"content": "hijacked"})),
        ),
        (Method::DELETE, format!("{root}/{title}"), None),
        (Method::GET, format!("{root}/{title}/history"), None),
        (
            Method::GET,
            format!("{root}/{title}/revisions/{rev_id}"),
            None,
        ),
    ]
}

// ── tests ────────────────────────────────────────────────────────────────────

/// A private repository's wiki is closed to anonymous callers (401) and to
/// authenticated outsiders (403) on every endpoint — reads included.
#[tokio::test]
async fn private_wiki_endpoints_reject_anonymous_and_outsiders() {
    let base = spawn_test_app().await;
    let owner = "wikiauthzowner";
    let outsider = "wikiauthzoutsider";
    let repo = "wikiauthzrepo";
    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let outsider_token =
        register_user(&base, outsider, &format!("{outsider}@example.com"), PW).await;
    create_private_repo(&base, &owner_token, repo).await;

    create_page(&base, &owner_token, owner, repo, "Secret", "first version").await;
    let rev_id = make_revision(&base, &owner_token, owner, repo, "Secret").await;

    for (method, url, body) in all_endpoints(&base, owner, repo, "Secret", rev_id) {
        let anonymous = call(method.clone(), &url, None, body.clone()).await;
        assert_eq!(
            anonymous, 401,
            "anonymous {method} {url} answered {anonymous}"
        );

        let stranger = call(method.clone(), &url, Some(&outsider_token), body.clone()).await;
        assert_eq!(stranger, 403, "outsider {method} {url} answered {stranger}");
    }

    // Nothing above went through: the page still holds the owner's content, and
    // the stranger's own page was never created.
    let page: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{repo}/wiki/Secret"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(page["content"], "second version");

    let pages: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{repo}/wiki"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        pages.as_array().unwrap().len(),
        1,
        "the outsider's POST left a page behind"
    );
}

/// The other side of the same check: the owner and a read collaborator still
/// get the private wiki, so the fix is a gate and not a wall.
#[tokio::test]
async fn owner_and_collaborator_still_read_a_private_wiki() {
    let base = spawn_test_app().await;
    let owner = "wikireadowner";
    let friend = "wikireadfriend";
    let repo = "wikireadrepo";
    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    let friend_token = register_user(&base, friend, &format!("{friend}@example.com"), PW).await;
    create_private_repo(&base, &owner_token, repo).await;
    add_collaborator(&base, &owner_token, owner, repo, friend).await;

    create_page(&base, &owner_token, owner, repo, "Notes", "first version").await;
    let rev_id = make_revision(&base, &owner_token, owner, repo, "Notes").await;

    let reads = all_endpoints(&base, owner, repo, "Notes", rev_id)
        .into_iter()
        .filter(|(method, _, _)| method == reqwest::Method::GET);

    for (method, url, _) in reads {
        for (who, token) in [("owner", &owner_token), ("collaborator", &friend_token)] {
            let status = call(method.clone(), &url, Some(token), None).await;
            assert_eq!(status, 200, "{who} {method} {url} answered {status}");
        }
    }
}

/// A revision id is global, so reading one has to be scoped to the page the
/// route names: the attacker's own repository must not be a window into the
/// victim's history.
#[tokio::test]
async fn wiki_revisions_stay_inside_their_repository() {
    let base = spawn_test_app().await;
    let victim = "wikiscopevictim";
    let attacker = "wikiscopeattacker";
    let victim_repo = "victimwiki";
    let attacker_repo = "attackerwiki";
    let victim_token = register_user(&base, victim, &format!("{victim}@example.com"), PW).await;
    let attacker_token =
        register_user(&base, attacker, &format!("{attacker}@example.com"), PW).await;

    create_private_repo(&base, &victim_token, victim_repo).await;
    create_repo(&base, &attacker_token, attacker_repo).await;

    create_page(
        &base,
        &victim_token,
        victim,
        victim_repo,
        "Secret",
        "classified",
    )
    .await;
    let victim_rev = make_revision(&base, &victim_token, victim, victim_repo, "Secret").await;

    // The attacker owns this repository and this page outright — only the
    // revision id is borrowed.
    create_page(
        &base,
        &attacker_token,
        attacker,
        attacker_repo,
        "Secret",
        "mine",
    )
    .await;
    make_revision(&base, &attacker_token, attacker, attacker_repo, "Secret").await;

    let stolen = format!(
        "{base}/api/v1/repos/{attacker}/{attacker_repo}/wiki/Secret/revisions/{victim_rev}"
    );
    let status = call(reqwest::Method::GET, &stolen, Some(&attacker_token), None).await;
    assert_eq!(
        status, 404,
        "a foreign revision id resolved through the attacker's own repository"
    );

    // Same id, but named through a page of the attacker's repository that does
    // not exist — still nothing.
    let stolen_absent = format!(
        "{base}/api/v1/repos/{attacker}/{attacker_repo}/wiki/Absent/revisions/{victim_rev}"
    );
    let status = call(
        reqwest::Method::GET,
        &stolen_absent,
        Some(&attacker_token),
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "an absent page still served a foreign revision"
    );

    // The victim's own route is unaffected.
    let own =
        format!("{base}/api/v1/repos/{victim}/{victim_repo}/wiki/Secret/revisions/{victim_rev}");
    let status = call(reqwest::Method::GET, &own, Some(&victim_token), None).await;
    assert_eq!(status, 200, "the owner lost access to its own revision");
}

/// A public repository's wiki stays anonymously readable — the gate is about
/// visibility, not about requiring a token everywhere.
#[tokio::test]
async fn public_wiki_stays_anonymously_readable() {
    let base = spawn_test_app().await;
    let owner = "wikipublicowner";
    let repo = "wikipublicrepo";
    let owner_token = register_user(&base, owner, &format!("{owner}@example.com"), PW).await;
    create_repo(&base, &owner_token, repo).await;

    create_page(&base, &owner_token, owner, repo, "Home", "first version").await;
    let rev_id = make_revision(&base, &owner_token, owner, repo, "Home").await;

    let reads = all_endpoints(&base, owner, repo, "Home", rev_id)
        .into_iter()
        .filter(|(method, _, _)| method == reqwest::Method::GET);

    for (method, url, _) in reads {
        let status = call(method.clone(), &url, None, None).await;
        assert_eq!(status, 200, "anonymous {method} {url} answered {status}");
    }
}
