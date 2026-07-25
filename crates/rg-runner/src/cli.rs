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

        /// Config file `--save` writes to — must be the same path `run` reads
        #[arg(long, default_value = "~/.forgekeep/runner.toml")]
        config: String,
    },

    /// Start the runner (register if needed, then poll and execute jobs)
    ///
    /// Settings that also exist as a `--config` key resolve in the order
    /// CLI arg > config file > built-in default.
    // Hence `--server` is an `Option` with no clap `default_value`: a clap
    // default is indistinguishable from a value the operator typed, so with one
    // the config file's `server` could never win over "the flag was not passed".
    // The default lives in `config::DEFAULT_SERVER` and is named in the help.
    Run {
        /// ForgeKeep server URL [config: server]
        /// [default: http://127.0.0.1:8080]
        #[arg(long)]
        server: Option<String>,

        /// Runner name [config: name] [default: system hostname]
        #[arg(long)]
        name: Option<String>,

        /// Runner labels (comma-separated) [config: labels]
        #[arg(long)]
        labels: Option<String>,

        /// Existing runner token (skip registration) [config: token]
        #[arg(long)]
        token: Option<String>,

        /// Existing runner ID (used with --token) [config: runner_id]
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
