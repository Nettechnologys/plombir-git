//! The runner refuses to start on a variable of the project's former name.
//!
//! `rg_process::refuse_retired_environment` is tested on its own; this proves
//! the binary actually asks it. `run` with a config that does not exist fails
//! anyway, so the assertion is on the reason, not on the exit status alone.

use std::process::{Command, Stdio};

#[test]
fn a_variable_of_the_former_name_refuses_the_start() {
    let dir = tempfile::tempdir().expect("a working directory");
    let output = Command::new(env!("CARGO_BIN_EXE_plombir-git-runner"))
        .args(["run", "--config"])
        .arg(dir.path().join("absent.toml"))
        .env("FORGEKEEP_RUNNER_TOKEN", "never-printed")
        .stdin(Stdio::null())
        .output()
        .expect("run plombir-git-runner");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("FORGEKEEP_RUNNER_TOKEN -> PLOMBIR_GIT_RUNNER_TOKEN"),
        "{stderr}"
    );
    assert!(!stderr.contains("never-printed"), "{stderr}");
}
