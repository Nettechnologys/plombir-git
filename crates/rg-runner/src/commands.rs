//! Subcommand handlers: `register` (one-shot registration) and `run` (register
//! if needed, then poll-and-execute loop).

use anyhow::{Context, Result};

use crate::api::{
    download_workspace, finish_job, poll_job, register_runner, restore_cache, save_cache,
    send_heartbeat, start_job, upload_log,
};
use crate::config::{load_config, resolve_auth_token, save_config, RunnerConfig};
use crate::executor::{job_variables, resolved_cache, run_job_docker, run_job_local};

/// Connect timeout (TCP + TLS handshake only) for the runner's HTTP client.
///
/// Mirrors `rg_core::net`'s outbound connect timeout so a dead/hung server can't
/// pin registration or the heartbeat task on the connect phase forever.
const RUNNER_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Build the runner's HTTP client.
///
/// It sets **only** `connect_timeout`, deliberately NOT a global request
/// `.timeout(...)`: `cmd_run` long-polls the job queue (`/jobs/poll?timeout=30`)
/// and streams potentially large workspace / cache archives, both of which a
/// whole-request timeout would abort. Bounding just the handshake still stops a
/// dead peer from hanging the runner, while leaving long-poll and big transfers
/// intact. The short, must-stay-snappy calls layer their own per-request
/// timeout on top (see [`crate::api::send_heartbeat`]).
fn build_runner_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(RUNNER_CONNECT_TIMEOUT)
        .build()
        .expect("failed to build runner HTTP client: no native TLS backend available")
}

/// Handle `forgekeep-runner register`: register a runner and optionally persist
/// its token to the config file.
pub(crate) async fn cmd_register(
    server: String,
    name: String,
    labels: Option<String>,
    save: bool,
    auth_token: Option<String>,
) -> Result<()> {
    let client = build_runner_client();
    let labels_vec: Vec<String> = labels
        .map(|s| s.split(',').map(|s| s.trim().to_string()).collect())
        .unwrap_or_default();
    let auth_token = resolve_auth_token(auth_token)
        .context("runner registration requires --auth-token or FORGEKEEP_AUTH_TOKEN")?;

    println!("Registering runner '{}' with {}...", name, server);
    let (runner_id, token) =
        register_runner(&client, &server, &name, &labels_vec, &auth_token).await?;
    println!("Runner registered successfully!");
    println!("  ID:    {}", runner_id);
    println!("  Token: {}", token);

    if save {
        let config = RunnerConfig {
            server: Some(server),
            runner_id: Some(runner_id),
            token: Some(token.clone()),
            name: Some(name),
            labels: Some(labels_vec),
        };
        let config_path = "~/.forgekeep/runner.toml";
        save_config(config_path, &config)?;
        println!("  Config saved to {}", config_path);
    }

    Ok(())
}

/// Handle `forgekeep-runner run`: resolve/register the runner, spawn the
/// heartbeat task, then long-poll for jobs and execute each one.
pub(crate) async fn cmd_run(
    server: String,
    name: Option<String>,
    labels: Option<String>,
    token: Option<String>,
    runner_id: Option<i64>,
    auth_token: Option<String>,
    config: String,
) -> Result<()> {
    let client = build_runner_client();

    // Resolve config: CLI args > config file > defaults
    let cfg = load_config(&config);
    let resolved_server = server.as_str();
    let (resolved_id, resolved_token, resolved_name) = match (runner_id, token, name) {
        (Some(id), Some(tok), Some(n)) => (id, tok, n),
        (Some(id), Some(tok), None) => (
            id,
            tok,
            cfg.as_ref()
                .and_then(|c| c.name.clone())
                .unwrap_or_default(),
        ),
        _ => {
            // Need to register
            let cfg_name = cfg
                .as_ref()
                .and_then(|c| c.name.clone())
                .unwrap_or_else(|| {
                    hostname::get().unwrap_or_else(|_| "unnamed-runner".to_string())
                });
            let cfg_labels = cfg
                .as_ref()
                .and_then(|c| c.labels.clone())
                .unwrap_or_default();
            let resolved_labels = labels
                .map(|s| {
                    s.split(',')
                        .map(|s| s.trim().to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or(cfg_labels);

            println!(
                "Registering runner '{}' with {}...",
                cfg_name, resolved_server
            );
            let auth_token = resolve_auth_token(auth_token).context(
                "runner auto-registration requires --auth-token or FORGEKEEP_AUTH_TOKEN; \
                 alternatively pass --runner-id and --token",
            )?;
            let (id, tok) = register_runner(
                &client,
                resolved_server,
                &cfg_name,
                &resolved_labels,
                &auth_token,
            )
            .await?;
            println!("Registered! ID={}, Token={}", id, tok);

            // Save for future runs
            let mut updated_cfg = cfg.clone().unwrap_or_default();
            updated_cfg.server = Some(resolved_server.to_string());
            updated_cfg.runner_id = Some(id);
            updated_cfg.token = Some(tok.clone());
            updated_cfg.name = Some(cfg_name.clone());
            updated_cfg.labels = Some(resolved_labels);
            if save_config(&config, &updated_cfg).is_ok() {
                println!("Config saved to {}", config);
            }

            (id, tok, cfg_name)
        }
    };

    println!(
        "Runner {} started (server={})",
        resolved_name, resolved_server
    );

    // Spawn heartbeat task (every 30s)
    let hb_client = client.clone();
    let hb_server = resolved_server.to_string();
    let hb_token = resolved_token.clone();
    let hb_id = resolved_id;
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            send_heartbeat(&hb_client, &hb_server, hb_id, &hb_token).await;
        }
    });

    // Main job polling loop
    loop {
        match poll_job(&client, resolved_server, resolved_id, &resolved_token).await {
            Ok(Some(job)) => {
                println!(
                    "→ Job #{}: {} (image={})",
                    job.job_id,
                    job.name,
                    job.image.as_deref().unwrap_or("local")
                );

                // Start
                start_job(
                    &client,
                    resolved_server,
                    resolved_id,
                    job.job_id,
                    &resolved_token,
                )
                .await;

                let workspace = match download_workspace(
                    &client,
                    resolved_server,
                    resolved_id,
                    job.job_id,
                    &resolved_token,
                )
                .await
                {
                    Ok(workspace) => workspace,
                    Err(error) => {
                        let log = format!("Failed to prepare job workspace: {error}");
                        upload_log(
                            &client,
                            resolved_server,
                            resolved_id,
                            job.job_id,
                            &resolved_token,
                            &log,
                        )
                        .await;
                        finish_job(
                            &client,
                            resolved_server,
                            resolved_id,
                            job.job_id,
                            &resolved_token,
                            "failure",
                            -1,
                        )
                        .await;
                        continue;
                    }
                };

                // Execute in the exact commit snapshot assigned by the server.
                let script_str = job.script.join("\n");
                let mut variables = job_variables(job.variables.as_ref());
                variables.push(("HOME".into(), workspace.to_string_lossy().into_owned()));
                let cache = match resolved_cache(&job, &variables) {
                    Ok(cache) => cache,
                    Err(error) => {
                        let log = format!("Invalid cache configuration: {error}");
                        upload_log(
                            &client,
                            resolved_server,
                            resolved_id,
                            job.job_id,
                            &resolved_token,
                            &log,
                        )
                        .await;
                        finish_job(
                            &client,
                            resolved_server,
                            resolved_id,
                            job.job_id,
                            &resolved_token,
                            "failure",
                            -1,
                        )
                        .await;
                        let _ = tokio::fs::remove_dir_all(&workspace).await;
                        continue;
                    }
                };
                if let Some((key, _)) = &cache {
                    if let Err(error) = restore_cache(
                        &client,
                        resolved_server,
                        resolved_id,
                        job.job_id,
                        &resolved_token,
                        key,
                        &workspace,
                    )
                    .await
                    {
                        tracing::warn!(job_id = job.job_id, %error, "cache restore failed; continuing");
                    }
                }
                let execution = async {
                    if let Some(img) = &job.image {
                        run_job_docker(img, &script_str, &variables, &workspace, job.job_id)
                            .await
                    } else {
                        run_job_local(&script_str, &variables, &workspace).await
                    }
                };
                let timeout_seconds =
                    u64::try_from(job.timeout).unwrap_or(3600).clamp(1, 86_400);
                let (exit_code, log) = match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_seconds),
                    execution,
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => {
                        if job.image.is_some() {
                            let _ = tokio::process::Command::new("docker")
                                .args([
                                    "rm",
                                    "-f",
                                    &format!("forgekeep-runner-job-{}", job.job_id),
                                ])
                                .output()
                                .await;
                        }
                        (-1, format!("Job timed out after {timeout_seconds} seconds"))
                    }
                };

                if exit_code == 0 {
                    if let Some((key, paths)) = &cache {
                        if let Err(error) = save_cache(
                            &client,
                            resolved_server,
                            resolved_id,
                            job.job_id,
                            &resolved_token,
                            key,
                            paths,
                            &workspace,
                        )
                        .await
                        {
                            tracing::warn!(job_id = job.job_id, %error, "cache save failed; job remains successful");
                        }
                    }
                }

                // Upload log
                upload_log(
                    &client,
                    resolved_server,
                    resolved_id,
                    job.job_id,
                    &resolved_token,
                    &log,
                )
                .await;

                // Finish
                let status = if exit_code == 0 { "success" } else { "failure" };
                finish_job(
                    &client,
                    resolved_server,
                    resolved_id,
                    job.job_id,
                    &resolved_token,
                    status,
                    exit_code,
                )
                .await;

                if let Err(error) = tokio::fs::remove_dir_all(&workspace).await {
                    tracing::warn!(job_id = job.job_id, %error, "failed to clean runner workspace");
                }

                println!("  ✓ {} (exit={})", status, exit_code);
            }
            Ok(None) => {
                // No job available (timeout) — continue polling
                continue;
            }
            Err(e) => {
                tracing::error!("Poll error: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        }
    }
}

/// Get the system hostname via `hostname` command.
mod hostname {
    pub fn get() -> std::io::Result<String> {
        std::process::Command::new("hostname")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .map_err(std::io::Error::other)
    }
}
