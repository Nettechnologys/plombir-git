//! A repository address resolves by the spelling its rows carry, and by no
//! other (card_3b3d49c60ab1).
//!
//! Some forty handlers build `<owner>/<repo>.git`, LFS keys, release keys and
//! staging paths from the request's path segments. On SQLite and PostgreSQL
//! that is the row's spelling by construction, because the lookup compares
//! names exactly. MySQL's default collation does not: `alice/myrepo` found
//! `Alice/MyRepo`, the gate let the request through, and the handler went on
//! to a path that does not exist — or, where it writes, to a second copy under
//! another prefix. The resolver now refuses the other spelling, so only a
//! server database with a case-insensitive collation can show the difference.
//! The registry is the one client that cannot spell an address as the rows
//! do, and it keeps its own folding resolver.

use std::path::Path;

use sha2::{Digest, Sha256};

use crate::common::{register_full, wait_for_listener};

async fn send(
    method: reqwest::Method,
    url: String,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (u16, String) {
    let mut request = reqwest::Client::new().request(method, url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.text().await.unwrap())
}

/// Every path under `root` that has a component spelled exactly `segment`.
fn paths_with_component(root: &Path, segment: &str) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_name().to_str() == Some(segment)
                || entry.file_name().to_str() == Some(&format!("{segment}.git"))
            {
                found.push(path.clone());
            }
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(path);
            }
        }
    }
    found
}

/// What one address answers to git, LFS and releases, as `(info/refs, LFS
/// upload batch, release creation)` statuses.
async fn statuses(base: &str, token: &str, owner: &str, repo: &str, tag: &str) -> [u16; 3] {
    let (git, _) = send(
        reqwest::Method::GET,
        format!("{base}/git/{owner}/{repo}.git/info/refs?service=git-upload-pack"),
        None,
        None,
    )
    .await;
    let payload = format!("an LFS object for {owner}/{repo}");
    let (lfs, _) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/{owner}/{repo}/lfs/objects/batch"),
        Some(token),
        Some(serde_json::json!({
            "operation": "upload",
            "objects": [{
                "oid": hex::encode(Sha256::digest(payload.as_bytes())),
                "size": payload.len(),
            }],
            "transfers": ["basic"],
        })),
    )
    .await;
    let (release, _) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/{owner}/{repo}/releases"),
        Some(token),
        Some(serde_json::json!({
            "tag_name": tag,
            "title": tag,
            "target_commitish": "main",
        })),
    )
    .await;
    [git, lfs, release]
}

#[tokio::test]
#[ignore = "requires PLOMBIR_GIT_TEST_DATABASE_URL pointing at disposable MySQL (case-insensitive collation)"]
async fn another_spelling_of_a_repository_address_is_not_found_and_writes_nothing() {
    let _shared = crate::common::SHARED_SERVER_DB.lock().await;
    crate::common::install_test_tracing();
    let database_url = std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL")
        .expect("PLOMBIR_GIT_TEST_DATABASE_URL must be set");
    assert!(
        database_url.starts_with("mysql://"),
        "only a case-insensitive collation reaches one repository by two spellings"
    );
    let db = rg_db::connect_with_pool(&database_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to disposable server database");
    rg_db::run_migrations(&db)
        .await
        .expect("migrate disposable server database");
    let dir = tempfile::tempdir().unwrap();
    let repo_root = dir.path().join("repos");
    let base = crate::common::spawn_test_app_over_db_with(
        db.clone(),
        repo_root.clone(),
        crate::common::StateOverrides::default(),
    )
    .await;
    wait_for_listener(base.trim_start_matches("http://")).await;

    // A server database outlives one run, so the names carry a suffix.
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..10];
    let owner = format!("Adr{suffix}");
    let lower_owner = owner.to_lowercase();
    let org = format!("AdrOrg{suffix}");
    let lower_org = org.to_lowercase();
    let (token, _) = register_full(&base, &owner, &format!("{lower_owner}@example.com")).await;
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/orgs"),
        Some(&token),
        Some(serde_json::json!({ "name": org })),
    )
    .await;
    assert!(status == 201 || status == 200, "{status}: {body}");
    for (namespace, repo) in [(None, "MyRepo"), (Some(org.as_str()), "OrgRepo")] {
        let mut request =
            serde_json::json!({ "name": repo, "auto_init": true, "readme": "default" });
        if let Some(org) = namespace {
            request["org"] = serde_json::json!(org);
        }
        let (status, body) = send(
            reqwest::Method::POST,
            format!("{base}/api/v1/repos"),
            Some(&token),
            Some(request),
        )
        .await;
        assert_eq!(status, 201, "{body}");
    }

    for (row_owner, row_repo, other_owner, other_repo) in [
        (owner.as_str(), "MyRepo", lower_owner.as_str(), "myrepo"),
        (owner.as_str(), "MyRepo", owner.as_str(), "myrepo"),
        (org.as_str(), "OrgRepo", lower_org.as_str(), "orgrepo"),
        (org.as_str(), "OrgRepo", lower_org.as_str(), "OrgRepo"),
    ] {
        assert_eq!(
            statuses(&base, &token, other_owner, other_repo, "v-other").await,
            [404, 404, 404],
            "{other_owner}/{other_repo} must not reach {row_owner}/{row_repo}"
        );
        assert!(
            rg_core::repo::service::find_repo_by_owner_name(&db, other_owner, other_repo)
                .await
                .unwrap()
                .is_none(),
            "the resolver answered {other_owner}/{other_repo}"
        );
        // The registry still reaches the row, and names it as the row does.
        let (repository, namespace) = rg_core::repo::service::find_repo_and_namespace_for_registry(
            &db,
            other_owner,
            other_repo,
        )
        .await
        .unwrap()
        .expect("the registry folds the address");
        assert_eq!(
            (namespace.as_str(), repository.name.as_str()),
            (row_owner, row_repo)
        );
    }

    // The rows' own spelling works end to end.
    assert_eq!(
        statuses(&base, &token, &owner, "MyRepo", "v-row").await,
        [200, 200, 201]
    );
    assert_eq!(
        statuses(&base, &token, &org, "OrgRepo", "v-row").await,
        [200, 200, 201]
    );

    // And nothing was ever written under another spelling.
    for spelling in [
        &lower_owner,
        &lower_org,
        &"myrepo".to_string(),
        &"orgrepo".to_string(),
    ] {
        assert_eq!(
            paths_with_component(dir.path(), spelling),
            Vec::<std::path::PathBuf>::new(),
            "something was written under {spelling}"
        );
    }
}
