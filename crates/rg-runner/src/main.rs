//! IronForge Runner Agent — polls jobs from the server and executes them.
//!
//! ## Usage
//!
//! ```bash
//! # Register and start running
//! ironforge-runner run --server http://127.0.0.1:8080 --name my-runner
//!
//! # Using a config file
//! ironforge-runner run --config ~/.ironforge/runner.toml
//!
//! # Register only (get token for later use)
//! ironforge-runner register --server http://127.0.0.1:8080 --name my-runner
//! ```
//!
//! Jobs that specify a container image fail closed when Docker is unavailable.
//! They are never silently re-run as local shell jobs.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ironforge-runner", about = "IronForge CI Runner Agent")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Register a new runner and get a token
    Register {
        /// IronForge server URL
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
        /// IronForge server URL
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
        #[arg(long, default_value = "~/.ironforge/runner.toml")]
        config: String,
    },
}

/// Runner configuration file.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
struct RunnerConfig {
    server: Option<String>,
    token: Option<String>,
    runner_id: Option<i64>,
    name: Option<String>,
    labels: Option<Vec<String>>,
}

fn config_path(path: &str) -> PathBuf {
    let expanded = if let Some(remain) = path.strip_prefix('~') {
        match home::home_dir() {
            Some(home) => {
                let trimmed = remain.trim_start_matches('/');
                let mut result = home;
                if !trimmed.is_empty() {
                    result.push(trimmed);
                }
                result.to_string_lossy().to_string()
            }
            None => path.to_string(),
        }
    } else {
        path.to_string()
    };

    PathBuf::from(expanded)
}

fn load_config(path: &str) -> Option<RunnerConfig> {
    let p = config_path(path);
    if p.exists() {
        let content = std::fs::read_to_string(&p).ok()?;
        toml::from_str(&content).ok()
    } else {
        None
    }
}

fn save_config(path: &str, config: &RunnerConfig) -> Result<()> {
    let p = config_path(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(config)?;
    std::fs::write(&p, content)?;
    Ok(())
}

fn resolve_auth_token(auth_token: Option<String>) -> Option<String> {
    auth_token.or_else(|| std::env::var("IRONFORGE_AUTH_TOKEN").ok())
}

/// Register a runner with the server.
async fn register_runner(
    client: &reqwest::Client,
    server: &str,
    name: &str,
    labels: &[String],
    auth_token: &str,
) -> Result<(i64, String)> {
    let resp = client
        .post(format!("{}/api/v1/runners/register", server))
        .bearer_auth(auth_token)
        .json(&serde_json::json!({
            "name": name,
            "labels": labels,
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Registration failed ({}): {}", status, body);
    }

    let data: serde_json::Value = resp.json().await?;
    let runner_id = data["id"].as_i64().context("missing id in response")?;
    let token = data["token"]
        .as_str()
        .context("missing token in response")?
        .to_string();

    Ok((runner_id, token))
}

/// Poll for a pending job (long-polling with 30s timeout).
async fn poll_job(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    token: &str,
) -> Result<Option<PollJobResponse>> {
    let resp = client
        .get(format!(
            "{}/api/v1/runners/{}/jobs/poll?timeout=30",
            server, runner_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?;

    match resp.status() {
        s if s == reqwest::StatusCode::NO_CONTENT => Ok(None),
        s if s.is_success() => {
            let job: PollJobResponse = resp.json().await?;
            Ok(Some(job))
        }
        s => {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Poll failed ({}): {}", s, body);
        }
    }
}

#[derive(serde::Deserialize)]
struct PollJobResponse {
    job_id: i64,
    name: String,
    script: Vec<String>,
    image: Option<String>,
    variables: Option<serde_json::Value>,
    cache_key: Option<String>,
    cache_paths: Option<Vec<String>>,
    #[allow(dead_code)]
    timeout: i64,
}

/// Send a heartbeat to keep the runner marked as online.
async fn send_heartbeat(client: &reqwest::Client, server: &str, runner_id: i64, token: &str) {
    let _ = client
        .post(format!("{}/api/v1/runners/{}/heartbeat", server, runner_id))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await;
}

/// Notify the server that job execution has started.
async fn start_job(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
) {
    let _ = client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/start",
            server, runner_id, job_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await;
}

/// Upload job log output.
async fn upload_log(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    log: &str,
) {
    let _ = client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/log",
            server, runner_id, job_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .body(log.to_string())
        .send()
        .await;
}

async fn download_workspace(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
) -> Result<PathBuf> {
    let response = client
        .get(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/workspace"
        ))
        .bearer_auth(token)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!(
            "workspace download failed ({status}): {}",
            response.text().await.unwrap_or_default()
        );
    }
    let archive = response.bytes().await?;
    let workspace = std::env::temp_dir()
        .join("ironforge-runner")
        .join("jobs")
        .join(job_id.to_string());
    let unpack_path = workspace.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if unpack_path.exists() {
            std::fs::remove_dir_all(&unpack_path).context("remove stale runner workspace")?;
        }
        std::fs::create_dir_all(&unpack_path).context("create runner workspace")?;
        tar::Archive::new(std::io::Cursor::new(archive))
            .unpack(&unpack_path)
            .context("unpack runner workspace")?;
        Ok(())
    })
    .await??;
    Ok(workspace)
}

fn resolved_cache(
    job: &PollJobResponse,
    variables: &[(String, String)],
) -> Result<Option<(String, Vec<String>)>> {
    let (Some(template), Some(paths)) = (&job.cache_key, &job.cache_paths) else {
        return Ok(None);
    };
    let mut key = template.clone();
    for (name, value) in variables {
        key = key
            .replace(&format!("${{{name}}}"), value)
            .replace(&format!("${name}"), value);
    }
    if key.is_empty() || key.len() > 512 {
        anyhow::bail!("cache key must contain 1-512 bytes");
    }
    if paths.is_empty() || paths.len() > 64 {
        anyhow::bail!("cache requires 1-64 paths");
    }
    for path in paths {
        let candidate = std::path::Path::new(path);
        if candidate.is_absolute()
            || candidate.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            anyhow::bail!("cache path must stay within workspace: {path}");
        }
    }
    Ok(Some((key, paths.clone())))
}

async fn restore_cache(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    key: &str,
    workspace: &std::path::Path,
) -> Result<bool> {
    let response = client
        .get(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/cache"
        ))
        .bearer_auth(token)
        .header("x-cache-key", key)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !response.status().is_success() {
        anyhow::bail!(
            "cache restore failed: {}",
            response.text().await.unwrap_or_default()
        );
    }
    let archive = response.bytes().await?;
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        tar::Archive::new(std::io::Cursor::new(archive))
            .unpack(workspace)
            .context("unpack job cache")
    })
    .await??;
    Ok(true)
}

async fn save_cache(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    key: &str,
    paths: &[String],
    workspace: &std::path::Path,
) -> Result<()> {
    let paths = paths.to_vec();
    let workspace = workspace.to_path_buf();
    let archive = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            for path in paths {
                let source = workspace.join(&path);
                if source.is_dir() {
                    builder.append_dir_all(&path, source)?;
                } else if source.is_file() {
                    builder.append_path_with_name(source, &path)?;
                }
            }
            builder.finish()?;
        }
        Ok(bytes)
    })
    .await??;
    if archive.is_empty() {
        return Ok(());
    }
    let response = client
        .put(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/cache"
        ))
        .bearer_auth(token)
        .header("x-cache-key", key)
        .body(archive)
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!(
            "cache upload failed: {}",
            response.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

/// Report job completion.
async fn finish_job(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    status: &str,
    exit_code: i32,
) {
    let _ = client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/finish",
            server, runner_id, job_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({"status": status, "exit_code": exit_code}))
        .send()
        .await;
}

/// Execute a job script locally via platform-appropriate shell.
async fn run_job_local(
    script: &str,
    variables: &[(String, String)],
    workspace: &std::path::Path,
) -> (i32, String) {
    #[cfg(unix)]
    let output = {
        let mut command = tokio::process::Command::new("sh");
        command
            .arg("-c")
            .arg(script)
            .current_dir(workspace)
            .env_clear()
            .kill_on_drop(true);
        for (key, value) in variables {
            command.env(key, value);
        }
        command.output().await
    };

    #[cfg(windows)]
    let output = {
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .args(&["-NoProfile", "-NonInteractive", "-Command", script])
            .current_dir(workspace)
            .env_clear()
            .kill_on_drop(true);
        for (key, value) in variables {
            command.env(key, value);
        }
        command.output().await
    };

    match output {
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let mut log = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            if !stderr.is_empty() {
                if !log.is_empty() {
                    log.push('\n');
                }
                log.push_str(&stderr);
            }
            (code, log)
        }
        Err(e) => (-1, format!("Failed to spawn job: {}", e)),
    }
}

/// Execute a job script inside a Docker container.
async fn run_job_docker(
    image: &str,
    script: &str,
    variables: &[(String, String)],
    workspace: &std::path::Path,
    job_id: i64,
) -> (i32, String) {
    // Check if Docker daemon is running
    let docker_ok = tokio::process::Command::new("docker")
        .arg("info")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !docker_ok {
        let msg = docker_unavailable_message(image);
        tracing::warn!("{}", msg);
        return (-1, msg);
    }

    let mut command = tokio::process::Command::new("docker");
    let container_name = format!("ironforge-runner-job-{job_id}");
    command.args(["run", "--rm", "--name", &container_name, "-v"]);
    command.arg(format!("{}:/workspace", workspace.to_string_lossy()));
    command.args(["-w", "/workspace"]);
    for (key, _) in variables {
        command.arg("-e").arg(key);
    }
    command.args([image, "sh", "-c", script]);
    for (key, value) in variables {
        command.env(key, value);
    }
    command.kill_on_drop(true);
    match command.output().await {
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let mut log = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            if !stderr.is_empty() {
                if !log.is_empty() {
                    log.push('\n');
                }
                log.push_str(&stderr);
            }
            if code != 0 && log.is_empty() {
                log = format!("Docker exited with code {}", code);
            }
            (code, log)
        }
        Err(e) => (-1, format!("Failed to run docker: {}", e)),
    }
}

fn docker_unavailable_message(image: &str) -> String {
    format!(
        "Docker daemon not available. Job requires image '{}' but cannot run in container. \
         Refusing to fall back to local execution.",
        image
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_unavailable_message_is_fail_closed() {
        let msg = docker_unavailable_message("alpine:3.20");

        assert!(msg.contains("alpine:3.20"));
        assert!(msg.contains("Refusing to fall back to local execution"));
    }

    #[tokio::test]
    async fn local_executor_injects_polled_variables_with_a_clean_environment() {
        let variables = vec![("RUNNER_MESSAGE".into(), "hello".into())];
        let (code, log) = run_job_local(
            "test \"$RUNNER_MESSAGE\" = hello && test -z \"$IRONFORGE_HOST_SECRET\" && echo ok",
            &variables,
            std::path::Path::new("."),
        )
        .await;
        assert_eq!(code, 0, "{log}");
        assert!(log.contains("ok"));
    }

    #[test]
    fn resolves_cache_key_from_polled_environment_and_rejects_escape() {
        let job = PollJobResponse {
            job_id: 1,
            name: "cache".into(),
            script: vec![],
            image: None,
            variables: None,
            cache_key: Some("build-${CI_SHA}".into()),
            cache_paths: Some(vec!["target".into()]),
            timeout: 60,
        };
        let cache = resolved_cache(&job, &[("CI_SHA".into(), "abc".into())])
            .unwrap()
            .unwrap();
        assert_eq!(cache.0, "build-abc");
        let invalid = PollJobResponse {
            cache_paths: Some(vec!["../outside".into()]),
            ..job
        };
        assert!(resolved_cache(&invalid, &[]).is_err());
    }
}

fn job_variables(value: Option<&serde_json::Value>) -> Vec<(String, String)> {
    let mut variables = value
        .and_then(serde_json::Value::as_object)
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    let value = match value {
                        serde_json::Value::String(value) => value.clone(),
                        serde_json::Value::Number(value) => value.to_string(),
                        serde_json::Value::Bool(value) => value.to_string(),
                        _ => return None,
                    };
                    Some((key.clone(), value))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Ok(path) = std::env::var("PATH") {
        variables.push(("PATH".into(), path));
    }
    if let Ok(lang) = std::env::var("LANG") {
        variables.push(("LANG".into(), lang));
    }
    variables
}

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
            name,
            labels,
            save,
            auth_token,
        } => {
            let client = reqwest::Client::new();
            let labels_vec: Vec<String> = labels
                .map(|s| s.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let auth_token = resolve_auth_token(auth_token)
                .context("runner registration requires --auth-token or IRONFORGE_AUTH_TOKEN")?;

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
                let config_path = "~/.ironforge/runner.toml";
                save_config(config_path, &config)?;
                println!("  Config saved to {}", config_path);
            }
        }

        Commands::Run {
            server,
            name,
            labels,
            token,
            runner_id,
            auth_token,
            config,
        } => {
            let client = reqwest::Client::new();

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
                        "runner auto-registration requires --auth-token or IRONFORGE_AUTH_TOKEN; \
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
                                            &format!("ironforge-runner-job-{}", job.job_id),
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
    }

    Ok(())
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
