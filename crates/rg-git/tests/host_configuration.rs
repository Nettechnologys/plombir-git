//! Whose git configuration a subprocess started by the gateway answers to.
//!
//! Every `git` ForgeKeep runs on its own repositories goes through
//! [`rg_git::cli_gateway::GitCommandGateway`], and until the gateway disarmed
//! its children each of them read `/etc/gitconfig` and the `~/.gitconfig` of
//! whichever account the server process runs under. That is the host deciding
//! what this instance does with somebody else's repository: `core.abbrev`
//! changes the object names it prints back, `tar.umask` changes the bytes — and
//! therefore the checksum — of every archive it hands out, and
//! `GIT_CONFIG_COUNT` injects settings straight from the server process's own
//! environment, which `GIT_CONFIG_NOSYSTEM` does not answer.
//!
//! Both spawn paths are measured, because they were fixed together and can be
//! broken apart: the synchronous one every reader uses, and the streaming one
//! behind `pack-objects`, `index-pack` and `git archive`.
//!
//! Each measurement is a pair. The control hands git the same configuration
//! explicitly — explicit values are applied after the disarming, so the control
//! is genuinely up against a config that is *there* — and a control that stopped
//! reaching git would make the disarmed half vacuously green.
//!
//! `/etc/gitconfig` cannot be written by a test, so `GIT_CONFIG_SYSTEM` stands
//! in for it with `GIT_CONFIG_NOSYSTEM=0` keeping that level switched on. The
//! planted variables have to be genuine inherited state and a test may not
//! mutate its own process environment, so the measurement runs in a child
//! process.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rg_git::cli_gateway::global_gateway;

/// Marks the child process spawned by the acceptance test below.
const HOSTILE_HOST_CONFIG_CHILD: &str = "FORGEKEEP_TEST_HOST_CONFIG_CHILD";

/// A setting nothing reads, so planting it changes no behaviour — what it
/// measures is whether the host could have set one at all. Deliberately not a
/// key any invocation policy pins: a pinned key would be answered on the
/// command line and would prove nothing about the environment.
const HOST_PROBE_KEY: &str = "forgekeep.hostprobe";
const HOST_PROBE_VALUE: &str = "the-host-decided-this";

/// The file whose archived mode `tar.umask` moves.
const FIXTURE_FILE: &str = "file.txt";

/// What an operator might have set for their own convenience, and what it costs
/// somebody else: object names that differ between two instances, and release
/// tarballs whose permission bits — and checksum — depend on the machine that
/// served them.
const HOSTILE_HOST_CONFIG: &str = "[core]\n\tabbrev = 16\n[tar]\n\tumask = 0077\n";

/// The abbreviated length `core.abbrev` above asks for.
const HOSTILE_ABBREV: usize = 16;

#[test]
fn the_host_git_configuration_never_reaches_a_gateway_child() {
    let directory = tempfile::tempdir().expect("host config directory");
    let system = directory.path().join("system-gitconfig");
    let global = directory.path().join("global-gitconfig");
    std::fs::write(&system, HOSTILE_HOST_CONFIG).expect("system config");
    std::fs::write(&global, HOSTILE_HOST_CONFIG).expect("global config");

    let executable = std::env::current_exe().expect("current test executable");
    let output = Command::new(executable)
        .env(HOSTILE_HOST_CONFIG_CHILD, "1")
        .env("GIT_CONFIG_SYSTEM", &system)
        .env("GIT_CONFIG_NOSYSTEM", "0")
        .env("GIT_CONFIG_GLOBAL", &global)
        // The placement neither `GIT_CONFIG_NOSYSTEM` nor `GIT_CONFIG_GLOBAL`
        // answers: configuration injected straight into the environment. Only
        // removing the inherited `GIT_*` closes it.
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", HOST_PROBE_KEY)
        .env("GIT_CONFIG_VALUE_0", HOST_PROBE_VALUE)
        .args([
            "--exact",
            "host_configuration_child",
            "--ignored",
            "--nocapture",
        ])
        .output()
        .expect("spawn the host-config child");

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "host-config child failed:\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );

    let reported = |key: &str| {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .unwrap_or_else(|| {
                panic!("child printed no `{key}` line:\nstdout:\n{stdout}\nstderr:\n{stderr}")
            })
            .trim()
            .to_owned()
    };

    // ── The synchronous path ────────────────────────────────────
    assert_eq!(
        reported("host-abbrev=").len(),
        HOSTILE_ABBREV,
        "the planted `core.abbrev` never reached git, so the half below would stay green \
         with the bug in place:\nstdout:\n{stdout}"
    );
    assert_ne!(
        reported("gateway-abbrev="),
        reported("host-abbrev="),
        "the host's `core.abbrev` decided the object name a gateway command printed:\
         \nstdout:\n{stdout}"
    );

    assert_eq!(
        reported("injected-host="),
        HOST_PROBE_VALUE,
        "`GIT_CONFIG_COUNT` never reached git, so the half below proves nothing about the \
         inherited environment:\nstdout:\n{stdout}"
    );
    assert_eq!(
        reported("injected-gateway="),
        "",
        "configuration injected through the server process's own `GIT_*` still reaches a \
         gateway child:\nstdout:\n{stdout}"
    );

    // ── The streaming path ──────────────────────────────────────
    assert_ne!(
        reported("hosted-archive-mode="),
        reported("gateway-archive-mode="),
        "the planted `tar.umask` never reached `git archive`, so the half below would stay \
         green with the bug in place:\nstdout:\n{stdout}"
    );
    assert_eq!(
        reported("streamed-archive-mode="),
        reported("gateway-archive-mode="),
        "the host's `tar.umask` decided the bytes of a streamed archive — the protocol hot \
         path answers to the machine it runs on:\nstdout:\n{stdout}"
    );
}

/// Driven only by the acceptance test above, which is what supplies the planted
/// environment. The early return keeps `--run-ignored all` honest instead of
/// failing on a bare invocation.
#[test]
#[ignore = "spawned by the_host_git_configuration_never_reaches_a_gateway_child"]
fn host_configuration_child() {
    if std::env::var_os(HOSTILE_HOST_CONFIG_CHILD).is_none() {
        return;
    }

    let git = global_gateway().as_ref().expect("git is installed");
    let directory = tempfile::tempdir().expect("child fixture directory");
    let repository = fixture(directory.path());

    // The host's own configuration, restated as explicit values. The gateway
    // applies a caller's environment after its disarming, so this is a git that
    // reads exactly what an undisarmed child would have inherited.
    let host = host_environment();
    let host: Vec<(&str, &str)> = host
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();

    let short = ["rev-parse", "--short", "HEAD"];
    println!(
        "host-abbrev={}",
        git.run_with_env(&short, Some(&repository), &host)
            .expect("git must run")
            .stdout_str()
            .trim()
    );
    println!(
        "gateway-abbrev={}",
        git.run(&short, Some(&repository))
            .expect("git must run")
            .stdout_str()
            .trim()
    );

    let probe = ["config", "--get", HOST_PROBE_KEY];
    println!(
        "injected-host={}",
        git.run_with_env(&probe, Some(&repository), &host)
            .expect("git must run")
            .stdout_str()
            .trim()
    );
    println!(
        "injected-gateway={}",
        git.run(&probe, Some(&repository))
            .expect("git must run")
            .stdout_str()
            .trim()
    );

    let archive = ["archive", "--format=tar", "HEAD"];
    println!(
        "hosted-archive-mode={}",
        archived_mode(
            &git.run_with_env(&archive, Some(&repository), &host)
                .expect("git must run")
                .stdout
        )
    );
    println!(
        "gateway-archive-mode={}",
        archived_mode(
            &git.run(&archive, Some(&repository))
                .expect("git must run")
                .stdout
        )
    );

    let streamed = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime")
        .block_on(async {
            use tokio::io::AsyncReadExt;

            let mut child = git
                .spawn_async(&archive, Some(&repository))
                .await
                .expect("git must spawn");
            let mut stdout = child.stdout.take().expect("piped stdout");
            let mut bytes = Vec::new();
            stdout
                .read_to_end(&mut bytes)
                .await
                .expect("read the streamed archive");
            let status = child.wait().await.expect("git must exit");
            assert!(status.success(), "the streamed archive failed: {status}");
            bytes
        });
    println!("streamed-archive-mode={}", archived_mode(&streamed));
}

/// A repository with one committed file, built without asking the host
/// anything: the identity lives in the repository's own config, so the fixture
/// is the same here and on a machine that configured none.
fn fixture(root: &Path) -> PathBuf {
    let git = global_gateway().as_ref().expect("git is installed");
    let repository = root.join("repo");
    std::fs::create_dir_all(&repository).expect("fixture directory");

    let run = |args: &[&str]| {
        let output = git
            .run(args, Some(&repository))
            .unwrap_or_else(|error| panic!("git {args:?} must run: {error}"));
        assert!(
            output.success(),
            "git {args:?} failed: {}",
            output.stderr_str()
        );
    };

    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.name", "ForgeKeep Test"]);
    run(&["config", "user.email", "forgekeep@example.test"]);
    std::fs::write(repository.join(FIXTURE_FILE), "one\ntwo\nthree\n").expect("fixture blob");
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "the fixture commit"]);
    repository
}

/// The inherited variables through which a host reaches a git subprocess.
fn host_environment() -> Vec<(String, String)> {
    std::env::vars()
        .filter(|(key, _)| key.starts_with("GIT_") || key == "HOME" || key == "XDG_CONFIG_HOME")
        .collect()
}

/// The permission bits `tar.umask` moves, read out of the archive's own header.
///
/// A tar archive is a sequence of 512-byte blocks; a header block carries the
/// entry name in its first 100 bytes and the mode in the eight after that. The
/// fixture file is looked up by name rather than by position because git writes
/// a `pax_global_header` entry in front of it.
fn archived_mode(archive: &[u8]) -> String {
    for block in archive.chunks(512) {
        if block.len() < 108 {
            break;
        }
        let name = block[..100].split(|byte| *byte == 0).next().unwrap_or(&[]);
        if name != FIXTURE_FILE.as_bytes() {
            continue;
        }
        let mode = block[100..108]
            .split(|byte| *byte == 0)
            .next()
            .unwrap_or(&[])
            .to_vec();
        return String::from_utf8(mode).expect("a tar mode field is ASCII");
    }
    panic!(
        "the archive carries no `{FIXTURE_FILE}` entry ({} bytes)",
        archive.len()
    );
}
