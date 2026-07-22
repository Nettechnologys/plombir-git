//! `ironforge-mcp` – MCP server entry point.
//!
//! # Transports
//! - **stdio** – run as subprocess of an AI agent.
//!
//! HTTP SSE transport is not implemented yet. Passing `--sse` exits with an
//! error instead of silently starting a partial server.
//!
//! # Environment
//! | Variable        | Default                 | Notes                     |
//! |-----------------|-------------------------|---------------------------|
//! | `IRONFORGE_URL` | `http://localhost:8080` | IronForge API base       |
//! | `IRONFORGE_PAT` | _(none)_              | Bearer token for API auth |

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

        let resp = dispatch(state, &req);
        write_json(&mut writer, &resp)?;
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

fn dispatch(state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    match req.method.as_str() {
        "initialize" => handle_initialize(state, req),
        "notifications/initialized" => make_success(req.id.clone(), serde_json::json!({})),
        "tools/list" => rg_mcp::tools::list_tools(state, req),
        "tools/call" => rg_mcp::tools::call_tool(state, req),
        "resources/list" => rg_mcp::resources::list_resources(state, req),
        "resources/read" => rg_mcp::resources::read_resource(state, req),
        "notifications/cancelled" => make_success(req.id.clone(), serde_json::json!({})),
        _ => make_error(
            req.id.clone(),
            -32601,
            &format!("method not found: {}", req.method),
        ),
    }
}

fn handle_initialize(_state: &AppState, req: &JsonRpcRequest) -> JsonRpcResponse {
    let result = serde_json::json!({
        "protocolVersion": "2024-11-05",
        "serverInfo": {
            "name": "ironforge-mcp",
            "version": "0.1.0"
        },
        "capabilities": {
            "tools": { "listChanged": true },
            "resources": { "subscribe": false, "listChanged": true }
        }
    });
    make_success(req.id.clone(), result)
}

fn main() -> anyhow::Result<()> {
    // Tools/resources are dispatched synchronously but perform async reqwest
    // calls via `Handle::current().block_on(...)`. Create and enter a runtime
    // for the whole stdio loop so those handlers never panic due to a missing
    // Tokio context.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()?;
    let _runtime_guard = runtime.enter();

    // 日志打到 stderr，不污染 stdio JSON-RPC 通道
    let _ = tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();

    let app_state = AppState::from_env()?;

    if std::env::args().any(|a| a == "--sse") {
        anyhow::bail!(
            "SSE transport is not implemented; use stdio by running ironforge-mcp without --sse"
        );
    }

    run_stdio(&app_state)?;
    Ok(())
}
