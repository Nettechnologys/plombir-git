use crate::common::{
    create_issue, create_repo, register_full, register_user, spawn_test_app,
    spawn_test_app_with_db, spawn_test_app_with_db_and_repo_root,
};
use chrono::Utc;
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
    // Upload records a SHA-256 digest of the bytes ("attachment body").
    let sha256 = attachment["sha256"]
        .as_str()
        .expect("attachment carries sha256");
    assert_eq!(sha256.len(), 64, "sha256 is 64 hex chars");
    assert!(sha256.bytes().all(|b| b.is_ascii_hexdigit()));
    assert_eq!(
        sha256,
        "baebb75e3b75608ff9c4483c5c93ae00b989a63378a9d0831fecc26f8c75f90e",
    );

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
    // Download echoes the digest so clients can verify the payload end-to-end.
    assert_eq!(
        download
            .headers()
            .get("x-checksum-sha256")
            .and_then(|v| v.to_str().ok()),
        Some(sha256),
    );
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

/// The upload contract is `RepoWrite`, with a signed-off `RepoAuthRead`
/// widening for the author of its target. Both halves are load bearing.
///
/// *Auth* is why an anonymous caller is turned away before the multipart parser
/// runs. The handlers used to take `RepoRead`, which admits anonymous callers on
/// a public repository, and look the session up further down — so the answer
/// came from `Multipart` as a `415` instead of from the gate as a `401`.
///
/// The handler's *read* gate is why a reader may still attach a file to
/// something they authored themselves. That allowance lives past the gate, in
/// the handler's `author_id` check; the route table meanwhile describes what an
/// arbitrary caller needs. Without this test, a future tightening could silently
/// remove the exceptional author path.
#[tokio::test]
async fn an_author_without_write_access_may_attach_to_each_own_target() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let owner = format!("attgateowner{}", &suffix[..8]);
    let reader = format!("attgatereader{}", &suffix[..8]);
    let repo = format!("attgate{}", &suffix[..8]);
    let (owner_token, _) = register_full(&base, &owner, &format!("{owner}@example.com")).await;
    let (reader_token, reader_id) =
        register_full(&base, &reader, &format!("{reader}@example.com")).await;
    let repo_id = create_repo(&base, &owner_token, &repo).await;

    let file = || {
        Form::new().part(
            "attachment",
            Part::bytes(b"reader evidence".to_vec())
                .file_name("evidence.txt")
                .mime_str("text/plain")
                .unwrap(),
        )
    };

    // The reader has no write access; filing an issue on read access is
    // deliberate, and so is attaching to the issue they just filed.
    let (_, own_issue) = create_issue(&base, &reader_token, &owner, &repo, "reader's issue").await;
    let own = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{own_issue}/assets"
        ))
        .bearer_auth(&reader_token)
        .multipart(file())
        .send()
        .await
        .unwrap();
    assert_eq!(
        own.status(),
        reqwest::StatusCode::CREATED,
        "an issue's own author may attach to it without write access"
    );

    // Somebody else's issue is where the allowance stops.
    let (_, owners_issue) = create_issue(&base, &owner_token, &owner, &repo, "owner's issue").await;
    let foreign = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{owners_issue}/assets"
        ))
        .bearer_auth(&reader_token)
        .multipart(file())
        .send()
        .await
        .unwrap();
    assert_eq!(
        foreign.status(),
        reqwest::StatusCode::FORBIDDEN,
        "a reader may not attach to an issue somebody else authored"
    );

    // The same allowance, one level down: a comment the reader wrote on the
    // owner's issue is still the reader's own.
    let comment: Value = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{owners_issue}/comments"
        ))
        .bearer_auth(&reader_token)
        .json(&serde_json::json!({"body": "mine to attach to"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let comment_id = comment["id"].as_i64().unwrap();
    let own_comment = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/comments/{comment_id}/assets"
        ))
        .bearer_auth(&reader_token)
        .multipart(file())
        .send()
        .await
        .unwrap();
    assert_eq!(
        own_comment.status(),
        reqwest::StatusCode::CREATED,
        "a comment's own author may attach to it without write access"
    );

    // The same exception covers a pull request and an inline review comment.
    // Seed the PR directly because the property under test is attachment
    // authorization, not the separate branch-validation contract of creating a
    // PR through HTTP.
    let reader_pull = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("reader's pull request".to_string()),
            body: Set(Some("reader's changes".to_string())),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(reader_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
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
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .unwrap();
    let own_pull = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/{}/assets",
            reader_pull.number
        ))
        .bearer_auth(&reader_token)
        .multipart(file())
        .send()
        .await
        .unwrap();
    assert_eq!(
        own_pull.status(),
        reqwest::StatusCode::CREATED,
        "a pull request's own author may attach to it without write access"
    );

    let review_comment: Value = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/{}/comments",
            reader_pull.number
        ))
        .bearer_auth(&reader_token)
        .json(&serde_json::json!({
            "path": "src/lib.rs",
            "line": 1,
            "side": "RIGHT",
            "body": "my review comment"
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let review_comment_id = review_comment["id"].as_i64().unwrap();
    let own_review_comment = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/comments/{review_comment_id}/assets"
        ))
        .bearer_auth(&reader_token)
        .multipart(file())
        .send()
        .await
        .unwrap();
    assert_eq!(
        own_review_comment.status(),
        reqwest::StatusCode::CREATED,
        "a review comment's own author may attach to it without write access"
    );

    // And the anonymous caller is answered by the gate, not by the parser: a
    // `415` here would mean the multipart reader got there first. The route
    // sweep covers the complementary public-outsider refusal for all four rows.
    for (what, url) in [
        (
            "issue",
            format!("{base}/api/v1/repos/{owner}/{repo}/issues/{own_issue}/assets"),
        ),
        (
            "issue comment",
            format!("{base}/api/v1/repos/{owner}/{repo}/issues/comments/{comment_id}/assets"),
        ),
        (
            "pull request",
            format!(
                "{base}/api/v1/repos/{owner}/{repo}/pulls/{}/assets",
                reader_pull.number
            ),
        ),
        (
            "review comment",
            format!("{base}/api/v1/repos/{owner}/{repo}/pulls/comments/{review_comment_id}/assets"),
        ),
    ] {
        let anonymous = client.post(&url).multipart(file()).send().await.unwrap();
        assert_eq!(
            anonymous.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "anonymous upload to a public repository's {what} must be answered by the gate"
        );
        // Not even a body: the gate runs before anything reads one.
        let bodyless = client.post(&url).send().await.unwrap();
        assert_eq!(
            bodyless.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "anonymous upload to a {what} with no body at all must still be a 401"
        );
    }
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
            uploader_id: Set(Some(owner_id)),
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
            sha256: Set(None),
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

/// A blob that no longer hashes to the digest recorded at upload must not be
/// served as a valid file.
///
/// The download path streams the attachment straight off the disk, so the check
/// cannot happen before the first byte goes out — it runs as the bytes pass and
/// fails the transfer at the end. The client therefore still sees `200` with the
/// promised `Content-Length`, but the body ends in a broken read instead of a
/// complete file, which is the whole difference between "this download failed"
/// and "here is your silently corrupted attachment".
#[tokio::test]
async fn a_tampered_attachment_blob_fails_the_download_instead_of_serving_bad_bytes() {
    let (base, db, repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let client = reqwest::Client::new();
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let owner = format!("tamper{}", &suffix[..8]);
    let repo = format!("tampered{}", &suffix[..8]);
    let token = register_user(&base, &owner, &format!("{owner}@example.com"), PASSWORD).await;
    create_repo(&base, &token, &repo).await;
    let (_, issue_number) = create_issue(&base, &token, &owner, &repo, "Tamper test").await;

    let upload = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets"
        ))
        .bearer_auth(&token)
        .multipart(
            Form::new().part(
                "attachment",
                Part::bytes(b"original bytes".to_vec())
                    .file_name("evidence.txt")
                    .mime_str("text/plain")
                    .unwrap(),
            ),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), reqwest::StatusCode::CREATED);
    let attachment: Value = upload.json().await.unwrap();
    let attachment_id = attachment["id"].as_i64().unwrap();

    let url =
        format!("{base}/api/v1/repos/{owner}/{repo}/issues/{issue_number}/assets/{attachment_id}");
    let healthy = client.get(&url).send().await.unwrap();
    assert_eq!(healthy.status(), reqwest::StatusCode::OK);
    assert_eq!(
        healthy.bytes().await.unwrap().as_ref(),
        b"original bytes",
        "an untampered attachment downloads intact"
    );

    // Swap the stored bytes for a same-length payload: the row's `size` and the
    // `Content-Length` still match, so the digest is the only thing that can
    // tell the difference.
    let row = rg_db::ops::attachment_ops::find_by_id(&db, attachment_id)
        .await
        .unwrap()
        .expect("attachment row");
    let blob_path = row
        .blob_key
        .split('/')
        .fold(repo_root, |path, segment| path.join(segment));
    std::fs::write(&blob_path, b"tampered bytes").expect("overwrite the stored blob");

    // What must hold is that the tampered bytes never arrive complete. *Where*
    // the client learns that is not ours to pin: the server answers `200` and
    // then aborts the body mid-stream, so on a busy machine the headers and the
    // truncated connection can reach the client together and `send()` itself
    // returns `IncompleteMessage` instead of `bytes()` doing so. Asserting the
    // second shape only made this test fail under load — a red run that says
    // nothing about the server (card_4b0ca8d89fec).
    match client.get(&url).send().await {
        Ok(tampered) => {
            assert_eq!(
                tampered.status(),
                reqwest::StatusCode::OK,
                "headers are already on the wire when the mismatch is discovered"
            );
            let body = tampered.bytes().await;
            assert!(
                body.is_err(),
                "a digest mismatch must break the transfer, not deliver the tampered bytes: \
                 {body:?}"
            );
        }
        Err(error) => assert!(
            error.is_request(),
            "the transfer must break on the truncated body, not on anything else: {error:?}"
        ),
    }
}
