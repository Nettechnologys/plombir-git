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

/// The artifact this job publishes, checked against what the server will
/// accept before the archive is packed.
///
/// Mirrors [`resolved_cache`]: the server validated the same rules when the
/// pipeline was created, and this runner re-states them because it is the party
/// that walks the workspace — a path that escapes it would pack files the job
/// was never given.
pub(crate) fn resolved_artifacts(job: &PollJobResponse) -> Result<Option<(String, Vec<String>)>> {
    let (Some(name), Some(paths)) = (&job.artifact_name, &job.artifact_paths) else {
        return Ok(None);
    };
    if name.is_empty() || name.len() > 100 {
        anyhow::bail!("artifact name must contain 1-100 characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
    {
        anyhow::bail!("artifact name must use only ASCII letters, digits, '.', '-' and '_'");
    }
    if paths.is_empty() || paths.len() > 64 {
        anyhow::bail!("artifacts requires 1-64 paths");
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
            anyhow::bail!("artifact path must stay within workspace: {path}");
        }
    }
    Ok(Some((name.clone(), paths.clone())))
}

/// Pack the declared paths, resolved inside `workspace`, into a `tar` at
/// `archive`.
///
/// A declaration that matched no file at all fails instead of producing an
/// empty archive: publishing one would show an artifact on the pipeline whose
/// emptiness is only discovered by whoever unpacks it.
pub(crate) fn pack_artifact(
    workspace: &std::path::Path,
    paths: &[String],
    archive: &std::path::Path,
) -> Result<()> {
    use anyhow::Context;

    let file = std::fs::File::create(archive).with_context(|| {
        format!(
            "failed to create the artifact archive `{}`",
            archive.display()
        )
    })?;
    let mut builder = rg_process::workspace_archive::WorkspaceArchive::new(workspace, file)?;
    let mut packed = 0usize;
    for path in paths {
        if builder
            .append_declared(path)
            .with_context(|| format!("failed to pack `{path}`"))?
        {
            packed += 1;
        }
    }
    builder
        .finish()
        .context("failed to finalize the artifact archive")?;
    if packed == 0 {
        anyhow::bail!(
            "none of the declared artifact paths exist in the workspace: {}",
            paths.join(", ")
        );
    }
    Ok(())
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
            .env_clear();
        for (key, value) in variables {
            command.env(key, value);
        }
        rg_process::output_in_process_tree(&mut command).await
    };

    #[cfg(windows)]
    let output = {
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .args(&["-NoProfile", "-NonInteractive", "-Command", script])
            .current_dir(workspace)
            .env_clear();
        for (key, value) in variables {
            command.env(key, value);
        }
        rg_process::output_in_process_tree(&mut command).await
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

/// Name of the Docker container a job runs in.
///
/// Written down once because three places have to agree on it: this executor,
/// which starts the container; the job-timeout branch, which kills a container
/// that outran its deadline; and the stop path, which removes the container of a
/// job interrupted mid-flight. The last two spelt the name out inline.
pub(crate) fn job_container_name(job_id: i64) -> String {
    format!("plombir-git-runner-job-{job_id}")
}

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

    let container_name = job_container_name(job_id);
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
            "plombir-git-runner-job-7",
        );

        // Defense-in-depth flags aligned with rg-ci PipelineRunner.
        let window =
            |flag: &str, value: &str| args.windows(2).any(|w| w[0] == flag && w[1] == value);
        assert!(
            window("--cap-drop", "ALL"),
            "missing --cap-drop=ALL: {args:?}"
        );
        assert!(
            window("--security-opt", "no-new-privileges"),
            "missing no-new-privileges: {args:?}"
        );
        assert!(
            window("--pids-limit", DOCKER_PIDS_LIMIT),
            "missing --pids-limit"
        );
        assert!(window("--memory", DOCKER_MEMORY_LIMIT), "missing --memory");
        assert!(window("--cpus", DOCKER_CPU_LIMIT), "missing --cpus");

        // Never grant a path to the host daemon / devices.
        assert!(
            !args.iter().any(|a| a == "--privileged"),
            "--privileged leaked in"
        );
        assert!(
            !args.iter().any(|a| a.contains("docker.sock")),
            "docker socket mounted"
        );

        // Secret values must not appear on the command line — only the name.
        assert!(args.iter().any(|a| a == "CI_JOB_TOKEN"));
        assert!(
            !args.iter().any(|a| a == "secret"),
            "secret value leaked into argv"
        );

        // Image and script still terminate the invocation.
        assert_eq!(
            &args[args.len() - 4..],
            &["alpine:3.20", "sh", "-c", "echo hi"]
        );
    }

    #[tokio::test]
    async fn local_executor_injects_polled_variables_with_a_clean_environment() {
        let variables = vec![("RUNNER_MESSAGE".into(), "hello".into())];
        let (code, log) = run_job_local(
            "test \"$RUNNER_MESSAGE\" = hello && test -z \"$PLOMBIR_GIT_HOST_SECRET\" && echo ok",
            &variables,
            std::path::Path::new("."),
        )
        .await;
        assert_eq!(code, 0, "{log}");
        assert!(log.contains("ok"));
    }

    #[cfg(unix)]
    fn process_is_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[cfg(unix)]
    async fn assert_process_stops(pid: u32, description: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while process_is_alive(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "{description} process {pid} survived the job timeout"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    fn recorded_pid(path: &std::path::Path) -> u32 {
        std::fs::read_to_string(path)
            .unwrap_or_else(|error| panic!("fixture did not record {}: {error}", path.display()))
            .trim()
            .parse()
            .unwrap_or_else(|error| {
                panic!("fixture recorded an invalid {}: {error}", path.display())
            })
    }

    #[cfg(unix)]
    struct RecordedPidCleanup(Vec<std::path::PathBuf>);

    #[cfg(unix)]
    impl Drop for RecordedPidCleanup {
        fn drop(&mut self) {
            for path in &self.0 {
                let Ok(pid) = std::fs::read_to_string(path) else {
                    continue;
                };
                drop(
                    std::process::Command::new("kill")
                        .args(["-KILL", pid.trim()])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status(),
                );
            }
        }
    }

    /// The polled runner's production local executor is canceled by the outer
    /// deadline in `run_jobs_until_shutdown`. Dropping that future must stop the
    /// direct shell and work it already forked, not merely return a timeout row.
    #[cfg(unix)]
    #[tokio::test]
    async fn local_executor_cancellation_kills_the_shell_and_its_descendant() {
        let temp = tempfile::tempdir().unwrap();
        let shell_pid_file = temp.path().join("runner-shell.pid");
        let child_pid_file = temp.path().join("runner-child.pid");
        let _pid_cleanup = RecordedPidCleanup(vec![shell_pid_file.clone(), child_pid_file.clone()]);
        let script = format!(
            "printf '%s\\n' \"$$\" > '{}'; sleep 30 & child=$!; \
             printf '%s\\n' \"$child\" > '{}'; wait \"$child\"",
            shell_pid_file.display(),
            child_pid_file.display()
        );

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            run_job_local(&script, &job_variables(None), temp.path()),
        )
        .await;
        assert!(
            result.is_err(),
            "fixture unexpectedly finished before timeout"
        );

        assert_process_stops(recorded_pid(&shell_pid_file), "external runner shell").await;
        assert_process_stops(
            recorded_pid(&child_pid_file),
            "external runner shell descendant",
        )
        .await;
    }

    /// The other half of the wire contract `rg-http`'s
    /// `a_runner_publishes_the_artifact_its_job_declared` asserts on the server
    /// side: this runner reads exactly the two field names the poll body
    /// carries. Deserialized from a body rather than built as a struct — a
    /// renamed field would still compile as a struct literal and would still
    /// arrive as `None` from a real server.
    #[test]
    fn a_polled_job_carries_the_artifact_its_workflow_declared() {
        let body = r#"{
            "job_id": 7,
            "name": "build",
            "script": ["echo ok"],
            "image": null,
            "variables": null,
            "cache_key": null,
            "cache_paths": null,
            "artifact_name": "build-report",
            "artifact_paths": ["out/report.txt"],
            "timeout": 60
        }"#;
        let job: PollJobResponse =
            serde_json::from_str(body).expect("a poll body this runner is handed");
        let (name, paths) = resolved_artifacts(&job)
            .expect("a declaration the server validated is one this runner accepts")
            .expect("a job that declares an artifact must resolve to one");
        assert_eq!(name, "build-report");
        assert_eq!(paths, vec!["out/report.txt".to_string()]);

        let escaping = PollJobResponse {
            artifact_paths: Some(vec!["../outside".into()]),
            ..job
        };
        assert!(
            resolved_artifacts(&escaping).is_err(),
            "a path that leaves the workspace would pack files this job was never given"
        );
    }

    /// Packing is what turns declared paths into the archive the server stores,
    /// and a declaration that matched nothing has to fail rather than produce
    /// an empty archive somebody downloads before finding out.
    #[test]
    fn packing_carries_the_declared_paths_and_refuses_an_empty_match() {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(workspace.path().join("out")).unwrap();
        std::fs::write(workspace.path().join("out/report.txt"), b"artifact-bytes").unwrap();
        let archive = workspace.path().join("packed.tar");

        pack_artifact(workspace.path(), &["out/report.txt".to_string()], &archive)
            .expect("a declared path that exists must pack");
        let mut unpacked = tar::Archive::new(std::fs::File::open(&archive).unwrap());
        let entries: Vec<String> = unpacked
            .entries()
            .unwrap()
            .map(|entry| entry.unwrap().path().unwrap().display().to_string())
            .collect();
        assert_eq!(entries, vec!["out/report.txt".to_string()]);

        let error = pack_artifact(
            workspace.path(),
            &["out/never-built.txt".to_string()],
            &workspace.path().join("empty.tar"),
        )
        .expect_err("an archive with nothing in it must not pass for a published artifact");
        assert!(
            format!("{error:#}").contains("none of the declared artifact paths exist"),
            "{error:#}"
        );
    }

    /// The runner host packs what a container left in the workspace, and the
    /// host can read its own `runner.toml`. A symlink out of the workspace —
    /// relative as committed, absolute as a job creates it — must reach the
    /// artifact as a link or as a refusal, never as the target's bytes
    /// (card_79b1c2a906ec).
    #[cfg(unix)]
    #[test]
    fn packing_an_artifact_never_follows_a_symlink_out_of_the_workspace() {
        use std::os::unix::fs::symlink;
        const SECRET: &[u8] = b"runner-host-token-secret";

        let root = tempfile::tempdir().unwrap();
        // Archives land outside the tree under test: one written inside a
        // directory a followed link walks would grow by reading itself.
        let output = tempfile::tempdir().unwrap();
        let workspace = root.path().join("job-7");
        std::fs::create_dir_all(workspace.join("out")).unwrap();
        std::fs::write(root.path().join("runner.toml"), SECRET).unwrap();
        symlink("../../runner.toml", workspace.join("out/relative")).unwrap();
        symlink(
            root.path().join("runner.toml"),
            workspace.join("out/absolute"),
        )
        .unwrap();
        symlink("../runner.toml", workspace.join("leak-relative")).unwrap();
        symlink(
            root.path().join("runner.toml"),
            workspace.join("leak-absolute"),
        )
        .unwrap();
        let carries_secret = |archive: &std::path::Path| {
            let bytes = std::fs::read(archive).unwrap();
            bytes.windows(SECRET.len()).any(|window| window == SECRET)
        };

        let archive = output.path().join("out.tar");
        pack_artifact(&workspace, &["out".to_string()], &archive).expect("out must pack");
        assert!(
            !carries_secret(&archive),
            "the artifact carries a file a symlink inside `out` points at"
        );

        for declared in ["leak-relative", "leak-absolute"] {
            let archive = output.path().join(format!("{declared}.tar"));
            let error = pack_artifact(&workspace, &[declared.to_string()], &archive)
                .expect_err("a declared symlink out of the workspace must be refused");
            assert!(
                format!("{error:#}").contains("leaves the CI workspace"),
                "{declared}: {error:#}"
            );
            assert!(
                !archive.exists() || !carries_secret(&archive),
                "the artifact for `{declared}` carries the target's bytes"
            );
        }
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
            artifact_name: None,
            artifact_paths: None,
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
