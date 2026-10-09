//! card_e83bf21a5e5b: the address a renamed repository left keeps leading to
//! it — the API redirects reads, refuses writes by name, `git clone` of the old
//! URL works — until another repository takes the name. A caller who may not
//! read the repository learns nothing from the old address.

use std::path::Path;

use crate::common::{register_full, spawn_test_app_with_db_and_repo_root};

fn no_redirects() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

async fn send(
    method: reqwest::Method,
    url: String,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> (u16, Option<String>, String) {
    let mut request = no_redirects().request(method, url);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get("location")
        .map(|value| value.to_str().unwrap().to_string());
    (status, location, response.text().await.unwrap())
}

fn git(cwd: &Path, args: &[&str]) -> (bool, String) {
    let output = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run_with_env(
            args,
            Some(cwd),
            &[
                ("GIT_AUTHOR_NAME", "redir"),
                ("GIT_AUTHOR_EMAIL", "redir@example.com"),
                ("GIT_COMMITTER_NAME", "redir"),
                ("GIT_COMMITTER_EMAIL", "redir@example.com"),
                ("GIT_TERMINAL_PROMPT", "0"),
            ],
        )
        .unwrap();
    (
        output.success(),
        format!("{}{}", output.stdout_str(), output.stderr_str()),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_renamed_repository_is_found_at_its_old_address_until_the_name_is_taken() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let (owner, _) = register_full(&base, "redir_owner", "redir_owner@example.com").await;
    let (outsider, _) = register_full(&base, "redir_out", "redir_out@example.com").await;
    let api = |path: &str| format!("{base}/api/v1/repos/redir_owner/{path}");
    for (name, private) in [("proj", false), ("secret", true)] {
        let (status, _, body) = send(
            reqwest::Method::POST,
            format!("{base}/api/v1/repos"),
            Some(&owner),
            Some(serde_json::json!({
                "name": name, "is_private": private, "auto_init": true, "readme": "default"
            })),
        )
        .await;
        assert_eq!(status, 201, "{body}");
    }
    let (status, _, pat) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/users/tokens"),
        Some(&owner),
        Some(serde_json::json!({ "name": "git", "scopes": "repo" })),
    )
    .await;
    assert_eq!(status, 201, "{pat}");
    let pat: serde_json::Value = serde_json::from_str(&pat).unwrap();
    let pat = pat["token"].as_str().unwrap().to_string();

    for (from, to) in [("proj", "renamed"), ("secret", "hidden")] {
        let (status, _, body) = send(
            reqwest::Method::PATCH,
            api(from),
            Some(&owner),
            Some(serde_json::json!({ "name": to })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
    }

    // Reads are redirected, path and query kept.
    let (status, location, _) = send(reqwest::Method::GET, api("proj"), Some(&owner), None).await;
    assert_eq!(status, 308);
    assert_eq!(
        location.as_deref(),
        Some("/api/v1/repos/redir_owner/renamed")
    );
    let (status, location, _) = send(
        reqwest::Method::GET,
        api("proj/issues?state=all"),
        None,
        None,
    )
    .await;
    assert_eq!(
        status, 308,
        "a public repository redirects anonymous reads too"
    );
    assert_eq!(
        location.as_deref(),
        Some("/api/v1/repos/redir_owner/renamed/issues?state=all")
    );

    // A write is not carried over: refused by name, and nothing is written.
    let (status, _, body) = send(
        reqwest::Method::POST,
        api("proj/issues"),
        Some(&owner),
        Some(serde_json::json!({ "title": "lost in the rename" })),
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("redir_owner/renamed"), "{body}");
    let (status, _, issues) = send(
        reqwest::Method::GET,
        api("renamed/issues?state=all"),
        Some(&owner),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert!(!issues.contains("lost in the rename"), "{issues}");

    // A private repository's old address tells an outsider nothing, and its
    // owner is redirected.
    let (status, location, _) =
        send(reqwest::Method::GET, api("secret"), Some(&outsider), None).await;
    assert_eq!((status, location), (404, None));
    let (status, location, _) = send(reqwest::Method::GET, api("secret"), None, None).await;
    assert_eq!((status, location), (404, None));
    let (status, location, _) = send(reqwest::Method::GET, api("secret"), Some(&owner), None).await;
    assert_eq!(status, 308);
    assert_eq!(
        location.as_deref(),
        Some("/api/v1/repos/redir_owner/hidden")
    );

    // `git clone` of the old URLs works, the private one with credentials;
    // anonymously the private one is challenged, as its live name would be.
    let (status, location, _) = send(
        reqwest::Method::GET,
        format!("{base}/git/redir_owner/secret.git/info/refs?service=git-upload-pack"),
        None,
        None,
    )
    .await;
    assert_eq!((status, location), (401, None));
    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_path_buf();
    let address = base.trim_start_matches("http://").to_string();
    let pat_for_git = pat.clone();
    let cloned = tokio::task::spawn_blocking(move || {
        let public = git(
            &root_path,
            &[
                "clone",
                "-q",
                &format!("http://{address}/git/redir_owner/proj.git"),
                "public",
            ],
        );
        let private = git(
            &root_path,
            &[
                "clone",
                "-q",
                &format!("http://redir_owner:{pat_for_git}@{address}/git/redir_owner/secret.git"),
                "private",
            ],
        );
        (public, private, root_path)
    })
    .await
    .unwrap();
    let (public, private, root_path) = cloned;
    assert!(public.0, "clone of the old public URL: {}", public.1);
    assert!(private.0, "clone of the old private URL: {}", private.1);
    assert!(root_path.join("public/README.md").exists());
    assert!(root_path.join("private/README.md").exists());

    // A second rename: the oldest address follows the repository to its newest.
    let (status, _, body) = send(
        reqwest::Method::PATCH,
        api("renamed"),
        Some(&owner),
        Some(serde_json::json!({ "name": "final" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, location, _) = send(reqwest::Method::GET, api("proj"), Some(&owner), None).await;
    assert_eq!(status, 308);
    assert_eq!(location.as_deref(), Some("/api/v1/repos/redir_owner/final"));

    // A new repository takes the old name: it owns the address from then on.
    let (status, _, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        Some(&owner),
        Some(serde_json::json!({ "name": "proj" })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, location, body) =
        send(reqwest::Method::GET, api("proj"), Some(&owner), None).await;
    assert_eq!((status, location), (200, None), "{body}");
    let (status, location, _) =
        send(reqwest::Method::GET, api("renamed"), Some(&owner), None).await;
    assert_eq!(status, 308, "the intermediate name still leads on");
    assert_eq!(location.as_deref(), Some("/api/v1/repos/redir_owner/final"));

    // And the old redirect is gone, not merely shadowed: once the repository
    // that took the name is deleted, the address leads nowhere rather than
    // back to the repository that left it long ago.
    let (status, _, body) = send(reqwest::Method::DELETE, api("proj"), Some(&owner), None).await;
    assert!(status == 200 || status == 204, "{status}: {body}");
    let (status, location, _) = send(reqwest::Method::GET, api("proj"), Some(&owner), None).await;
    assert_eq!((status, location), (404, None));
}
