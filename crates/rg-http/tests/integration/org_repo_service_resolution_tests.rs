//! card_79478d256678: repository-scoped rg-core services must resolve both
//! personal and organization namespaces through the canonical resolver.

use crate::common::source_scan::rust_code_only;
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

#[derive(Debug)]
struct ResolveRepoHelper {
    line: usize,
    body: String,
}

fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn token_at(bytes: &[u8], at: usize, token: &[u8]) -> bool {
    bytes.get(at..at + token.len()) == Some(token)
        && (at == 0 || !is_ident_byte(bytes[at - 1]))
        && bytes
            .get(at + token.len())
            .is_none_or(|byte| !is_ident_byte(*byte))
}

fn attribute_at(bytes: &[u8], at: usize) -> Option<(usize, bool)> {
    if bytes.get(at..at + 2) != Some(b"#[") {
        return None;
    }

    let mut depth = 1usize;
    let mut end = at + 2;
    while end < bytes.len() && depth > 0 {
        match bytes[end] {
            b'[' => depth += 1,
            b']' => depth -= 1,
            _ => {}
        }
        end += 1;
    }
    if depth != 0 {
        return None;
    }

    let compact: Vec<u8> = bytes[at..end]
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    Some((end, compact == b"#[cfg(test)]"))
}

fn function_end(code: &str, fn_at: usize) -> Option<usize> {
    let bytes = code.as_bytes();
    let mut at = fn_at;
    while at < bytes.len() {
        match bytes[at] {
            b';' => return None,
            b'{' => break,
            _ => at += 1,
        }
    }
    if bytes.get(at) != Some(&b'{') {
        return None;
    }

    let mut depth = 1usize;
    at += 1;
    while at < bytes.len() {
        match bytes[at] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            _ => {}
        }
        at += 1;
    }
    None
}

/// Production, top-level `resolve_repo` helpers and their code-only bodies.
///
/// The shared source view removes every Rust comment and literal without moving
/// a byte. This small, contract-specific range scan then keeps nested test
/// modules out by construction and rejects a directly `#[cfg(test)]` helper.
fn resolve_repo_helpers(source: &str) -> Vec<ResolveRepoHelper> {
    let code = rust_code_only(source);
    let bytes = code.as_bytes();
    let mut helpers = Vec::new();
    let mut at = 0usize;
    let mut depth = 0usize;
    let mut cfg_test = false;

    while at < bytes.len() {
        if depth == 0 {
            if let Some((end, is_cfg_test)) = attribute_at(bytes, at) {
                cfg_test |= is_cfg_test;
                at = end;
                continue;
            }
            if token_at(bytes, at, b"fn") {
                let mut name_start = at + 2;
                while bytes
                    .get(name_start)
                    .is_some_and(|byte| byte.is_ascii_whitespace())
                {
                    name_start += 1;
                }
                let mut name_end = name_start;
                while bytes.get(name_end).is_some_and(|byte| is_ident_byte(*byte)) {
                    name_end += 1;
                }
                let end = function_end(&code, name_end).unwrap_or(name_end);
                if !cfg_test && bytes.get(name_start..name_end) == Some(b"resolve_repo") {
                    helpers.push(ResolveRepoHelper {
                        line: code[..at].bytes().filter(|byte| *byte == b'\n').count() + 1,
                        body: code[at..end].to_owned(),
                    });
                }
                cfg_test = false;
                at = end.max(at + 2);
                continue;
            }
        }

        match bytes[at] {
            b'{' => {
                if depth == 0 {
                    cfg_test = false;
                }
                depth += 1;
            }
            b'}' => depth = depth.saturating_sub(1),
            b';' if depth == 0 => cfg_test = false,
            _ => {}
        }
        at += 1;
    }

    helpers
}

fn calls_named(code: &str, name: &str) -> bool {
    let bytes = code.as_bytes();
    let needle = name.as_bytes();
    let mut cursor = 0usize;

    while let Some(relative) = code[cursor..].find(name) {
        let at = cursor + relative;
        let end = at + needle.len();
        let bounded = (at == 0 || !is_ident_byte(bytes[at - 1]))
            && bytes.get(end).is_none_or(|byte| !is_ident_byte(*byte));
        let mut call_at = end;
        while bytes
            .get(call_at)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            call_at += 1;
        }
        if bounded && bytes.get(call_at) == Some(&b'(') {
            return true;
        }
        cursor = end;
    }

    false
}

fn resolve_repo_offender_lines(source: &str) -> Vec<usize> {
    resolve_repo_helpers(source)
        .into_iter()
        .filter(|helper| {
            calls_named(&helper.body, "find_by_username")
                || calls_named(&helper.body, "find_personal_by_owner_and_name")
                || !calls_named(&helper.body, "find_repo_by_owner_name")
        })
        .map(|helper| helper.line)
        .collect()
}

#[test]
fn resolve_repo_scan_ignores_rust_data_and_test_helpers() {
    const SAMPLE: &str = r#####"
// async fn resolve_repo() { find_repo_by_owner_name(); }
/* async fn resolve_repo() { find_by_username(); } */
const NORMAL: &str = "async fn resolve_repo() { find_repo_by_owner_name(); }";
const RAW: &str = r#"async fn resolve_repo() { find_by_username(); }"#;
const BYTES: &[u8] = b"async fn resolve_repo() { find_repo_by_owner_name(); }";
const RAW_BYTES: &[u8] = br#"async fn resolve_repo() { find_by_username(); }"#;

#[cfg(test)]
async fn resolve_repo() {
    find_repo_by_owner_name();
}

#[cfg(test)]
mod tests {
    async fn resolve_repo() {
        find_repo_by_owner_name();
    }
}

async
fn resolve_repo
(
    db: &DatabaseConnection,
) -> Result<Repository> {
    let normal = "find_by_username(db)";
    let raw = r#"find_personal_by_owner_and_name(db)"#;
    let _ = (normal, raw);
    crate::repo::service::find_repo_by_owner_name
        (db)
}
"#####;

    let helpers = resolve_repo_helpers(SAMPLE);
    assert_eq!(helpers.len(), 1, "unexpected helpers: {helpers:#?}");
    let expected_line = SAMPLE
        .lines()
        .position(|line| line == "fn resolve_repo")
        .expect("live helper declaration")
        + 1;
    assert_eq!(helpers[0].line, expected_line);
    assert!(resolve_repo_offender_lines(SAMPLE).is_empty());
}

#[test]
fn raw_source_mutation_cannot_be_rescued_by_a_literal_canonical_call() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rg-core/src/collaborator/service.rs");
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let helper_lines = resolve_repo_helpers(&source)
        .into_iter()
        .map(|helper| helper.line)
        .collect::<Vec<_>>();
    assert_eq!(
        helper_lines.len(),
        1,
        "the production fixture must contain exactly one resolve_repo helper"
    );
    assert!(
        resolve_repo_offender_lines(&source).is_empty(),
        "the unmodified production fixture is already an offender"
    );

    let live_call = "crate::repo::service::find_repo_by_owner_name";
    let mut mutated = source.replacen(live_call, "missing_namespace_canon", 1);
    assert_ne!(mutated, source, "the raw-source mutation changed nothing");
    mutated.push_str(
        r####"
const CANONICAL_DECOY: &str =
    r#"async fn resolve_repo() { crate::repo::service::find_repo_by_owner_name(); }"#;
"####,
    );

    let offenders = resolve_repo_offender_lines(&mutated);
    assert_eq!(
        offenders, helper_lines,
        "a canonical call stored as Rust data rescued the mutated production helper"
    );
}

#[test]
fn rg_core_resolve_repo_helpers_delegate_to_the_namespace_canon() {
    let rg_core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../rg-core/src");
    let mut offenders = Vec::new();
    let mut helpers_checked = 0usize;

    for path in rust_sources(&rg_core) {
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        helpers_checked += resolve_repo_helpers(&source).len();
        for line in resolve_repo_offender_lines(&source) {
            offenders.push(format!("{}:{line}", path.display()));
        }
    }

    assert!(
        helpers_checked >= 6,
        "only {helpers_checked} production resolve_repo helper(s) found — the source census is not running"
    );
    assert!(
        offenders.is_empty(),
        "repository helpers bypass the namespace-aware canonical resolver:\n{}",
        offenders.join("\n")
    );
}
