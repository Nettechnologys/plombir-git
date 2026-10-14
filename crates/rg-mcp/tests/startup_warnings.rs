//! Startup warnings reach stderr without `RUST_LOG`, and stdout stays JSON-RPC.
//!
//! The subscriber used to be built with `EnvFilter::from_default_env()`, which
//! keeps only ERROR when `RUST_LOG` is unset. An operator who opted into
//! sending the PAT over plaintext HTTP was then never told so.

use std::io::Write;
use std::process::{Command, Stdio};

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;

fn run_mcp(envs: &[(&str, &str)]) -> (String, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_plombir-git-mcp"));
    command
        .env_remove("RUST_LOG")
        .env_remove("PLOMBIR_GIT_URL")
        .env_remove("PLOMBIR_GIT_PAT")
        .env_remove("PLOMBIR_GIT_ALLOW_INSECURE_HTTP")
        .envs(envs.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("run plombir-git-mcp");
    {
        let mut stdin = child.stdin.take().expect("stdin");
        writeln!(stdin, "{INITIALIZE}").expect("write initialize");
    }
    let output = child.wait_with_output().expect("wait for plombir-git-mcp");
    let stdout = String::from_utf8(output.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    (stdout, stderr)
}

fn assert_stdout_is_json_rpc(stdout: &str) {
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    let response: serde_json::Value = serde_json::from_str(lines[0]).expect("JSON-RPC line");
    assert_eq!(response["jsonrpc"], "2.0", "{stdout}");
    assert_eq!(response["id"], 1, "{stdout}");
}

#[test]
fn plaintext_pat_transport_is_announced_without_rust_log() {
    let (stdout, stderr) = run_mcp(&[
        ("PLOMBIR_GIT_URL", "http://forge.invalid"),
        ("PLOMBIR_GIT_PAT", "never-printed"),
        ("PLOMBIR_GIT_ALLOW_INSECURE_HTTP", "true"),
    ]);

    assert!(
        stderr.contains("the MCP PAT may cross plaintext HTTP"),
        "{stderr}"
    );
    assert!(!stderr.contains("never-printed"), "{stderr}");
    assert!(!stdout.contains("plaintext HTTP"), "{stdout}");
    assert_stdout_is_json_rpc(&stdout);
}

#[test]
fn a_missing_pat_is_announced_without_rust_log() {
    let (stdout, stderr) = run_mcp(&[("PLOMBIR_GIT_URL", "https://forge.invalid")]);

    assert!(stderr.contains("PLOMBIR_GIT_PAT not set"), "{stderr}");
    assert_stdout_is_json_rpc(&stdout);
}
