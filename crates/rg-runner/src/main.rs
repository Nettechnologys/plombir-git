//! ForgeKeep Runner Agent — polls jobs from the server and executes them.
//!
//! ## Usage
//!
//! ```bash
//! # Register and start running
//! forgekeep-runner run --server http://127.0.0.1:8080 --name my-runner
//!
//! # Using a config file
//! forgekeep-runner run --config ~/.forgekeep/runner.toml
//!
//! # Register only (get token for later use)
//! forgekeep-runner register --server http://127.0.0.1:8080 --name my-runner
//! ```
//!
//! Remote servers require HTTPS unless `--allow-insecure-http` (or the matching
//! `runner.toml` key) explicitly permits credentials on that one HTTP origin.
//!
//! Jobs that specify a container image fail closed when Docker is unavailable.
//! They are never silently re-run as local shell jobs.

mod cli;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Commands};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Register {
            server,
            allow_insecure_http,
            name,
            labels,
            save,
            auth_token,
            config,
        } => {
            rg_runner::cmd_register(
                server,
                allow_insecure_http,
                name,
                labels,
                save,
                auth_token,
                config,
            )
            .await?;
        }

        Commands::Run {
            server,
            allow_insecure_http,
            name,
            labels,
            token,
            runner_id,
            auth_token,
            config,
        } => {
            rg_runner::cmd_run(rg_runner::RunCommand {
                server,
                allow_insecure_http,
                name,
                labels,
                token,
                runner_id,
                auth_token,
                config,
            })
            .await?;
        }
    }

    Ok(())
}
