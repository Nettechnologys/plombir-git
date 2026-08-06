//! Acceptance for card_313cfb29febf: every signed `limit` on the AI API is
//! validated before it can become an unsigned collection or SQL limit.

use crate::common::source_scan;
use crate::common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";
const OWNER: &str = "ai-limit-owner";
const REPO: &str = "ai-limit-repo";

#[test]
fn every_signed_ai_limit_reaches_the_shared_validator() {
    let source = include_str!("../../src/api/ai.rs");
    let handlers = ["ai_list_issues", "ai_list_prs", "ai_search_code"];

    assert_eq!(
        source.matches("pub limit: Option<i64>").count(),
        handlers.len(),
        "a new signed AI limit needs to join the shared validation contract"
    );

    let functions = source_scan::functions(source);
    for handler in handlers {
        let body = functions
            .iter()
            .find(|function| function.name == handler)
            .unwrap_or_else(|| panic!("{handler} is not declared in api/ai.rs"));
        assert!(
            body.body.contains("ai_limit(params.limit)?"),
            "{handler} bypasses the shared signed-limit validator"
        );
        assert!(
            !body.body.contains("params.limit.unwrap_or") && !body.body.contains("params.limit as"),
            "{handler} converts the signed request value locally"
        );
    }
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
