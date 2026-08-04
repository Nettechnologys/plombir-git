//! What an outbound `git` inherits from the host it runs on.
//!
//! [`credential_invocation`] is used for remotes the **user** named — mirror
//! sync and repository import. The address is theirs; the ambient authority git
//! would otherwise reach for is the server's. A credential helper in
//! `/etc/gitconfig`, or an `insteadOf` rewrite in the operator's `~/.gitconfig`,
//! turns "clone this public repo" into "clone it with the server's credentials"
//! or "clone something else entirely" — without a single row of ours leaking,
//! which is why nothing on the storage side catches it.
//!
//! Every test here runs the real `git` binary twice against the same stub
//! remote: once with the ambient config in reach (the control — it proves the
//! bench can actually leak) and once through `credential_invocation`. A control
//! that stops leaking on its own would make the hardened half vacuously green,
//! so the control names the planted config explicitly rather than trusting
//! whatever `GIT_*` the developer running the suite happens to export.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, Mutex};

use rg_git::cli_gateway::global_gateway;
use rg_git::credentials::credential_invocation;

/// One HTTP request the stub remote saw.
#[derive(Clone, Debug)]
struct Seen {
    request_line: String,
    authorization: Option<String>,
}

/// A remote that answers `401` to everything and records what it was asked.
///
/// Refusing every request is the point: git only consults a credential helper
/// after a `401`, so this is the shape that makes a helper speak up. Nothing
/// here ever succeeds — the assertions are about what reached the socket.
fn spawn_recording_remote() -> (String, Arc<Mutex<Vec<Seen>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).is_err() {
                continue;
            }
            let mut authorization = None;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line
                    .strip_prefix("Authorization: ")
                    .or_else(|| line.strip_prefix("authorization: "))
                {
                    authorization = Some(value.trim().to_string());
                }
            }
            recorder.lock().expect("lock").push(Seen {
                request_line: request_line.trim().to_string(),
                authorization,
            });
            // A hang-up before the refusal is written is git's prerogative and
            // costs the test nothing — the request is recorded above either way.
            if stream
                .write_all(
                    b"HTTP/1.1 401 Unauthorized\r\n\
                      WWW-Authenticate: Basic realm=\"forgekeep-test\"\r\n\
                      Content-Length: 0\r\n\r\n",
                )
                .is_err()
            {
                continue;
            }
        }
    });

    (address, seen)
}

/// A git config that hands out a fixed password to anyone who asks.
fn helper_config(username: &str, password: &str) -> String {
    format!(
        "[credential]\n\thelper = \"!f() {{ echo username={username}; echo password={password}; }}; f\"\n"
    )
}

/// Which half of the comparison an attempt is.
enum Reach {
    /// The host config is in reach — what an unhardened invocation looks like.
    Ambient,
    /// The invocation [`credential_invocation`] builds for an anonymous remote.
    Hardened,
}

/// Attempt `git clone --bare <url>` and return what the remote was asked.
///
/// Goes through the gateway rather than spawning the binary directly — the same
/// rule (and the same regression guard) the production callers live under.
///
/// The environment is the gateway's own plus these overrides. Both halves name
/// the same two config levels — `GIT_CONFIG_GLOBAL` at the planted file and
/// `GIT_CONFIG_SYSTEM` at the stand-in for `/etc/gitconfig`, which a test cannot
/// write to — so nothing is inherited and, crucially, the hardened half is up
/// against a config that is genuinely *there*: skipping the `GIT_CONFIG_SYSTEM`
/// override on that side would pass by never planting the config at all.
fn clone_attempt(url: &str, home: &Path, destination: &Path, reach: Reach, system: Option<&Path>) {
    let global = home.join(".gitconfig");
    let mut env: Vec<(String, String)> = vec![
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        (
            "XDG_CONFIG_HOME".to_string(),
            home.join(".config").to_string_lossy().into_owned(),
        ),
        ("GIT_TERMINAL_PROMPT".to_string(), "0".to_string()),
        (
            "GIT_CONFIG_GLOBAL".to_string(),
            global.to_string_lossy().into_owned(),
        ),
        ("GIT_CONFIG_NOSYSTEM".to_string(), "0".to_string()),
    ];
    if let Some(system) = system {
        env.push((
            "GIT_CONFIG_SYSTEM".to_string(),
            system.to_string_lossy().into_owned(),
        ));
    }
    let mut args: Vec<String> = Vec::new();

    if matches!(reach, Reach::Hardened) {
        // Last write wins in the gateway, so the hardened values land on top of
        // the ambient ones set above.
        let (hardened_args, hardened_env) = credential_invocation(None);
        args.extend(hardened_args);
        env.extend(hardened_env);
    }

    let destination = destination.to_string_lossy().into_owned();
    args.extend([
        "clone".to_string(),
        "--bare".to_string(),
        url.to_string(),
        destination,
    ]);

    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let env: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let output = global_gateway()
        .as_ref()
        .expect("git is installed")
        .run_with_env(&args, None, &env)
        .expect("git ran");
    assert!(
        !output.success(),
        "the stub remote refuses everyone — the clone cannot succeed"
    );
}

fn authorizations(seen: &Arc<Mutex<Vec<Seen>>>) -> Vec<String> {
    seen.lock()
        .expect("lock")
        .iter()
        .filter_map(|request| request.authorization.clone())
        .collect()
}

/// The acceptance check of `card_29bad91a931e`: a helper configured in the
/// operator's own git config must not answer for a remote the *user* named.
///
/// Before the fix the helper reset was assembled below the early return for
/// "no credential", so this — the common case for a public mirror or import —
/// went out with the host's helper list untouched.
#[test]
fn a_helper_in_the_host_config_never_answers_for_an_anonymous_remote() {
    let (address, seen) = spawn_recording_remote();
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join(".gitconfig"),
        helper_config("leaked-user", "leaked-password"),
    )
    .expect("write config");
    let url = format!("http://{address}/upstream.git");

    // Control: the same clone with the host config in reach. If this stops
    // leaking, the bench is broken and the hardened half below proves nothing.
    clone_attempt(
        &url,
        home.path(),
        &home.path().join("control.git"),
        Reach::Ambient,
        None,
    );
    let leaked = authorizations(&seen);
    assert!(
        leaked.iter().any(|value| value.starts_with("Basic ")),
        "the control never leaked, so this test cannot detect the fix: {leaked:?}"
    );

    seen.lock().expect("lock").clear();
    clone_attempt(
        &url,
        home.path(),
        &home.path().join("hardened.git"),
        Reach::Hardened,
        None,
    );

    let requests = seen.lock().expect("lock").clone();
    assert!(
        !requests.is_empty(),
        "the hardened clone never reached the remote at all: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.authorization.is_none()),
        "the host's credential helper answered for a user-supplied remote: {requests:?}"
    );
}

/// The same authority, one config level up: `/etc/gitconfig` is not reachable
/// from a test, so `GIT_CONFIG_SYSTEM` stands in for it — which is exactly what
/// `GIT_CONFIG_NOSYSTEM=1` is asserted to override.
#[test]
fn a_system_wide_helper_is_out_of_reach_too() {
    let (address, seen) = spawn_recording_remote();
    let home = tempfile::tempdir().expect("tempdir");
    let system_config = home.path().join("system-gitconfig");
    std::fs::write(
        &system_config,
        helper_config("system-user", "system-password"),
    )
    .expect("write config");
    let url = format!("http://{address}/upstream.git");

    clone_attempt(
        &url,
        home.path(),
        &home.path().join("control.git"),
        Reach::Ambient,
        Some(&system_config),
    );
    let leaked = authorizations(&seen);
    assert!(
        leaked.iter().any(|value| value.starts_with("Basic ")),
        "the control never leaked, so this test cannot detect the fix: {leaked:?}"
    );

    seen.lock().expect("lock").clear();
    clone_attempt(
        &url,
        home.path(),
        &home.path().join("hardened.git"),
        Reach::Hardened,
        Some(&system_config),
    );

    let requests = seen.lock().expect("lock").clone();
    assert!(
        !requests.is_empty(),
        "the hardened clone never reached the remote at all: {requests:?}"
    );
    assert!(
        requests
            .iter()
            .all(|request| request.authorization.is_none()),
        "a system-wide credential helper answered for a user-supplied remote: {requests:?}"
    );
}

/// The host config decides more than who answers a `401`: an
/// `url.<base>.insteadOf` in it rewrites the remote *after* `guard_git_url` has
/// approved the address the user supplied, which is a way past the SSRF guard
/// rather than a way past authentication.
#[test]
fn an_insteadof_rewrite_in_the_host_config_cannot_redirect_the_remote() {
    let (approved, approved_seen) = spawn_recording_remote();
    let (elsewhere, elsewhere_seen) = spawn_recording_remote();
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join(".gitconfig"),
        format!("[url \"http://{elsewhere}/\"]\n\tinsteadOf = http://{approved}/\n"),
    )
    .expect("write config");
    let url = format!("http://{approved}/upstream.git");

    clone_attempt(
        &url,
        home.path(),
        &home.path().join("control.git"),
        Reach::Ambient,
        None,
    );
    assert!(
        !elsewhere_seen.lock().expect("lock").is_empty(),
        "the control was not redirected, so this test cannot detect the fix"
    );

    approved_seen.lock().expect("lock").clear();
    elsewhere_seen.lock().expect("lock").clear();
    clone_attempt(
        &url,
        home.path(),
        &home.path().join("hardened.git"),
        Reach::Hardened,
        None,
    );

    let redirected = elsewhere_seen.lock().expect("lock").clone();
    assert!(
        redirected.is_empty(),
        "the host config redirected a remote the SSRF guard had already approved: {redirected:?}"
    );
    let approved_requests = approved_seen.lock().expect("lock").clone();
    assert!(
        approved_requests
            .iter()
            .any(|request| request.request_line.contains("/upstream.git/")),
        "the hardened clone asked for something other than the approved remote: {approved_requests:?}"
    );
}
