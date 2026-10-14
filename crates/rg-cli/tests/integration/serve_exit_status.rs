//! Process-level checks for `plombir-git serve` startup failures.
//!
//! A transport task returning `Err` is not the same thing as its Tokio task
//! panicking. The former used to be logged inside the spawned future and then
//! converted into `Ok(())`, so `main` reported a successful process exit even
//! though the HTTP listener had never been acquired (card_99fa6be81723).

use std::net::TcpListener;
use std::process::{Command, Output};

fn diagnostic(output: &Output) -> String {
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn an_occupied_http_port_makes_serve_exit_unsuccessfully() {
    let dir = tempfile::tempdir().expect("temporary instance directory");
    let occupied = TcpListener::bind("127.0.0.1:0").expect("reserve an HTTP port");
    let http_addr = occupied.local_addr().expect("reserved HTTP address");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("plombir-git.db").display()
    );
    let config = dir.path().join("plombir-git.toml");
    std::fs::write(&config, "[server]\nshutdown_grace_secs = 1\n").expect("write the test config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .expect("make the test config owner-only");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_plombir-git"))
        .args([
            "serve",
            "--repo-root",
            dir.path().join("repos").to_str().expect("UTF-8 repo root"),
            "--http-addr",
            &http_addr.to_string(),
            "--ssh-addr",
            "127.0.0.1:0",
            "--host-key",
            dir.path()
                .join("host_ed25519")
                .to_str()
                .expect("UTF-8 host key"),
            "--db-url",
            &database_url,
            "--jwt-secret",
            "test-jwt-secret-long-enough-for-startup-validation",
            "--encryption-key",
            "test-at-rest-key-independent-from-the-jwt-secret",
            "--config",
            config.to_str().expect("UTF-8 config path"),
        ])
        .current_dir(dir.path())
        .output()
        .expect("run plombir-git serve with its HTTP port occupied");
    let text = diagnostic(&output);

    assert!(
        !output.status.success(),
        "a process with no HTTP listener must not report success:\n{text}"
    );
    assert!(
        text.contains("failed to bind"),
        "missing bind failure:\n{text}"
    );
    assert!(
        text.contains(&http_addr.to_string()),
        "the bind failure must name the requested address:\n{text}"
    );
    assert!(
        !text.contains("Plombir Git server started"),
        "startup must not be announced before the listener owns its address:\n{text}"
    );
}

#[test]
fn explicit_zero_log_size_is_rejected_by_serve_wiring() {
    let dir = tempfile::tempdir().expect("temporary instance directory");
    // If the size branch is disconnected, the occupied port makes the process
    // exit instead of leaving this regression test waiting on a live server.
    let occupied = TcpListener::bind("127.0.0.1:0").expect("reserve an HTTP port");
    let http_addr = occupied.local_addr().expect("reserved HTTP address");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("plombir-git.db").display()
    );
    let output = Command::new(env!("CARGO_BIN_EXE_plombir-git"))
        .args([
            "serve",
            "--repo-root",
            dir.path().join("repos").to_str().expect("UTF-8 repo root"),
            "--http-addr",
            &http_addr.to_string(),
            "--ssh-addr",
            "127.0.0.1:0",
            "--host-key",
            dir.path()
                .join("host_ed25519")
                .to_str()
                .expect("UTF-8 host key"),
            "--db-url",
            &database_url,
            "--log-file",
            dir.path().join("app.log").to_str().expect("UTF-8 log path"),
            "--log-max-size-mb",
            "0",
            "--jwt-secret",
            "test-jwt-secret-long-enough-for-startup-validation",
            "--encryption-key",
            "test-at-rest-key-independent-from-the-jwt-secret",
        ])
        .current_dir(dir.path())
        .output()
        .expect("run plombir-git serve with an explicit zero log cap");
    let text = diagnostic(&output);

    assert!(
        !output.status.success(),
        "zero log cap was accepted:\n{text}"
    );
    assert!(
        text.contains("logging.max_size_mb"),
        "the requested cap must be rejected by the selected file appender:\n{text}"
    );
}

/// What the resolved settings mean for an operator has to reach the log. These
/// notices used to be written while the configuration was resolved, before
/// the subscriber existed, so a server allowed to deliver webhooks in plaintext
/// and serving `/metrics` to anyone said so to nobody.
#[test]
fn startup_notices_about_the_resolved_settings_reach_the_log() {
    let dir = tempfile::tempdir().expect("temporary instance directory");
    // The occupied port makes the process exit right after its startup, so the
    // test reads a finished log instead of stopping a live server.
    let occupied = TcpListener::bind("127.0.0.1:0").expect("reserve an HTTP port");
    let http_addr = occupied.local_addr().expect("reserved HTTP address");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("plombir-git.db").display()
    );
    let config = dir.path().join("plombir-git.toml");
    std::fs::write(
        &config,
        "[server]\nshutdown_grace_secs = 1\n\n[webhooks]\nallow_insecure_http = true\n",
    )
    .expect("write the test config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .expect("make the test config owner-only");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_plombir-git"))
        .args([
            "serve",
            "--repo-root",
            dir.path().join("repos").to_str().expect("UTF-8 repo root"),
            "--http-addr",
            &http_addr.to_string(),
            "--ssh-addr",
            "127.0.0.1:0",
            "--host-key",
            dir.path()
                .join("host_ed25519")
                .to_str()
                .expect("UTF-8 host key"),
            "--db-url",
            &database_url,
            "--jwt-secret",
            "test-jwt-secret-long-enough-for-startup-validation",
            "--encryption-key",
            "test-at-rest-key-independent-from-the-jwt-secret",
            "--config",
            config.to_str().expect("UTF-8 config path"),
        ])
        .env_remove("PLOMBIR_GIT_METRICS_TOKEN")
        .current_dir(dir.path())
        .output()
        .expect("run plombir-git serve with its HTTP port occupied");
    let text = diagnostic(&output);

    for notice in [
        "Outbound webhook delivery over plaintext HTTP is enabled",
        "GET /metrics is open to anyone who reaches the HTTP port",
    ] {
        assert!(
            text.contains(notice),
            "the startup log is missing {notice:?}:\n{text}"
        );
    }
    // The source offer (AGPL). Its wording depends on whether the build
    // recorded a commit; the field name is wrapped in colour codes, so the
    // message is what is matched.
    assert!(
        text.contains("is offered at the link") || text.contains("the UI links the repository"),
        "the startup log is missing the source offer:\n{text}"
    );
}
