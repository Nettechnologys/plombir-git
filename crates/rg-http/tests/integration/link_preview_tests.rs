//! card_86579dfda909: the SPA is built with `ssr = false` and link-preview
//! crawlers do not run JavaScript, so a repository's own `og:*` tags have to
//! be written into the shell by the server. Only for a repository an anonymous
//! visitor may read: a private one, and one that does not exist, get the same
//! generic shell, so the shell answers no existence question.

use std::sync::Arc;

use crate::common::{build_test_app_state, register_full, setup_test_db, wait_for_listener};

const SHELL: &str = "<!doctype html><html><head>\
<meta name=\"description\" content=\"Generic instance text.\" />\
<script>boot()</script></head><body></body></html>";

async fn spawn_with_shell() -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let spa_build_dir = dir.path().join("spa-build");
    std::fs::create_dir_all(&spa_build_dir).unwrap();
    std::fs::write(spa_build_dir.join("index.html"), SHELL).unwrap();
    let mut state = build_test_app_state(db, repo_root);
    state.spa_build_dir = Arc::new(spa_build_dir);
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
    format!("http://{addr}")
}

async fn shell(base: &str, path: &str) -> String {
    let response = reqwest::get(format!("{base}{path}")).await.unwrap();
    assert_eq!(response.status(), 200, "{path}: the SPA shell is served");
    response.text().await.unwrap()
}

#[tokio::test]
async fn a_public_repository_page_carries_its_own_preview_and_a_private_one_does_not() {
    let base = spawn_with_shell().await;
    let (token, _) = register_full(&base, "preview_owner", "preview_owner@example.com").await;
    let client = reqwest::Client::new();
    for (name, private, description) in [
        ("shown", false, "Fast <b>\"quoted\"</b> & safe"),
        ("hidden", true, "Secret plans"),
    ] {
        let resp = client
            .post(format!("{base}/api/v1/repos"))
            .bearer_auth(&token)
            .json(&serde_json::json!({
                "name": name,
                "is_private": private,
                "description": description,
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "create {name}");
    }

    // The public repository: its name and its description, escaped, with the
    // generic description gone rather than doubled — on the repository's root
    // and on any page under it.
    for path in ["/preview_owner/shown", "/preview_owner/shown/issues/1"] {
        let page = shell(&base, path).await;
        assert!(
            page.contains("<meta property=\"og:title\" content=\"preview_owner/shown\" />"),
            "{path}: {page}"
        );
        assert!(
            page.contains(
                "<meta property=\"og:description\" content=\"Fast &lt;b&gt;&quot;quoted&quot;&lt;/b&gt; &amp; safe\" />"
            ),
            "{path}: {page}"
        );
        assert!(
            !page.contains("<b>"),
            "{path}: unescaped description: {page}"
        );
        assert!(!page.contains("Generic instance text."), "{path}: {page}");
        assert_eq!(
            page.matches("name=\"description\"").count(),
            1,
            "{path}: {page}"
        );
        assert!(
            page.contains("<script nonce="),
            "{path}: the CSP nonce is still injected"
        );
    }

    // The private repository, a missing one and an instance page: the generic
    // shell, byte-for-byte the same apart from the per-request nonce.
    let normalise = |page: String| {
        let start = page.find("nonce=\"").unwrap();
        let end = start + 7 + page[start + 7..].find('"').unwrap();
        format!("{}{}", &page[..start], &page[end + 1..])
    };
    let generic = normalise(shell(&base, "/dashboard").await);
    assert!(generic.contains("Generic instance text."));
    assert!(!generic.contains("og:"));
    for path in [
        "/preview_owner/hidden",
        "/preview_owner/missing",
        "/nobody_here/shown",
    ] {
        let page = normalise(shell(&base, path).await);
        assert_eq!(
            page, generic,
            "{path} must not be told apart from a missing repository"
        );
        assert!(!page.contains("Secret plans"), "{path}");
    }
}
