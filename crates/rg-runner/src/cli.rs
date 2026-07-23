//! Command-line interface definitions for the runner agent.

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "forgekeep-runner", about = "ForgeKeep CI Runner Agent")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
    /// Register a new runner and get a token
    Register {
        /// ForgeKeep server URL
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,

        /// Runner name
        #[arg(long)]
        name: String,

        /// Runner labels (comma-separated, e.g. "docker,linux,amd64")
        #[arg(long)]
        labels: Option<String>,

        /// Save token to config file
        #[arg(long)]
        save: bool,

        /// Admin user JWT used only for runner registration
        #[arg(long)]
        auth_token: Option<String>,
    },

    /// Start the runner (register if needed, then poll and execute jobs)
    Run {
        /// ForgeKeep server URL
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,

        /// Runner name
        #[arg(long)]
        name: Option<String>,

        /// Runner labels (comma-separated)
        #[arg(long)]
        labels: Option<String>,

        /// Existing runner token (skip registration)
        #[arg(long)]
        token: Option<String>,

        /// Existing runner ID (used with --token)
        #[arg(long)]
        runner_id: Option<i64>,

        /// Admin user JWT used only when this command needs to register a runner
        #[arg(long)]
        auth_token: Option<String>,

        /// Path to config file
        #[arg(long, default_value = "~/.forgekeep/runner.toml")]
        config: String,
    },
}
