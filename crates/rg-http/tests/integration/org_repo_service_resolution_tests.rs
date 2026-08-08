//! card_79478d256678: repository-scoped rg-core services must resolve both
//! personal and organization namespaces through the canonical resolver.

use crate::common::{register_full, spawn_test_app_with_db};
use chrono::Utc;
use sea_orm::Set;
use std::path::{Path, PathBuf};

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
        "baseline: organization creation failed: {}",
        response.text().await.expect("organization response body")
    );
}

async fn create_repo(base: &str, token: &str, name: &str, org: Option<&str>) {
    let mut body = serde_json::json!({ "name": name });
    if let Some(org) = org {
        body["org"] = serde_json::json!(org);
    }
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("create repository");
    assert_eq!(
        response.status(),
        201,
        "baseline: repository creation failed: {}",
        response.text().await.expect("repository response body")
    );
}

async fn insert_pr(db: &rg_db::DatabaseConnection, repo_id: i64, author_id: i64, number: i64) {
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(number),
            title: Set(format!("namespace resolver PR {number}")),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some("0".repeat(40))),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("insert pull request");
}

async fn assert_surfaces_resolve(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    namespace_kind: &str,
) {
    let paths = [
        "labels".to_string(),
        "collaborators".to_string(),
        "branches/protection".to_string(),
        "pulls".to_string(),
        "pulls/1/reviews".to_string(),
    ];

    for suffix in paths {
        let response = client
            .get(format!("{base}/api/v1/repos/{owner}/{repo}/{suffix}"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap_or_else(|error| panic!("{namespace_kind} {suffix} request failed: {error}"));
        let status = response.status();
        let body = response.text().await.expect("surface response body");
        assert_eq!(
            status, 200,
            "{namespace_kind} repository failed to resolve on {suffix}: {body}"
        );
    }
}

#[tokio::test]
async fn personal_and_org_repositories_reach_all_five_repo_services() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, owner_id) =
        register_full(&base, "resolver-owner", "resolver-owner@example.com").await;

    create_org(&base, &token, "resolver-org").await;
    create_repo(&base, &token, "personal", None).await;
    create_repo(&base, &token, "organizational", Some("resolver-org")).await;

    let personal =
        rg_core::repo::service::find_repo_by_owner_name(&db, "resolver-owner", "personal")
            .await
            .expect("resolve personal repository")
            .expect("personal repository exists");
    let organizational =
        rg_core::repo::service::find_repo_by_owner_name(&db, "resolver-org", "organizational")
            .await
            .expect("resolve organization repository")
            .expect("organization repository exists");
    insert_pr(&db, personal.id, owner_id, 1).await;
    insert_pr(&db, organizational.id, owner_id, 1).await;

    let client = reqwest::Client::new();
    assert_surfaces_resolve(
        &client,
        &base,
        &token,
        "resolver-owner",
        "personal",
        "personal baseline",
    )
    .await;
    assert_surfaces_resolve(
        &client,
        &base,
        &token,
        "resolver-org",
        "organizational",
        "organization",
    )
    .await;
}

fn rust_sources(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut sources = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
        {
            let path = entry.expect("directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().and_then(|extension| extension.to_str()) == Some("rs") {
                sources.push(path);
            }
        }
    }
    sources
}

#[test]
fn rg_core_resolve_repo_helpers_delegate_to_the_namespace_canon() {
    let rg_core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rg-core/src");
    let mut offenders = Vec::new();

    for path in rust_sources(&rg_core) {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let lines: Vec<_> = source.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains("async fn resolve_repo(") {
                continue;
            }
            let body = lines[index..lines.len().min(index + 30)].join("\n");
            if body.contains("find_by_username")
                || body.contains("find_personal_by_owner_and_name")
                || !body.contains("find_repo_by_owner_name")
            {
                offenders.push(format!("{}:{}", path.display(), index + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "repository helpers bypass the namespace-aware canonical resolver:\n{}",
        offenders.join("\n")
    );
}
