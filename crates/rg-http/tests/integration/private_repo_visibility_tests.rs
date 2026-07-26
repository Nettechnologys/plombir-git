//! Regression coverage for card_76e22c3a8364: a private repository must not be
//! visible from outside it.
//!
//! Three entrances used to hand it over to anyone who asked. `get_repo` returned
//! the model without ever looking at the caller; `list_repos` listed an owner's
//! repositories straight from `list_by_owner_paginated`, which filters on
//! `deleted_at` and nothing else; and global search took no viewer at all, so
//! `rg_core::search::service::search` had nothing to filter on and leaked the
//! repository, its issues and its wiki alike.
//!
//! The rest of the repository-scoped read surface is covered here too — the
//! same handlers that turned up in the sweep: collaborators, milestones,
//! stargazers, forks, commit statuses, mirror and releases. For the routes that
//! address their object by a global id (releases, assets, milestones) the check
//! has two halves, and the second one is tested separately below: a caller who
//! can read *some* repository must not be able to read another repository's
//! objects through it.

use crate::common::{register_full, spawn_test_app};

/// Create a repository with an explicit `is_private`; returns its id.
async fn create_repo_with_visibility(base: &str, token: &str, name: &str, private: bool) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "description": "top secret plans",
            "is_private": private,
        }))
        .send()
        .await
        .expect("create repo");
    assert_eq!(resp.status(), 201, "create_repo({name}) should succeed");
    resp.json::<serde_json::Value>().await.expect("json")["id"]
        .as_i64()
        .expect("repo id")
}

/// GET `path` with an optional bearer token; returns (status, body).
async fn get(base: &str, path: &str, token: Option<&str>) -> (u16, serde_json::Value) {
    let mut req = reqwest::Client::new().get(format!("{base}{path}"));
    if let Some(token) = token {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.expect("request");
    let status = resp.status().as_u16();
    let body = resp.json::<serde_json::Value>().await.unwrap_or_default();
    (status, body)
}

/// Does a search response mention this repository anywhere?
fn mentions_repo(body: &serde_json::Value, repo: &str) -> bool {
    body["results"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .any(|r| r["repo_name"].as_str() == Some(repo))
        })
        .unwrap_or(false)
}

#[tokio::test]
async fn private_repo_is_hidden_from_anonymous_and_outsiders() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "vis-owner", "vis-owner@example.com").await;
    let (outsider_token, _) =
        register_full(&base, "vis-outsider", "vis-outsider@example.com").await;

    create_repo_with_visibility(&base, &owner_token, "secret-repo", true).await;
    create_repo_with_visibility(&base, &owner_token, "public-repo", false).await;

    for (label, token) in [
        ("anonymous", None),
        ("an outsider", Some(outsider_token.as_str())),
    ] {
        // 1. The repository card itself.
        let (status, _) = get(&base, "/api/v1/repos/vis-owner/secret-repo", token).await;
        assert!(
            status == 401 || status == 403,
            "{label} read the private repo card: got {status}"
        );

        // 2. The owner's repository listing.
        let (status, body) = get(&base, "/api/v1/repos/vis-owner", token).await;
        assert_eq!(status, 200, "{label} should still see the owner's listing");
        let names: Vec<&str> = body["data"]
            .as_array()
            .expect("listing data")
            .iter()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert!(
            names.contains(&"public-repo"),
            "{label} should see the public repo, got {names:?}"
        );
        assert!(
            !names.contains(&"secret-repo"),
            "{label} saw the private repo in the owner listing: {names:?}"
        );
        // `total` is what the client pages against — it has to agree with the
        // rows, or the leak just moves from the body to the page count.
        assert_eq!(
            body["pagination"]["total"].as_u64(),
            Some(names.len() as u64),
            "{label}: pagination total disagrees with the returned rows"
        );

        // 3. Global search, all three result types.
        for search_type in ["repos", "all"] {
            let (status, body) = get(
                &base,
                &format!("/api/v1/search?q=secret&type={search_type}"),
                token,
            )
            .await;
            assert_eq!(status, 200, "{label}: search should answer");
            assert!(
                !mentions_repo(&body, "secret-repo"),
                "{label} found the private repo via search type={search_type}: {body}"
            );
        }
    }
}

#[tokio::test]
async fn owner_and_collaborator_still_see_their_private_repo() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "keep-owner", "keep-owner@example.com").await;
    let (collab_token, collab_id) =
        register_full(&base, "keep-collab", "keep-collab@example.com").await;

    create_repo_with_visibility(&base, &owner_token, "secret-repo", true).await;

    let added = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/keep-owner/secret-repo/collaborators"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"user_id": collab_id, "permission": "read"}))
        .send()
        .await
        .expect("add collaborator");
    assert_eq!(added.status(), 201, "adding a collaborator should succeed");

    for (label, token) in [
        ("the owner", owner_token.as_str()),
        ("a collaborator", collab_token.as_str()),
    ] {
        let (status, body) = get(&base, "/api/v1/repos/keep-owner/secret-repo", Some(token)).await;
        assert_eq!(status, 200, "{label} must still read the repo card");
        assert_eq!(body["is_private"].as_bool(), Some(true));

        let (status, body) = get(&base, "/api/v1/repos/keep-owner", Some(token)).await;
        assert_eq!(status, 200);
        let names: Vec<&str> = body["data"]
            .as_array()
            .expect("listing data")
            .iter()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert!(
            names.contains(&"secret-repo"),
            "{label} lost the private repo from the listing: {names:?}"
        );

        let (status, body) = get(&base, "/api/v1/search?q=secret&type=repos", Some(token)).await;
        assert_eq!(status, 200);
        assert!(
            mentions_repo(&body, "secret-repo"),
            "{label} lost the private repo from search: {body}"
        );
    }
}

#[tokio::test]
async fn private_repo_side_endpoints_are_closed_to_outsiders() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "side-owner", "side-owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "side-outsider", "side-out@example.com").await;

    create_repo_with_visibility(&base, &owner_token, "secret-repo", true).await;

    let prefix = "/api/v1/repos/side-owner/secret-repo";
    let paths = [
        format!("{prefix}/collaborators"),
        format!("{prefix}/milestones"),
        format!("{prefix}/stargazers"),
        format!("{prefix}/forks"),
        format!("{prefix}/releases"),
        format!("{prefix}/mirror"),
        format!("{prefix}/commits/deadbeef/statuses"),
        format!("{prefix}/commits/deadbeef/status"),
    ];

    for path in &paths {
        let (anon_status, _) = get(&base, path, None).await;
        assert_eq!(
            anon_status, 401,
            "anonymous reached {path} on a private repo"
        );

        let (outsider_status, _) = get(&base, path, Some(&outsider_token)).await;
        assert_eq!(
            outsider_status, 403,
            "an outsider reached {path} on a private repo"
        );

        // The owner is unaffected — 404 is fine where nothing was created,
        // 200 where the collection is simply empty.
        let (owner_status, _) = get(&base, path, Some(&owner_token)).await;
        assert!(
            owner_status == 200 || owner_status == 404,
            "the owner lost access to {path}: got {owner_status}"
        );
    }
}

#[tokio::test]
async fn objects_are_not_readable_through_another_repo() {
    let base = spawn_test_app().await;
    let (victim_token, _) = register_full(&base, "idor-victim", "idor-victim@example.com").await;
    let (attacker_token, _) = register_full(&base, "idor-thief", "idor-thief@example.com").await;

    create_repo_with_visibility(&base, &victim_token, "secret-repo", true).await;
    create_repo_with_visibility(&base, &attacker_token, "own-repo", false).await;

    let client = reqwest::Client::new();

    // A release and a milestone inside the private repository.
    let release_id = client
        .post(format!(
            "{base}/api/v1/repos/idor-victim/secret-repo/releases"
        ))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({"tag_name": "v1.0.0", "title": "Secret release"}))
        .send()
        .await
        .expect("create release")
        .json::<serde_json::Value>()
        .await
        .expect("json")["id"]
        .as_i64()
        .expect("release id");

    let milestone_id = client
        .post(format!(
            "{base}/api/v1/repos/idor-victim/secret-repo/milestones"
        ))
        .bearer_auth(&victim_token)
        .json(&serde_json::json!({"title": "Secret milestone"}))
        .send()
        .await
        .expect("create milestone")
        .json::<serde_json::Value>()
        .await
        .expect("json")["id"]
        .as_i64()
        .expect("milestone id");

    // The attacker owns `own-repo`, so the repository half of the check passes.
    // The object half must not: these ids live somewhere else.
    let own = "/api/v1/repos/idor-thief/own-repo";
    for path in [
        format!("{own}/releases/{release_id}"),
        format!("{own}/releases/{release_id}/assets"),
        format!("{own}/milestones/{milestone_id}"),
    ] {
        let (status, _) = get(&base, &path, Some(&attacker_token)).await;
        assert_eq!(
            status, 404,
            "{path} read another repository's object through the attacker's own repo"
        );
    }

    // And the victim still reaches their own objects.
    let victim = "/api/v1/repos/idor-victim/secret-repo";
    for path in [
        format!("{victim}/releases/{release_id}"),
        format!("{victim}/releases/{release_id}/assets"),
        format!("{victim}/milestones/{milestone_id}"),
    ] {
        let (status, _) = get(&base, &path, Some(&victim_token)).await;
        assert_eq!(status, 200, "the owner lost access to {path}");
    }
}

#[tokio::test]
async fn private_repo_issues_and_wiki_stay_out_of_search() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "fts-owner", "fts-owner@example.com").await;
    let (outsider_token, _) = register_full(&base, "fts-outsider", "fts-out@example.com").await;

    create_repo_with_visibility(&base, &owner_token, "secret-repo", true).await;
    let client = reqwest::Client::new();

    let issue = client
        .post(format!("{base}/api/v1/repos/fts-owner/secret-repo/issues"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"title": "classified defect", "body": "do not disclose"}))
        .send()
        .await
        .expect("create issue");
    assert_eq!(issue.status(), 201);

    let page = client
        .post(format!("{base}/api/v1/repos/fts-owner/secret-repo/wiki"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"title": "Classified", "content": "classified runbook"}))
        .send()
        .await
        .expect("create wiki page");
    assert_eq!(page.status(), 201);

    for (label, token) in [
        ("anonymous", None),
        ("an outsider", Some(outsider_token.as_str())),
    ] {
        for search_type in ["issues", "wiki", "all"] {
            let (status, body) = get(
                &base,
                &format!("/api/v1/search?q=classified&type={search_type}"),
                token,
            )
            .await;
            assert_eq!(status, 200);
            assert_eq!(
                body["results"].as_array().map(Vec::len),
                Some(0),
                "{label} found private {search_type} content: {body}"
            );
            assert_eq!(
                body["total"].as_i64(),
                Some(0),
                "{label}: total still counts private {search_type} rows"
            );
        }
    }

    // The owner still finds both.
    for search_type in ["issues", "wiki"] {
        let (status, body) = get(
            &base,
            &format!("/api/v1/search?q=classified&type={search_type}"),
            Some(&owner_token),
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(
            body["results"].as_array().map(Vec::len),
            Some(1),
            "the owner lost their own {search_type} results: {body}"
        );
    }
}

#[tokio::test]
async fn org_members_see_private_org_repos_and_outsiders_do_not() {
    let base = spawn_test_app().await;
    let (owner_token, _) = register_full(&base, "org-owner", "org-owner@example.com").await;
    let (member_token, member_id) =
        register_full(&base, "org-member", "org-member@example.com").await;
    let (outsider_token, _) = register_full(&base, "org-stranger", "org-str@example.com").await;

    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": "secretcorp", "visibility": "public"}))
        .send()
        .await
        .expect("create org");
    assert_eq!(created.status(), 201);

    let added = client
        .post(format!("{base}/api/v1/orgs/secretcorp/members"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"user_id": member_id, "role": "member"}))
        .send()
        .await
        .expect("add org member");
    assert_eq!(added.status(), 201, "adding an org member should succeed");

    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "name": "secret-repo",
            "org": "secretcorp",
            "is_private": true,
        }))
        .send()
        .await
        .expect("create org repo");
    assert_eq!(created.status(), 201);

    let listing = "/api/v1/repos/secretcorp";
    for (label, token) in [
        ("an org member", member_token.as_str()),
        ("the org owner", owner_token.as_str()),
    ] {
        let (status, body) = get(&base, listing, Some(token)).await;
        assert_eq!(status, 200);
        let names: Vec<&str> = body["data"]
            .as_array()
            .expect("listing data")
            .iter()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert!(
            names.contains(&"secret-repo"),
            "{label} cannot see the org's private repo: {names:?}"
        );
    }

    for (label, token) in [
        ("anonymous", None),
        ("a stranger", Some(outsider_token.as_str())),
    ] {
        let (status, body) = get(&base, listing, token).await;
        assert_eq!(status, 200);
        let names: Vec<&str> = body["data"]
            .as_array()
            .expect("listing data")
            .iter()
            .filter_map(|r| r["name"].as_str())
            .collect();
        assert!(
            names.is_empty(),
            "{label} saw the org's private repo: {names:?}"
        );
    }
}
