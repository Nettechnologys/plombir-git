//! What an outbound `git` inherits from the host it runs on.
//!
//! [`credential_invocation`] is used for remotes the **user** named — mirror
//! sync and repository import. The address is theirs; the ambient authority git
//! would otherwise reach for is the server's. A credential helper in
//! `/etc/gitconfig`, credentials in the operator's `~/.netrc`, or an `insteadOf`
//! rewrite in `~/.gitconfig` turn "clone this public repo" into "clone it with
//! the server's credentials" or "clone something else entirely" — without a
//! single row of ours leaking, which is why nothing on the storage side catches
//! it.
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
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use rg_git::cli_gateway::global_gateway;
use rg_git::credentials::credential_invocation;

const TRANSPORT_CHILD_URL: &str = "FORGEKEEP_TEST_TRANSPORT_URL";
const TRANSPORT_CHILD_DESTINATION: &str = "FORGEKEEP_TEST_TRANSPORT_DESTINATION";
const TRANSPORT_CHILD_HARDENED: &str = "FORGEKEEP_TEST_TRANSPORT_HARDENED";

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
    let env: Vec<(&str, &str)> = env
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    run_transport_child(matches!(reach, Reach::Hardened), url, destination, &env);
}

/// Run this integration-test binary in a fresh process so the planted
/// transport variables are genuine inherited state, without mutating the
/// process-global environment of a parallel test runner.
fn run_transport_child(hardened: bool, url: &str, destination: &Path, env: &[(&str, &str)]) {
    let executable = std::env::current_exe().expect("current test executable");
    let path = std::env::var_os("PATH").expect("PATH is set");
    let mut command = Command::new(executable);
    command
        .env_clear()
        .env("PATH", path)
        .env("HOME", "/dev/null")
        .env("XDG_CONFIG_HOME", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(TRANSPORT_CHILD_URL, url)
        .env(
            TRANSPORT_CHILD_DESTINATION,
            destination.to_string_lossy().as_ref(),
        )
        .env(TRANSPORT_CHILD_HARDENED, if hardened { "1" } else { "0" })
        .args([
            "--exact",
            "transport_environment_child",
            "--ignored",
            "--nocapture",
        ]);
    for (key, value) in env {
        command.env(key, value);
    }

    let output = command.output().expect("spawn transport child");
    assert!(
        output.status.success(),
        "transport child failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Helper selected only by the parent acceptance tests in this file. The early
/// return keeps `--run-ignored all` useful: the parent is what supplies a
/// complete fixture and drives both the control and hardened mutations.
#[test]
#[ignore = "spawned by the ambient-authority acceptance tests"]
fn transport_environment_child() {
    let Ok(url) = std::env::var(TRANSPORT_CHILD_URL) else {
        return;
    };
    let destination = std::env::var(TRANSPORT_CHILD_DESTINATION).expect("destination");
    let hardened = std::env::var(TRANSPORT_CHILD_HARDENED).expect("mode") == "1";
    let git = global_gateway().as_ref().expect("git is installed");
    let args = ["clone", "--bare", url.as_str(), destination.as_str()];

    let output = if hardened {
        credential_invocation(None)
            .run(git, &args, None)
            .expect("hardened git ran")
    } else {
        git.run(&args, None).expect("control git ran")
    };
    assert!(
        !output.success(),
        "the deliberately unreachable remote unexpectedly cloned"
    );
}

/// Acceptance for `card_37fd88e76723`.
///
/// Each variable gets a live control that proves real git obeyed it, followed
/// by the outbound invocation against the same fixture. Removing the
/// environment scrub from `OutboundGitInvocation::run` makes both hardened
/// halves fail.
#[test]
fn transport_environment_is_not_inherited() {
    let directory = tempfile::tempdir().expect("tempdir");

    let proxy_script = directory.path().join("git-proxy-command.sh");
    let proxy_marker = directory.path().join("git-proxy-command-ran");
    std::fs::write(
        &proxy_script,
        "#!/bin/sh\n: > \"$FORGEKEEP_TEST_PROXY_MARKER\"\nexit 1\n",
    )
    .expect("write proxy command");
    let mut permissions = std::fs::metadata(&proxy_script)
        .expect("proxy command metadata")
        .permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&proxy_script, permissions).expect("make proxy command executable");

    let proxy_script = proxy_script.to_string_lossy().into_owned();
    let proxy_marker_string = proxy_marker.to_string_lossy().into_owned();
    let git_url = "git://127.0.0.1:9/upstream.git";
    let git_proxy_env = [
        ("GIT_PROXY_COMMAND", proxy_script.as_str()),
        ("FORGEKEEP_TEST_PROXY_MARKER", proxy_marker_string.as_str()),
    ];
    run_transport_child(
        false,
        git_url,
        &directory.path().join("git-proxy-control.git"),
        &git_proxy_env,
    );
    assert!(
        proxy_marker.exists(),
        "the GIT_PROXY_COMMAND control never ran, so it proves no influence"
    );

    std::fs::remove_file(&proxy_marker).expect("reset proxy marker");
    run_transport_child(
        true,
        git_url,
        &directory.path().join("git-proxy-hardened.git"),
        &git_proxy_env,
    );
    assert!(
        !proxy_marker.exists(),
        "GIT_PROXY_COMMAND from the server process ran for a user-selected remote"
    );

    let remote_url = "http://127.0.0.1:9/upstream.git";
    let (control_proxy, control_seen) = spawn_recording_remote();
    let control_proxy = format!("http://{control_proxy}");
    run_transport_child(
        false,
        remote_url,
        &directory.path().join("http-proxy-control.git"),
        &[("http_proxy", control_proxy.as_str()), ("NO_PROXY", "")],
    );
    assert!(
        !control_seen.lock().expect("lock").is_empty(),
        "the http_proxy control never reached the proxy, so it proves no influence"
    );

    let (hardened_proxy, hardened_seen) = spawn_recording_remote();
    let hardened_proxy = format!("http://{hardened_proxy}");
    run_transport_child(
        true,
        remote_url,
        &directory.path().join("http-proxy-hardened.git"),
        &[("http_proxy", hardened_proxy.as_str()), ("NO_PROXY", "")],
    );
    assert!(
        hardened_seen.lock().expect("lock").is_empty(),
        "http_proxy from the server process redirected a user-selected remote"
    );
}

fn authorizations(seen: &Arc<Mutex<Vec<Seen>>>) -> Vec<String> {
    seen.lock()
        .expect("lock")
        .iter()
        .filter_map(|request| request.authorization.clone())
        .collect()
}

/// The acceptance check of `card_4ee296c4a0d6`: curl reads `~/.netrc` below
/// git's credential-helper layer, so resetting `credential.helper` alone cannot
/// stop it from answering for a user-supplied remote.
#[test]
fn netrc_credentials_in_the_server_home_never_reach_the_remote() {
    let (address, seen) = spawn_recording_remote();
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::write(
        home.path().join(".netrc"),
        "machine 127.0.0.1 login netrc-user password netrc-password\n",
    )
    .expect("write .netrc");
    let url = format!("http://{address}/upstream.git");

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
        "credentials from the server's .netrc reached a user-supplied remote: {requests:?}"
    );
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
