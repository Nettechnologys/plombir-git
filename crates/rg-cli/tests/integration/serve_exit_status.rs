//! Process-level checks for `forgekeep serve` startup failures.
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
        dir.path().join("forgekeep.db").display()
    );
    let config = dir.path().join("forgekeep.toml");
    std::fs::write(&config, "[server]\nshutdown_grace_secs = 1\n").expect("write the test config");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600))
            .expect("make the test config owner-only");
    }

    let output = Command::new(env!("CARGO_BIN_EXE_forgekeep"))
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
        .expect("run forgekeep serve with its HTTP port occupied");
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
        !text.contains("ForgeKeep server started"),
        "startup must not be announced before the listener owns its address:\n{text}"
    );
}
