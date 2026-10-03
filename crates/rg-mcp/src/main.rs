//! `plombir-git-mcp` – MCP server entry point.
//!
//! # Transports
//! - **stdio** – run as subprocess of an AI agent.
//!
//! Over HTTP the same tools are served by the Plombir Git server itself at
//! `POST /api/v1/mcp`, with no local binary at all. Passing `--sse` exits with
//! an error instead of silently starting a partial server.
//!
//! # Environment
//! | Variable                        | Default                 | Notes                                      |
//! |---------------------------------|-------------------------|--------------------------------------------|
//! | `PLOMBIR_GIT_URL`                 | `http://localhost:8080` | Plombir Git API base                         |
//! | `PLOMBIR_GIT_PAT`                 | _(none)_                | Bearer token for API auth                  |
//! | `PLOMBIR_GIT_ALLOW_INSECURE_HTTP` | `false`                 | Explicit opt-in for remote plaintext HTTP |

use std::io::{self, BufRead, BufWriter, Write};
use std::io::{stdin, stdout};

// Pull everything from the library crate.
use rg_mcp::protocol::*;
use rg_mcp::AppState;

// ── stdio main loop ─────────────────────────────────────────────

fn run_stdio(state: &AppState) -> io::Result<()> {
    let stdin = stdin();
    let stdout = stdout();
    let reader = io::BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("read error: {}", e);
                break;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(line) {
            Ok(r) => r,
            Err(e) => {
                let resp = make_error(
                    serde_json::Value::Null,
                    -32700,
                    &format!("parse error: {}", e),
                );
                write_json(&mut writer, &resp)?;
                continue;
            }
        };

        if let Some(resp) = rg_mcp::dispatch(state, &req) {
            write_json(&mut writer, &resp)?;
        }
    }
    Ok(())
}

fn write_json<W: Write>(w: &mut W, resp: &JsonRpcResponse) -> io::Result<()> {
    let s =
        serde_json::to_string(resp).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    writeln!(w, "{}", s)?;
    w.flush()?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    rg_process::refuse_retired_environment()?;

    // Tools/resources are dispatched synchronously but perform async reqwest
    // calls via `Handle::current().block_on(...)`. Create and enter a runtime
    // for the whole stdio loop so those handlers never panic due to a missing
    // Tokio context.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let _runtime_guard = runtime.enter();

    // Log to stderr so the stdio JSON-RPC channel stays clean.
    if tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init()
        .is_err()
    {
        // Another embedding binary/test installed a subscriber first.
    }

    let app_state = AppState::from_env()?;

    if std::env::args().any(|a| a == "--sse") {
        anyhow::bail!(
            "SSE transport is not implemented; use stdio by running plombir-git-mcp without --sse, or point an HTTP MCP client at <server>/api/v1/mcp"
        );
    }

    run_stdio(&app_state)?;
    Ok(())
}
