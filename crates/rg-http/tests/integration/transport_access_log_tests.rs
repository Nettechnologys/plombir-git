//! card_02520afde020: a git or LFS request leaves a line in the server log
//! whatever it answered.
//!
//! The request trace logs a response only when it is a 5xx, so on the default
//! `info` filter a `401` on the LFS batch — what a password gets where a token
//! is needed — left nothing at all, and the operator of a live instance could
//! not tell how a user's LFS push failed.
//!
//! The router is served on this test's own current-thread runtime, so the
//! thread-local subscriber installed here is the one every request runs under.

use crate::common::{register_user, spawn_test_app};

#[derive(Clone, Default)]
struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl CapturedLogs {
    fn lines(&self) -> Vec<String> {
        String::from_utf8_lossy(&self.0.lock().expect("log lock"))
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl std::io::Write for CapturedLogs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log lock").extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
    type Writer = CapturedLogs;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

#[tokio::test]
async fn a_refused_and_an_accepted_lfs_batch_both_leave_a_line_at_the_default_level() {
    let logs = CapturedLogs::default();
    // The production default: `EnvFilter::new("info")` in `rg-cli`.
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .finish();
    let _guard = tracing::subscriber::set_default(subscriber);

    let base = spawn_test_app().await;
    let password = "Qz7$wRtm";
    let jwt = register_user(&base, "lfslog", "lfslog@example.com", password).await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({ "name": "assets", "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let token: serde_json::Value = client
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({ "name": "git-lfs" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pat = token["token"].as_str().expect("a token").to_string();

    // What `git lfs push` sends: the batch at the endpoint it derives from the
    // remote, with whatever the credential helper handed it.
    let batch = |secret: String| {
        client
            .post(format!(
                "{base}/git/lfslog/assets.git/info/lfs/objects/batch?attempt=1"
            ))
            .basic_auth("lfslog", Some(secret))
            .header("Accept", "application/vnd.git-lfs+json")
            .header("Content-Type", "application/vnd.git-lfs+json")
            .body(
                serde_json::json!({
                    "operation": "upload",
                    "transfers": ["basic"],
                    "objects": [{ "oid": "a".repeat(64), "size": 1 }],
                })
                .to_string(),
            )
            .send()
    };
    let refused = batch(password.to_string()).await.unwrap();
    assert_eq!(refused.status(), 401, "a password is not an LFS credential");
    let accepted = batch(pat.clone()).await.unwrap();
    assert_eq!(accepted.status(), 200, "a token is");
    let anonymous = client
        .get(format!(
            "{base}/git/lfslog/assets.git/info/refs?service=git-upload-pack"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);

    let lines = logs.lines();
    let batch_lines = lines
        .iter()
        .filter(|line| line.contains("path=/api/v1/repos/lfslog/assets/lfs/objects/batch"))
        .collect::<Vec<_>>();
    assert!(
        batch_lines
            .iter()
            .any(|line| line.contains(" WARN ") && line.contains("status=401")),
        "the refused batch left no warning: {lines:#?}"
    );
    assert!(
        batch_lines
            .iter()
            .any(|line| line.contains(" INFO ") && line.contains("status=200")),
        "the accepted batch left no line: {lines:#?}"
    );
    assert!(
        lines.iter().any(|line| line.contains(" WARN ")
            && line.contains("transport=git")
            && line.contains("path=/git/lfslog/assets.git/info/refs")
            && line.contains("status=401")),
        "the refused smart-HTTP request left no warning: {lines:#?}"
    );
    for secret in [password, pat.as_str()] {
        assert!(
            !lines.iter().any(|line| line.contains(secret)),
            "a credential reached the log"
        );
    }
}
