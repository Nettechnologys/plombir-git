//! HTTP client calls against the ForgeKeep server's runner API
//! (registration, job polling, heartbeats, workspace/cache transfer, status).

use std::path::PathBuf;

use anyhow::{Context, Result};

/// Register a runner with the server.
pub(crate) async fn register_runner(
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
pub(crate) async fn poll_job(
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
pub(crate) struct PollJobResponse {
    pub(crate) job_id: i64,
    pub(crate) name: String,
    pub(crate) script: Vec<String>,
    pub(crate) image: Option<String>,
    pub(crate) variables: Option<serde_json::Value>,
    pub(crate) cache_key: Option<String>,
    pub(crate) cache_paths: Option<Vec<String>>,
    #[allow(dead_code)]
    pub(crate) timeout: i64,
}

/// Send a heartbeat to keep the runner marked as online.
pub(crate) async fn send_heartbeat(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    token: &str,
) {
    let _ = client
        .post(format!("{}/api/v1/runners/{}/heartbeat", server, runner_id))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await;
}

/// Notify the server that job execution has started.
pub(crate) async fn start_job(
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
pub(crate) async fn upload_log(
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

pub(crate) async fn download_workspace(
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
        .join("forgekeep-runner")
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

pub(crate) async fn restore_cache(
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn save_cache(
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
pub(crate) async fn finish_job(
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
