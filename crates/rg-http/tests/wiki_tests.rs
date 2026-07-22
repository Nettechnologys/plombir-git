//! Integration tests for Wiki endpoints + revision history.
//!
//! Guards:
//!   POST   /repos/:o/:r/wiki                   — create page
//!   GET    /repos/:o/:r/wiki                   — list pages
//!   GET    /repos/:o/:r/wiki/:title            — get page
//!   PATCH  /repos/:o/:r/wiki/:title            — update page (saves revision)
//!   DELETE /repos/:o/:r/wiki/:title            — delete page
//!   GET    /repos/:o/:r/wiki/:title/history    — list revisions
//!   GET    /repos/:o/:r/wiki/:title/revisions/:id — get single revision

mod common;

use common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

async fn setup(suffix: &str) -> (String, String, String, String) {
    let base = spawn_test_app().await;
    let owner = format!("wikiuser{suffix}");
    let token = register_user(&base, &owner, &format!("wikiuser{suffix}@example.com"), PW).await;
    let repo = format!("wikirepo{suffix}");
    create_repo(&base, &token, &repo).await;
    (base, token, owner, repo)
}

#[tokio::test]
async fn test_wiki_create_and_get() {
    let (base, token, owner, repo) = setup("1").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/repos/{}/{}/wiki", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Home", "content": "# Welcome"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "create wiki page failed: {}",
        resp.status()
    );
    let page: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(page["title"], "Home");
    assert_eq!(page["content"], "# Welcome");

    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/wiki/Home",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let got: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(got["content"], "# Welcome");
}

#[tokio::test]
async fn test_wiki_list_pages() {
    let (base, token, owner, repo) = setup("2").await;
    let client = reqwest::Client::new();

    for title in &["Alpha", "Beta", "Gamma"] {
        client
            .post(format!("{}/api/v1/repos/{}/{}/wiki", base, owner, repo))
            .bearer_auth(&token)
            .json(&serde_json::json!({"title": title, "content": format!("# {}", title)}))
            .send()
            .await
            .unwrap();
    }

    let resp = client
        .get(format!("{}/api/v1/repos/{}/{}/wiki", base, owner, repo))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let pages: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(pages.as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn test_wiki_update_page() {
    let (base, token, owner, repo) = setup("3").await;
    let client = reqwest::Client::new();

    client
        .post(format!("{}/api/v1/repos/{}/{}/wiki", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Guide", "content": "v1"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/wiki/Guide",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"content": "v2", "message": "Updated guide"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "update failed: {}", resp.status());
    let page: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(page["content"], "v2");
}

#[tokio::test]
async fn test_wiki_revision_history() {
    let (base, token, owner, repo) = setup("4").await;
    let client = reqwest::Client::new();

    // Create page
    client
        .post(format!("{}/api/v1/repos/{}/{}/wiki", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Changelog", "content": "initial"}))
        .send()
        .await
        .unwrap();

    // History should be empty (no edits yet)
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/wiki/Changelog/history",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let revs: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        revs.as_array().unwrap().len(),
        0,
        "no revisions before first update"
    );

    // Update once → revision of the initial content is saved
    client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/wiki/Changelog",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"content": "v2 content", "message": "second"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/wiki/Changelog/history",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let revs: serde_json::Value = resp.json().await.unwrap();
    let arr = revs.as_array().unwrap();
    assert_eq!(arr.len(), 1, "expected 1 revision after first update");
    assert_eq!(arr[0]["version"], 1);
    assert_eq!(arr[0]["content"], "initial");

    // Update again → 2 revisions total
    client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/wiki/Changelog",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"content": "v3 content", "message": "third"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/wiki/Changelog/history",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap();
    let revs2: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(revs2.as_array().unwrap().len(), 2, "expected 2 revisions");
}

#[tokio::test]
async fn test_wiki_get_specific_revision() {
    let (base, token, owner, repo) = setup("5").await;
    let client = reqwest::Client::new();

    client
        .post(format!("{}/api/v1/repos/{}/{}/wiki", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Notes", "content": "original text"}))
        .send()
        .await
        .unwrap();

    client
        .patch(format!(
            "{}/api/v1/repos/{}/{}/wiki/Notes",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"content": "updated text"}))
        .send()
        .await
        .unwrap();

    // Fetch history to get revision id
    let revs: serde_json::Value = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/wiki/Notes/history",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rev_id = revs[0]["id"].as_i64().unwrap();

    // Fetch that specific revision
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/wiki/Notes/revisions/{}",
            base, owner, repo, rev_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let rev: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(rev["content"], "original text");
    assert_eq!(rev["id"], rev_id);
}

#[tokio::test]
async fn test_wiki_delete_page() {
    let (base, token, owner, repo) = setup("6").await;
    let client = reqwest::Client::new();

    client
        .post(format!("{}/api/v1/repos/{}/{}/wiki", base, owner, repo))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "Temp", "content": "delete me"}))
        .send()
        .await
        .unwrap();

    let resp = client
        .delete(format!(
            "{}/api/v1/repos/{}/{}/wiki/Temp",
            base, owner, repo
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "delete failed: {}",
        resp.status()
    );

    // Should be 404 now
    let resp = client
        .get(format!(
            "{}/api/v1/repos/{}/{}/wiki/Temp",
            base, owner, repo
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}
