//! Acceptance for card_313cfb29febf: every signed `limit` on the AI API is
//! validated before it can become an unsigned collection or SQL limit.
//!
//! And for card_c386beea2fe0, the other half of the same boundary: a validated
//! limit has to reach the database. `ai_list_issues` and `ai_list_prs` used to
//! call the unpaginated service and cut the page out of the result with
//! `.take(limit)`, so `?limit=20` against a repository with 20 000 open issues
//! read 20 000 rows to answer with twenty.

use crate::common::source_scan;
use crate::common::{
    create_issue, create_repo, register_full, register_user, spawn_test_app, spawn_test_app_with_db,
};
use sea_orm::ActiveValue::{NotSet, Set};

#[allow(dead_code)]
mod rust_source {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/support/rust_source.rs"
    ));
}

const PW: &str = "Qz7$wRtm";
const OWNER: &str = "ai-limit-owner";
const REPO: &str = "ai-limit-repo";

fn skip_source_whitespace(source: &str, mut at: usize) -> usize {
    while let Some(ch) = source[at..].chars().next() {
        if !ch.is_whitespace() {
            break;
        }
        at += ch.len_utf8();
    }
    at
}

fn source_token_end(source: &str, at: usize, token: &str) -> Option<usize> {
    let at = skip_source_whitespace(source, at);
    let end = at + token.len();
    if source.get(at..end) != Some(token) {
        return None;
    }

    let is_identifier = token.chars().all(|ch| ch.is_alphanumeric() || ch == '_');
    if is_identifier
        && (source[..at]
            .chars()
            .next_back()
            .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
            || source[end..]
                .chars()
                .next()
                .is_some_and(|ch| ch.is_alphanumeric() || ch == '_'))
    {
        return None;
    }

    Some(end)
}

fn signed_limit_field_lines_in_code(code: &str) -> Vec<usize> {
    code.match_indices("pub")
        .filter_map(|(pub_at, _)| {
            let mut at = pub_at;
            for token in ["pub", "limit", ":", "Option", "<", "i64", ">", ","] {
                at = source_token_end(code, at, token)?;
            }
            Some(code[..pub_at].bytes().filter(|byte| *byte == b'\n').count() + 1)
        })
        .collect()
}

fn signed_limit_field_lines(source: &str) -> Vec<usize> {
    signed_limit_field_lines_in_code(&rust_source::production_rust_code_only(source))
}

#[test]
fn every_signed_ai_limit_reaches_the_shared_validator() {
    let source = include_str!("../../src/api/ai.rs");
    let handlers = ["ai_list_issues", "ai_list_prs", "ai_search_code"];
    let limit_fields = signed_limit_field_lines(source);

    assert_eq!(
        limit_fields.len(),
        handlers.len(),
        "a new signed AI limit needs to join the shared validation contract; \
         production fields are at lines {limit_fields:?}"
    );

    let functions = source_scan::functions(source);
    for handler in handlers {
        let body = functions
            .iter()
            .find(|function| function.name == handler)
            .unwrap_or_else(|| panic!("{handler} is not declared in api/ai.rs"));
        let (uses_validator, converts_locally) = signed_limit_body_facts(&body.body);
        assert!(
            uses_validator,
            "{handler} bypasses the shared signed-limit validator"
        );
        assert!(
            !converts_locally,
            "{handler} converts the signed request value locally"
        );
    }
}

#[test]
fn signed_limit_field_census_reads_only_production_fields() {
    const SAMPLE: &str = r####"
// pub limit: Option<i64>,
/* pub limit: Option<i64>, */
let normal = "pub limit: Option<i64>,";
let raw = r#"pub limit: Option<i64>,"#;
let bytes = b"pub limit: Option<i64>,";

pub struct First {
    pub
        limit : Option < i64 >,
    pub limit_extra: Option<i64>,
    pub other_limit: Option<i64>,
    pub limit: Option<u64>,
}

#[cfg(test)]
mod early_tests {
    pub struct Hidden {
        pub limit: Option<i64>,
    }
}

pub struct AfterTests {
    pub limit: Option<i64>,
}
"####;

    let fields = signed_limit_field_lines(SAMPLE);
    assert_eq!(fields.len(), 2, "production fields were {fields:?}");

    let raw_fields = signed_limit_field_lines_in_code(SAMPLE);
    assert!(
        raw_fields.len() > fields.len(),
        "the raw-source mutation unexpectedly ignored Rust data and test-only fields"
    );
}

#[test]
fn removing_a_production_signed_limit_field_fails_the_census() {
    let source = include_str!("../../src/api/ai.rs");
    let fields = signed_limit_field_lines(source);
    assert_eq!(
        fields.len(),
        3,
        "the production fixture changed: {fields:?}"
    );

    let mutated = source.replacen(
        "pub limit: Option<i64>,",
        "pub removed_limit: Option<i64>,",
        1,
    );
    assert_ne!(
        mutated, source,
        "the field-removal mutation changed nothing"
    );
    assert_eq!(
        signed_limit_field_lines(&mutated).len(),
        fields.len() - 1,
        "removing one production field did not lower the census"
    );
}

fn signed_limit_body_facts(body: &str) -> (bool, bool) {
    let code = source_scan::rust_code_only(body);
    (
        code.contains("ai_limit(params.limit)?"),
        code.contains("params.limit.unwrap_or") || code.contains("params.limit as"),
    )
}

#[test]
fn signed_limit_guard_ignores_non_code_decoys_and_keeps_live_calls() {
    const DECOYS: &str = r###"
let normal = "ai_limit(params.limit)? params.limit.unwrap_or params.limit as";
let raw = r#"ai_limit(params.limit)? params.limit.unwrap_or params.limit as"#;
let bytes = b"ai_limit(params.limit)? params.limit.unwrap_or params.limit as";
// ai_limit(params.limit)? params.limit.unwrap_or params.limit as
/* ai_limit(params.limit)? params.limit.unwrap_or params.limit as */
"###;

    assert_eq!(signed_limit_body_facts(DECOYS), (false, false));
    assert_eq!(
        signed_limit_body_facts(&format!("{DECOYS}\nai_limit(params.limit)?;")),
        (true, false)
    );
    assert_eq!(
        signed_limit_body_facts(&format!("{DECOYS}\nlet _ = params.limit.unwrap_or(20);")),
        (false, true)
    );
}

/// What a listing handler's body says about how its page is bounded.
///
/// Both facts are read from the byte-aligned code-only view: a call-shaped
/// string literal is Rust *data*, and the paragraph above each call names the
/// truncation it replaced, so prose that mentions `.take(` is not a truncation
/// and a comment mentioning the paginated read is not the read.
fn listing_bound_facts(body: &str, bounded_call: &str) -> (bool, bool) {
    let code = source_scan::rust_code_only(body);
    (code.contains(bounded_call), code.contains(".take("))
}

/// card_c386beea2fe0: the validated limit has to be spent on the query.
///
/// This is a source guard rather than a request because the defect is invisible
/// from outside: reading every row and dropping all but `limit` of them answers
/// byte-for-byte the same as a `LIMIT`, and the row count sea-orm actually read
/// is not observable through this crate's test harness. What separates the two
/// is which call the handler makes, so that is what is asserted — and it is the
/// assertion that reddens if the truncation comes back.
#[test]
fn every_ai_listing_spends_its_limit_on_the_query() {
    let source = include_str!("../../src/api/ai.rs");
    let functions = source_scan::functions(source);

    for (handler, bounded_call) in [
        ("ai_list_issues", "list_issues_paginated("),
        ("ai_list_prs", "list_prs_paginated("),
        ("ai_search_code", "search_code(&params.q"),
    ] {
        let body = &functions
            .iter()
            .find(|function| function.name == handler)
            .unwrap_or_else(|| panic!("{handler} is not declared in api/ai.rs"))
            .body;

        let (reads_through_the_bound, truncates_in_memory) =
            listing_bound_facts(body, bounded_call);
        assert!(
            reads_through_the_bound,
            "{handler} must read through `{bounded_call}` so the limit reaches SQL"
        );
        assert!(
            !truncates_in_memory,
            "{handler} truncates in Rust: the rows above the limit were read from the \
             database before being dropped, which is the cost the limit exists to bound"
        );
    }
}

#[test]
fn listing_bound_guard_ignores_non_code_decoys_and_keeps_live_calls() {
    const DECOYS: &str = r###"
let normal = "list_issues_paginated(&state.db) .take(limit)";
let raw = r#"list_issues_paginated(&state.db) .take(limit)"#;
let bytes = b"list_issues_paginated(&state.db) .take(limit)";
// list_issues_paginated(&state.db) .take(limit)
/* list_issues_paginated(&state.db) .take(limit) */
"###;

    assert_eq!(
        listing_bound_facts(DECOYS, "list_issues_paginated("),
        (false, false)
    );
    assert_eq!(
        listing_bound_facts(
            &format!("{DECOYS}\nlist_issues_paginated(&state.db, page).await?;"),
            "list_issues_paginated(",
        ),
        (true, false)
    );
    assert_eq!(
        listing_bound_facts(
            &format!("{DECOYS}\nlet page = rows.into_iter().take(limit).collect();"),
            "list_issues_paginated(",
        ),
        (false, true)
    );
}

/// The behaviour the guard above cannot see on its own: the bound still has to
/// compose with the state filter and with the order the listing promises. A
/// page of one is the newest matching row, not the newest row of any state.
#[tokio::test]
async fn an_ai_listing_page_is_bounded_and_still_filtered_and_ordered() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "ai-page-owner", "ai-page@example.test").await;
    let repo_id = create_repo(&base, &token, "ai-page-repo").await;

    for n in 1..=3 {
        create_issue(
            &base,
            &token,
            "ai-page-owner",
            "ai-page-repo",
            &format!("issue {n}"),
        )
        .await;
        seed_pull_request(&db, repo_id, user_id, n, "open").await;
    }
    // Newer than everything above and in the state the listing must not serve —
    // a page that ignored `state` would hand this one back first.
    seed_pull_request(&db, repo_id, user_id, 4, "closed").await;

    let issues = ai_listing(&base, &token, "issues", "1").await;
    assert_eq!(issues.len(), 1, "a page of one must carry one issue");
    assert_eq!(
        issues[0]["title"], "issue 3",
        "the page of one is the newest open issue, not an arbitrary row: {issues:?}"
    );

    let prs = ai_listing(&base, &token, "prs", "1").await;
    assert_eq!(prs.len(), 1, "a page of one must carry one pull request");
    assert_eq!(
        prs[0]["number"], 3,
        "the page of one is the newest OPEN pull request — #4 is closed: {prs:?}"
    );

    // A limit above the row count neither invents rows nor drops any.
    assert_eq!(ai_listing(&base, &token, "issues", "100").await.len(), 3);
    assert_eq!(ai_listing(&base, &token, "prs", "100").await.len(), 3);
}

/// The AI listings answer with a bare array, not a `{data, pagination}` page.
async fn ai_listing(
    base: &str,
    token: &str,
    endpoint: &str,
    limit: &str,
) -> Vec<serde_json::Value> {
    let response = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/ai/repos/ai-page-owner/ai-page-repo/{endpoint}"
        ))
        .bearer_auth(token)
        .query(&[("limit", limit)])
        .send()
        .await
        .expect("AI list request");
    let status = response.status();
    let body = response.text().await.expect("read AI list body");
    assert_eq!(status, 200, "GET {endpoint}?limit={limit} answered: {body}");
    serde_json::from_str(&body).expect("AI listing is a JSON array")
}

async fn seed_pull_request(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    n: i64,
    state: &str,
) {
    // Distinct, increasing timestamps: the order the assertions rely on must
    // come from the data, not from how fast the seeding loop ran.
    let created_at = chrono::Utc::now() + chrono::Duration::seconds(n);
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(n),
            title: Set(format!("pull {n}")),
            body: Set(None),
            state: Set(state.to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(None),
            head_branch: Set(format!("feature-{n}")),
            base_branch: Set("main".to_string()),
            head_sha: Set(None),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(created_at),
            updated_at: Set(created_at),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed pull request");
}

#[tokio::test]
async fn every_ai_listing_rejects_a_non_positive_limit() {
    let base = spawn_test_app().await;
    let token = register_user(&base, OWNER, "ai-limit-owner@example.test", PW).await;
    create_repo(&base, &token, REPO).await;
    let client = reqwest::Client::new();

    for invalid in ["-1", "0"] {
        for endpoint in ["issues", "prs", "search/code"] {
            let mut query = vec![("limit", invalid)];
            if endpoint == "search/code" {
                query.push(("q", "needle"));
            }

            let response = client
                .get(format!("{base}/api/v1/ai/repos/{OWNER}/{REPO}/{endpoint}"))
                .bearer_auth(&token)
                .query(&query)
                .send()
                .await
                .expect("AI list request");
            let status = response.status();
            let body: serde_json::Value = response.json().await.expect("JSON error body");

            assert_eq!(
                status, 400,
                "{endpoint}?limit={invalid} must reject the malformed boundary: {body}"
            );
            assert_eq!(
                body["error"]["message"], "limit must be greater than zero",
                "{endpoint}?limit={invalid} must diagnose the violated boundary: {body}"
            );
        }
    }
}
