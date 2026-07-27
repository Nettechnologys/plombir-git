//! ForgeKeep CLI — main entry point.
//!
//! The command surface is defined in [`cli`]; each subcommand's implementation
//! lives in a focused module:
//!  - [`serve`] — the `serve` command (startup validation + HTTP/SSH bootstrap)
//!  - [`runner`] — the deprecated `runner` alias for `forgekeep-runner run`
//!  - [`commands`] — the remaining one-shot subcommands
//!  - [`admin`] — SQLite backup/restore + JWT secret helpers
//!  - [`config`] — the TOML config model + `CLI > config > default` resolution,
//!    shared by `serve` and every one-shot subcommand
//!  - [`dbconn`] — the single database-connect path, with the SQLite
//!    unwritable-directory diagnostic

mod admin;
mod cli;
mod commands;
mod config;
mod dbconn;
mod runner;
mod serve;
mod telemetry;

use clap::Parser;

use cli::{Cli, Commands};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI args first (without initializing logging, to avoid early output)
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            repo_root,
            http_addr,
            ssh_addr,
            host_key,
            db_url,
            jwt_secret,
            docker,
            external_runners,
            allow_host_runner,
            rate_limit_max,
            rate_limit_window,
            rate_limit_trusted_proxies,
            smtp_host,
            smtp_port,
            smtp_user,
            smtp_pass,
            smtp_from,
            tls_cert,
            tls_key,
            config,
            log_file,
            log_max_size_mb,
            log_max_files,
        } => {
            serve::run_serve(
                repo_root,
                http_addr,
                ssh_addr,
                host_key,
                db_url,
                jwt_secret,
                docker,
                external_runners,
                allow_host_runner,
                rate_limit_max,
                rate_limit_window,
                rate_limit_trusted_proxies,
                smtp_host,
                smtp_port,
                smtp_user,
                smtp_pass,
                smtp_from,
                tls_cert,
                tls_key,
                config,
                log_file,
                log_max_size_mb,
                log_max_files,
            )
            .await?;
        }

        Commands::Migrate { db_url, config } => commands::cmd_migrate(db_url, config).await?,

        Commands::GenSecret => commands::cmd_gen_secret(),

        Commands::RebuildFts { db_url, config } => {
            commands::cmd_rebuild_fts(db_url, config).await?
        }

        Commands::BackupDb {
            db_url,
            config,
            output,
            force,
        } => commands::cmd_backup_db(db_url, config, output, force).await?,

        Commands::RestoreDb {
            db_url,
            config,
            input,
            force,
        } => commands::cmd_restore_db(db_url, config, input, force)?,

        Commands::CreateRepo {
            owner,
            name,
            repo_root,
            config,
        } => commands::cmd_create_repo(owner, name, repo_root, config)?,

        Commands::Runner {
            server,
            name,
            labels,
            runner_id,
            token,
            auth_token,
            config,
        } => runner::cmd_runner(server, name, labels, runner_id, token, auth_token, config).await?,

        Commands::Import {
            platform,
            source_url,
            target_owner,
            target_name,
            token,
            repo_root,
            db_url,
            config,
            skip_repo,
            skip_issues,
            skip_prs,
            skip_labels,
            skip_milestones,
            skip_releases,
            import_wiki,
        } => {
            commands::cmd_import(
                platform,
                source_url,
                target_owner,
                target_name,
                token,
                repo_root,
                db_url,
                config,
                skip_repo,
                skip_issues,
                skip_prs,
                skip_labels,
                skip_milestones,
                skip_releases,
                import_wiki,
            )
            .await?
        }

        Commands::Package { cmd } => commands::cmd_package(cmd).await?,

        Commands::IndexRepo {
            repo_slug,
            repo_root,
            db_url,
            config,
            ref_name,
        } => commands::cmd_index_repo(repo_slug, repo_root, db_url, config, ref_name).await?,
    }

    Ok(())
}
