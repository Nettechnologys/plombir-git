//! Every resource a client would show a person carries the page it lives on
//! (card_dca6f5ecbc87): `html_url`, built from `[server].external_url`, on the
//! repository, the pull request, the issue, an issue comment, a review comment,
//! a pipeline and a release — and absent, not guessed, when the instance has
//! no `external_url`.
//!
//! Each case goes through the routed server, on the route a client creates the
//! resource with and on the routes it reads it back through.

use serde_json::{json, Value};

use crate::common::{
    create_initialised_repo, register_full, spawn_test_app_with_overrides, StateOverrides,
};

const OWNER: &str = "html_url_owner";
const REPO: &str = "site";
const PUBLIC: &str = "https://forge.example.test";

struct Fixture {
    base: String,
    db: rg_db::DatabaseConnection,
    token: String,
    repo_id: i64,
    http: reqwest::Client,
}

impl Fixture {
    async fn new(external_url: Option<&str>) -> Self {
        let (base, db) = spawn_test_app_with_overrides(StateOverrides {
            external_url: external_url.map(str::to_string),
            ..Default::default()
        })
        .await;
        let (token, _) = register_full(&base, OWNER, "html_url_owner@example.test").await;
        let repo_id = create_initialised_repo(&base, &token, REPO).await;
        Self {
            base,
            db,
            token,
            repo_id,
            http: reqwest::Client::new(),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v1/repos/{OWNER}/{REPO}{path}", self.base)
    }

    async fn send(&self, request: reqwest::RequestBuilder, expected: u16) -> Value {
        let response = request.bearer_auth(&self.token).send().await.unwrap();
        let status = response.status().as_u16();
        let body = response.text().await.unwrap();
        assert_eq!(status, expected, "{body}");
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("not JSON: {body}"))
    }

    async fn get(&self, path: &str) -> Value {
        self.send(self.http.get(self.url(path)), 200).await
    }

    async fn post(&self, path: &str, body: Value) -> Value {
        self.send(self.http.post(self.url(path)).json(&body), 201)
            .await
    }
}

fn page(path: &str) -> String {
    format!("{PUBLIC}/{OWNER}/{REPO}{path}")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_listed_resource_names_its_page() {
    let f = Fixture::new(Some("https://forge.example.test/")).await;

    // Repository: the read, and the owner's listing.
    assert_eq!(f.get("").await["html_url"], page(""));
    let listing = f
        .send(f.http.get(format!("{}/api/v1/repos/{OWNER}", f.base)), 200)
        .await;
    let listed = listing["data"]
        .as_array()
        .and_then(|repos| repos.iter().find(|repo| repo["name"] == REPO))
        .unwrap_or_else(|| panic!("{REPO} not listed: {listing}"));
    assert_eq!(listed["html_url"], page(""));

    // Issue: create, read, list.
    let issue = f.post("/issues", json!({ "title": "Broken link" })).await;
    let number = issue["number"].as_i64().unwrap();
    let issue_page = page(&format!("/issues/{number}"));
    assert_eq!(issue["html_url"], issue_page);
    assert_eq!(
        f.get(&format!("/issues/{number}")).await["html_url"],
        issue_page
    );
    assert_eq!(f.get("/issues").await["data"][0]["html_url"], issue_page);

    // Issue comment: create, list.
    let comment = f
        .post(
            &format!("/issues/{number}/comments"),
            json!({ "body": "Seen it" }),
        )
        .await;
    let comment_page = page(&format!(
        "/issues/{number}#comment-{}",
        comment["id"].as_i64().unwrap()
    ));
    assert_eq!(comment["html_url"], comment_page);
    assert_eq!(
        f.get(&format!("/issues/{number}/comments")).await[0]["html_url"],
        comment_page
    );

    // Pull request: create, read, list.
    let written = f
        .http
        .post(f.url("/contents/feature.txt"))
        .bearer_auth(&f.token)
        .json(&json!({ "content": "feature\n", "message": "feature", "branch": "feature" }))
        .send()
        .await
        .unwrap();
    assert!(
        written.status().is_success(),
        "{}",
        written.text().await.unwrap()
    );
    let pr = f
        .post(
            "/pulls",
            json!({ "title": "Feature", "head": "feature", "base": "main" }),
        )
        .await;
    let pr_number = pr["number"].as_i64().unwrap();
    let pr_page = page(&format!("/pulls/{pr_number}"));
    assert_eq!(pr["html_url"], pr_page);
    assert_eq!(
        f.get(&format!("/pulls/{pr_number}")).await["html_url"],
        pr_page
    );
    assert_eq!(f.get("/pulls").await["data"][0]["html_url"], pr_page);

    // Review comment: create, list.
    let review_comment = f
        .post(
            &format!("/pulls/{pr_number}/comments"),
            json!({ "path": "feature.txt", "body": "Nice", "line": 1, "side": "RIGHT" }),
        )
        .await;
    let review_comment_page = page(&format!(
        "/pulls/{pr_number}#review-comment-{}",
        review_comment["id"].as_i64().unwrap()
    ));
    assert_eq!(review_comment["html_url"], review_comment_page);
    assert_eq!(
        f.get(&format!("/pulls/{pr_number}/comments")).await[0]["html_url"],
        review_comment_page
    );

    // Release: create, read, update, list.
    let release = f
        .post(
            "/releases",
            json!({ "tag_name": "v1.0.0", "title": "First" }),
        )
        .await;
    let release_id = release["id"].as_i64().unwrap();
    let release_page = page(&format!("/releases?release={release_id}"));
    assert_eq!(release["html_url"], release_page);
    assert_eq!(
        f.get(&format!("/releases/{release_id}")).await["html_url"],
        release_page
    );
    let updated = f
        .send(
            f.http
                .patch(f.url(&format!("/releases/{release_id}")))
                .json(&json!({ "title": "First, renamed" })),
            200,
        )
        .await;
    assert_eq!(updated["html_url"], release_page);
    assert_eq!(
        f.get("/releases").await["data"][0]["html_url"],
        release_page
    );

    // Pipeline: read, list. There is no CI config to trigger one from, so the
    // row is made the way the runner fixtures make theirs.
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &f.db,
        f.repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();
    let pipeline_page = page(&format!("/pipelines?pipeline={}", pipeline.id));
    assert_eq!(
        f.get(&format!("/pipelines/{}", pipeline.id)).await["pipeline"]["html_url"],
        pipeline_page
    );
    assert_eq!(
        f.get("/pipelines").await["data"][0]["html_url"],
        pipeline_page
    );
}

/// Without `external_url` there is no address to name: the field is absent
/// rather than a relative path or one guessed from the request's `Host`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_an_external_url_no_resource_claims_a_page() {
    let f = Fixture::new(None).await;
    let repo = f.get("").await;
    assert!(repo.get("html_url").is_none(), "{repo}");
    let issue = f.post("/issues", json!({ "title": "Broken link" })).await;
    assert!(issue.get("html_url").is_none(), "{issue}");
    let number = issue["number"].as_i64().unwrap();
    let comment = f
        .post(
            &format!("/issues/{number}/comments"),
            json!({ "body": "Seen it" }),
        )
        .await;
    assert!(comment.get("html_url").is_none(), "{comment}");
    let release = f
        .post(
            "/releases",
            json!({ "tag_name": "v1.0.0", "title": "First" }),
        )
        .await;
    assert!(release.get("html_url").is_none(), "{release}");
}
