//! End-to-end coverage for ordinary PR merge strategies and conflict recovery.

use std::path::Path;

use crate::common::{build_test_app_state, register_full, setup_test_db};

fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
    let output = gateway.run(args, cwd).unwrap();
    output.ensure_success().unwrap();
    output.stdout_str().trim().to_string()
}

fn configure_worktree(path: &Path) {
    git(&["config", "user.name", "Merge Test"], Some(path));
    git(
        &["config", "user.email", "merge-test@example.com"],
        Some(path),
    );
}

fn seed_diverged_repository(bare_path: &Path) -> (tempfile::TempDir, String) {
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path();
    let path_arg = path.to_string_lossy();
    git(&["init", "--initial-branch=main", &path_arg], None);
    configure_worktree(path);

    std::fs::write(path.join("README.md"), "base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "base"], Some(path));
    let bare_arg = bare_path.to_string_lossy();
    git(&["remote", "add", "origin", &bare_arg], Some(path));
    git(&["push", "origin", "main"], Some(path));

    git(&["checkout", "-b", "feature"], Some(path));
    std::fs::write(path.join("feature-a.txt"), "feature a\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "feature a"], Some(path));
    std::fs::write(path.join("feature-b.txt"), "feature b\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "feature b"], Some(path));
    git(&["push", "origin", "feature"], Some(path));

    git(&["checkout", "main"], Some(path));
    std::fs::write(path.join("base-only.txt"), "advanced base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "advance base"], Some(path));
    git(&["push", "origin", "main"], Some(path));
    let base_sha = git(&["rev-parse", "refs/heads/main"], Some(bare_path));

    (worktree, base_sha)
}

fn seed_conflicting_repository(bare_path: &Path) -> (tempfile::TempDir, String) {
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path();
    let path_arg = path.to_string_lossy();
    git(&["init", "--initial-branch=main", &path_arg], None);
    configure_worktree(path);

    std::fs::write(path.join("conflict.txt"), "base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "base"], Some(path));
    let bare_arg = bare_path.to_string_lossy();
    git(&["remote", "add", "origin", &bare_arg], Some(path));
    git(&["push", "origin", "main"], Some(path));

    git(&["checkout", "-b", "feature"], Some(path));
    std::fs::write(path.join("conflict.txt"), "feature\n").unwrap();
    git(&["commit", "-am", "feature conflict"], Some(path));
    git(&["push", "origin", "feature"], Some(path));

    git(&["checkout", "main"], Some(path));
    std::fs::write(path.join("conflict.txt"), "main\n").unwrap();
    git(&["commit", "-am", "base conflict"], Some(path));
    git(&["push", "origin", "main"], Some(path));
    let base_sha = git(&["rev-parse", "refs/heads/main"], Some(bare_path));

    (worktree, base_sha)
}

async fn create_repo_and_pr(base: &str, token: &str, repo: &str) {
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": repo, "is_private": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201, "{}", created.text().await.unwrap());
}

async fn open_pr(base: &str, token: &str, repo: &str, base_branch: &str) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/merge-owner/{repo}/pulls"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "title": "Merge the feature",
            "head": "feature",
            "base": base_branch
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 201, "{}", response.text().await.unwrap());
}

#[tokio::test]
async fn merge_squash_and_rebase_update_refs_and_pr_state() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "merge-owner", "merge-owner@example.com").await;
    // The pull request is opened by someone other than the merger, so the two
    // halves of a merge's identity — whose work, who merged it — can be told
    // apart in the commit (card_422fc7bcac73).
    let (author_token, _) = register_full(&base, "pr-author", "pr-author@example.com").await;
    let client = reqwest::Client::new();

    for (strategy, expected_new_commits, expected_parent_fields) in
        [("merge", 3, 3), ("squash", 1, 2), ("rebase", 2, 2)]
    {
        let repo = format!("{strategy}-repo");
        create_repo_and_pr(&base, &token, &repo).await;
        let collaborator = client
            .post(format!(
                "{base}/api/v1/repos/merge-owner/{repo}/collaborators"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"username": "pr-author", "permission": "write"}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            collaborator.status(),
            201,
            "{}",
            collaborator.text().await.unwrap()
        );
        let bare_path = repo_root.join(format!("merge-owner/{repo}.git"));
        let (_worktree, base_sha) = seed_diverged_repository(&bare_path);
        open_pr(&base, &author_token, &repo, "main").await;

        let merged = client
            .post(format!(
                "{base}/api/v1/repos/merge-owner/{repo}/pulls/1/merge"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": strategy}))
            .send()
            .await
            .unwrap();
        assert_eq!(
            merged.status(),
            200,
            "{strategy} failed: {}",
            merged.text().await.unwrap()
        );
        let merged = merged.json::<serde_json::Value>().await.unwrap();
        assert_eq!(merged["strategy"], strategy);

        let main_sha = git(&["rev-parse", "refs/heads/main"], Some(&bare_path));
        assert_eq!(merged["merge_commit_sha"], main_sha);
        let parents = git(
            &["rev-list", "--parents", "-n", "1", "refs/heads/main"],
            Some(&bare_path),
        );
        assert_eq!(parents.split_whitespace().count(), expected_parent_fields);
        let new_commit_count = git(
            &[
                "rev-list",
                "--count",
                &format!("{base_sha}..refs/heads/main"),
            ],
            Some(&bare_path),
        );
        assert_eq!(
            new_commit_count.parse::<usize>().unwrap(),
            expected_new_commits
        );
        assert_eq!(
            git(&["show", "refs/heads/main:feature-a.txt"], Some(&bare_path)),
            "feature a"
        );
        assert_eq!(
            git(&["show", "refs/heads/main:feature-b.txt"], Some(&bare_path)),
            "feature b"
        );

        // The commit this merge produced names people, not the server and not
        // the host. `gix`'s plain `Repository::commit` read the identity from
        // the host's git configuration — 500 in a container, the machine
        // owner's name elsewhere — and the fix for that signed every merge as
        // `Plombir Git <noreply@plombir-git.local>`, which erased the author of
        // a squashed pull request from history (card_422fc7bcac73). The
        // worktree that seeded this repository is configured as
        // `Merge Test <merge-test@example.com>`, so an inherited host identity
        // is visible here rather than plausible.
        //
        // The committer is whoever merged, for all three strategies. The
        // author is whose work it is: the pull request's author for a squash,
        // the merger for the merge commit they made, and — what a rebase is —
        // each replayed commit's original author.
        let show = |format: &str| {
            git(
                &[
                    "show",
                    "-s",
                    &format!("--format={format}"),
                    "refs/heads/main",
                ],
                Some(&bare_path),
            )
        };
        assert_eq!(
            show("%cn <%ce>"),
            "merge-owner <merge-owner@example.com>",
            "{strategy} merge must be committed by the account that merged it"
        );
        let expected_author = match strategy {
            "merge" => "merge-owner <merge-owner@example.com>",
            "squash" => "pr-author <pr-author@example.com>",
            _ => "Merge Test <merge-test@example.com>",
        };
        assert_eq!(
            show("%an <%ae>"),
            expected_author,
            "{strategy} merge named the wrong author"
        );
        match strategy {
            "merge" => assert_eq!(
                show("%B").trim_end(),
                "Merge pull request #1 from feature\n\nMerge the feature"
            ),
            // The feature commits are `Merge Test`'s, not the pull request
            // author's, so the squash credits them (card_fbdf6fe490e9).
            "squash" => assert_eq!(
                show("%B").trim_end(),
                "Merge the feature (#1)\n\n* feature a\n* feature b\n\n\
                 Co-authored-by: Merge Test <merge-test@example.com>"
            ),
            _ => assert_eq!(show("%s"), "feature b"),
        }

        let pr = client
            .get(format!("{base}/api/v1/repos/merge-owner/{repo}/pulls/1"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(pr["state"], "merged");
        assert_eq!(pr["merge_strategy"], strategy);
        assert_eq!(pr["merge_commit_sha"], main_sha);
    }

    server.abort();
}

/// A base commit on `main`, then one commit on `feature` per `(author,
/// message)`, each written as that author.
fn seed_feature_by(bare_path: &Path, commits: &[(&str, &str)]) -> tempfile::TempDir {
    let worktree = tempfile::tempdir().unwrap();
    let path = worktree.path();
    let path_arg = path.to_string_lossy();
    git(&["init", "--initial-branch=main", &path_arg], None);
    configure_worktree(path);

    std::fs::write(path.join("README.md"), "base\n").unwrap();
    git(&["add", "."], Some(path));
    git(&["commit", "-m", "base"], Some(path));
    let bare_arg = bare_path.to_string_lossy();
    git(&["remote", "add", "origin", &bare_arg], Some(path));
    git(&["push", "origin", "main"], Some(path));

    git(&["checkout", "-b", "feature"], Some(path));
    for (index, (author, message)) in commits.iter().enumerate() {
        std::fs::write(path.join(format!("part-{index}.txt")), "part\n").unwrap();
        git(&["add", "."], Some(path));
        git(&["commit", "--author", author, "-m", message], Some(path));
    }
    git(&["push", "origin", "feature"], Some(path));
    worktree
}

/// card_fbdf6fe490e9: a squash names one author, the pull request's. Everyone
/// else who wrote one of the commits it replaces — by the commit's author, or
/// by that commit's own `Co-authored-by:` — gets one trailer, as GitHub and
/// GitLab write them; nobody gets two, and a pull request of one person's
/// commits gets none.
#[tokio::test]
async fn a_squash_credits_the_other_authors_of_the_commits_it_replaces() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "merge-owner", "merge-owner@example.com").await;
    let (author_token, _) = register_full(&base, "pr-author", "pr-author@example.com").await;
    let client = reqwest::Client::new();

    let pairing = "pair on the parser\n\nCo-authored-by: Third Dev <third@example.com>";
    for (repo, commits, expected_trailers) in [
        (
            "many-authors",
            vec![
                // The pull request's author, from another machine: the same
                // person by email, whatever its case.
                (
                    "PR Author (laptop) <PR-Author@Example.com>",
                    "start the parser",
                ),
                ("Second Dev <second@example.com>", "handle escapes"),
                ("Second Dev <second@example.com>", "handle quotes"),
                ("Second Dev <second@example.com>", pairing),
            ],
            "Co-authored-by: Second Dev <second@example.com>\n\
             Co-authored-by: Third Dev <third@example.com>",
        ),
        (
            "one-author",
            vec![
                ("pr-author <pr-author@example.com>", "start the parser"),
                ("pr-author <pr-author@example.com>", "finish the parser"),
            ],
            "",
        ),
    ] {
        create_repo_and_pr(&base, &token, repo).await;
        let collaborator = client
            .post(format!(
                "{base}/api/v1/repos/merge-owner/{repo}/collaborators"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"username": "pr-author", "permission": "write"}))
            .send()
            .await
            .unwrap();
        assert_eq!(collaborator.status(), 201);
        let bare_path = repo_root.join(format!("merge-owner/{repo}.git"));
        let _worktree = seed_feature_by(&bare_path, &commits);
        open_pr(&base, &author_token, repo, "main").await;

        let merged = client
            .post(format!(
                "{base}/api/v1/repos/merge-owner/{repo}/pulls/1/merge"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": "squash"}))
            .send()
            .await
            .unwrap();
        assert_eq!(merged.status(), 200, "{}", merged.text().await.unwrap());

        let show = |format: &str| {
            git(
                &[
                    "show",
                    "-s",
                    &format!("--format={format}"),
                    "refs/heads/main",
                ],
                Some(&bare_path),
            )
        };
        assert_eq!(show("%an <%ae>"), "pr-author <pr-author@example.com>");
        let message = show("%B");
        let trailers: Vec<&str> = message
            .lines()
            .filter(|line| line.starts_with("Co-authored-by:"))
            .collect();
        assert_eq!(trailers.join("\n"), expected_trailers, "{repo}: {message}");
        // Trailers are the message's last paragraph, where git reads them.
        if !expected_trailers.is_empty() {
            assert!(
                message
                    .trim_end()
                    .ends_with(&format!("\n\n{expected_trailers}")),
                "{repo}: {message}"
            );
            let parsed = git(
                &[
                    "log",
                    "-1",
                    "--format=%(trailers:key=Co-authored-by,valueonly)",
                    "refs/heads/main",
                ],
                Some(&bare_path),
            );
            assert_eq!(
                parsed.lines().count(),
                trailers.len(),
                "git does not read the trailers: {parsed}"
            );
        }
    }

    server.abort();
}

#[tokio::test]
async fn merge_strategies_update_the_selected_base_without_moving_repo_head() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "merge-owner", "merge-owner@example.com").await;
    let client = reqwest::Client::new();

    for strategy in ["merge", "squash", "rebase"] {
        let repo = format!("nondefault-{strategy}");
        create_repo_and_pr(&base, &token, &repo).await;
        let bare_path = repo_root.join(format!("merge-owner/{repo}.git"));
        let (worktree, main_before) = seed_diverged_repository(&bare_path);
        assert_eq!(
            git(&["symbolic-ref", "HEAD"], Some(&bare_path)),
            "refs/heads/main"
        );

        let path = worktree.path();
        git(&["checkout", "-b", "develop", "main"], Some(path));
        std::fs::write(path.join("develop-only.txt"), "selected base\n").unwrap();
        git(&["add", "."], Some(path));
        git(&["commit", "-m", "advance develop"], Some(path));
        git(&["push", "origin", "develop"], Some(path));
        let develop_before = git(&["rev-parse", "refs/heads/develop"], Some(&bare_path));
        assert_ne!(develop_before, main_before);

        open_pr(&base, &token, &repo, "develop").await;
        let response = client
            .post(format!(
                "{base}/api/v1/repos/merge-owner/{repo}/pulls/1/merge"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": strategy}))
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, 200, "{strategy}: {body}");

        let develop_after = git(&["rev-parse", "refs/heads/develop"], Some(&bare_path));
        assert_ne!(
            develop_after, develop_before,
            "{strategy}: base did not move"
        );
        assert_eq!(body["merge_commit_sha"], develop_after, "{strategy}");
        assert_eq!(
            git(&["rev-parse", "refs/heads/main"], Some(&bare_path)),
            main_before,
            "{strategy}: repository HEAD branch moved"
        );
        assert_eq!(
            git(&["symbolic-ref", "HEAD"], Some(&bare_path)),
            "refs/heads/main"
        );
        assert_eq!(
            git(&["show", "develop:develop-only.txt"], Some(&bare_path)),
            "selected base"
        );
        assert_eq!(
            git(&["show", "develop:feature-a.txt"], Some(&bare_path)),
            "feature a"
        );

        let pr: serde_json::Value = client
            .get(format!("{base}/api/v1/repos/merge-owner/{repo}/pulls/1"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(pr["state"], "merged");
        assert_eq!(pr["merge_commit_sha"], develop_after);
    }

    server.abort();
}

#[tokio::test]
async fn merge_conflict_keeps_base_ref_and_restores_open_pr_state() {
    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root.clone()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;

    let (token, _) = register_full(&base, "merge-owner", "merge-owner@example.com").await;
    let client = reqwest::Client::new();

    // Every strategy, because the answer used to depend on which one the caller
    // picked: `merge` and `squash` reported the conflict as a `409`, while
    // `rebase` declared it with a bare `bail!` and came out of the funnel as a
    // `500` — "the server is broken" about a pull request the server read
    // perfectly (card_592138c542be).
    for strategy in ["merge", "squash", "rebase"] {
        let repo = format!("conflict-{strategy}-repo");
        create_repo_and_pr(&base, &token, &repo).await;
        let bare_path = repo_root.join(format!("merge-owner/{repo}.git"));
        let (_worktree, base_sha) = seed_conflicting_repository(&bare_path);
        open_pr(&base, &token, &repo, "main").await;

        let failed = client
            .post(format!(
                "{base}/api/v1/repos/merge-owner/{repo}/pulls/1/merge"
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({"strategy": strategy}))
            .send()
            .await
            .unwrap();
        // 409, not 400: the request was well-formed and the caller may retry it
        // once the branches stop conflicting. 400 said "you sent something wrong",
        // which was both untrue and indistinguishable from the storage failures the
        // same arm used to catch. And not 500 either — see the loop's note above.
        assert_eq!(
            failed.status(),
            409,
            "{strategy} answered the conflict with"
        );
        let body = failed.text().await.unwrap();
        assert!(body.to_lowercase().contains("conflict"), "{body}");
        // A 409 body reaches the client verbatim, so what git printed about the
        // server's own filesystem must not be in it (H-05).
        // `CONFLICT (` and not `CONFLICT`: the latter is this API's own error
        // code, which every 409 body carries by design.
        for leak in [
            "hint:",
            "fatal:",
            "CONFLICT (",
            "Merge conflict in",
            "plombir-git-rebase",
            ".git",
        ] {
            assert!(
                !body.contains(leak),
                "{strategy} put git's output ({leak:?}) in the response body: {body}"
            );
        }
        assert_eq!(
            git(&["rev-parse", "refs/heads/main"], Some(&bare_path)),
            base_sha,
            "{strategy} moved the base ref while refusing the merge"
        );

        let pr = client
            .get(format!("{base}/api/v1/repos/merge-owner/{repo}/pulls/1"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(pr["state"], "open", "{strategy} left the PR");
        assert!(pr["merge_strategy"].is_null());
        assert!(pr["merge_commit_sha"].is_null());
    }

    server.abort();
}
