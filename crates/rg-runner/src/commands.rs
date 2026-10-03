//! Subcommand handlers: `register` (one-shot registration) and `run` (register
//! if needed, then poll-and-execute loop).

use anyhow::{Context, Result};

use crate::api::{
    deregister_runner, download_workspace, finish_job, poll_job, publish_artifact, register_runner,
    restore_cache, save_cache, send_heartbeat, stage_artifact, start_job, upload_log,
};
use crate::config::{
    config_not_persisted_warning, load_config, resolve_auth_token, resolve_runner, save_config,
    ResolvedRunner, RunnerCliArgs, RunnerIdentity,
};
use crate::executor::{
    job_container_name, job_variables, pack_artifact, resolved_artifacts, resolved_cache,
    run_job_docker, run_job_local,
};
use crate::workspace::{
    job_artifact_path, sweep_stale_job_entries, MAX_EXTERNAL_JOB_TIMEOUT_SECS, STALE_JOB_ENTRY_AGE,
};

/// Connect timeout (TCP + TLS handshake only) for the runner's HTTP client.
///
/// Mirrors `rg_core::net`'s outbound connect timeout so a dead/hung server can't
/// pin registration or the heartbeat task on the connect phase forever.
const RUNNER_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Keep runner credentials on the exact origin of each initiating request.
/// Reqwest's built-in sensitive-header stripping ignores scheme-only changes,
/// so an explicit policy is the credential boundary for the shared client.
fn same_origin_redirect_policy() -> reqwest::redirect::Policy {
    let default = reqwest::redirect::Policy::default();
    reqwest::redirect::Policy::custom(move |attempt| {
        let stays_on_origin = attempt.previous().first().is_some_and(|initial| {
            initial.scheme() == attempt.url().scheme()
                && initial.host_str() == attempt.url().host_str()
                && initial.port_or_known_default() == attempt.url().port_or_known_default()
        });
        if stays_on_origin {
            default.redirect(attempt)
        } else {
            attempt.stop()
        }
    })
}

/// Bounds of the job deadline the server may hand out, mirroring
/// `rg_core::ci::JOB_TIMEOUT_{MIN,MAX}_SECS`. Duplicated rather than imported:
/// the agent is a standalone binary that talks to the server over HTTP only and
/// deliberately does not link the server's crates.
const POLLED_TIMEOUT_MIN_SECS: i64 = 1;

/// Fallback deadline for a `timeout` field the server should never have sent.
const POLLED_TIMEOUT_FALLBACK_SECS: u64 = 3600;

/// Turn the polled `timeout` field into the deadline this job runs under.
///
/// The server resolves and range-checks the value before it goes on the wire,
/// so the fallback is a guard against a server that is broken or not the version
/// this agent expects — and it says so, instead of quietly running the job for a
/// different length of time than the pipeline asked for.
fn resolve_polled_timeout(job_id: i64, polled: i64) -> u64 {
    if (POLLED_TIMEOUT_MIN_SECS..=MAX_EXTERNAL_JOB_TIMEOUT_SECS).contains(&polled) {
        u64::try_from(polled).unwrap_or(POLLED_TIMEOUT_FALLBACK_SECS)
    } else {
        tracing::warn!(
            job_id,
            polled_timeout_seconds = polled,
            effective_timeout_seconds = POLLED_TIMEOUT_FALLBACK_SECS,
            "server sent a job timeout outside {POLLED_TIMEOUT_MIN_SECS}..={MAX_EXTERNAL_JOB_TIMEOUT_SECS} seconds; using the agent default"
        );
        POLLED_TIMEOUT_FALLBACK_SECS
    }
}

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
        .redirect(same_origin_redirect_policy())
        .build()
        .expect("failed to build runner HTTP client: no native TLS backend available")
}

fn is_loopback_host(url: &reqwest::Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    let literal = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    literal
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| match ip {
            std::net::IpAddr::V4(ip) => ip.is_loopback(),
            std::net::IpAddr::V6(ip) => {
                ip.is_loopback()
                    || ip
                        .to_ipv4_mapped()
                        .is_some_and(|mapped| mapped.is_loopback())
            }
        })
}

fn require_confidential_runner_server(server: &str, allow_insecure_http: bool) -> Result<()> {
    let url = reqwest::Url::parse(server)
        .with_context(|| "runner server must be an absolute http(s) URL")?;
    if url.host_str().is_none() {
        anyhow::bail!("runner server URL must name a host");
    }
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("runner server URL must not contain user information");
    }

    match url.scheme() {
        "https" => Ok(()),
        "http" if is_loopback_host(&url) => Ok(()),
        "http" if allow_insecure_http => {
            tracing::warn!(
                "allow_insecure_http is enabled: runner credentials may cross plaintext HTTP"
            );
            Ok(())
        }
        "http" => anyhow::bail!(
            "runner server uses plaintext HTTP on a non-loopback host; use HTTPS, keep local development on localhost/loopback, or explicitly set --allow-insecure-http / allow_insecure_http = true"
        ),
        scheme => anyhow::bail!("runner server URL must use http or https, not {scheme}"),
    }
}

/// Handle `plombir-git-runner register`: register a runner and optionally persist
/// its token to the config file.
///
/// The config file is read first, and every setting resolves `CLI arg > config
/// file > built-in default` through the same [`resolve_runner`] that `run` uses.
/// Before that this command ignored `runner.toml` entirely: `--server` carried a
/// clap default, so a runner whose config already pointed at a remote server
/// registered against `127.0.0.1:8080` instead — and with `--save` that localhost
/// was then written back over the operator's own value, taking `name` and
/// `labels` with it.
pub struct RegisterCommand {
    pub server: Option<String>,
    pub allow_insecure_http: bool,
    pub repository: Option<String>,
    pub name: Option<String>,
    /// Legacy comma-separated input kept for CLI compatibility.
    pub labels: Option<String>,
    /// Structural `--label` values, one element per occurrence.
    pub label: Vec<String>,
    pub save: bool,
    pub auth_token: Option<String>,
    pub config: String,
}

pub async fn cmd_register(command: RegisterCommand) -> Result<()> {
    let RegisterCommand {
        server,
        allow_insecure_http,
        repository,
        name,
        labels,
        label,
        save,
        auth_token,
        config,
    } = command;
    // Same read-or-fail contract as `run`: a missing file is the legitimate
    // "no config yet", anything unreadable aborts rather than quietly becoming
    // "no config" and registering against the wrong server.
    let cfg = load_config(&config)?;
    // `register` mints a fresh identity by definition, so any `runner_id` /
    // `token` already in the file is deliberately not fed into the resolution —
    // only `server`, `name` and `labels` are.
    let ResolvedRunner {
        server,
        allow_insecure_http,
        repository,
        name,
        labels: labels_vec,
        ..
    } = resolve_runner(
        RunnerCliArgs {
            server,
            allow_insecure_http,
            repository,
            name,
            labels,
            label,
            token: None,
            runner_id: None,
        },
        cfg.as_ref(),
    )?;

    let repository = repository.context(
        "runner registration requires --repository owner/repo or a repository key in runner.toml",
    )?;

    let auth_token = resolve_auth_token(auth_token)
        .context("runner registration requires --auth-token or PLOMBIR_GIT_AUTH_TOKEN")?;
    require_confidential_runner_server(&server, allow_insecure_http)?;
    let client = build_runner_client();

    println!(
        "Registering runner '{}' for {} with {}...",
        name, repository, server
    );
    let (runner_id, token) = register_runner(
        &client,
        &server,
        &repository,
        &name,
        &labels_vec,
        &auth_token,
    )
    .await?;
    println!("Runner registered successfully!");
    println!("  ID:    {}", runner_id);
    println!("  Token: {}", token);

    if save {
        // Merge onto what the file already holds instead of writing a fresh
        // `RunnerConfig`: a wholesale write drops every key this invocation did
        // not name, so `register --save --name x` used to erase the operator's
        // `server` and `labels`.
        let mut saved = cfg.unwrap_or_default();
        saved.server = Some(server);
        saved.allow_insecure_http = Some(allow_insecure_http);
        saved.runner_id = Some(runner_id);
        saved.token = Some(token.clone());
        saved.repository = Some(repository);
        saved.name = Some(name);
        saved.labels = Some(labels_vec);
        // `--config`, not a hardcoded `~/.plombir-git/runner.toml`: `run` reads the
        // path it was given, so writing the identity anywhere else means `run`
        // never finds it and registers yet another runner on every start.
        save_config(&config, &saved)?;
        println!("  Config saved to {}", config);
    }

    Ok(())
}

/// Handle `plombir-git-runner run`: resolve/register the runner, then run the
/// poll-and-execute loop until this process is asked to stop.
/// Pack and publish the artifact this job declared, if it declared one.
///
/// Returns the line to append to the job log when something went wrong, and
/// `None` when there was nothing to publish or the publication succeeded. The
/// job's own result is never touched: the script has already run.
async fn publish_job_artifact(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    token: &str,
    job: &crate::api::PollJobResponse,
    workspace: &std::path::Path,
) -> Option<String> {
    let (name, paths) = match resolved_artifacts(job) {
        Ok(Some(spec)) => spec,
        Ok(None) => return None,
        Err(error) => {
            tracing::warn!(
                job_id = job.job_id,
                error = %format!("{error:#}"),
                "invalid artifact configuration; nothing was published"
            );
            return Some(format!(
                "CI artifact was not published; job remains successful: {error:#}"
            ));
        }
    };

    // Packed beside the workspace rather than inside it: an archive written
    // into the very tree it is packing races the walk that is reading it.
    let archive = job_artifact_path(workspace);
    let pack_workspace = workspace.to_path_buf();
    let pack_archive = archive.clone();
    let packed =
        tokio::task::spawn_blocking(move || pack_artifact(&pack_workspace, &paths, &pack_archive))
            .await;

    let outcome = match packed {
        Ok(Ok(())) => {
            match stage_artifact(client, server, runner_id, job.job_id, token, &archive).await {
                Ok(staged) => {
                    publish_artifact(client, server, runner_id, job.job_id, token, &name, &staged)
                        .await
                }
                Err(error) => Err(error),
            }
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(anyhow::anyhow!("packing the artifact panicked: {error}")),
    };
    if let Err(error) = tokio::fs::remove_file(&archive).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                job_id = job.job_id,
                path = %archive.display(),
                %error,
                "failed to remove the packed artifact archive"
            );
        }
    }

    match outcome {
        Ok(()) => None,
        Err(error) => {
            tracing::warn!(
                job_id = job.job_id,
                artifact = %name,
                error = %format!("{error:#}"),
                "artifact publication failed; job remains successful"
            );
            Some(format!(
                "CI artifact '{name}' was not published; job remains successful: {error:#}"
            ))
        }
    }
}

/// Inputs to the long-running runner command.
///
/// Keep the command boundary typed: all fields originate in Clap, but both the
/// dedicated binary and the deprecated `plombir-git runner` alias call it.
pub struct RunCommand {
    pub server: Option<String>,
    pub allow_insecure_http: bool,
    pub repository: Option<String>,
    pub name: Option<String>,
    /// Legacy comma-separated input kept for CLI compatibility.
    pub labels: Option<String>,
    /// Structural `--label` values, one element per occurrence.
    pub label: Vec<String>,
    pub token: Option<String>,
    pub runner_id: Option<i64>,
    pub auth_token: Option<String>,
    pub config: String,
}

pub async fn cmd_run(command: RunCommand) -> Result<()> {
    let RunCommand {
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
    } = command;
    // Resolve config: CLI args > config file > defaults.
    //
    // A missing file is fine (`None` — fall back to flags and auto-registration),
    // but an unreadable or malformed one aborts the start instead of silently
    // degrading to "no config": continuing would re-register the runner under a
    // fresh identity and leave the operator's file quietly ignored.
    let cfg = load_config(&config)?;
    let ResolvedRunner {
        server: server_url,
        state_permissions,
        allow_insecure_http,
        identity,
        repository: resolved_repository,
        name: resolved_name,
        labels: resolved_labels,
    } = resolve_runner(
        RunnerCliArgs {
            server,
            allow_insecure_http,
            repository,
            name,
            labels,
            label,
            token,
            runner_id,
        },
        cfg.as_ref(),
    )?;
    // Install before the startup sweep: cache archives, workspaces, artifact
    // spools and every child job process inherit this process-wide policy.
    state_permissions.install();
    tracing::info!(
        state_permissions = %state_permissions,
        umask = %format!("{:04o}", state_permissions.umask()),
        regular_file_mode = %format!("{:04o}", state_permissions.regular_file_mode()),
        directory_mode = %format!("{:04o}", state_permissions.directory_mode()),
        "Installed the process-wide creation policy for runner-owned state"
    );
    let resolved_server = server_url.as_str();
    require_confidential_runner_server(resolved_server, allow_insecure_http)?;

    // A job normally retires its checkout and transfer archives on every path
    // this process survives. A previous SIGKILL, OOM kill or container restart
    // ran none of those cleanups, so reclaim entries old enough that no valid
    // externally-run job can still own them before this process takes work.
    let sweep = sweep_stale_job_entries(STALE_JOB_ENTRY_AGE).await;
    if sweep != crate::workspace::SweepReport::default() {
        tracing::info!(
            removed = sweep.removed,
            retained = sweep.retained,
            failed = sweep.failed,
            "swept runner job files left behind by a previous run"
        );
    }

    let client = build_runner_client();

    let (resolved_id, resolved_token) = match identity {
        // The identity came from `--runner-id`/`--token` or from the config file.
        // Registering again would mint a duplicate runner row and a second token
        // for a machine that already has both.
        RunnerIdentity::Existing { runner_id, token } => (runner_id, token),
        RunnerIdentity::Register => {
            let repository = resolved_repository.as_deref().context(
                "runner auto-registration requires --repository owner/repo or a repository key in runner.toml",
            )?;
            println!(
                "Registering runner '{}' for {} with {}...",
                resolved_name, repository, resolved_server
            );
            let auth_token = resolve_auth_token(auth_token).context(
                "runner auto-registration requires --auth-token or PLOMBIR_GIT_AUTH_TOKEN; \
                 alternatively pass --runner-id and --token",
            )?;
            let (id, tok) = register_runner(
                &client,
                resolved_server,
                repository,
                &resolved_name,
                &resolved_labels,
                &auth_token,
            )
            .await?;
            println!("Registered! ID={}, Token={}", id, tok);

            // Save for future runs — the half that makes the resolution above
            // worth anything: the next start reads these back instead of
            // registering all over again.
            let mut updated_cfg = cfg.clone().unwrap_or_default();
            updated_cfg.server = Some(resolved_server.to_string());
            updated_cfg.allow_insecure_http = Some(allow_insecure_http);
            updated_cfg.runner_id = Some(id);
            updated_cfg.token = Some(tok.clone());
            updated_cfg.repository = Some(repository.to_string());
            updated_cfg.name = Some(resolved_name.clone());
            updated_cfg.labels = Some(resolved_labels);
            match save_config(&config, &updated_cfg) {
                Ok(()) => println!("Config saved to {}", config),
                // Not fatal: this run is already registered and fully usable.
                // But the failure must be visible — otherwise the only symptom
                // is a missing line on stdout and a server that collects a new
                // duplicate runner on every restart.
                Err(error) => tracing::warn!("{}", config_not_persisted_warning(&config, &error)),
            }

            (id, tok)
        }
    };

    println!(
        "Runner {} started (server={})",
        resolved_name, resolved_server
    );

    // The poll-and-execute loop is the runner's whole working life; it returns
    // when the operator asks this process to stop.
    run_jobs_until_shutdown(
        client,
        resolved_server.to_string(),
        resolved_id,
        resolved_token,
        shutdown_requested(),
    )
    .await;

    Ok(())
}

/// Longest this runner waits on `docker rm`.
///
/// Both callers are already on a clock — a job that has blown its timeout, or a
/// process that has been told to stop and will be `SIGKILL`ed if it dawdles — so
/// an unreachable Docker daemon must cost a bounded pause, not the whole
/// remaining grace period.
const CONTAINER_REMOVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Force-remove the container a job ran in.
///
/// `docker run` here is not `-d`, but the container is still not this process's
/// child: killing the docker *client* leaves the container running, so it has to
/// be removed by name. Never fails the caller — there is nothing left to abort —
/// but the failure is named, because what it leaves behind is a container that
/// keeps holding CPU, memory and the job's workspace mount.
async fn remove_job_container(job_id: i64) {
    let removal = tokio::process::Command::new("docker")
        .args(["rm", "-f", &job_container_name(job_id)])
        .output();
    match tokio::time::timeout(CONTAINER_REMOVAL_TIMEOUT, removal).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(job_id, %error, "failed to remove the job's docker container")
        }
        Err(_) => tracing::warn!(
            job_id,
            "`docker rm` did not answer within {CONTAINER_REMOVAL_TIMEOUT:?}; the job's container \
             may still be running"
        ),
    }
}

/// The job id whose container is running right now, or `None`.
type LiveContainer = std::sync::Mutex<Option<i64>>;

fn remember_live_container(slot: &LiveContainer, job_id: i64) {
    *slot
        .lock()
        .expect("the live-container slot is not poisoned") = Some(job_id);
}

fn forget_live_container(slot: &LiveContainer) -> Option<i64> {
    slot.lock()
        .expect("the live-container slot is not poisoned")
        .take()
}

/// Resolve when this process is interrupted from a terminal.
///
/// A handler that cannot be installed parks forever instead of resolving:
/// reporting "the operator asked us to stop" because the *listener* failed would
/// shut a healthy runner down the moment it started.
async fn ctrl_c_requested() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(
            %error,
            "cannot listen for Ctrl-C; this runner will not hand its job back when interrupted"
        );
        std::future::pending::<()>().await;
    }
}

/// Resolve when the operator asks this process to stop.
///
/// Both spellings are honoured because both are how a runner is actually
/// stopped: `SIGINT` when someone runs it in a terminal, and `SIGTERM` — the one
/// that happens on every deploy — from `docker stop`, `docker compose down` and
/// systemd. Without a handler for the second, a *planned* restart is
/// indistinguishable from a crash: the job the runner was holding stays
/// `running` until the stuck-job sweep reclaims it minutes later, and the runner
/// stays in the pool until its heartbeat expires.
#[cfg(unix)]
async fn shutdown_requested() {
    use tokio::signal::unix::{signal, SignalKind};

    match signal(SignalKind::terminate()) {
        Ok(mut terminate) => {
            tokio::select! {
                () = ctrl_c_requested() => {}
                _ = terminate.recv() => {}
            }
        }
        // Not fatal — Ctrl-C still stops this runner cleanly — but the operator
        // has to be told that the signal a container stop sends will not.
        Err(error) => {
            tracing::warn!(
                %error,
                "cannot listen for SIGTERM; a container stop will kill this runner instead of \
                 letting it hand its job back"
            );
            ctrl_c_requested().await;
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_requested() {
    ctrl_c_requested().await;
}

/// Poll for jobs and execute them until `shutdown` resolves, then hand the work
/// back and leave the pool.
///
/// Split out of [`cmd_run`] with the stop signal as an argument so the stop path
/// can be driven by a test. What has to be proven is that a runner being stopped
/// reaches `POST /runners/{id}/deregister` and that the job it was holding comes
/// back as `pending` without the stuck-job sweep — and a process-wide signal
/// cannot prove it without taking the test binary down with the runner.
///
/// The order on the way out is not arbitrary. The heartbeat stops first, so a
/// tick cannot land behind the deregistration and re-announce a runner that has
/// just left; the interrupted container is removed next, while its job id is
/// still known here; the deregistration goes last, because it is what returns
/// the job to the pool and any other runner may pick it up the moment it lands.
pub async fn run_jobs_until_shutdown(
    client: reqwest::Client,
    server: String,
    runner_id: i64,
    token: String,
    shutdown: impl std::future::Future<Output = ()>,
) {
    // Heartbeat task (every 30s).
    let heartbeat = tokio::spawn({
        let client = client.clone();
        let server = server.clone();
        let token = token.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                send_heartbeat(&client, &server, runner_id, &token).await;
            }
        }
    });

    let live_container = LiveContainer::default();
    let server = server.as_str();
    let token = token.as_str();
    let live_container = &live_container;
    let mut shutdown = std::pin::pin!(shutdown);

    // Main job polling loop
    loop {
        // One poll-and-execute cycle as a single future, so a stop request is
        // honoured *inside* a running job and not only between two of them: a
        // ten-minute job would otherwise outlive the container's grace period
        // and be `SIGKILL`ed, which is the very outcome this path exists to
        // avoid. Dropping the cycle cancels the job — `run_job_local` kills its
        // shell on drop, and the docker branch is cleaned up below.
        let cycle = async {
            match poll_job(&client, server, runner_id, token).await {
                Ok(Some(job)) => {
                    println!(
                        "→ Job #{}: {} (image={})",
                        job.job_id,
                        job.name,
                        job.image.as_deref().unwrap_or("local")
                    );

                    // Start
                    start_job(&client, server, runner_id, job.job_id, token).await;

                    let workspace =
                        match download_workspace(&client, server, runner_id, job.job_id, token)
                            .await
                        {
                            Ok(workspace) => workspace,
                            Err(error) => {
                                let log = format!("Failed to prepare job workspace: {error}");
                                upload_log(&client, server, runner_id, job.job_id, token, &log)
                                    .await;
                                finish_job(
                                    &client, server, runner_id, job.job_id, token, "failure", -1,
                                )
                                .await;
                                return;
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
                            upload_log(&client, server, runner_id, job.job_id, token, &log).await;
                            finish_job(
                                &client, server, runner_id, job.job_id, token, "failure", -1,
                            )
                            .await;
                            if let Err(error) = tokio::fs::remove_dir_all(&workspace).await {
                                tracing::warn!(job_id = job.job_id, %error, "failed to clean runner workspace");
                            }
                            return;
                        }
                    };
                    if let Some((key, _)) = &cache {
                        if let Err(error) = restore_cache(
                            &client, server, runner_id, job.job_id, token, key, &workspace,
                        )
                        .await
                        {
                            tracing::warn!(
                                job_id = job.job_id,
                                error = %format!("{error:#}"),
                                "cache restore failed; continuing"
                            );
                        }
                    }
                    // A containerised job is remembered for the stop path: `docker run`
                    // is not `-d`, but dropping this process's docker client does not
                    // stop what it started, so an interrupted container has to be
                    // removed by name.
                    if job.image.is_some() {
                        remember_live_container(live_container, job.job_id);
                    }
                    let execution = async {
                        if let Some(img) = &job.image {
                            run_job_docker(img, &script_str, &variables, &workspace, job.job_id)
                                .await
                        } else {
                            run_job_local(&script_str, &variables, &workspace).await
                        }
                    };
                    let timeout_seconds = resolve_polled_timeout(job.job_id, job.timeout);
                    let (exit_code, log) = match tokio::time::timeout(
                        std::time::Duration::from_secs(timeout_seconds),
                        execution,
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            if job.image.is_some() {
                                remove_job_container(job.job_id).await;
                            }
                            (-1, format!("Job timed out after {timeout_seconds} seconds"))
                        }
                    };

                    forget_live_container(live_container);

                    let mut log = log;
                    if exit_code == 0 {
                        if let Some((key, paths)) = &cache {
                            if let Err(error) = save_cache(
                                &client, server, runner_id, job.job_id, token, key, paths,
                                &workspace,
                            )
                            .await
                            {
                                tracing::warn!(
                                    job_id = job.job_id,
                                    cache_key = %key,
                                    error = %format!("{error:#}"),
                                    "cache save failed; job remains successful"
                                );
                                // The job stays successful — a cache is an
                                // optimisation, not a result — but the notice
                                // goes into the job log for the same reason the
                                // artifact one does: whoever has to act on a
                                // build that keeps recompiling from scratch is
                                // reading the pipeline, not this process's
                                // stderr.
                                if !log.is_empty() {
                                    log.push('\n');
                                }
                                log.push_str(&format!(
                                    "CI cache '{key}' was not saved; job remains successful: {error:#}"
                                ));
                            }
                        }
                        // Artifacts are published only for a job that succeeded: a
                        // failed run's output is a partial build, and publishing it
                        // under the name a green run uses hands whoever downloads
                        // it something broken with nothing saying so.
                        //
                        // The failure never fails the job — the script has already
                        // run and its exit code is the answer — but the notice goes
                        // into the job log rather than only this runner's, because
                        // the person who has to act on a missing artifact is
                        // reading the pipeline, not this process's stderr.
                        if let Some(notice) = publish_job_artifact(
                            &client, server, runner_id, token, &job, &workspace,
                        )
                        .await
                        {
                            if !log.is_empty() {
                                log.push('\n');
                            }
                            log.push_str(&notice);
                        }
                    }

                    // Upload log
                    upload_log(&client, server, runner_id, job.job_id, token, &log).await;

                    // Finish
                    let status = if exit_code == 0 { "success" } else { "failure" };
                    finish_job(
                        &client, server, runner_id, job.job_id, token, status, exit_code,
                    )
                    .await;

                    if let Err(error) = tokio::fs::remove_dir_all(&workspace).await {
                        tracing::warn!(job_id = job.job_id, %error, "failed to clean runner workspace");
                    }

                    println!("  ✓ {} (exit={})", status, exit_code);
                }
                Ok(None) => {
                    // No job available (the long poll timed out) — poll again.
                }
                Err(e) => {
                    tracing::error!("Poll error: {}", e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        };
        tokio::select! {
            // Biased so a stop request that arrives together with a finished
            // cycle is not passed over in favour of starting one more job.
            biased;
            () = &mut shutdown => break,
            () = cycle => {}
        }
    }

    heartbeat.abort();
    if let Some(job_id) = forget_live_container(live_container) {
        remove_job_container(job_id).await;
    }
    println!("Runner {runner_id} stopping — handing its work back to the server");
    deregister_runner(&client, server, runner_id, token).await;
}

#[cfg(test)]
mod redirect_tests {
    use super::{
        build_runner_client, cmd_register, cmd_run, require_confidential_runner_server,
        RegisterCommand, RunCommand,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_headers(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn write_response(stream: &mut TcpStream, status: &str, headers: &str) {
        let response =
            format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    enum OriginChange {
        Scheme,
        Host,
        Port,
    }

    async fn assert_origin_change_is_stopped(
        client: &reqwest::Client,
        expected_authorization: &str,
        change: OriginChange,
    ) {
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_address = source.local_addr().unwrap();
        let sink = if matches!(change, OriginChange::Port) {
            Some(TcpListener::bind("127.0.0.1:0").await.unwrap())
        } else {
            None
        };
        let sink_address = sink.as_ref().map(|listener| listener.local_addr().unwrap());
        let location = match change {
            OriginChange::Scheme => {
                format!("https://127.0.0.1:{}/changed-scheme", source_address.port())
            }
            OriginChange::Host => {
                format!("http://127.0.0.1:{}/changed-host", source_address.port())
            }
            OriginChange::Port => format!("http://{}/changed-port", sink_address.unwrap()),
        };
        let initial_host = if matches!(change, OriginChange::Host) {
            "localhost"
        } else {
            "127.0.0.1"
        };
        let initial_url = format!("http://{initial_host}:{}/start", source_address.port());

        let sink_task = sink.map(|sink| {
            tokio::spawn(async move {
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), sink.accept()).await,
                    Ok(Ok(_))
                )
            })
        });
        let source_task = tokio::spawn(async move {
            let (mut first, _) = source.accept().await.unwrap();
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: {location}\r\n"),
            )
            .await;
            let same_listener_followed = if matches!(change, OriginChange::Port) {
                false
            } else {
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), source.accept()).await,
                    Ok(Ok(_))
                )
            };
            (first_request, same_listener_followed)
        });

        let response = client
            .get(initial_url)
            .header("Authorization", "Bearer runner-token")
            .send()
            .await
            .expect("the cross-origin redirect must be returned, not followed");
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);

        let (first_request, same_listener_followed) = source_task.await.unwrap();
        assert!(
            first_request
                .to_ascii_lowercase()
                .contains(expected_authorization),
            "baseline request did not carry its credential: {first_request}"
        );
        let separate_sink_followed = match sink_task {
            Some(task) => task.await.unwrap(),
            None => false,
        };
        assert!(
            !same_listener_followed && !separate_sink_followed,
            "{change:?}-changing destination was contacted"
        );
    }

    #[tokio::test]
    async fn runner_client_stops_every_origin_change_before_sending_the_token() {
        let client = build_runner_client();
        for change in [OriginChange::Scheme, OriginChange::Host, OriginChange::Port] {
            assert_origin_change_is_stopped(&client, "authorization: bearer runner-token", change)
                .await;
        }
    }

    #[tokio::test]
    async fn runner_client_keeps_same_origin_redirects_and_the_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: http://{address}/renamed\r\n"),
            )
            .await;
            let (mut second, _) = listener.accept().await.unwrap();
            let second_request = read_headers(&mut second).await;
            write_response(&mut second, "204 No Content", "").await;
            (first_request, second_request)
        });

        let response = build_runner_client()
            .get(format!("http://{address}/start"))
            .bearer_auth("runner-token")
            .send()
            .await
            .expect("same-origin redirect");
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

        let (first, second) = server.await.unwrap();
        for request in [first, second] {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer runner-token"),
                "same-origin request lost the runner token: {request}"
            );
        }
    }

    fn local_non_loopback_ipv4() -> std::net::Ipv4Addr {
        let probe = std::net::UdpSocket::bind("0.0.0.0:0").unwrap();
        probe.connect("192.0.2.1:9").unwrap();
        let std::net::IpAddr::V4(ip) = probe.local_addr().unwrap().ip() else {
            panic!("the test host has no routable IPv4 address");
        };
        assert!(!ip.is_loopback(), "test address must exercise remote HTTP");
        ip
    }

    #[test]
    fn runner_transport_policy_keeps_loopback_http_and_remote_https() {
        for server in [
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
            "https://forge.example.com",
        ] {
            require_confidential_runner_server(server, false)
                .unwrap_or_else(|error| panic!("{server} should be accepted: {error:#}"));
        }
    }

    async fn registration_sink(
        listener: TcpListener,
        timeout: std::time::Duration,
    ) -> Option<String> {
        let Ok(Ok((mut stream, _))) = tokio::time::timeout(timeout, listener.accept()).await else {
            return None;
        };
        let request = read_headers(&mut stream).await;
        let body = r#"{"id":7,"token":"runner-token"}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(response.as_bytes()).await.unwrap();
        Some(request)
    }

    #[tokio::test]
    async fn remote_plaintext_is_refused_before_registration_reaches_a_live_sink() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let server = format!(
            "http://{}:{}",
            local_non_loopback_ipv4(),
            listener.local_addr().unwrap().port()
        );
        let sink = tokio::spawn(registration_sink(
            listener,
            std::time::Duration::from_millis(500),
        ));
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("runner.toml");

        let error = cmd_register(RegisterCommand {
            server: Some(server),
            allow_insecure_http: false,
            repository: Some("owner/project".to_string()),
            name: Some("builder".to_string()),
            labels: None,
            label: Vec::new(),
            save: false,
            auth_token: Some("admin-token".to_string()),
            config: config.to_string_lossy().into_owned(),
        })
        .await
        .expect_err("remote HTTP registration must fail closed");
        assert!(format!("{error:#}").contains("--allow-insecure-http"));
        assert!(
            sink.await.unwrap().is_none(),
            "the refused runner registration origin received a request"
        );
    }

    #[tokio::test]
    async fn remote_plaintext_is_refused_before_existing_runner_polling_starts() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let server = format!(
            "http://{}:{}",
            local_non_loopback_ipv4(),
            listener.local_addr().unwrap().port()
        );
        let sink = tokio::spawn(registration_sink(
            listener,
            std::time::Duration::from_millis(500),
        ));
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("runner.toml");

        let result = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            cmd_run(RunCommand {
                server: Some(server),
                allow_insecure_http: false,
                repository: Some("owner/project".to_string()),
                name: Some("builder".to_string()),
                labels: None,
                label: Vec::new(),
                token: Some("runner-token".to_string()),
                runner_id: Some(7),
                auth_token: None,
                config: config.to_string_lossy().into_owned(),
            }),
        )
        .await;
        let error = result
            .expect("transport refusal must precede the runner loop")
            .expect_err("remote HTTP polling must fail closed");
        assert!(format!("{error:#}").contains("--allow-insecure-http"));
        assert!(
            sink.await.unwrap().is_none(),
            "the refused runner polling origin received a request"
        );
    }

    #[tokio::test]
    async fn explicit_runner_plaintext_opt_in_sends_the_admin_token_to_its_origin() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let server = format!(
            "http://{}:{}",
            local_non_loopback_ipv4(),
            listener.local_addr().unwrap().port()
        );
        let sink = tokio::spawn(registration_sink(
            listener,
            std::time::Duration::from_secs(2),
        ));
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("runner.toml");

        cmd_register(RegisterCommand {
            server: Some(server),
            allow_insecure_http: true,
            repository: Some("owner/project".to_string()),
            name: Some("builder".to_string()),
            labels: None,
            label: Vec::new(),
            save: false,
            auth_token: Some("admin-token".to_string()),
            config: config.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
        let request = sink
            .await
            .unwrap()
            .expect("opted-in registration did not reach its configured origin")
            .to_ascii_lowercase();
        assert!(request.contains("authorization: bearer admin-token"));
    }
}
