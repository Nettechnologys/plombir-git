//! Draft notes, assets and provenance share one publication boundary.
use crate::common::{
    create_initialised_repo, register_user, setup_test_db, spawn_test_app_over_db_with,
    StateOverrides,
};
use rg_db::ops::{release_ops, repo_collaborator_ops, user_ops};
use sea_orm::ActiveValue::Set;
use serde_json::{json, Value};

async fn scoped_pat(
    db: &rg_db::DatabaseConnection,
    user_id: i64,
    repositories: Vec<i64>,
) -> String {
    rg_core::auth::access_token::issue(
        db,
        rg_core::auth::access_token::NewAccessToken {
            user_id,
            name: "release visibility".to_string(),
            scopes: "repo".to_string(),
            expires_at: None,
            repository_ids: Some(repositories),
            mcp_tools: None,
            deny_protected_merge: false,
        },
    )
    .await
    .unwrap()
    .raw
}

async fn publication_boundary(private: bool) {
    let (db, dir) = setup_test_db().await;
    let base = spawn_test_app_over_db_with(
        db.clone(),
        dir.path().join("repos"),
        StateOverrides::default(),
    )
    .await;
    let client = reqwest::Client::new();
    let owner = register_user(&base, "draftowner", "draftowner@example.com", "Qz7$wRtm").await;
    let reader = register_user(&base, "draftreader", "draftreader@example.com", "Qz7$wRtm").await;
    let writer = register_user(&base, "draftwriter", "draftwriter@example.com", "Qz7$wRtm").await;
    create_initialised_repo(&base, &owner, "drafts").await;
    let repo_url = format!("{base}/api/v1/repos/draftowner/drafts");
    let repository: Value = client
        .get(&repo_url)
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let repo_id = repository["id"].as_i64().unwrap();
    let reader_user = user_ops::find_by_username(&db, "draftreader")
        .await
        .unwrap()
        .unwrap();
    let writer_user = user_ops::find_by_username(&db, "draftwriter")
        .await
        .unwrap()
        .unwrap();
    for (user, permission) in [(&reader_user, "read"), (&writer_user, "write")] {
        repo_collaborator_ops::create(
            &db,
            rg_db::entities::repo_collaborator::ActiveModel {
                repo_id: Set(repo_id),
                user_id: Set(user.id),
                permission: Set(permission.to_string()),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    if private {
        let response = client
            .patch(&repo_url)
            .bearer_auth(&owner)
            .json(&json!({"is_private":true}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
    let reader_pat = scoped_pat(&db, reader_user.id, vec![repo_id]).await;
    let writer_pat = scoped_pat(&db, writer_user.id, vec![repo_id]).await;
    let confined_elsewhere = scoped_pat(&db, writer_user.id, vec![]).await;
    let releases_url = format!("{repo_url}/releases");
    let published: Value = client
        .post(&releases_url)
        .bearer_auth(&owner)
        .json(&json!({
            "tag_name":"v1-public", "title":"Published", "body":"PUBLIC-NOTES"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let draft_response = client
        .post(&releases_url)
        .bearer_auth(&owner)
        .json(&json!({
            "tag_name":"v2-draft", "title":"SECRET-TITLE", "body":"SECRET-NOTES", "is_draft":true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(draft_response.status(), 201);
    let draft: Value = draft_response.json().await.unwrap();
    let release_id = draft["id"].as_i64().unwrap();
    let uploaded = client
        .post(format!("{releases_url}/{release_id}/assets"))
        .bearer_auth(&owner)
        .header("content-type", "application/octet-stream")
        .header("content-disposition", "attachment; filename=secret.bin")
        .body("SECRET-BYTES")
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), 201);
    let asset: Value = uploaded.json().await.unwrap();
    let asset_id = asset["id"].as_i64().unwrap();
    let signed = client
        .post(format!("{releases_url}/assets/{asset_id}/attestation"))
        .bearer_auth(&writer_pat)
        .send()
        .await
        .unwrap();
    assert_eq!(signed.status(), 201);
    let routes = [
        (
            reqwest::Method::GET,
            format!("/{release_id}"),
            "/9223372036854775807".to_string(),
        ),
        (
            reqwest::Method::GET,
            format!("/{release_id}/assets"),
            "/9223372036854775807/assets".to_string(),
        ),
        (
            reqwest::Method::GET,
            format!("/assets/{asset_id}"),
            "/assets/9223372036854775807".to_string(),
        ),
        (
            reqwest::Method::GET,
            format!("/assets/{asset_id}/download"),
            "/assets/9223372036854775807/download".to_string(),
        ),
        (
            reqwest::Method::GET,
            format!("/assets/{asset_id}/attestation"),
            "/assets/9223372036854775807/attestation".to_string(),
        ),
        (
            reqwest::Method::POST,
            format!("/assets/{asset_id}/attestation/verify"),
            "/assets/9223372036854775807/attestation/verify".to_string(),
        ),
    ];
    for is_draft in [true, false, true] {
        let update = client
            .patch(format!("{releases_url}/{release_id}"))
            .bearer_auth(&owner)
            .json(&json!({"is_draft":is_draft}))
            .send()
            .await
            .unwrap();
        assert_eq!(update.status(), 200);
        for (token, may_write) in [
            (None, false),
            (Some(&reader), false),
            (Some(&reader_pat), false),
            (Some(&owner), true),
            (Some(&writer), true),
            (Some(&writer_pat), true),
        ] {
            let visible = !is_draft || may_write;
            let can_read_repo = !private || token.is_some();
            let mut request = client.get(format!("{releases_url}?per_page=1"));
            if let Some(token) = token {
                request = request.bearer_auth(token);
            }
            let response = request.send().await.unwrap();
            assert_eq!(
                response.status().as_u16(),
                if can_read_repo { 200 } else { 404 }
            );
            if can_read_repo {
                assert_eq!(response.headers()["cache-control"], "private, no-store");
                let page: Value = response.json().await.unwrap();
                assert_eq!(page["pagination"]["total"], if visible { 2 } else { 1 });
                assert_eq!(
                    page["data"].as_array().unwrap().len(),
                    1,
                    "filter before LIMIT"
                );
                if !visible {
                    assert_eq!(page["data"][0]["id"], published["id"]);
                    assert!(!page.to_string().contains("SECRET-"));
                }
            }
            for (method, route, absent) in &routes {
                let before = release_ops::find_asset_by_id(&db, asset_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .download_count;
                let mut request = client.request(method.clone(), format!("{releases_url}{route}"));
                if let Some(token) = token {
                    request = request.bearer_auth(token);
                }
                let response = request.send().await.unwrap();
                assert_eq!(
                    response.status().as_u16(),
                    if visible && can_read_repo { 200 } else { 404 },
                    "{method} {route}, draft={is_draft}, writer={may_write}"
                );
                if can_read_repo {
                    assert_eq!(response.headers()["cache-control"], "private, no-store");
                }
                if !visible || !can_read_repo {
                    let mut body: serde_json::Value = response.json().await.unwrap();
                    let mut missing =
                        client.request(method.clone(), format!("{releases_url}{absent}"));
                    if let Some(token) = token {
                        missing = missing.bearer_auth(token);
                    }
                    let missing = missing.send().await.unwrap();
                    assert_eq!(missing.status(), 404);
                    let mut missing: serde_json::Value = missing.json().await.unwrap();
                    // Per-request trace ids vary independently of visibility.
                    // Compare every other response field, not just the status.
                    for envelope in [&mut body, &mut missing] {
                        assert!(envelope["error"]["request_id"].is_string());
                        envelope["error"]
                            .as_object_mut()
                            .unwrap()
                            .remove("request_id");
                    }
                    assert_eq!(
                        body, missing,
                        "hidden and missing must be indistinguishable"
                    );
                    let after = release_ops::find_asset_by_id(&db, asset_id)
                        .await
                        .unwrap()
                        .unwrap()
                        .download_count;
                    assert_eq!(before, after, "denial must not count a download");
                }
            }
        }
        for (method, route, _) in &routes {
            let response = client
                .request(method.clone(), format!("{releases_url}{route}"))
                .bearer_auth(&confined_elsewhere)
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                404,
                "writer PAT confinement still applies"
            );
        }
    }
}

#[tokio::test]
async fn public_release_drafts_are_only_visible_to_writers() {
    publication_boundary(false).await;
}

#[tokio::test]
async fn private_release_drafts_are_only_visible_to_writers() {
    publication_boundary(true).await;
}
