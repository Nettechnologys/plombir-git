//! Live HTTP smart-transport clone coverage.
//!
//! Guards the upload-pack **response** path, which streams the finished pack to
//! the client through a bounded, idle-guarded channel (`git_response_body_with_idle`)
//! instead of a single in-memory frame (the download-side slow-drip defense,
//! `card_751408c41e0c`). A real `git clone` against a live `axum::serve` proves
//! the streamed, chunked response is a byte-valid pack a stock git client
//! accepts — i.e. no clone regression from the streaming change.

use std::path::Path;

use crate::common::{build_test_app_state, setup_test_db, wait_for_listener};

/// Run git through the sanctioned gateway (the `test_no_raw_git_command_in_crates`
/// regression guard forbids raw git process construction). Returns trimmed stdout.
/// A `cwd` is passed as the gateway's repo path (`-C <cwd>`).
fn git(args: &[&str], cwd: Option<&Path>) -> String {
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway must initialize");
    let output = gateway.run(args, cwd).expect("git invocation failed");
    assert!(
        output.success(),
        "git {args:?} failed: {}",
        output.stderr_str().trim()
    );
    output.stdout_str().trim().to_string()
}

/// A public repo with a multi-frame-sized pack clones cleanly over HTTP: the
/// streamed upload-pack response reconstructs to the exact committed content.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn public_repo_clones_over_live_http() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();

    let user = rg_db::ops::user_ops::create_user(
        &db,
        "http-owner",
        "http-owner@example.com",
        "",
        "HTTP Owner",
    )
    .await
    .unwrap();
    // Public (is_private = false) so an anonymous clone is authorized.
    rg_core::repo::service::create_repo(&db, user.id, "clone-repo", None, false, &repo_root, None)
        .await
        .unwrap();
    let bare_path = repo_root.join("http-owner/clone-repo.git");

    // ── Seed the bare repo with a commit whose pack spans several 64 KiB
    //    stream frames. Incompressible bytes keep the pack from shrinking so the
    //    response genuinely exercises multi-chunk streaming + backpressure. ──
    let worktree = tempfile::tempdir().unwrap();
    git(&["init", "--initial-branch=main"], Some(worktree.path()));
    git(
        &["config", "user.name", "HTTP Integration"],
        Some(worktree.path()),
    );
    git(
        &["config", "user.email", "http-integration@example.com"],
        Some(worktree.path()),
    );
    // ~1 MiB of pseudo-random, poorly-compressible content.
    let mut blob = Vec::with_capacity(1024 * 1024);
    let mut x: u32 = 0x9e3779b9;
    for _ in 0..(1024 * 1024) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        blob.push((x & 0xff) as u8);
    }
    std::fs::write(worktree.path().join("big.bin"), &blob).unwrap();
    std::fs::write(worktree.path().join("README.md"), "cloned over HTTP\n").unwrap();
    git(&["add", "."], Some(worktree.path()));
    git(&["commit", "-m", "seed content"], Some(worktree.path()));
    // Push into the bare repo directly (filesystem), then point HEAD at main.
    let bare_str = bare_path.to_string_lossy().to_string();
    git(&["push", &bare_str, "main"], Some(worktree.path()));
    git(
        &[
            "--git-dir",
            &bare_str,
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
        None,
    );
    let expected_sha = git(&["rev-parse", "HEAD"], Some(worktree.path()));

    // ── Spawn the live HTTP app. ──
    let state = build_test_app_state(db.clone(), repo_root.clone());
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;

    // ── Clone anonymously over HTTP — exercises the streamed upload-pack body. ──
    let dest = tempfile::tempdir().unwrap();
    let clone_path = dest.path().join("clone");
    let url = format!("http://{addr}/http-owner/clone-repo.git");
    git(&["clone", &url, &clone_path.to_string_lossy()], None);

    // The streamed pack must reconstruct the exact committed content + tip.
    let cloned_blob = std::fs::read(clone_path.join("big.bin")).expect("big.bin must be cloned");
    assert_eq!(cloned_blob, blob, "cloned blob must match byte-for-byte");
    let cloned_sha = git(&["rev-parse", "HEAD"], Some(clone_path.as_path()));
    assert_eq!(
        cloned_sha, expected_sha,
        "cloned HEAD must match origin tip"
    );

    server.abort();
}

/// The `.pack` files under a clone's object store.
fn packs_in(clone: &Path) -> std::collections::BTreeSet<std::path::PathBuf> {
    std::fs::read_dir(clone.join(".git/objects/pack"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "pack"))
        .collect()
}

fn pack_object_count(pack: &Path) -> u32 {
    let bytes = std::fs::read(pack).unwrap();
    assert_eq!(&bytes[..4], b"PACK");
    u32::from_be_bytes(bytes[8..12].try_into().unwrap())
}

/// card_ad83ad72d14a: a protocol v0/v1 fetch — libgit2, JGit, go-git, any git
/// older than 2.26 — used to get `pack-objects --all` on every request: the
/// whole repository, the objects of hidden refs included, and a pack in answer
/// to a negotiation round that expected acknowledgments. A stock git client
/// pinned to v0 now clones without the hidden ref's objects, and an
/// incremental fetch from a clone with commits of its own — more than one
/// stateless round of haves — receives only what it lacks.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_protocol_v0_fetch_gets_what_it_lacks_and_never_a_hidden_ref() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let user = rg_db::ops::user_ops::create_user(&db, "v0-owner", "v0-owner@example.com", "", "V0")
        .await
        .unwrap();
    rg_core::repo::service::create_repo(&db, user.id, "v0-repo", None, false, &repo_root, None)
        .await
        .unwrap();
    let bare = repo_root.join("v0-owner/v0-repo.git");
    let bare_str = bare.to_string_lossy().to_string();

    let scratch = tempfile::tempdir().unwrap();
    let seed = scratch.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(&["init", "-q", "--initial-branch=main"], Some(&seed));
    git(&["config", "user.name", "V0 Seed"], Some(&seed));
    git(
        &["config", "user.email", "v0-seed@example.com"],
        Some(&seed),
    );
    let commit = |work: &Path, name: &str| {
        std::fs::write(work.join(name), format!("{name}\n")).unwrap();
        git(&["add", name], Some(work));
        git(&["commit", "-q", "-m", name], Some(work));
        git(&["rev-parse", "HEAD"], Some(work))
    };
    for index in 0..20 {
        commit(&seed, &format!("history-{index}.txt"));
    }
    git(&["push", "-q", &bare_str, "main"], Some(&seed));
    git(
        &[
            "--git-dir",
            &bare_str,
            "symbolic-ref",
            "HEAD",
            "refs/heads/main",
        ],
        None,
    );
    // A commit only a server-private ref holds.
    git(&["checkout", "-q", "-b", "secret"], Some(&seed));
    let secret = commit(&seed, "secret.txt");
    git(
        &["push", "-q", &bare_str, "secret:refs/forks/x"],
        Some(&seed),
    );
    git(&["checkout", "-q", "main"], Some(&seed));

    let state = build_test_app_state(db.clone(), repo_root.clone());
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    wait_for_listener(&addr).await;

    let url = format!("http://{addr}/v0-owner/v0-repo.git");
    let clone = scratch.path().join("clone");
    let (clone_for_git, url_for_git) = (clone.clone(), url.clone());
    tokio::task::spawn_blocking(move || {
        git(
            &[
                "-c",
                "protocol.version=0",
                "clone",
                "-q",
                &url_for_git,
                &clone_for_git.to_string_lossy(),
            ],
            None,
        );
    })
    .await
    .unwrap();
    let probe = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .unwrap()
        .run(&["cat-file", "-e", &secret], Some(&clone))
        .unwrap();
    assert!(
        !probe.success(),
        "a v0 clone received the commit only refs/forks/x holds"
    );

    // Commits of the clone's own, so its haves start with objects the server
    // has never seen and take more than one stateless round.
    git(&["config", "user.name", "V0 Client"], Some(&clone));
    git(
        &["config", "user.email", "v0-client@example.com"],
        Some(&clone),
    );
    for index in 0..40 {
        commit(&clone, &format!("local-{index}.txt"));
    }
    let before = git(&["rev-parse", "main"], Some(&seed));
    let upstream = commit(&seed, "upstream.txt");
    git(&["push", "-q", &bare_str, "main"], Some(&seed));

    let packs_before = packs_in(&clone);
    let clone_for_git = clone.clone();
    tokio::task::spawn_blocking(move || {
        git(
            &[
                "-c",
                "protocol.version=0",
                "-c",
                "fetch.unpackLimit=1",
                "fetch",
                "-q",
                "origin",
            ],
            Some(&clone_for_git),
        );
    })
    .await
    .unwrap();
    assert_eq!(git(&["rev-parse", "origin/main"], Some(&clone)), upstream);
    let new_packs: Vec<_> = packs_in(&clone)
        .difference(&packs_before)
        .cloned()
        .collect();
    let [pack] = new_packs.as_slice() else {
        panic!("one fetched pack expected, got {new_packs:?}");
    };
    let lacking = git(
        &["rev-list", "--objects", &upstream, "--not", &before],
        Some(&bare),
    )
    .lines()
    .count() as u32;
    let whole = git(&["rev-list", "--objects", "--all"], Some(&bare))
        .lines()
        .count() as u32;
    // A thin pack is completed on arrival: `index-pack --fix-thin` appends
    // the delta bases it needed from the clone, so the kept pack may carry a
    // few more objects than were sent — never the repository.
    let count = pack_object_count(pack);
    assert!(
        (lacking..=2 * lacking).contains(&count) && count < whole / 4,
        "the incremental v0 fetch kept {count} objects; it lacked {lacking}, the repository holds {whole}"
    );

    server.abort();
    drop(dir);
}

/// Stock git sends every stateless upload-pack request over 1 KiB with
/// `Content-Encoding: gzip`. The body used to reach the protocol parser still
/// compressed, so a fetch with enough `have` lines died with `500 invalid utf-8
/// sequence` — intermittently, because the number of haves per round is the
/// client negotiator's choice. Here the request is gzipped on purpose.
#[tokio::test]
async fn a_gzipped_upload_pack_request_is_read_decoded() {
    use std::io::Write as _;

    let base = crate::common::spawn_test_app().await;
    let token =
        crate::common::register_user(&base, "gzip-owner", "gzip-owner@example.com", "Qz7$wRtm")
            .await;
    crate::common::create_repo(&base, &token, "gzip-repo").await;

    let mut request = b"0014command=ls-refs\n0001".to_vec();
    for n in 0..60 {
        let line = format!("ref-prefix refs/heads/branch-{n:03}\n");
        request.extend_from_slice(format!("{:04x}{line}", line.len() + 4).as_bytes());
    }
    request.extend_from_slice(b"0000");
    assert!(
        request.len() > 1024,
        "large enough that git itself would gzip it"
    );
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&request).unwrap();
    let compressed = encoder.finish().unwrap();

    let response = reqwest::Client::new()
        .post(format!("{base}/gzip-owner/gzip-repo.git/git-upload-pack"))
        .bearer_auth(&token)
        .header("Git-Protocol", "version=2")
        .header("Content-Type", "application/x-git-upload-pack-request")
        .header("Content-Encoding", "gzip")
        .body(compressed)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.bytes().await.unwrap();
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert!(
        body.ends_with(b"0000"),
        "an ls-refs answer ends in a flush: {body:?}"
    );
}
