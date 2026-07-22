mod common;

use chrono::Utc;
use common::{
    create_issue, create_repo, register_full, register_user, spawn_test_app, spawn_test_app_with_db,
};
use reqwest::multipart::{Form, Part};
use sea_orm::Set;
use serde_json::Value;

const PASSWORD: &str = "Qz7$wRtm";

#[tokio::test]
async fn issue_attachment_roundtrip_enforces_type_permission_and_ownership() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let owner = format!("attowner{}", &suffix[..8]);
    let outsider = format!("attother{}", &suffix[..8]);
    let repo = format!("attachments{}", &suffix[..8]);
    let other_repo = format!("other{}", &suffix[..8]);
    let owner_token = register_user(&base, &owner, &format!("{owner}@example.com"), PASSWORD).await;
    let outsider_token = register_user(
        &base,
        &outsider,
        &format!("{outsider}@example.com"),
        PASSWORD,
    )
    .await;
    create_repo(&base, &owner_token, &repo).await;
    create_repo(&base, &owner_token, &other_repo).await;
    let (_, issue_number) =
        create_issue(&base, &owner_token, &owner, &repo, "Attachment test").await;
    let (_, other_issue_number) =
        create_issue(&base, &owner_token, &owner, &other_repo, "Other issue").await;

    let upload = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets"
        ))
        .bearer_auth(&owner_token)
        .multipart(
            Form::new().part(
                "attachment",
                Part::bytes(b"attachment body".to_vec())
                    .file_name("evidence.txt")
                    .mime_str("text/plain")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    let upload_status = upload.status();
    let upload_body = upload.text().await.unwrap();
    assert_eq!(
        upload_status,
        reqwest::StatusCode::CREATED,
        "upload failed: {upload_body}"
    );
    let attachment: Value = serde_json::from_str(&upload_body).unwrap();
    let attachment_id = attachment["id"].as_i64().unwrap();
    assert_eq!(attachment["name"], "evidence.txt");
    assert_eq!(attachment["size"], 15);
    assert!(attachment["browser_download_url"]
        .as_str()
        .unwrap()
        .ends_with(&format!("/assets/{attachment_id}")));

    let listed: Vec<Value> = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets"
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["id"], attachment_id);

    let comment: Value = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/comments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"body": "comment with evidence"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let comment_id = comment["id"].as_i64().unwrap();
    let comment_upload = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/comments/{comment_id}/assets"
        ))
        .bearer_auth(&owner_token)
        .multipart(
            Form::new().part(
                "attachment",
                Part::bytes(b"comment file".to_vec())
                    .file_name("comment.md")
                    .mime_str("text/markdown")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(comment_upload.status(), reqwest::StatusCode::CREATED);
    let comment_attachment: Value = comment_upload.json().await.unwrap();
    let comment_attachment_id = comment_attachment["id"].as_i64().unwrap();
    let comment_download = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/comments/{comment_id}/assets/{comment_attachment_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(comment_download.status(), reqwest::StatusCode::OK);
    assert_eq!(
        comment_download.bytes().await.unwrap().as_ref(),
        b"comment file"
    );

    let download = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets/{attachment_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(download.status(), reqwest::StatusCode::OK);
    assert_eq!(download.headers()["content-type"], "text/plain");
    assert_eq!(download.bytes().await.unwrap().as_ref(), b"attachment body");

    let wrong_repo = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{other_repo}/issues/{other_issue_number}/assets/{attachment_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_repo.status(), reqwest::StatusCode::NOT_FOUND);

    let forbidden_delete = client
        .delete(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets/{attachment_id}"
        ))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden_delete.status(), reqwest::StatusCode::FORBIDDEN);

    let bad_type = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets"
        ))
        .bearer_auth(&owner_token)
        .multipart(Form::new().part(
            "attachment",
            Part::bytes(vec![0, 1, 2]).file_name("payload.exe"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_type.status(), reqwest::StatusCode::BAD_REQUEST);

    let deleted = client
        .delete(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets/{attachment_id}"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    let missing = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets/{attachment_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn private_pr_and_review_comment_attachments_enforce_access_and_target_scope() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let owner = format!("privateowner{}", &suffix[..8]);
    let outsider = format!("privateother{}", &suffix[..8]);
    let repo = format!("privateattachments{}", &suffix[..8]);
    let (owner_token, owner_id) =
        register_full(&base, &owner, &format!("{owner}@example.com")).await;
    let (outsider_token, _) =
        register_full(&base, &outsider, &format!("{outsider}@example.com")).await;

    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": repo, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let repo_id = created.json::<Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();
    let (_, issue_number) = create_issue(
        &base,
        &owner_token,
        &owner,
        &repo,
        "Private attachment scope",
    )
    .await;

    let anonymous_issue = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous_issue.status(), reqwest::StatusCode::UNAUTHORIZED);
    let outsider_issue = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets"
        ))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(outsider_issue.status(), reqwest::StatusCode::FORBIDDEN);

    let pull = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("Attachment pull request".to_string()),
            body: Set(Some("Private changes".to_string())),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(owner_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(None),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .unwrap();

    let pr_upload = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/1/assets?name=renamed.patch"
        ))
        .bearer_auth(&owner_token)
        .multipart(
            Form::new().part(
                "attachment",
                Part::bytes(b"diff --git a/a b/a\n".to_vec())
                    .file_name("original.patch")
                    .mime_str("text/x-patch")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(pr_upload.status(), reqwest::StatusCode::CREATED);
    let pr_attachment = pr_upload.json::<Value>().await.unwrap();
    let pr_attachment_id = pr_attachment["id"].as_i64().unwrap();
    assert_eq!(pr_attachment["name"], "renamed.patch");

    let review_comment = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/1/comments"
        ))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({
            "path": "src/lib.rs",
            "line": 1,
            "side": "RIGHT",
            "body": "Review attachment target"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(review_comment.status(), reqwest::StatusCode::CREATED);
    let review_comment_id = review_comment.json::<Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let comment_upload = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/comments/{review_comment_id}/assets"
        ))
        .bearer_auth(&owner_token)
        .multipart(
            Form::new().part(
                "attachment",
                Part::bytes(b"review evidence".to_vec())
                    .file_name("review.txt")
                    .mime_str("text/plain")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(comment_upload.status(), reqwest::StatusCode::CREATED);
    let comment_attachment_id = comment_upload.json::<Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let downloaded = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/comments/{review_comment_id}/assets/{comment_attachment_id}"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), reqwest::StatusCode::OK);
    assert_eq!(
        downloaded.bytes().await.unwrap().as_ref(),
        b"review evidence"
    );

    let anonymous_pr = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/1/assets/{pr_attachment_id}"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous_pr.status(), reqwest::StatusCode::UNAUTHORIZED);
    let outsider_pr = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/1/assets/{pr_attachment_id}"
        ))
        .bearer_auth(&outsider_token)
        .send()
        .await
        .unwrap();
    assert_eq!(outsider_pr.status(), reqwest::StatusCode::FORBIDDEN);

    let wrong_target = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets/{pr_attachment_id}"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_target.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted = client
        .delete(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/comments/{review_comment_id}/assets/{comment_attachment_id}"
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    rg_db::ops::attachment_ops::create(
        &db,
        rg_db::entities::attachment::ActiveModel {
            id: sea_orm::NotSet,
            uuid: Set(uuid::Uuid::new_v4().to_string()),
            repo_id: Set(repo_id),
            uploader_id: Set(owner_id),
            issue_id: Set(None),
            pull_request_id: Set(Some(pull.id)),
            issue_comment_id: Set(None),
            review_comment_id: Set(None),
            filename: Set("quota.txt".to_string()),
            blob_key: Set(format!("attachments/{repo_id}/quota/quota.txt")),
            content_type: Set("text/plain".to_string()),
            size: Set(rg_core::attachment::DEFAULT_REPO_ATTACHMENT_QUOTA - 1),
            download_count: Set(0),
            created_at: Set(Utc::now()),
        },
    )
    .await
    .unwrap();
    let quota_rejected = client
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/pulls/1/assets"))
        .bearer_auth(&owner_token)
        .multipart(Form::new().part(
            "attachment",
            Part::bytes(b"xx".to_vec()).file_name("over-quota.txt"),
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(quota_rejected.status(), reqwest::StatusCode::BAD_REQUEST);
    assert!(quota_rejected
        .text()
        .await
        .unwrap()
        .contains("quota exceeded"));

    assert_eq!(pull.repo_id, repo_id);
}
