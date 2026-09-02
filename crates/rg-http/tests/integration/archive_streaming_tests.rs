//! Regression coverage for card_fbdae59573ca: the "Download ZIP / tar.gz"
//! button built its answer with the synchronous `GitCommandGateway::run`.
//!
//! Two defects lived in that one call. The archive was materialised whole into
//! a `Vec` and handed to `Body::from` as a single frame, so the peak memory of
//! a download was chosen by the repository's content and not by any configured
//! limit; and `run` is *blocking* — a `recv_timeout` on the calling thread —
//! invoked inside an `async fn`, so one request parked a tokio worker for up to
//! `git_cmd_secs` (120s by default) while it waited.
//!
//! Buffering is not directly observable from the outside, but its footprint is:
//! a body whose length is known up front is sent with a `Content-Length`, while
//! a streamed one is chunked. So "the response declares no length" is the
//! externally visible proof that nothing collected the archive before answering
//! — and it goes red the moment somebody hands a finished `Vec` back to
//! `Body::from`.
//!
//! The byte-for-byte comparison against `git archive` is the other half: a
//! streaming path that truncates or reorders is worse than a buffering one, so
//! the download has to still be exactly the archive git produced.

use crate::common::{create_repo, register_full, spawn_test_app_with_repo_root};

/// Enough poorly-compressible content that the archive spans many 64 KiB
/// chunks — a single-chunk payload would stream and buffer identically.
///
/// The contents API takes UTF-8, not bytes, so this is random text rather than
/// random bytes; deflate still only shaves a quarter off a uniform alphabet.
fn incompressible_text() -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut text = String::with_capacity(600 * 1024);
    let mut x: u32 = 0x1234_5678;
    for _ in 0..(600 * 1024) {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        text.push(ALPHABET[(x as usize) % ALPHABET.len()] as char);
    }
    text
}

/// Commit a file through the contents API, so the repository has a `main` to
/// archive without this test invoking git itself.
async fn commit_blob(client: &reqwest::Client, base: &str, token: &str, repo: &str, blob: &str) {
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/archstream-owner/{repo}/contents/blob.txt"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "content": blob,
            "message": "add blob.txt",
        }))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "the fixture needs a commit: {}",
        resp.text().await.unwrap_or_default()
    );
}

#[tokio::test]
async fn a_repository_archive_is_streamed_and_not_collected_first() {
    let (base, repo_root) = spawn_test_app_with_repo_root().await;
    let (token, _) = register_full(&base, "archstream-owner", "archstream@example.com").await;
    create_repo(&base, &token, "archstream-repo").await;
    let client = reqwest::Client::new();
    let blob = incompressible_text();
    commit_blob(&client, &base, &token, "archstream-repo", &blob).await;

    let url = format!("{base}/api/v1/repos/archstream-owner/archstream-repo/archive");
    let bare = repo_root.join("archstream-owner/archstream-repo.git");
    let gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway");

    for (ext, format_flag, magic) in [
        ("zip", "zip", &b"PK"[..]),
        ("tar.gz", "tar.gz", &[0x1f, 0x8b][..]),
    ] {
        let resp = client
            .get(format!("{url}/main.{ext}"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        assert_eq!(resp.status(), 200, "the default branch must still archive");

        // The whole point: a collected archive would arrive with its length
        // declared, because the handler would have known it before answering.
        assert_eq!(
            resp.content_length(),
            None,
            "the {ext} archive declared a Content-Length, so it was collected \
             before the first byte was sent — the response is sized by the \
             repository and must be streamed"
        );
        assert!(
            resp.headers()
                .get(reqwest::header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok())
                .is_some_and(|value| value.contains("archstream-repo-")),
            "the streamed response must keep its filename header"
        );

        let downloaded = resp.bytes().await.expect("body").to_vec();
        assert!(
            downloaded.starts_with(magic),
            "the streamed {ext} download must be a real archive"
        );
        assert!(
            downloaded.len() > 256 * 1024,
            "the {ext} fixture must be large enough to span many chunks, got {} bytes",
            downloaded.len()
        );

        // gzip carries a timestamp, so only the uncompressed `tar` and the
        // `zip` are reproducible enough to compare byte-for-byte; both are
        // produced by the same `git archive` this endpoint spawns.
        if ext == "zip" {
            let expected = gateway
                .run(
                    &["archive", &format!("--format={format_flag}"), "main"],
                    Some(&bare),
                )
                .expect("git archive");
            assert!(expected.success(), "{}", expected.stderr_str());
            assert_eq!(
                downloaded, expected.stdout,
                "the streamed archive must be byte-identical to what git produced"
            );
        }
    }
}
