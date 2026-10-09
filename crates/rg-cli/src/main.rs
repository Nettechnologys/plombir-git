//! Plombir Git CLI — main entry point.
//!
//! The command surface is defined in [`cli`]; each subcommand's implementation
//! lives in a focused module:
//!  - [`serve`] — the `serve` command (startup validation + HTTP/SSH bootstrap)
//!  - [`runner`] — the deprecated `runner` alias for `plombir-git-runner run`
//!  - [`commands`] — the remaining one-shot subcommands
//!  - [`admin`] — SQLite backup/restore + JWT secret helpers
//!  - [`config`] — the TOML config model + `CLI > config > default` resolution,
//!    shared by `serve` and every one-shot subcommand
//!  - [`dbconn`] — the single database-connect path, with the SQLite
//!    unwritable-directory diagnostic
//!  - [`repo_root`] — the single answer to "the repository storage root is not
//!    there": create it on a clean install, refuse it on a populated one

mod admin;
mod cli;
mod commands;
mod config;
mod dbconn;
mod repo_root;
mod runner;
mod serve;
mod telemetry;

use clap::Parser;

use cli::{Cli, Commands};

fn main() -> anyhow::Result<()> {
    // Cap glibc's per-thread arenas before Tokio starts worker threads. The
    // default can retain large, THP-backed arenas after transient git/API
    // traffic. An explicit operator setting still takes precedence.
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    if std::env::var_os("MALLOC_ARENA_MAX").is_none() {
        let applied = unsafe { libc::mallopt(libc::M_ARENA_MAX, 2) };
        anyhow::ensure!(applied != 0, "failed to cap glibc malloc arenas");
    }

    run()
}

#[tokio::main]
async fn run() -> anyhow::Result<()> {
    // Parse CLI args first (without initializing logging, to avoid early output)
    let cli = Cli::parse();
    rg_process::refuse_retired_environment()?;
    let state_writer = commands::prepare_state_writer(&cli.command)?;
    let state_cfg = || {
        state_writer
            .as_ref()
            .expect("server-owned command must prepare its state policy")
            .as_ref()
    };

    match cli.command {
        Commands::Serve {
            repo_root,
            http_addr,
            ssh_addr,
            host_key,
            db_url,
            jwt_secret,
            encryption_key,
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
            log_format,
            log_max_size_mb,
            log_max_files,
            listen_address_file,
        } => {
            serve::run_serve(
                repo_root,
                http_addr,
                ssh_addr,
                host_key,
                db_url,
                jwt_secret,
                encryption_key,
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
                log_format,
                log_max_size_mb,
                log_max_files,
                listen_address_file,
            )
            .await?;
        }

        Commands::Migrate { db_url, config: _ } => {
            commands::cmd_migrate(db_url, state_cfg()).await?
        }

        Commands::GenSecret => commands::cmd_gen_secret(),

        Commands::RotateInstanceKey {
            db_url,
            config: _,
            jwt_secret,
            encryption_key,
            yes,
        } => {
            commands::cmd_rotate_instance_key(db_url, state_cfg(), jwt_secret, encryption_key, yes)
                .await?
        }

        Commands::RotateEncryptionKey {
            db_url,
            config: _,
            jwt_secret,
            old,
            new,
            dry_run,
            yes,
        } => {
            commands::cmd_rotate_encryption_key(
                db_url,
                state_cfg(),
                jwt_secret,
                old,
                new,
                dry_run,
                yes,
            )
            .await?
        }

        Commands::RebuildFts { db_url, config: _ } => {
            commands::cmd_rebuild_fts(db_url, state_cfg()).await?
        }

        Commands::BackupDb {
            db_url,
            config,
            output,
            force,
        } => commands::cmd_backup_db(db_url, config, output, force).await?,

        Commands::RestoreDb {
            db_url,
            config: _,
            input,
            force,
        } => commands::cmd_restore_db(db_url, state_cfg(), input, force)?,

        Commands::CreateRepo {
            owner,
            name,
            repo_root,
            config: _,
        } => commands::cmd_create_repo(owner, name, repo_root, state_cfg())?,

        Commands::CreateAdmin {
            username,
            email,
            password_stdin,
            db_url,
            config: _,
        } => {
            commands::cmd_create_admin(username, email, password_stdin, db_url, state_cfg()).await?
        }

        Commands::Runner {
            server,
            allow_insecure_http,
            repository,
            name,
            labels,
            label,
            runner_id,
            token,
            auth_token,
            config,
        } => {
            runner::cmd_runner(rg_runner::RunCommand {
                server,
                allow_insecure_http,
                repository,
                name,
                labels,
                label,
                token,
                runner_id,
                auth_token,
                config,
            })
            .await?
        }

        Commands::Import {
            platform,
            source_url,
            target_owner,
            target_name,
            token,
            repo_root,
            db_url,
            config: _,
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
                state_cfg(),
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

        Commands::Package { cmd } => {
            commands::cmd_package(
                cmd,
                state_writer.as_ref().and_then(|config| config.as_ref()),
            )
            .await?
        }

        Commands::ListTombstones { repo_root, config } => {
            commands::cmd_list_tombstones(repo_root, config).await?
        }

        Commands::IndexRepo {
            repo_slug,
            repo_root,
            db_url,
            config: _,
            ref_name,
        } => commands::cmd_index_repo(repo_slug, repo_root, db_url, state_cfg(), ref_name).await?,
    }

    Ok(())
}
