//! Revoke/change authority after the router admits a request but while its
//! body is being polled. No timing sleeps or production test hooks are needed.
use axum::body::{Body, Bytes};
use axum::http::{header, Request, StatusCode};
use base64::Engine as _;
use rg_db::entities::{access_token, repository, user};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use std::future::Future;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tower::ServiceExt as _;

use crate::common::{register_full, spawn_test_app_with_state};

struct Fixture {
    base: String,
    db: rg_db::DatabaseConnection,
    state: rg_http::AppState,
    owner: String,
    writer: String,
    writer_id: i64,
    repo_id: i64,
}

impl Fixture {
    async fn new(private: bool) -> Self {
        let (base, db, state) = spawn_test_app_with_state().await;
        let (owner, _) = register_full(&base, "stage-owner", "stage-owner@example.test").await;
        let (writer, writer_id) =
            register_full(&base, "stage-writer", "stage-writer@example.test").await;
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&owner)
            .json(&serde_json::json!({
                "name": "staged", "is_private": private, "auto_init": true, "readme": "default"
            }))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let repo: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, 201, "{repo}");
        let granted = client
            .post(format!(
                "{base}/api/v1/repos/stage-owner/staged/collaborators"
            ))
            .bearer_auth(&owner)
            .json(&serde_json::json!({"username":"stage-writer", "permission":"write"}))
            .send()
            .await
            .unwrap();
        assert_eq!(granted.status(), 201, "{}", granted.text().await.unwrap());
        Self {
            base,
            db,
            state,
            owner,
            writer,
            writer_id,
            repo_id: repo["id"].as_i64().unwrap(),
        }
    }

    async fn pat(&self, session: &str) -> String {
        let response = reqwest::Client::new()
            .post(format!("{}/api/v1/users/tokens", self.base))
            .bearer_auth(session)
            .json(&serde_json::json!({"name":"staging", "scopes":"repo"}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, 201, "{body}");
        body["token"].as_str().unwrap().to_string()
    }

    fn git(&self, args: &[&str]) -> String {
        let path = self.state.repo_root.join("stage-owner/staged.git");
        let result = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .unwrap()
            .run(args, Some(&path))
            .unwrap();
        result.ensure_success().unwrap();
        result.stdout_str().trim().to_string()
    }

    fn refs(&self) -> String {
        self.git(&["for-each-ref", "--format=%(refname) %(objectname)"])
    }

    fn payload(&self, push: bool) -> Vec<u8> {
        if !push {
            return b"0014command=ls-refs\n00010000".to_vec();
        }
        let head = self.git(&["rev-parse", "HEAD"]);
        let command = format!(
            "{} {head} refs/heads/after-upload\0report-status\n",
            "0".repeat(40)
        );
        let mut data = format!("{:04x}{command}0000", command.len() + 4).into_bytes();
        // A valid empty pack: the object named by this ref already exists.
        data.extend_from_slice(b"PACK\0\0\0\x02\0\0\0\0");
        data.extend_from_slice(
            gix::ObjectId::from_hex(b"029d08823bd8a8eab510ad6ac75c823cfd3ed31e")
                .unwrap()
                .as_slice(),
        );
        data
    }
}

async fn while_staging<F, Fut>(
    fixture: &Fixture,
    push: bool,
    authorization: Option<String>,
    mutate: F,
) -> (StatusCode, Option<String>, String)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    let payload = fixture.payload(push);
    let area = rg_core::staging::StagingArea::GitRequests.path_in(&fixture.state.repo_root);
    let check_area = area.clone();
    let mutation_ran = Arc::new(AtomicBool::new(false));
    let mark = mutation_ran.clone();
    let body = Body::from_stream(futures::stream::once(async move {
        assert!(
            std::fs::read_dir(&check_area).unwrap().any(|entry| entry
                .unwrap()
                .path()
                .extension()
                .is_some_and(|ext| ext == "request")),
            "body polled before staging: initial authorization has not run"
        );
        mutate().await;
        mark.store(true, Ordering::SeqCst);
        Ok::<_, std::io::Error>(Bytes::from(payload))
    }));
    let service = if push { "receive" } else { "upload" };
    let mut builder = Request::builder()
        .method("POST")
        .uri(format!("/git/stage-owner/staged.git/git-{service}-pack"))
        .header(
            header::CONTENT_TYPE,
            format!("application/x-git-{service}-pack-request"),
        );
    if !push {
        builder = builder.header("git-protocol", "version=2");
    }
    if let Some(authorization) = authorization {
        builder = builder.header(header::AUTHORIZATION, authorization);
    }
    let response = rg_http::create_router_for_test(fixture.state.clone())
        .oneshot(builder.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let challenge = response
        .headers()
        .get(header::WWW_AUTHENTICATE)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .unwrap();
    assert!(
        mutation_ran.load(Ordering::SeqCst),
        "request failed before reaching the intended race"
    );
    assert_eq!(
        std::fs::read_dir(area).unwrap().count(),
        0,
        "staged request leaked after completion"
    );
    (
        status,
        challenge,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn staged_credentials_cannot_survive_revocation_or_switch_basic_identity() {
    for push in [false, true] {
        for change in [
            "jwt",
            "inactive",
            "pat",
            "expired",
            "scope",
            "restricted",
            "basic-fallback",
        ] {
            let fixture = Fixture::new(true).await;
            let before = fixture.refs();
            let mut authorization = format!("Bearer {}", fixture.writer);
            let mut pat_id = None;
            if !matches!(change, "jwt" | "inactive") {
                let pat = fixture.pat(&fixture.writer).await;
                pat_id = Some(
                    access_token::Entity::find()
                        .filter(access_token::Column::UserId.eq(fixture.writer_id))
                        .one(&fixture.db)
                        .await
                        .unwrap()
                        .unwrap()
                        .id,
                );
                authorization = if change == "basic-fallback" {
                    let second = fixture.pat(&fixture.owner).await;
                    format!(
                        "Basic {}",
                        base64::engine::general_purpose::STANDARD.encode(format!("{second}:{pat}"))
                    )
                } else {
                    format!("Bearer {pat}")
                };
            }
            let db = fixture.db.clone();
            let writer = fixture.writer_id;
            let (status, challenge, body) =
                while_staging(&fixture, push, Some(authorization), move || async move {
                    match change {
                        "jwt" => {
                            rg_db::ops::user_ops::invalidate_sessions(&db, writer)
                                .await
                                .unwrap();
                        }
                        "inactive" => {
                            let mut row = user::Entity::find_by_id(writer)
                                .one(&db)
                                .await
                                .unwrap()
                                .unwrap()
                                .into_active_model();
                            row.is_active = Set(false);
                            row.update(&db).await.unwrap();
                        }
                        "pat" | "basic-fallback" => {
                            access_token::Entity::delete_by_id(pat_id.unwrap())
                                .exec(&db)
                                .await
                                .unwrap();
                        }
                        "scope" | "restricted" | "expired" => {
                            let mut row = access_token::Entity::find_by_id(pat_id.unwrap())
                                .one(&db)
                                .await
                                .unwrap()
                                .unwrap()
                                .into_active_model();
                            if change == "scope" {
                                row.scopes = Set("user".to_string());
                            } else if change == "expired" {
                                row.expires_at =
                                    Set(Some(chrono::Utc::now() - chrono::Duration::minutes(1)));
                            } else {
                                row.repo_restricted = Set(true);
                            }
                            row.update(&db).await.unwrap();
                        }
                        _ => unreachable!(),
                    }
                })
                .await;
            if change == "restricted" {
                assert_eq!(status, 404, "push={push} {change}: {body}");
            } else {
                assert_eq!(status, 401, "push={push} {change}: {body}");
                assert_eq!(challenge.as_deref(), Some("Basic realm=\"Plombir Git\""));
            }
            assert_eq!(fixture.refs(), before, "push={push} {change}");
        }
    }
}

#[tokio::test]
async fn staged_requests_recheck_grants_visibility_and_repository_identity() {
    for push in [false, true] {
        for change in ["collaborator", "replaced", "private"] {
            if push && change == "private" {
                continue;
            }
            let fixture = Fixture::new(change != "private").await;
            let before = fixture.refs();
            let credential = match change {
                "private" => None,
                "replaced" => Some(format!("Bearer {}", fixture.owner)),
                _ => Some(format!("Bearer {}", fixture.writer)),
            };
            let db = fixture.db.clone();
            let repo_id = fixture.repo_id;
            let writer = fixture.writer_id;
            let (status, challenge, body) =
                while_staging(&fixture, push, credential, move || async move {
                    if change == "collaborator" {
                        rg_db::ops::repo_collaborator_ops::delete_by_repo_and_user(
                            &db, repo_id, writer,
                        )
                        .await
                        .unwrap();
                    } else {
                        let mut row = repository::Entity::find_by_id(repo_id)
                            .one(&db)
                            .await
                            .unwrap()
                            .unwrap()
                            .into_active_model();
                        if change == "private" {
                            row.is_private = Set(true);
                            row.update(&db).await.unwrap();
                        } else {
                            repository::Entity::delete_by_id(repo_id)
                                .exec(&db)
                                .await
                                .unwrap();
                            row.id = Set(repo_id + 10_000);
                            row.insert(&db).await.unwrap();
                        }
                    }
                })
                .await;
            assert_eq!(
                status,
                if change == "private" { 401 } else { 404 },
                "push={push} {change}: {body}"
            );
            if change == "private" {
                assert_eq!(challenge.as_deref(), Some("Basic realm=\"Plombir Git\""));
            }
            assert_eq!(fixture.refs(), before);
        }
    }
}

#[tokio::test]
async fn staged_push_uses_the_branch_protection_installed_during_upload() {
    for protect in [false, true] {
        let fixture = Fixture::new(true).await;
        let before = fixture.refs();
        let base = fixture.base.clone();
        let owner = fixture.owner.clone();
        let (status, _, body) = while_staging(
            &fixture,
            true,
            Some(format!("Bearer {}", fixture.writer)),
            move || async move {
                if protect {
                    let response = reqwest::Client::new()
                        .post(format!(
                            "{base}/api/v1/repos/stage-owner/staged/branches/protection"
                        ))
                        .bearer_auth(owner)
                        .json(&serde_json::json!({"branch_name":"after-upload", "require_pr":true}))
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
                }
            },
        )
        .await;
        assert_eq!(status, 200, "{body}");
        if protect {
            assert!(body.contains("ng refs/heads/after-upload"), "{body}");
            assert_eq!(fixture.refs(), before);
        } else {
            assert!(body.contains("ok refs/heads/after-upload"), "{body}");
            assert_ne!(fixture.refs(), before, "control push must actually land");
        }
    }
}

#[tokio::test]
async fn staged_push_reloads_the_tokens_protected_branch_restriction() {
    for narrow in [false, true] {
        let fixture = Fixture::new(true).await;
        let response = reqwest::Client::new()
            .post(format!(
                "{}/api/v1/repos/stage-owner/staged/branches/protection",
                fixture.base
            ))
            .bearer_auth(&fixture.owner)
            .json(&serde_json::json!({
                "branch_name":"after-upload", "require_pr":false, "allow_force_push":true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
        let pat = fixture.pat(&fixture.writer).await;
        let token = access_token::Entity::find()
            .filter(access_token::Column::UserId.eq(fixture.writer_id))
            .one(&fixture.db)
            .await
            .unwrap()
            .unwrap();
        assert!(!token.deny_protected_merge);
        let db = fixture.db.clone();
        let before = fixture.refs();
        let (status, _, body) = while_staging(
            &fixture,
            true,
            Some(format!("Bearer {pat}")),
            move || async move {
                if narrow {
                    let mut row = token.into_active_model();
                    row.deny_protected_merge = Set(true);
                    row.update(&db).await.unwrap();
                }
            },
        )
        .await;
        assert_eq!(status, 200, "{body}");
        if narrow {
            assert!(body.contains("ng refs/heads/after-upload"), "{body}");
            assert_eq!(fixture.refs(), before);
        } else {
            assert!(body.contains("ok refs/heads/after-upload"), "{body}");
            assert_ne!(fixture.refs(), before, "unrestricted PAT control must land");
        }
    }
}

#[tokio::test]
async fn staged_fetch_with_unchanged_authority_still_lists_refs() {
    let fixture = Fixture::new(true).await;
    let head = fixture.git(&["rev-parse", "HEAD"]);
    let (status, _, body) = while_staging(
        &fixture,
        false,
        Some(format!("Bearer {}", fixture.writer)),
        || async {},
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert!(body.contains(&head), "{body}");
}
