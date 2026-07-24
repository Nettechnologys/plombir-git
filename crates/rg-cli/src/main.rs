//! ForgeKeep CLI — main entry point.
//!
//! The command surface is defined in [`cli`]; each subcommand's implementation
//! lives in a focused module:
//!  - [`serve`] — the `serve` command (config model + HTTP/SSH bootstrap)
//!  - [`runner`] — the `runner` command (CI job polling + execution)
//!  - [`commands`] — the remaining one-shot subcommands
//!  - [`admin`] — SQLite backup/restore + JWT secret helpers

mod admin;
mod cli;
mod commands;
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

        Commands::Migrate { db_url } => commands::cmd_migrate(db_url).await?,

        Commands::GenSecret => commands::cmd_gen_secret(),

        Commands::RebuildFts { db_url } => commands::cmd_rebuild_fts(db_url).await?,

        Commands::BackupDb {
            db_url,
            output,
            force,
        } => commands::cmd_backup_db(db_url, output, force).await?,

        Commands::RestoreDb {
            db_url,
            input,
            force,
        } => commands::cmd_restore_db(db_url, input, force)?,

        Commands::CreateRepo {
            owner,
            name,
            repo_root,
        } => commands::cmd_create_repo(owner, name, repo_root)?,

        Commands::Runner {
            server,
            name,
            runner_id,
            token,
            auth_token,
        } => runner::cmd_runner(server, name, runner_id, token, auth_token).await?,

        Commands::Import {
            platform,
            source_url,
            target_owner,
            target_name,
            token,
            repo_root,
            db_url,
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
            ref_name,
        } => commands::cmd_index_repo(repo_slug, repo_root, db_url, ref_name).await?,
    }

    Ok(())
}
