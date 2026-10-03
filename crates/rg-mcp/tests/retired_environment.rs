//! The MCP server refuses to start on a variable of the project's former name.
//!
//! Without the refusal a forgotten `FORGEKEEP_URL` / `FORGEKEEP_PAT` falls back
//! to `localhost` with no token. stdin is closed, so a binary that skipped the
//! check would read EOF and exit successfully instead of hanging the test.

use std::process::{Command, Stdio};

#[test]
fn a_variable_of_the_former_name_refuses_the_start() {
    let output = Command::new(env!("CARGO_BIN_EXE_plombir-git-mcp"))
        .env("FORGEKEEP_PAT", "never-printed")
        .stdin(Stdio::null())
        .output()
        .expect("run plombir-git-mcp");
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(!output.status.success(), "{stderr}");
    assert!(
        stderr.contains("FORGEKEEP_PAT -> PLOMBIR_GIT_PAT"),
        "{stderr}"
    );
    assert!(!stderr.contains("never-printed"), "{stderr}");
}
