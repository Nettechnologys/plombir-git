//! The person given access to a repository can see that they were.
//!
//! A collaborator grant used to be visible only from the granting side: the
//! account it was given to found the repository nowhere and was told nothing
//! (card_d96c59297b09, card_9da74035521e).

use sea_orm::ConnectionTrait;

use crate::common::{register_full, spawn_test_app_with_db};

async fn send(
    method: reqwest::Method,
    url: String,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let mut request = reqwest::Client::new().request(method, url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(serde_json::json!(text)),
    )
}

async fn create(base: &str, token: &str, name: &str, private: bool) {
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        Some(token),
        Some(serde_json::json!({ "name": name, "is_private": private })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
}

async fn grant(base: &str, token: &str, repo: &str, username: &str, permission: &str) {
    let (status, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/vis_owner/{repo}/collaborators"),
        Some(token),
        Some(serde_json::json!({ "username": username, "permission": permission })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_collaborator_finds_the_repositories_shared_with_them() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner, _) = register_full(&base, "vis_owner", "vis_owner@example.com").await;
    let (bob, _) = register_full(&base, "vis_bob", "vis_bob@example.com").await;
    let (carol, _) = register_full(&base, "vis_carol", "vis_carol@example.com").await;
    create(&base, &owner, "public_assets", false).await;
    create(&base, &owner, "private_assets", true).await;
    create(&base, &owner, "gone", false).await;
    create(&base, &owner, "not_shared", false).await;
    grant(&base, &owner, "public_assets", "vis_bob", "write").await;
    grant(&base, &owner, "private_assets", "vis_bob", "admin").await;
    grant(&base, &owner, "gone", "vis_bob", "read").await;
    db.execute_unprepared(
        "UPDATE repositories SET deleted_at = '2020-01-01 00:00:00' WHERE name = 'gone'",
    )
    .await
    .unwrap();

    let url = format!("{base}/api/v1/users/me/collaborations");
    let (status, body) = send(reqwest::Method::GET, url.clone(), Some(&bob), None).await;
    assert_eq!(status, 200, "{body}");
    // Newest grant first; the deleted repository is not offered, and is not
    // counted either.
    let listed: Vec<(String, String, String)> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|repo| {
            (
                repo["owner"].as_str().unwrap().to_string(),
                repo["name"].as_str().unwrap().to_string(),
                repo["permission"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        listed,
        [
            ("vis_owner".into(), "private_assets".into(), "admin".into()),
            ("vis_owner".into(), "public_assets".into(), "write".into()),
        ]
    );
    assert_eq!(body["pagination"]["total"], 2, "{body}");
    assert_eq!(body["data"][0]["is_private"], true);

    // The page is cut in SQL: one per page still reports both.
    let (status, page) = send(
        reqwest::Method::GET,
        format!("{url}?per_page=1&page=2"),
        Some(&bob),
        None,
    )
    .await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["data"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"][0]["name"], "public_assets");
    assert_eq!(page["pagination"]["total"], 2);

    // Someone else's grants are not yours, and the owner's own repositories
    // are not "shared with" them.
    for token in [&carol, &owner] {
        let (status, body) = send(reqwest::Method::GET, url.clone(), Some(token), None).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(body["data"], serde_json::json!([]), "{body}");
        assert_eq!(body["pagination"]["total"], 0);
    }
    assert_eq!(send(reqwest::Method::GET, url, None, None).await.0, 401);
}

async fn inbox(base: &str, token: &str) -> Vec<serde_json::Value> {
    let (status, body) = send(
        reqwest::Method::GET,
        format!("{base}/api/v1/notifications?per_page=100"),
        Some(token),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["data"].as_array().cloned().unwrap_or_default()
}

/// Wait for the detached delivery to land a notification titled `title`.
async fn eventually(base: &str, token: &str, title: &str) -> serde_json::Value {
    for _ in 0..100 {
        if let Some(found) = inbox(base, token)
            .await
            .into_iter()
            .find(|row| row["title"] == title)
        {
            return found;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    panic!("no notification {title:?}: {:?}", inbox(base, token).await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_collaborator_is_told_when_their_access_changes() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (owner, _) = register_full(&base, "vis_owner", "vis_owner@example.com").await;
    let (bob, bob_id) = register_full(&base, "vis_bob", "vis_bob@example.com").await;
    create(&base, &owner, "secret_assets", true).await;

    // Granted: the row says who, what and where, and opens the repository.
    grant(&base, &owner, "secret_assets", "vis_bob", "write").await;
    let granted = eventually(
        &base,
        &bob,
        "vis_owner gave you write access to vis_owner/secret_assets",
    )
    .await;
    assert_eq!(granted["event_type"], "access");
    assert_eq!(granted["link"], "/vis_owner/secret_assets");
    assert_eq!(granted["is_read"], false);

    // Changed.
    let (status, list) = send(
        reqwest::Method::GET,
        format!("{base}/api/v1/repos/vis_owner/secret_assets/collaborators"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, 200, "{list}");
    let row_id = list[0]["id"].as_i64().unwrap();
    let (status, body) = send(
        reqwest::Method::PATCH,
        format!("{base}/api/v1/repos/vis_owner/secret_assets/collaborators/{row_id}"),
        Some(&owner),
        Some(serde_json::json!({ "permission": "admin" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    eventually(
        &base,
        &bob,
        "vis_owner changed your access to vis_owner/secret_assets to admin",
    )
    .await;

    // Revoked: still told, and not sent to a page that now refuses them.
    let (status, body) = send(
        reqwest::Method::DELETE,
        format!("{base}/api/v1/repos/vis_owner/secret_assets/collaborators/{bob_id}"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, 204, "{body}");
    let revoked = eventually(
        &base,
        &bob,
        "vis_owner removed your access to vis_owner/secret_assets",
    )
    .await;
    assert_eq!(revoked["link"], serde_json::Value::Null);

    // The admin who made the changes was not told about them.
    assert!(
        inbox(&base, &owner)
            .await
            .iter()
            .all(|row| row["event_type"] != "access"),
        "{:?}",
        inbox(&base, &owner).await
    );
}
