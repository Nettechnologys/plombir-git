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

/// Register `user`, create the public `open` and private `closed`, rename them
/// to `open-now` / `closed-now`, and hand back the owner's session, a PAT and
/// an outsider's PAT.
async fn renamed_pair(base: &str, user: &str) -> (String, String, String) {
    let (owner, _) = register_full(base, user, &format!("{user}@example.com")).await;
    let outsider_name = format!("{user}_out");
    let (outsider, _) = register_full(
        base,
        &outsider_name,
        &format!("{outsider_name}@example.com"),
    )
    .await;
    let pat = |session: String| async move {
        let (status, _, body) = send(
            reqwest::Method::POST,
            format!("{base}/api/v1/users/tokens"),
            Some(&session),
            Some(serde_json::json!({ "name": "lfs", "scopes": "repo" })),
        )
        .await;
        assert_eq!(status, 201, "{body}");
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["token"]
            .as_str()
            .unwrap()
            .to_string()
    };
    for (name, private) in [("open", false), ("closed", true)] {
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
        let (status, _, body) = send(
            reqwest::Method::PATCH,
            format!("{base}/api/v1/repos/{user}/{name}"),
            Some(&owner),
            Some(serde_json::json!({ "name": format!("{name}-now") })),
        )
        .await;
        assert_eq!(status, 200, "{body}");
    }
    let owner_pat = pat(owner.clone()).await;
    let outsider_pat = pat(outsider).await;
    (owner, owner_pat, outsider_pat)
}

/// card_e0351e77eabd: a clone of the old URL keeps the old remote, and
/// `git lfs` derives its batch endpoint from it. The batch is a `POST`, which
/// the REST redirect refuses for every other API — so the LFS API is followed
/// with a `307` (method and body kept), and an anonymous request for a private
/// repository gets the `401` that makes `git lfs` retry with credentials.
#[tokio::test]
async fn the_lfs_api_at_an_old_address_follows_the_repository() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let user = "lfs_moved";
    let (_owner, owner_pat, outsider_pat) = renamed_pair(&base, user).await;

    let batch = |repo: &str, credential: Option<(&str, &str)>| {
        let mut request = no_redirects()
            .post(format!(
                "{base}/git/{user}/{repo}.git/info/lfs/objects/batch"
            ))
            .header("Accept", "application/vnd.git-lfs+json")
            .header("Content-Type", "application/vnd.git-lfs+json")
            .body(
                serde_json::json!({
                    "operation": "download",
                    "transfers": ["basic"],
                    "objects": [{ "oid": "0".repeat(64), "size": 1 }],
                })
                .to_string(),
            );
        if let Some((name, secret)) = credential {
            request = request.basic_auth(name, Some(secret));
        }
        async move {
            let response = request.send().await.unwrap();
            let status = response.status().as_u16();
            let location = response
                .headers()
                .get("location")
                .map(|value| value.to_str().unwrap().to_string());
            (status, location, response.text().await.unwrap())
        }
    };

    for (repo, credential) in [
        ("open", None),
        ("open", Some((user, owner_pat.as_str()))),
        ("closed", Some((user, owner_pat.as_str()))),
    ] {
        let (status, location, body) = batch(repo, credential).await;
        assert_eq!(status, 307, "{repo} with {credential:?}: {body}");
        assert_eq!(
            location.as_deref(),
            Some(format!("/api/v1/repos/{user}/{repo}-now/lfs/objects/batch").as_str()),
        );
    }

    // Anonymous, private: the challenge, not the new name.
    let (status, location, body) = batch("closed", None).await;
    assert_eq!((status, location.as_deref()), (401, None), "{body}");
    assert!(!body.contains("closed-now"), "{body}");
    // Signed in, but unable to read it: the address leads nowhere.
    let (status, location, body) = batch("closed", Some(("someone", outsider_pat.as_str()))).await;
    assert_eq!((status, location.as_deref()), (404, None), "{body}");
    assert!(!body.contains("closed-now"), "{body}");

    // A REST write outside the LFS API is still never carried over.
    let (status, location, _) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos/{user}/open/issues"),
        None,
        Some(serde_json::json!({ "title": "lost" })),
    )
    .await;
    assert_ne!(status, 307);
    assert_eq!(location, None);
}

/// card_e0351e77eabd, the registry half. A registry client cannot follow a
/// rename — blobs are stored by name and the pull token is scoped to the name
/// asked for — so the old name says where the repository went, to a caller who
/// may read it there, through the token flow `docker pull` actually takes.
#[tokio::test]
async fn the_registry_at_an_old_name_says_where_the_repository_went() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let user = "oci_moved";
    let (owner, _owner_pat, _) = renamed_pair(&base, user).await;

    let manifest = |repo: &str, bearer: Option<String>| {
        let mut request = no_redirects().get(format!("{base}/v2/{user}/{repo}/manifests/latest"));
        if let Some(bearer) = bearer {
            request = request.bearer_auth(bearer);
        }
        async move {
            let response = request.send().await.unwrap();
            (response.status().as_u16(), response.text().await.unwrap())
        }
    };
    let pull_token = |repo: &str| {
        let url =
            format!("{base}/v2/auth/token?service=plombir-git&scope=repository:{user}/{repo}:pull");
        async move {
            let response = reqwest::get(url).await.unwrap();
            assert_eq!(response.status(), 200);
            let body: serde_json::Value = response.json().await.unwrap();
            body["token"].as_str().unwrap().to_string()
        }
    };

    // docker's own path: an anonymous token for the old public name, then the pull.
    let token = pull_token("open").await;
    let (status, body) = manifest("open", Some(token)).await;
    assert_eq!(status, 404, "{body}");
    assert!(body.contains("NAME_UNKNOWN"), "{body}");
    assert!(
        body.contains(&format!("renamed to '{user}/open-now'")),
        "{body}"
    );

    // A signed-in reader of the private one is told too.
    let (status, body) = manifest("closed", Some(owner)).await;
    assert_eq!(status, 404, "{body}");
    assert!(
        body.contains(&format!("renamed to '{user}/closed-now'")),
        "{body}"
    );

    // Anyone else gets what they got before, and no new name.
    let token = pull_token("closed").await;
    let (status, body) = manifest("closed", Some(token)).await;
    assert_eq!(status, 401, "{body}");
    assert!(!body.contains("closed-now"), "{body}");
}

/// The card's acceptance against the stock client: a repository with an LFS
/// file is renamed, and a clone of the OLD URL checks the file out with its
/// content rather than its pointer.
///
/// Ignored by default: it needs `git-lfs` on `PATH`. Run it with
/// `cargo nextest run -p rg-http --run-ignored only -E 'test(/old_url_clone_with_lfs/)'`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "drives the stock git-lfs client; run with --run-ignored where git-lfs is installed"]
async fn old_url_clone_with_lfs_checks_out_the_file_content() {
    let (base, _db, _repo_root) = spawn_test_app_with_db_and_repo_root().await;
    let user = "lfs_clone";
    let (owner, _) = register_full(&base, user, "lfs_clone@example.com").await;
    let (status, _, body) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/repos"),
        Some(&owner),
        Some(serde_json::json!({
            "name": "assets", "is_private": true, "auto_init": true, "readme": "default"
        })),
    )
    .await;
    assert_eq!(status, 201, "{body}");
    let (status, _, pat) = send(
        reqwest::Method::POST,
        format!("{base}/api/v1/users/tokens"),
        Some(&owner),
        Some(serde_json::json!({ "name": "lfs", "scopes": "repo" })),
    )
    .await;
    assert_eq!(status, 201, "{pat}");
    let pat = serde_json::from_str::<serde_json::Value>(&pat).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();
    let address = base.trim_start_matches("http://").to_string();
    let url = |name: &str| format!("http://{user}:{pat}@{address}/git/{user}/{name}");
    const FILTERS: &[&str] = &[
        "-c",
        "filter.lfs.smudge=git-lfs smudge -- %f",
        "-c",
        "filter.lfs.process=git-lfs filter-process",
        "-c",
        "filter.lfs.clean=git-lfs clean -- %f",
        "-c",
        "filter.lfs.required=true",
    ];
    let content = b"\0binary level data that lives in LFS\0".to_vec();

    let root = tempfile::tempdir().unwrap();
    let root_path = root.path().to_path_buf();
    {
        let (root_path, url, content) = (root_path.clone(), url("assets"), content.clone());
        tokio::task::spawn_blocking(move || {
            let ok = |cwd: &Path, args: &[&str]| {
                let (success, output) = git(cwd, args);
                assert!(success, "git {args:?}: {output}");
            };
            let mut clone: Vec<&str> = FILTERS.to_vec();
            clone.extend(["clone", "-q", &url, "work"]);
            ok(&root_path, &clone);
            let work = root_path.join("work");
            ok(&work, &["lfs", "install", "--local"]);
            ok(&work, &["lfs", "track", "*.bin"]);
            std::fs::write(work.join("level.bin"), &content).unwrap();
            ok(&work, &["add", ".gitattributes", "level.bin"]);
            ok(&work, &["commit", "-qm", "add a level"]);
            ok(&work, &["push", "-q", "origin", "HEAD"]);
        })
        .await
        .unwrap();
    }

    let (status, _, body) = send(
        reqwest::Method::PATCH,
        format!("{base}/api/v1/repos/{user}/assets"),
        Some(&owner),
        Some(serde_json::json!({ "name": "assets-now" })),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (cloned, output) = {
        let (root_path, url) = (root_path.clone(), url("assets"));
        tokio::task::spawn_blocking(move || {
            let mut clone: Vec<&str> = FILTERS.to_vec();
            clone.extend(["clone", url.as_str(), "again"]);
            let (success, output) = git(&root_path, &clone);
            (success, output)
        })
        .await
        .unwrap()
    };
    assert!(cloned, "clone of the old URL: {output}");
    let checked_out = std::fs::read(root_path.join("again/level.bin")).unwrap();
    assert_eq!(
        checked_out,
        content,
        "the LFS file of the old-URL clone is not its content:\n{}",
        String::from_utf8_lossy(&checked_out)
    );
}
