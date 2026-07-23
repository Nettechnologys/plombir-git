//! Local and Docker job execution, cache-key resolution, and variable assembly.

use anyhow::Result;

use crate::api::PollJobResponse;

pub(crate) fn resolved_cache(
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

/// Execute a job script locally via platform-appropriate shell.
pub(crate) async fn run_job_local(
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

/// Hard cap on the number of processes a job container may spawn (fork-bomb guard).
const DOCKER_PIDS_LIMIT: &str = "512";
/// Default memory ceiling for a job container.
const DOCKER_MEMORY_LIMIT: &str = "2g";
/// Default CPU quota for a job container.
const DOCKER_CPU_LIMIT: &str = "2";

/// Execute a job script inside a Docker container.
pub(crate) async fn run_job_docker(
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

    let container_name = format!("forgekeep-runner-job-{job_id}");
    let args = docker_run_args(image, script, variables, workspace, &container_name);

    let mut command = tokio::process::Command::new("docker");
    command.args(&args);
    // Only the variable name is passed on the command line above; the value is
    // inherited from the Docker CLI environment so secrets are not exposed in the
    // host process arguments.
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

/// Build the `docker run` argument vector for a job container.
///
/// SECURITY (CWE-269 privilege escalation): the container is confined so a
/// malicious job cannot break out onto the runner host. Aligned with the
/// internal rg-ci `PipelineRunner`:
/// - `--cap-drop=ALL` strips every Linux capability (no raw sockets, no mount…).
/// - `--security-opt=no-new-privileges` blocks setuid/gain-privilege via execve.
/// - `--pids-limit` / `--memory` / `--cpus` bound resource exhaustion (fork bomb, OOM).
///
/// The Docker socket is deliberately NOT mounted and `--privileged` is never
/// passed, so the job has no path to the daemon or host devices. Only variable
/// *names* are placed on the command line; values are inherited from the CLI
/// environment so secrets never appear in the host process arguments.
fn docker_run_args(
    image: &str,
    script: &str,
    variables: &[(String, String)],
    workspace: &std::path::Path,
    container_name: &str,
) -> Vec<String> {
    let mut args = vec![
        "run".to_string(),
        "--rm".to_string(),
        "--name".to_string(),
        container_name.to_string(),
        "--cap-drop".to_string(),
        "ALL".to_string(),
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
        "--pids-limit".to_string(),
        DOCKER_PIDS_LIMIT.to_string(),
        "--memory".to_string(),
        DOCKER_MEMORY_LIMIT.to_string(),
        "--cpus".to_string(),
        DOCKER_CPU_LIMIT.to_string(),
        "-v".to_string(),
        format!("{}:/workspace", workspace.to_string_lossy()),
        "-w".to_string(),
        "/workspace".to_string(),
    ];
    for (key, _) in variables {
        args.push("-e".to_string());
        args.push(key.clone());
    }
    args.push(image.to_string());
    args.push("sh".to_string());
    args.push("-c".to_string());
    args.push(script.to_string());
    args
}

fn docker_unavailable_message(image: &str) -> String {
    format!(
        "Docker daemon not available. Job requires image '{}' but cannot run in container. \
         Refusing to fall back to local execution.",
        image
    )
}

pub(crate) fn job_variables(value: Option<&serde_json::Value>) -> Vec<(String, String)> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_unavailable_message_is_fail_closed() {
        let msg = docker_unavailable_message("alpine:3.20");

        assert!(msg.contains("alpine:3.20"));
        assert!(msg.contains("Refusing to fall back to local execution"));
    }

    #[test]
    fn docker_run_args_apply_sandbox_hardening() {
        let variables = vec![("CI_JOB_TOKEN".to_string(), "secret".to_string())];
        let args = docker_run_args(
            "alpine:3.20",
            "echo hi",
            &variables,
            std::path::Path::new("/workspace/repo"),
            "forgekeep-runner-job-7",
        );

        // Defense-in-depth flags aligned with rg-ci PipelineRunner.
        let window = |flag: &str, value: &str| {
            args.windows(2)
                .any(|w| w[0] == flag && w[1] == value)
        };
        assert!(window("--cap-drop", "ALL"), "missing --cap-drop=ALL: {args:?}");
        assert!(
            window("--security-opt", "no-new-privileges"),
            "missing no-new-privileges: {args:?}"
        );
        assert!(window("--pids-limit", DOCKER_PIDS_LIMIT), "missing --pids-limit");
        assert!(window("--memory", DOCKER_MEMORY_LIMIT), "missing --memory");
        assert!(window("--cpus", DOCKER_CPU_LIMIT), "missing --cpus");

        // Never grant a path to the host daemon / devices.
        assert!(!args.iter().any(|a| a == "--privileged"), "--privileged leaked in");
        assert!(
            !args.iter().any(|a| a.contains("docker.sock")),
            "docker socket mounted"
        );

        // Secret values must not appear on the command line — only the name.
        assert!(args.iter().any(|a| a == "CI_JOB_TOKEN"));
        assert!(!args.iter().any(|a| a == "secret"), "secret value leaked into argv");

        // Image and script still terminate the invocation.
        assert_eq!(&args[args.len() - 4..], &["alpine:3.20", "sh", "-c", "echo hi"]);
    }

    #[tokio::test]
    async fn local_executor_injects_polled_variables_with_a_clean_environment() {
        let variables = vec![("RUNNER_MESSAGE".into(), "hello".into())];
        let (code, log) = run_job_local(
            "test \"$RUNNER_MESSAGE\" = hello && test -z \"$FORGEKEEP_HOST_SECRET\" && echo ok",
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
