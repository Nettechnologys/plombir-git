//! A repository name supplied by the caller is still a guess. Hidden and
//! absent repositories must have the same answer, including the HTML document.

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};
use reqwest::{Client, Method};
use serde_json::{json, Value};
use std::sync::Arc;

async fn fixture() -> (String, String, String) {
    let (db, dir) = setup_test_db().await;
    let root = dir.path().join("repos");
    std::fs::create_dir_all(&root).unwrap();
    let web = dir.path().join("web");
    std::fs::create_dir_all(&web).unwrap();
    std::fs::write(
        web.join("index.html"),
        "<!doctype html><html><head><title>Plombir Git</title></head><body>SPA</body></html>",
    )
    .unwrap();
    let mut state = build_test_app_state(db, root);
    state.spa_build_dir = Arc::new(web);
    let app = rg_http::create_router_for_test_with_static_files(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    wait_for_listener(&addr.to_string()).await;
    let base = format!("http://{addr}");
    let (owner, _) = register_full(&base, "visible-owner", "visible-owner@example.com").await;
    let (outsider, _) =
        register_full(&base, "visible-outsider", "visible-outsider@example.com").await;
    for (name, private) in [("secret", true), ("open", false)] {
        let response = Client::new().post(format!("{base}/api/v1/repos"))
            .bearer_auth(&owner)
            .json(&json!({"name": name, "is_private": private, "auto_init": true, "readme": "default"}))
            .send().await.unwrap();
        assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
    }
    (base, owner, outsider)
}

async fn answer(base: &str, path: &str, method: Method, credential: Option<&str>) -> (u16, Value) {
    let mut request = Client::new().request(method, format!("{base}{path}"));
    if let Some(credential) = credential {
        request = request.bearer_auth(credential);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let mut body: Value = response.json().await.unwrap();
    if let Some(error) = body.get_mut("error").and_then(Value::as_object_mut) {
        error.remove("request_id");
    }
    (status, body)
}

#[tokio::test]
async fn hidden_and_absent_repository_rest_reads_are_identical_for_sessions_and_pats() {
    let (base, owner, outsider) = fixture().await;
    let token = Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&outsider)
        .json(&json!({"name": "visibility", "scopes": "repo"}))
        .send()
        .await
        .unwrap();
    assert_eq!(token.status(), 201);
    let token = token.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    for credential in [None, Some(outsider.as_str()), Some(token.as_str())] {
        for suffix in [
            "",
            "/tree",
            "/branches",
            "/tags",
            "/log",
            "/stargazers",
            "/forks",
            "/issues",
            "/pulls",
            "/milestones",
            "/releases",
            "/wiki",
            "/collaborators",
            "/mirror",
        ] {
            let private = answer(
                &base,
                &format!("/api/v1/repos/visible-owner/secret{suffix}"),
                Method::GET,
                credential,
            )
            .await;
            let missing = answer(
                &base,
                &format!("/api/v1/repos/visible-owner/absent{suffix}"),
                Method::GET,
                credential,
            )
            .await;
            assert_eq!(private.0, 404, "{suffix}: {private:?}");
            assert_eq!(private, missing, "{suffix}: hidden and absent differ");
            assert_eq!(private.1["error"]["code"], "NOT_FOUND");
            assert_eq!(private.1["error"]["message"], "repository not found");
        }
    }
    assert_eq!(
        answer(
            &base,
            "/api/v1/repos/visible-owner/secret",
            Method::GET,
            Some(&owner)
        )
        .await
        .0,
        200
    );
    assert_eq!(
        answer(&base, "/api/v1/repos/visible-owner/open", Method::GET, None)
            .await
            .0,
        200
    );
}

#[tokio::test]
async fn token_confinement_and_repository_mutations_do_not_confirm_hidden_names() {
    let (base, owner, outsider) = fixture().await;
    let token = Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&owner)
        .json(&json!({"name": "only-open", "repositories": ["visible-owner/open"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(token.status(), 201);
    let token = token.json::<Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    for method in [Method::GET, Method::DELETE] {
        for credential in [&outsider, &token] {
            let private = answer(
                &base,
                "/api/v1/repos/visible-owner/secret",
                method.clone(),
                Some(credential),
            )
            .await;
            let missing = answer(
                &base,
                "/api/v1/repos/visible-owner/absent",
                method.clone(),
                Some(credential),
            )
            .await;
            assert_eq!(private.0, 404, "{method}: {private:?}");
            assert_eq!(private, missing, "{method}");
        }
    }
    assert_eq!(
        answer(
            &base,
            "/api/v1/repos/visible-owner/open",
            Method::GET,
            Some(&token)
        )
        .await
        .0,
        200
    );
    assert_eq!(
        answer(
            &base,
            "/api/v1/repos/visible-owner/secret",
            Method::GET,
            Some(&owner)
        )
        .await
        .0,
        200
    );
}

#[tokio::test]
async fn html_uses_a_real_404_without_private_metadata_and_never_caches_it() {
    let (base, owner, outsider) = fixture().await;
    for credential in [None, Some(outsider.as_str())] {
        for suffix in ["", "/network", "/blob/README.md", "/settings"] {
            let mut bodies = Vec::new();
            for name in ["secret", "absent"] {
                let mut request = Client::new().get(format!("{base}/visible-owner/{name}{suffix}"));
                if let Some(credential) = credential {
                    request = request.bearer_auth(credential);
                }
                let response = request.send().await.unwrap();
                assert_eq!(response.status(), 404, "{name}{suffix}");
                assert!(response.headers()["content-type"]
                    .to_str()
                    .unwrap()
                    .starts_with("text/html"));
                assert_eq!(response.headers()["cache-control"], "private, no-store");
                assert_eq!(response.headers()["vary"], "Cookie, Authorization");
                let body = response.text().await.unwrap();
                assert!(!body.contains("secret") && !body.contains("visible-owner"));
                bodies.push(body);
            }
            assert_eq!(bodies[0], bodies[1]);
        }
    }
    for (path, credential) in [
        ("/visible-owner/secret", Some(owner.as_str())),
        ("/visible-owner/open", None),
        ("/settings/profile", None),
    ] {
        let mut request = Client::new().get(format!("{base}{path}"));
        if let Some(credential) = credential {
            request = request.bearer_auth(credential);
        }
        assert_eq!(request.send().await.unwrap().status(), 200, "{path}");
    }
}

#[tokio::test]
async fn git_challenges_guesses_equally_and_authenticated_outsiders_get_not_found() {
    let (base, owner, outsider) = fixture().await;
    for (credential, expected) in [(None, 401), (Some(outsider.as_str()), 404)] {
        let mut replies = Vec::new();
        for name in ["secret", "absent"] {
            let mut request = Client::new().get(format!(
                "{base}/git/visible-owner/{name}/info/refs?service=git-upload-pack"
            ));
            if let Some(credential) = credential {
                request = request.bearer_auth(credential);
            }
            let response = request.send().await.unwrap();
            assert_eq!(response.status(), expected);
            let challenge = response.headers().get("www-authenticate").cloned();
            assert_eq!(challenge.is_some(), credential.is_none());
            replies.push((challenge, response.text().await.unwrap()));
        }
        assert_eq!(replies[0], replies[1]);
    }
    let response = Client::new()
        .get(format!(
            "{base}/git/visible-owner/secret/info/refs?service=git-upload-pack"
        ))
        .bearer_auth(&owner)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn lfs_guesses_and_forged_capabilities_do_not_disclose_repositories() {
    let (base, owner, outsider) = fixture().await;
    let client = Client::new();
    let oid = "a".repeat(64);
    let secret = answer(
        &base,
        "/api/v1/repos/visible-owner/secret",
        Method::GET,
        Some(&owner),
    )
    .await
    .1;
    let revoked = rg_core::lfs::service::sign_ssh_grant(
        b"test-secret-key",
        rg_core::lfs::service::LfsActionKind::Download,
        secret["id"].as_i64().unwrap(),
        chrono::Utc::now().timestamp() + 600,
        rg_core::lfs::service::LfsActor::DeployKey { key_id: i64::MAX },
    )
    .unwrap();
    for (authorization, expected) in [
        (None, 401),
        (Some(format!("Bearer {outsider}")), 404),
        (Some("SSH-LFS forged".to_string()), 401),
        (Some(format!("SSH-LFS {revoked}")), 401),
    ] {
        for locks in [false, true] {
            let mut replies = Vec::new();
            for name in ["secret", "absent"] {
                let path = if locks { "locks" } else { "objects/batch" };
                let url = format!("{base}/api/v1/repos/visible-owner/{name}/lfs/{path}");
                let mut request = if locks {
                    client.get(url)
                } else {
                    client
                        .post(url)
                        .json(&json!({"operation":"download","objects":[{"oid":oid,"size":1}]}))
                };
                if let Some(header) = &authorization {
                    request = request.header("Authorization", header);
                }
                let response = request.send().await.unwrap();
                assert_eq!(response.status(), expected, "{name}/{path}");
                let mut body: Value = response.json().await.unwrap();
                body["error"].as_object_mut().unwrap().remove("request_id");
                replies.push(body);
            }
            assert_eq!(replies[0], replies[1]);
        }
    }
    // A forged (including forged-expired) signed URL gets the same 404 as a
    // missing repository. An authenticated, genuinely expired capability still
    // retains its actionable 410, covered by lfs_signed_url_tests.
    for expires in [1, chrono::Utc::now().timestamp() + 600] {
        let mut replies = Vec::new();
        for name in ["secret", "absent"] {
            let response = client.get(format!("{base}/api/v1/repos/visible-owner/{name}/lfs/objects/{oid}?expires={expires}&signature=bad"))
                .send().await.unwrap();
            assert_eq!(response.status(), 404);
            let mut body: Value = response.json().await.unwrap();
            body["error"].as_object_mut().unwrap().remove("request_id");
            replies.push(body);
        }
        assert_eq!(replies[0], replies[1]);
    }
    let response = client
        .post(format!(
            "{base}/api/v1/repos/visible-owner/secret/lfs/objects/batch"
        ))
        .bearer_auth(owner)
        .json(&json!({"operation":"download","objects":[]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
}
