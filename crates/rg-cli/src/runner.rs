//! `forgekeep runner` subcommand: poll the server for CI jobs and execute them
//! locally or inside a Docker sandbox.

use anyhow::Context;

/// Connect timeout (TCP + TLS handshake only) for the CLI runner's HTTP client.
///
/// The client sets **only** this — deliberately NOT a global request
/// `.timeout(...)`: the main loop long-polls `/jobs/poll?timeout=30`, which a
/// whole-request timeout would abort. Bounding just the handshake still stops a
/// dead/hung `--server` from pinning the runner on connect forever. The short
/// control-plane calls (register / start / log / finish) layer their own
/// per-request [`RUNNER_REQUEST_TIMEOUT`] on top; the long-poll is left uncapped.
const RUNNER_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Per-request timeout for the short control-plane calls (everything except the
/// long-poll), so a server that completes the handshake but then hangs the
/// response can't stall registration or the post-job bookkeeping forever.
const RUNNER_REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Run as a CI runner: register (if needed), then poll and execute jobs forever.
pub(crate) async fn cmd_runner(
    server: String,
    name: Option<String>,
    runner_id: Option<i64>,
    token: Option<String>,
    auth_token: Option<String>,
) -> anyhow::Result<()> {
    use reqwest::header;

    let client = reqwest::Client::builder()
        .connect_timeout(RUNNER_CONNECT_TIMEOUT)
        .build()
        .context("failed to build runner HTTP client")?;

    // ── Register or use existing credentials ─────────
    let (runner_id, token) = match (runner_id, token) {
        (Some(rid), Some(tok)) => (rid, tok),
        _ => {
            // Register new runner
            let name = name.as_deref().unwrap_or("default-runner");
            let auth_token = auth_token
                .or_else(|| {
                    rg_core::env_compat::env_var_compat(
                        "FORGEKEEP_AUTH_TOKEN",
                        "IRONFORGE_AUTH_TOKEN",
                    )
                })
                .context(
                    "runner auto-registration requires --auth-token or \
                     FORGEKEEP_AUTH_TOKEN; alternatively pass --runner-id and --token",
                )?;
            let resp: serde_json::Value = client
                .post(format!("{}/api/v1/runners/register", server))
                .timeout(RUNNER_REQUEST_TIMEOUT)
                .bearer_auth(auth_token)
                .json(&serde_json::json!({"name": name}))
                .send()
                .await
                .context("failed to register runner")?
                .json()
                .await?;
            let rid = resp["id"]
                .as_i64()
                .ok_or_else(|| anyhow::anyhow!("invalid register response: missing 'id'"))?;
            let tok = resp["token"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("invalid register response: missing 'token'"))?
                .to_string();
            eprintln!("Runner registered: id={}, token={}", rid, tok);
            eprintln!("Save these credentials for future runs!");
            (rid, tok)
        }
    };

    eprintln!("Runner started: server={}, id={}", server, runner_id);

    // ── Main poll loop ─────────────────────────────
    let auth_header = format!("Bearer {}", token);
    loop {
        // 1. Poll for job
        let poll_resp = client
            .get(format!(
                "{}/api/v1/runners/{}/jobs/poll?timeout=30",
                server, runner_id
            ))
            .header(header::AUTHORIZATION, &auth_header)
            .send()
            .await;

        let job: serde_json::Value = match poll_resp {
            Ok(r) if r.status() == reqwest::StatusCode::NO_CONTENT => {
                continue;
            }
            Ok(r) => r.json().await?,
            Err(e) => {
                eprintln!("Poll error: {}, retrying in 5s", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let job_id = job["job_id"]
            .as_i64()
            .ok_or_else(|| anyhow::anyhow!("invalid poll response"))?;
        let script: Vec<&str> = job["script"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let image = job["image"].as_str();

        eprintln!("Got job {}: {}", job_id, job["name"].as_str().unwrap_or(""));

        // 2. Start job
        let _ = client
            .post(format!(
                "{}/api/v1/runners/{}/jobs/{}/start",
                server, runner_id, job_id
            ))
            .timeout(RUNNER_REQUEST_TIMEOUT)
            .header(header::AUTHORIZATION, &auth_header)
            .send()
            .await;

        // 3. Execute job
        let script_str = script.join("\n");
        let (exit_code, log) = if let Some(img) = image {
            run_job_docker(img, &script_str).await
        } else {
            run_job_local(&script_str).await
        };

        // 4. Upload log
        let _ = client
            .post(format!(
                "{}/api/v1/runners/{}/jobs/{}/log",
                server, runner_id, job_id
            ))
            .timeout(RUNNER_REQUEST_TIMEOUT)
            .header(header::AUTHORIZATION, &auth_header)
            .body(log.clone())
            .send()
            .await;

        // 5. Finish job
        let status = if exit_code == 0 { "success" } else { "failure" };
        let _ = client
            .post(format!(
                "{}/api/v1/runners/{}/jobs/{}/finish",
                server, runner_id, job_id
            ))
            .timeout(RUNNER_REQUEST_TIMEOUT)
            .header(header::AUTHORIZATION, &auth_header)
            .json(&serde_json::json!({"status": status, "exit_code": exit_code}))
            .send()
            .await;

        eprintln!(
            "Job {} finished: status={}, exit_code={}",
            job_id, status, exit_code
        );
    }
}

/// Execute a job script locally via platform-appropriate shell.
async fn run_job_local(script: &str) -> (i32, String) {
    #[cfg(unix)]
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .await;

    #[cfg(windows)]
    let output = tokio::process::Command::new("powershell.exe")
        .args(&["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .await;

    match output {
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let mut log = String::new();
            if !o.stdout.is_empty() {
                log.push_str(&String::from_utf8_lossy(&o.stdout));
            }
            if !o.stderr.is_empty() {
                if !log.is_empty() {
                    log.push('\n');
                }
                log.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            (code, log)
        }
        Err(e) => (-1, format!("Failed to spawn job: {}", e)),
    }
}

/// Hard cap on the number of processes a job container may spawn (fork-bomb guard).
const DOCKER_PIDS_LIMIT: &str = "512";
/// Default memory ceiling for a job container.
const DOCKER_MEMORY_LIMIT: &str = "2g";
/// Default CPU quota for a job container.
const DOCKER_CPU_LIMIT: &str = "2";

/// Message returned when a job declared a Docker `image:` but the daemon is
/// unavailable. Mirrors `rg-runner`'s wording so both runners fail identically.
fn docker_unavailable_message(image: &str) -> String {
    format!(
        "Docker daemon not available. Job requires image '{}' but cannot run in container. \
         Refusing to fall back to local execution.",
        image
    )
}

/// Execute a job script inside a Docker container.
///
/// SECURITY (CWE-269 privilege escalation): a job that declares `image:` expects
/// to run inside a sandbox. If the Docker daemon is unavailable we FAIL the job
/// instead of silently executing its script on the runner host with `sh -c` — the
/// same fail-closed contract the internal `PipelineRunner` and `rg-runner` enforce.
/// The container is confined with `--cap-drop=ALL` (strip all Linux capabilities),
/// `--security-opt=no-new-privileges` (block setuid escalation) and `--pids-limit`
/// / `--memory` / `--cpus` (resource-exhaustion guards). The Docker socket is never
/// mounted and `--privileged` is never passed, so a malicious job has no path to the
/// host daemon or devices.
async fn run_job_docker(image: &str, script: &str) -> (i32, String) {
    // Check if the Docker daemon is running. Fail closed if it is not, rather
    // than silently dropping to host execution of a job that expected a sandbox.
    let docker_ok = tokio::process::Command::new("docker")
        .arg("info")
        .output()
        .await
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !docker_ok {
        let msg = docker_unavailable_message(image);
        eprintln!("{}", msg);
        return (-1, msg);
    }

    let output = tokio::process::Command::new("docker")
        .args([
            "run",
            "--rm",
            "--cap-drop",
            "ALL",
            "--security-opt",
            "no-new-privileges",
            "--pids-limit",
            DOCKER_PIDS_LIMIT,
            "--memory",
            DOCKER_MEMORY_LIMIT,
            "--cpus",
            DOCKER_CPU_LIMIT,
            image,
            "sh",
            "-c",
            script,
        ])
        .output()
        .await;

    match output {
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let mut log = String::new();
            if !o.stdout.is_empty() {
                log.push_str(&String::from_utf8_lossy(&o.stdout));
            }
            if !o.stderr.is_empty() {
                if !log.is_empty() {
                    log.push('\n');
                }
                log.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            if code != 0 && log.is_empty() {
                log = format!("Docker exited with code {}", code);
            }
            (code, log)
        }
        Err(e) => (-1, format!("Failed to run docker: {}", e)),
    }
}

#[cfg(test)]
mod runner_docker_tests {
    use super::docker_unavailable_message;

    #[test]
    fn docker_unavailable_message_is_fail_closed() {
        // A job that declared an image must never silently drop to host `sh -c`.
        let msg = docker_unavailable_message("alpine:3.20");
        assert!(msg.contains("alpine:3.20"));
        assert!(msg.contains("Refusing to fall back to local execution"));
    }
}
