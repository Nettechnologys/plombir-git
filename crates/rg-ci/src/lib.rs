//! ForgeKeep CI/CD Engine.
//!
//! Parses `.forgekeep-ci.yml` or `.gitea/workflows/*.yml` (Gitea Actions format)
//! from the repository and executes pipelines.
//!
//! ## Native format (`.forgekeep-ci.yml`)
//!
//! ```yaml
//! stages:
//!   - build
//!   - test
//!
//! build_app:
//!   stage: build
//!   script:
//!     - cargo build
//! ```
//!
//! ## Gitea Actions format (`.gitea/workflows/*.yml`)
//!
//! ```yaml
//! name: CI
//! on: push
//! jobs:
//!   build:
//!     runs-on: ubuntu-latest
//!     steps:
//!       - uses: actions/checkout@v4
//!       - run: cargo build
//! ```

pub mod condition;
pub mod config;
pub mod gitea_actions;
pub mod runner;

use anyhow::{Context, Result};
use gix::bstr::ByteSlice;

use config::CiConfig;
use runner::PipelineRunner;

// M-14: TriggerPipelineParams and has_ci_config are now defined in rg-core.
// Re-export for backward compatibility with any code that still imports from rg_ci.
pub use rg_core::ci::{has_ci_config, ResumePipelineParams, TriggerPipelineParams};

/// CI engine implementation. Implements `rg_core::ci::CiTrigger` so that
/// `rg-http` can trigger pipelines without a direct dependency on `rg-ci`.
///
/// M-14: This struct decouples the HTTP layer from the CI engine crate.
pub struct CiEngine;

impl rg_core::ci::CiTrigger for CiEngine {
    fn has_ci_config(&self, repo_path: &std::path::Path, commit_sha: &str) -> bool {
        rg_core::ci::has_ci_config(repo_path, commit_sha)
    }

    fn has_workflow_for_event(
        &self,
        repo_path: &std::path::Path,
        commit_sha: &str,
        event: &str,
        ref_name: &str,
        base_branch: Option<&str>,
    ) -> bool {
        match workflow_matches_event(repo_path, commit_sha, event, ref_name, base_branch) {
            Ok(matched) => matched,
            Err(error) => {
                // Fail-closed, unlike `has_ci_config`: this gate answers "should
                // an event nobody asked for produce a pipeline", and the honest
                // answer to "the workflows are unreadable" is "not this one".
                // `trigger_pipeline` reports the same failure with its full
                // context on the paths that do reach it.
                tracing::warn!(
                    repo = %repo_path.display(),
                    event,
                    "cannot decide whether a workflow is triggered by this event: {:#}",
                    error
                );
                false
            }
        }
    }

    fn trigger_pipeline<'a>(
        &'a self,
        params: rg_core::ci::TriggerPipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<i64>> + Send + 'a>> {
        Box::pin(trigger_pipeline(params))
    }

    fn resume_pipeline<'a>(
        &'a self,
        params: rg_core::ci::ResumePipelineParams<'a>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(resume_pipeline(params))
    }
}

/// Resume an already-created pipeline. External runners only need the job to
/// be moved back to `pending`; an internal runner is recreated from persisted
/// pipeline state and skips terminal jobs.
pub async fn resume_pipeline(params: ResumePipelineParams<'_>) -> Result<()> {
    if params.external_runners {
        tracing::info!(
            pipeline_id = params.pipeline_id,
            "manual pipeline released for external runners"
        );
        return Ok(());
    }
    spawn_internal_runner(
        params.db,
        params.repo_path,
        params.repo_id,
        params.pipeline_id,
        params.docker_enabled,
        params.allow_host_runner,
        params.jwt_secret,
        params.external_url,
    );
    Ok(())
}

/// Trigger a CI pipeline for a push event.
///
/// This function:
/// 1. Reads `.forgekeep-ci.yml` from the repo at the given commit
/// 2. Parses the CI configuration
/// 3. Checks concurrency control (if configured)
/// 4. Creates pipeline/stage/job records in the DB
/// 5. Spawns the pipeline runner in a background task, injecting CI_JOB_TOKEN
pub async fn trigger_pipeline(params: TriggerPipelineParams<'_>) -> Result<i64> {
    let TriggerPipelineParams {
        db,
        repo_path,
        repo_id,
        commit_sha,
        ref_name,
        trigger_type,
        base_branch,
        triggered_by,
        docker_enabled,
        external_runners,
        allow_host_runner,
        jwt_secret,
        external_url,
    } = params;

    // 1. Read CI config from repo
    let config = read_ci_config(repo_path, commit_sha, ref_name, trigger_type, base_branch)?;
    validate_execution_semantics(&config)?;

    // 2. Concurrency control
    if let Some(ref concurrency) = config.concurrency {
        let group =
            rg_db::ops::pipeline_ops::resolve_concurrency_group(&concurrency.group, ref_name);
        let active =
            rg_db::ops::pipeline_ops::find_active_pipelines_by_ref(db, repo_id, ref_name).await?;

        if !active.is_empty() {
            if concurrency.cancel_in_progress {
                tracing::info!(
                    concurrency_group = %group,
                    "Cancelling {} in-progress pipeline(s) for concurrency group",
                    active.len()
                );
                for p in &active {
                    if let Err(e) = rg_db::ops::pipeline_ops::cancel_pipeline_chain(db, p.id).await
                    {
                        tracing::warn!(pipeline_id = p.id, "Failed to cancel pipeline: {:#}", e);
                    }
                }
            } else {
                return Err(anyhow::anyhow!(
                    "Concurrency group '{}' has {} active pipeline(s). \
                     Set cancel_in_progress: true to auto-cancel, or wait for them to finish.",
                    group,
                    active.len()
                ));
            }
        }
    }

    // 3. Create pipeline record
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        commit_sha,
        ref_name,
        trigger_type,
        triggered_by,
    )
    .await?;

    let pipeline_id = pipeline.id;

    // 4. Create stages
    let stage_names = config.stages.as_ref().cloned().unwrap_or_default();
    let mut stage_id_map: std::collections::HashMap<String, i64> = std::collections::HashMap::new();

    for (order, stage_name) in stage_names.iter().enumerate() {
        let stage =
            rg_db::ops::pipeline_ops::create_stage(db, pipeline_id, stage_name, order as i32)
                .await?;
        stage_id_map.insert(stage_name.clone(), stage.id);
    }

    // 5. Create jobs
    for (job_name, job_config) in &config.jobs {
        // Filter by `only` — if specified, skip jobs that don't match the ref
        if let Some(only) = &job_config.only {
            let ref_short = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
            if !only
                .iter()
                .any(|pattern| pattern == ref_short || pattern == ref_name)
            {
                continue;
            }
        }

        let stage_name = job_config.stage.as_deref().unwrap_or("default");
        let stage_id = stage_id_map.get(stage_name).copied().unwrap_or(-1);

        if stage_id < 0 {
            tracing::warn!(job = %job_name, stage = %stage_name, "Job references unknown stage, skipping");
            continue;
        }

        // Serialize tags to JSON for storage
        let tags_json = job_config
            .tags
            .as_ref()
            .map(|t| serde_json::to_string(t).unwrap_or_default());
        for variant in expand_matrix(job_name, job_config)? {
            let variables_json = if variant.variables.is_empty() {
                None
            } else {
                Some(serde_json::to_string(&variant.variables)?)
            };
            let cache_paths_json = job_config
                .cache
                .as_ref()
                .map(|cache| serde_json::to_string(&cache.paths))
                .transpose()?;
            let job = rg_db::ops::pipeline_ops::create_job(
                db,
                stage_id,
                &variant.name,
                &job_config.script.join("\n"),
                job_config.image.as_deref(),
                tags_json.as_deref(),
                variables_json.as_deref(),
                job_config.cache.as_ref().map(|cache| cache.key.as_str()),
                cache_paths_json.as_deref(),
                job_config.allow_failure.unwrap_or(false),
                job_config.timeout_seconds.map(|seconds| seconds as i64),
                job_config.when.as_deref(),
                job_config.condition.as_deref(),
            )
            .await?;
            let should_run = if let Some(condition) = job_config.condition.as_deref() {
                condition::evaluate_condition(
                    condition,
                    &job_condition_context(
                        ref_name,
                        trigger_type,
                        commit_sha,
                        &variant.variables,
                        job_config,
                    ),
                )?
            } else {
                true
            };
            if !should_run {
                let now = chrono::Utc::now().naive_utc();
                rg_db::ops::pipeline_ops::update_job_result(
                    db,
                    job.id,
                    "skipped",
                    None,
                    None,
                    None,
                    Some(now),
                )
                .await?;
            } else if let Some(environment_name) = job_config.environment.as_deref() {
                let environment =
                    rg_db::ops::ci_environment_ops::find_by_name(db, repo_id, environment_name)
                        .await?;
                rg_db::ops::ci_environment_ops::attach_job(
                    db,
                    job.id,
                    environment.as_ref(),
                    environment_name,
                )
                .await?;
            }
        }
    }

    for stage in rg_db::ops::pipeline_ops::list_stages_by_pipeline(db, pipeline_id).await? {
        rg_db::ops::pipeline_ops::try_update_stage(db, stage.id).await?;
    }
    rg_db::ops::pipeline_ops::try_update_pipeline(db, pipeline_id).await?;

    if rg_db::ops::pipeline_ops::get_pipeline(db, pipeline_id)
        .await?
        .is_some_and(|pipeline| pipeline.status == "success")
    {
        evaluate_initial_success(
            db,
            repo_path,
            repo_id,
            commit_sha,
            docker_enabled,
            external_runners,
            allow_host_runner,
            jwt_secret,
            external_url,
        )
        .await;
        return Ok(pipeline_id);
    }

    // 6. Spawn pipeline runner in background (only if not using external runners)
    if !external_runners {
        spawn_internal_runner(
            db,
            repo_path,
            repo_id,
            pipeline_id,
            docker_enabled,
            allow_host_runner,
            jwt_secret,
            external_url,
        );
    } else {
        if let Some(first_stage) =
            rg_db::ops::pipeline_ops::list_stages_by_pipeline(db, pipeline_id)
                .await?
                .into_iter()
                .next()
        {
            rg_db::ops::pipeline_ops::try_pause_stage_at_manual(db, first_stage.id).await?;
        }
        tracing::info!(
            pipeline_id = pipeline_id,
            "Pipeline created with external runner mode — jobs will be picked up by registered runners"
        );
    }

    Ok(pipeline_id)
}

#[allow(clippy::too_many_arguments)]
async fn evaluate_initial_success(
    db: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    repo_id: i64,
    commit_sha: &str,
    docker_enabled: bool,
    external_runners: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    external_url: Option<&str>,
) {
    let Some(repo_root) = repo_path.parent().and_then(std::path::Path::parent) else {
        return;
    };
    post_push_context(
        repo_root,
        docker_enabled,
        external_runners,
        allow_host_runner,
        jwt_secret,
        external_url,
    )
    .evaluate_merges_and_spawn_hooks(db, repo_id, commit_sha, None)
    .await;
}

/// This crate's post-push wiring, for the merges a finished pipeline unblocks.
///
/// A pipeline that goes green is the trigger of the most common auto-merge there
/// is, and the merge commit it puts on the base branch owes the same automation
/// a push does — a pipeline of its own, the `push` webhook, the watch fan-out.
/// Until card_73a1ec5b32f3 both CI-completion paths here ran the merge and threw
/// the ref move away, so that commit was seen by nothing.
///
/// Two of the hooks' inputs simply do not exist in this crate: there is no
/// WebSocket hub (`notifier: None` — the real-time `push` event is the HTTP
/// layer's) and no SMTP configuration (`smtp_config: None` — no "pipeline
/// triggered" email). Everything that reaches storage — the pipeline, the
/// webhooks, the notifications — runs in full.
fn post_push_context(
    repo_root: &std::path::Path,
    docker_enabled: bool,
    external_runners: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    external_url: Option<&str>,
) -> rg_core::push_hooks::PostPushContext {
    rg_core::push_hooks::PostPushContext {
        repo_root: repo_root.to_path_buf(),
        docker_enabled,
        external_runners,
        allow_host_runner,
        jwt_secret: jwt_secret.map(str::to_string),
        smtp_config: None,
        ci_engine: std::sync::Arc::new(CiEngine),
        external_url: external_url.map(str::to_string),
        notifier: None,
    }
}

#[allow(clippy::too_many_arguments)]
fn spawn_internal_runner(
    db: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    repo_id: i64,
    pipeline_id: i64,
    docker_enabled: bool,
    allow_host_runner: bool,
    jwt_secret: Option<&str>,
    external_url: Option<&str>,
) {
    let db_clone = db.clone();
    let repo_path_owned = repo_path.to_path_buf();
    let jwt_secret_owned = jwt_secret.map(str::to_string);
    let oidc_token_url =
        external_url.map(|url| format!("{}/api/v1/ci/oidc/token", url.trim_end_matches('/')));
    tokio::spawn(async move {
        let mut runner = if docker_enabled {
            PipelineRunner::new(db_clone, &repo_path_owned, pipeline_id)
        } else {
            PipelineRunner::new_local_only(db_clone, &repo_path_owned, pipeline_id)
        };
        runner.set_repo_id(repo_id);
        runner.set_allow_host_runner(allow_host_runner);
        if let Some(secret) = jwt_secret_owned {
            runner.set_jwt_secret(secret);
        }
        if let Some(url) = oidc_token_url {
            runner.set_oidc_token_url(url);
        }
        if let Err(error) = runner.run().await {
            tracing::error!(pipeline_id, error = %format!("{error:#}"), "pipeline runner error");
        }
    });
}

fn validate_execution_semantics(config: &CiConfig) -> Result<()> {
    for (name, job) in &config.jobs {
        if let Some(when) = job.when.as_deref() {
            if when != "on_success" && when != "manual" {
                anyhow::bail!("job '{name}' uses unsupported when: '{when}'; supported values are 'on_success' and 'manual'");
            }
        }
        if job.timeout_seconds == Some(0) || job.timeout_seconds.is_some_and(|value| value > 86_400)
        {
            anyhow::bail!("job '{name}' timeout_seconds must be between 1 and 86400");
        }
        if let Some(environment) = job.environment.as_deref() {
            if environment.is_empty()
                || environment.len() > 255
                || environment.chars().any(char::is_control)
            {
                anyhow::bail!("job '{name}' has an invalid environment name");
            }
        }
        if let Some(condition) = job.condition.as_deref() {
            crate::condition::validate_condition(condition)
                .with_context(|| format!("job '{name}' has an unsupported if condition"))?;
        }
    }
    Ok(())
}

fn job_condition_context(
    ref_name: &str,
    event: &str,
    sha: &str,
    variables: &std::collections::BTreeMap<String, String>,
    config: &config::JobConfig,
) -> std::collections::HashMap<String, String> {
    let mut context = std::collections::HashMap::from([
        ("github.ref".into(), ref_name.to_string()),
        (
            "github.ref_name".into(),
            ref_name
                .strip_prefix("refs/heads/")
                .or_else(|| ref_name.strip_prefix("refs/tags/"))
                .unwrap_or(ref_name)
                .to_string(),
        ),
        ("github.event_name".into(), event.to_string()),
        ("github.sha".into(), sha.to_string()),
    ]);
    for (name, value) in variables {
        context.insert(format!("env.{name}"), value.clone());
    }
    if let Some(matrix) = &config.matrix {
        for name in matrix.keys() {
            if let Some(value) = variables.get(name) {
                context.insert(format!("matrix.{name}"), value.clone());
            }
        }
    }
    context
}

#[derive(Debug)]
struct MatrixVariant {
    name: String,
    variables: std::collections::BTreeMap<String, String>,
}

fn expand_matrix(job_name: &str, config: &config::JobConfig) -> Result<Vec<MatrixVariant>> {
    let base: std::collections::BTreeMap<String, String> = config
        .variables
        .clone()
        .unwrap_or_default()
        .into_iter()
        .collect();
    let Some(matrix) = &config.matrix else {
        return Ok(vec![MatrixVariant {
            name: job_name.to_owned(),
            variables: base,
        }]);
    };
    if matrix.values().any(Vec::is_empty) {
        anyhow::bail!("job '{job_name}' has an empty matrix dimension");
    }
    let count = matrix
        .values()
        .try_fold(1usize, |total, values| total.checked_mul(values.len()))
        .context("matrix size overflow")?;
    if count > 256 {
        anyhow::bail!("job '{job_name}' matrix expands to {count} variants; maximum is 256");
    }
    let mut variants = vec![(Vec::<(String, String)>::new(), base)];
    for (key, values) in matrix {
        let mut next = Vec::new();
        for (labels, variables) in variants {
            for value in values {
                let mut labels = labels.clone();
                labels.push((key.clone(), value.clone()));
                let mut variables = variables.clone();
                variables.insert(key.clone(), value.clone());
                variables.insert(
                    format!("MATRIX_{}", key.to_ascii_uppercase().replace('-', "_")),
                    value.clone(),
                );
                next.push((labels, variables));
            }
        }
        variants = next;
    }
    Ok(variants
        .into_iter()
        .map(|(labels, variables)| MatrixVariant {
            name: format!(
                "{job_name} [{}]",
                labels
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            variables,
        })
        .collect())
}

/// Read CI configuration from the repo at the given commit.
///
/// Tries formats in order:
/// 1. `.gitea/workflows/*.yml` (Gitea Actions format)
/// 2. `.forgekeep-ci.yml` (native format)
///
/// For Gitea Actions workflows, multiple files are merged into a single `CiConfig`.
/// Jobs from different workflow files are placed in separate stages.
///
/// The fallback from the first format to the second happens only when
/// `.gitea/workflows` is absent, or holds no workflow triggered by this event.
/// A workflow file that *is* there but cannot be read or parsed is an error
/// naming the file and the reason — never a silent "no CI config found".
fn read_ci_config(
    repo_path: &std::path::Path,
    commit_sha: &str,
    ref_name: &str,
    event: &str,
    base_branch: Option<&str>,
) -> Result<CiConfig> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    // Try Gitea Actions format first
    let gitea = try_read_gitea_workflows(&repo, commit_sha, ref_name, event, base_branch)?;
    let workflows_untriggered = matches!(gitea, GiteaWorkflows::NoneTriggered);
    if let GiteaWorkflows::Config(config) = gitea {
        tracing::info!("Using Gitea Actions workflow from {}/", WORKFLOW_DIR);
        return Ok(config);
    }

    // Fall back to the native CI config: the current ForgeKeep filename, then the
    // deprecated IronForge name for backwards compatibility (removed in a future release).
    let ci_filename = [".forgekeep-ci.yml", ".ironforge-ci.yml"]
        .into_iter()
        .find(|name| {
            repo.rev_parse_single(format!("{}:{}", commit_sha, name).as_str())
                .is_ok()
        })
        .ok_or_else(|| {
            // Workflows that exist but sit out this event are not "no config":
            // saying so is the difference between fixing an `on:` filter and
            // hunting for a file that is already there.
            if workflows_untriggered {
                anyhow::anyhow!(
                    "no workflow in {}/ is triggered by event {} on {}, and no native CI config (.forgekeep-ci.yml) at commit {}",
                    WORKFLOW_DIR,
                    event,
                    ref_name,
                    commit_sha
                )
            } else {
                anyhow::anyhow!(
                    "no CI config found (.gitea/workflows/*.yml, .forgekeep-ci.yml, or .ironforge-ci.yml) at commit {}",
                    commit_sha
                )
            }
        })?;

    let revspec = format!("{}:{}", commit_sha, ci_filename);
    let object_id = repo
        .rev_parse_single(revspec.as_str())
        .context("failed to resolve CI config object")?;

    let object_id = object_id.object().context("failed to resolve object")?;
    let blob = object_id
        .try_into_blob()
        .with_context(|| format!("expected a blob object for {}", ci_filename))?;

    let ci_yml = String::from_utf8(blob.data.to_vec())
        .with_context(|| format!("{} is not valid UTF-8", ci_filename))?;

    // The reason (with its line/column) goes into the message, not into a
    // `with_context` source: callers log this with `Display`. Dumping the whole
    // file here used to bury the actual complaint.
    let config: CiConfig = serde_yaml::from_str(&ci_yml)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {}", ci_filename, e))?;

    Ok(config)
}

/// Directory holding Gitea Actions workflow files, relative to the repo root.
const WORKFLOW_DIR: &str = ".gitea/workflows";

/// Outcome of looking for Gitea Actions workflows at a commit.
enum GiteaWorkflows {
    /// The commit has no `.gitea/workflows` directory at all.
    Absent,
    /// The directory is there, but no workflow in it is triggered by this event.
    NoneTriggered,
    /// Merged configuration of every workflow triggered by this event.
    Config(CiConfig),
}

/// Try to find and parse Gitea Actions workflow files in `.gitea/workflows/`.
///
/// `Absent` / `NoneTriggered` are the two legitimate fallbacks to the native
/// `.forgekeep-ci.yml`, and they are told apart so the caller can explain which
/// one happened.
///
/// Anything else — a workflow that is not valid UTF-8, does not parse as YAML,
/// references a missing reusable workflow, or uses an unsupported action — is an
/// `Err` naming the offending file, so a typo in a workflow surfaces as a
/// parsing error instead of being mistaken for "no CI config at all".
fn try_read_gitea_workflows(
    repo: &gix::Repository,
    commit_sha: &str,
    ref_name: &str,
    event: &str,
    base_branch: Option<&str>,
) -> Result<GiteaWorkflows> {
    let Some(workflow_sources) = load_workflow_sources(repo, commit_sha)? else {
        // Nothing at that path in this commit: the native format is next in line.
        return Ok(GiteaWorkflows::Absent);
    };

    let match_branch = event_match_branch(repo, base_branch)?;

    let mut all_jobs: std::collections::HashMap<String, config::JobConfig> =
        std::collections::HashMap::new();
    let mut all_stages: Vec<String> = Vec::new();

    for (name, yml) in sorted_workflows(&workflow_sources) {
        // The cause is folded into the message instead of being a `with_context`
        // source: callers log this error with `Display`, and the YAML line/column
        // is the whole point of reporting it.
        let workflow = gitea_actions::GiteaWorkflow::parse(yml)
            .map_err(|e| anyhow::anyhow!("failed to parse {WORKFLOW_DIR}/{name}: {e}"))?;

        // Check if this workflow should be triggered
        if !workflow.matches_event(event, ref_name, &match_branch) {
            continue;
        }
        let workflow = workflow
            .expand_local_reusable_workflows(&workflow_sources)
            .map_err(|e| anyhow::anyhow!("failed to expand {WORKFLOW_DIR}/{name}: {e:#}"))?;
        workflow
            .validate_supported_actions()
            .map_err(|e| anyhow::anyhow!("unsupported workflow {WORKFLOW_DIR}/{name}: {e:#}"))?;

        tracing::info!("Triggering workflow from {}/{}", WORKFLOW_DIR, name);

        let ctx = gitea_actions::WorkflowContext {
            ref_name: ref_name.to_string(),
            sha: commit_sha.to_string(),
            event: event.to_string(),
            repo_owner: String::new(), // filled later
            repo_name: String::new(),
        };

        let mut wf_config = workflow.to_ci_config(&ctx);

        // Prefix job names with workflow filename to avoid collisions
        let wf_prefix = name.trim_end_matches(".yml").trim_end_matches(".yaml");
        let mut renamed_jobs = std::collections::HashMap::new();
        for (job_name, mut job) in wf_config.jobs {
            let new_name = format!("{}/{}", wf_prefix, job_name);
            // Prefix stage names too
            if let Some(ref stage) = job.stage {
                job.stage = Some(format!("{}/{}", wf_prefix, stage));
            }
            renamed_jobs.insert(new_name, job);
        }
        wf_config.jobs = renamed_jobs;

        // Add stages
        if let Some(ref stages) = wf_config.stages {
            for stage in stages {
                all_stages.push(format!("{}/{}", wf_prefix, stage));
            }
        }

        // Merge jobs
        all_jobs.extend(wf_config.jobs);
    }

    if all_jobs.is_empty() {
        tracing::debug!(
            "No workflow in {}/ is triggered by event {} on {}; trying the native CI config",
            WORKFLOW_DIR,
            event,
            ref_name
        );
        return Ok(GiteaWorkflows::NoneTriggered);
    }

    Ok(GiteaWorkflows::Config(CiConfig {
        stages: Some(all_stages),
        concurrency: None, // per-workflow concurrency not merged
        jobs: all_jobs,
    }))
}

/// Read every `*.yml` / `*.yaml` blob under [`WORKFLOW_DIR`] at `commit_sha`.
///
/// `None` means the directory is not in this commit at all — the one outcome
/// that legitimately falls back to the native config. Everything else that goes
/// wrong is an `Err` naming the file: a workflow that exists but cannot be read
/// must never be mistaken for "no CI config here".
///
/// Sources are loaded as a set rather than one at a time so a workflow can
/// resolve a repository-local reusable workflow from the same immutable tree.
fn load_workflow_sources(
    repo: &gix::Repository,
    commit_sha: &str,
) -> Result<Option<std::collections::HashMap<String, String>>> {
    let tree_revspec = format!("{}:{}", commit_sha, WORKFLOW_DIR);
    let Ok(object_id) = repo.rev_parse_single(tree_revspec.as_str()) else {
        return Ok(None);
    };

    let object = object_id
        .object()
        .with_context(|| format!("failed to read {} at commit {}", WORKFLOW_DIR, commit_sha))?;
    let tree = object.try_into_tree().map_err(|_| {
        anyhow::anyhow!(
            "{} exists at commit {} but is a file, not a directory",
            WORKFLOW_DIR,
            commit_sha
        )
    })?;

    let mut workflow_sources = std::collections::HashMap::new();
    for entry in tree.iter() {
        let entry = entry
            .with_context(|| format!("failed to list {} at commit {}", WORKFLOW_DIR, commit_sha))?;
        let name = entry.filename().to_string();
        if !name.ends_with(".yml") && !name.ends_with(".yaml") {
            continue;
        }
        let mode = entry.mode();
        if !mode.is_blob() && !mode.is_executable() {
            // A directory or submodule that happens to be named `*.yml` is not a
            // workflow; say so instead of failing the whole pipeline over it.
            tracing::warn!("Skipping {}/{}: not a regular file", WORKFLOW_DIR, name);
            continue;
        }

        let entry_object = repo.find_object(entry.oid()).with_context(|| {
            format!(
                "failed to read {}/{} from the object database",
                WORKFLOW_DIR, name
            )
        })?;
        let blob = entry_object
            .try_into_blob()
            .map_err(|_| anyhow::anyhow!("{}/{} is not a file", WORKFLOW_DIR, name))?;
        let yml = String::from_utf8(blob.data.to_vec())
            .with_context(|| format!("{}/{} is not valid UTF-8", WORKFLOW_DIR, name))?;
        workflow_sources.insert(name, yml);
    }
    Ok(Some(workflow_sources))
}

/// Workflow sources by filename, in a deterministic order.
///
/// Stage ordering of the merged config — and which broken file is reported
/// first — must not depend on hash-map iteration order.
fn sorted_workflows(
    sources: &std::collections::HashMap<String, String>,
) -> Vec<(&String, &String)> {
    let mut workflows: Vec<(&String, &String)> = sources.iter().collect();
    workflows.sort_by_key(|(name, _)| *name);
    workflows
}

/// The branch `on:`-filters are matched against for this event.
///
/// The caller's `base_branch` when it has one (a PR's target branch), the
/// repository's default branch otherwise.
fn event_match_branch(repo: &gix::Repository, base_branch: Option<&str>) -> Result<String> {
    match base_branch {
        Some(base_branch) => Ok(base_branch.to_string()),
        None => get_default_branch(repo),
    }
}

/// Whether any workflow at `commit_sha` is triggered by `event`.
///
/// Cheaper and narrower than [`read_ci_config`]: it only asks the `on:` block,
/// so a workflow that this event does not select is never expanded or validated.
/// A file that fails to parse cannot answer, so it counts as "not triggered" and
/// says so in the log — the caller is deciding whether an event should produce a
/// pipeline at all, and a broken unrelated workflow must not conjure one.
fn workflow_matches_event(
    repo_path: &std::path::Path,
    commit_sha: &str,
    event: &str,
    ref_name: &str,
    base_branch: Option<&str>,
) -> Result<bool> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;
    let Some(sources) = load_workflow_sources(&repo, commit_sha)? else {
        return Ok(false);
    };
    let match_branch = event_match_branch(&repo, base_branch)?;

    for (name, yml) in sorted_workflows(&sources) {
        match gitea_actions::GiteaWorkflow::parse(yml) {
            Ok(workflow) => {
                if workflow.matches_event(event, ref_name, &match_branch) {
                    return Ok(true);
                }
            }
            Err(error) => tracing::warn!(
                "failed to parse {}/{} while matching event {}: {}",
                WORKFLOW_DIR,
                name,
                event,
                error
            ),
        }
    }
    Ok(false)
}

/// Get the default branch name of the repository.
fn get_default_branch(repo: &gix::Repository) -> Result<String> {
    // Try to read HEAD reference
    if let Ok(Some(head_ref)) = repo.head_ref() {
        if let Ok(name) = head_ref.name().shorten().to_str() {
            return Ok(name.to_string());
        }
    }
    Ok("main".to_string())
}

// M-14: has_ci_config moved to rg_core::ci::has_ci_config and re-exported above.

#[cfg(test)]
mod matrix_tests {
    use super::*;
    use sea_orm::{NotSet, Set};
    use std::collections::{BTreeMap, HashMap};

    fn config(matrix: BTreeMap<String, Vec<String>>) -> config::JobConfig {
        config::JobConfig {
            stage: Some("test".into()),
            script: vec!["echo ok".into()],
            image: None,
            only: None,
            variables: Some(HashMap::from([("BASE".into(), "yes".into())])),
            when: None,
            condition: None,
            environment: None,
            allow_failure: None,
            timeout_seconds: None,
            tags: None,
            matrix: Some(matrix),
            cache: None,
        }
    }

    #[test]
    fn expands_cartesian_matrix_with_deterministic_names_and_variables() {
        let variants = expand_matrix(
            "test",
            &config(BTreeMap::from([
                ("os".into(), vec!["linux".into(), "macos".into()]),
                ("rust".into(), vec!["stable".into(), "beta".into()]),
            ])),
        )
        .unwrap();
        assert_eq!(variants.len(), 4);
        assert_eq!(variants[0].name, "test [os=linux, rust=stable]");
        assert_eq!(variants[0].variables["MATRIX_OS"], "linux");
        assert_eq!(variants[0].variables["BASE"], "yes");
    }

    #[test]
    fn rejects_excessive_matrix() {
        let error = expand_matrix(
            "huge",
            &config(BTreeMap::from([
                ("a".into(), (0..17).map(|v| v.to_string()).collect()),
                ("b".into(), (0..17).map(|v| v.to_string()).collect()),
            ])),
        )
        .unwrap_err();
        assert!(error.to_string().contains("maximum is 256"));
    }

    #[tokio::test]
    async fn matrix_job_conditions_persist_and_skip_only_false_variants() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(
            temp.path().join(".forgekeep-ci.yml"),
            "stages: [test, deploy]\nconditional:\n  stage: test\n  if: matrix.os == 'linux' && github.ref_name == 'main'\n  script: [echo ok]\n  matrix:\n    os: [linux, macos]\ndeploy:\n  stage: deploy\n  if: github.ref_name == 'main'\n  script: [echo deploy]\n",
        ).unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(temp.path())).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(temp.path())
            )
            .unwrap()
            .success());
        assert!(git
            .run(&["add", ".forgekeep-ci.yml"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "conditional"], Some(temp.path()))
            .unwrap()
            .success());
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_string();
        let db = rg_db::connect(&format!(
            "sqlite://{}?mode=rwc",
            temp.path().join("conditions.db").display()
        ))
        .await
        .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "condition-owner",
            "condition@example.com",
            "unused",
            "Condition Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("conditions".into()),
                description: Set(None),
                is_private: Set(true),
                default_branch: Set("main".into()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .unwrap();
        let pipeline_id = trigger_pipeline(TriggerPipelineParams {
            db: &db,
            repo_path: temp.path(),
            repo_id: repo.id,
            commit_sha: &sha,
            ref_name: "refs/heads/main",
            trigger_type: "push",
            base_branch: None,
            triggered_by: Some(user.id),
            docker_enabled: false,
            external_runners: true,
            allow_host_runner: false,
            jwt_secret: Some("secret"),
            external_url: None,
        })
        .await
        .unwrap();
        let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
            .await
            .unwrap();
        assert_eq!(jobs.len(), 3);
        let runnable = jobs
            .iter()
            .find(|job| job.name.contains("os=linux"))
            .unwrap();
        assert_eq!(runnable.status, "pending");
        let skipped = jobs
            .iter()
            .find(|job| job.name.contains("os=macos"))
            .unwrap();
        assert_eq!(skipped.status, "skipped");
        assert!(skipped
            .if_condition
            .as_deref()
            .unwrap()
            .contains("matrix.os"));

        rg_db::ops::pipeline_ops::update_job_result(
            &db,
            runnable.id,
            "assigned",
            None,
            None,
            Some(chrono::Utc::now().naive_utc()),
            None,
        )
        .await
        .unwrap();
        assert!(
            rg_db::ops::pipeline_ops::find_pending_job_matching_labels(&db, &[])
                .await
                .unwrap()
                .is_none()
        );
        rg_db::ops::pipeline_ops::update_job_result(
            &db,
            runnable.id,
            "failed",
            Some(1),
            None,
            None,
            Some(chrono::Utc::now().naive_utc()),
        )
        .await
        .unwrap();
        assert_eq!(
            rg_db::ops::pipeline_ops::try_update_stage(&db, runnable.stage_id)
                .await
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::try_update_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .as_deref(),
            Some("failed")
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::get_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .unwrap()
                .status,
            "failed"
        );
        assert_eq!(
            rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, pipeline_id)
                .await
                .unwrap()
                .into_iter()
                .find(|job| job.name == "deploy")
                .unwrap()
                .status,
            "skipped"
        );

        let all_skipped_pipeline_id = trigger_pipeline(TriggerPipelineParams {
            db: &db,
            repo_path: temp.path(),
            repo_id: repo.id,
            commit_sha: &sha,
            ref_name: "refs/heads/dev",
            trigger_type: "push",
            base_branch: None,
            triggered_by: Some(user.id),
            docker_enabled: false,
            external_runners: true,
            allow_host_runner: false,
            jwt_secret: Some("secret"),
            external_url: None,
        })
        .await
        .unwrap();
        let all_skipped_pipeline =
            rg_db::ops::pipeline_ops::get_pipeline(&db, all_skipped_pipeline_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(all_skipped_pipeline.status, "success");
        assert!(
            rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&db, all_skipped_pipeline_id)
                .await
                .unwrap()
                .iter()
                .all(|job| job.status == "skipped")
        );
    }

    #[test]
    fn repository_reader_expands_local_reusable_workflow_at_commit() {
        let temp = tempfile::tempdir().unwrap();
        let workflows = temp.path().join(".gitea/workflows");
        std::fs::create_dir_all(&workflows).unwrap();
        std::fs::write(
            workflows.join("main.yml"),
            "on: push\njobs:\n  shared:\n    uses: ./.gitea/workflows/shared.yml\n    with:\n      target: staging\n",
        ).unwrap();
        std::fs::write(
            workflows.join("shared.yml"),
            "on: workflow_call\njobs:\n  build:\n    steps:\n      - run: echo '${{ inputs.target }}'\n",
        ).unwrap();
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(temp.path())).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(temp.path())
            )
            .unwrap()
            .success());
        assert!(git
            .run(&["add", ".gitea/workflows"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "workflows"], Some(temp.path()))
            .unwrap()
            .success());
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        let config = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap();
        let job = config.jobs.get("main/shared/build").unwrap();
        assert!(job
            .script
            .iter()
            .any(|line| line.contains("${INPUT_TARGET}")));
        assert_eq!(job.variables.as_ref().unwrap()["INPUT_TARGET"], "staging");
    }

    /// Commit `files` (relative path → contents) into a fresh repo and return
    /// the temp dir plus the commit sha.
    fn commit_repo(files: &[(&str, &[u8])]) -> (tempfile::TempDir, String) {
        let temp = tempfile::tempdir().unwrap();
        for (path, contents) in files {
            let target = temp.path().join(path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(target, contents).unwrap();
        }
        let git = rg_git::cli_gateway::global_gateway().as_ref().unwrap();
        assert!(git.run(&["init"], Some(temp.path())).unwrap().success());
        assert!(git
            .run(&["config", "user.name", "CI"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(
                &["config", "user.email", "ci@example.com"],
                Some(temp.path())
            )
            .unwrap()
            .success());
        assert!(git
            .run(&["add", "-A"], Some(temp.path()))
            .unwrap()
            .success());
        assert!(git
            .run(&["commit", "-m", "fixture"], Some(temp.path()))
            .unwrap()
            .success());
        let sha = git
            .run(&["rev-parse", "HEAD"], Some(temp.path()))
            .unwrap()
            .stdout_str()
            .trim()
            .to_owned();
        (temp, sha)
    }

    #[test]
    fn broken_workflow_yaml_reports_the_file_and_the_parse_error() {
        // Present-but-broken must never degrade into "no CI config found":
        // the `.forgekeep-ci.yml` fallback below would otherwise hide the typo.
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/ci.yml",
                b"on: push\njobs:\n  build:\n   steps:\n  - run: echo broken\n" as &[u8],
            ),
            (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);

        let error = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap_err();
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(".gitea/workflows/ci.yml"),
            "error must name the offending file: {rendered}"
        );
        assert!(
            !rendered.contains("no CI config found"),
            "a broken workflow must not be reported as a missing config: {rendered}"
        );
        // The YAML reason (with its position) has to survive to the log line.
        assert!(
            rendered.contains("line") || rendered.contains("column"),
            "error must carry the parse reason: {rendered}"
        );
    }

    #[test]
    fn non_utf8_workflow_reports_the_file_instead_of_parsing_an_empty_string() {
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/ci.yml", &[0xff, 0xfe, b'o', b'n', b':']),
            (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);

        let error = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap_err();
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(".gitea/workflows/ci.yml") && rendered.contains("UTF-8"),
            "error must name the file and the encoding problem: {rendered}"
        );
    }

    #[test]
    fn unsupported_action_in_a_workflow_is_reported_not_swallowed() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/ci.yml",
            b"on: push\njobs:\n  build:\n    steps:\n      - uses: actions/setup-node@v4\n"
                as &[u8],
        )]);

        let error = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap_err();
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(".gitea/workflows/ci.yml") && rendered.contains("setup-node"),
            "error must name the file and the unsupported action: {rendered}"
        );
    }

    #[test]
    fn broken_native_config_reports_the_reason_not_the_whole_file() {
        let (temp, sha) = commit_repo(&[(
            ".forgekeep-ci.yml",
            b"build:\n  script: [echo ok]\n   nested: bad\n" as &[u8],
        )]);

        let error = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap_err();
        let rendered = format!("{error}");
        assert!(
            rendered.contains(".forgekeep-ci.yml") && rendered.contains("line"),
            "error must name the file and the parse position: {rendered}"
        );
        assert!(
            !rendered.contains("echo ok"),
            "the file body must not be dumped into the message: {rendered}"
        );
    }

    #[test]
    fn missing_workflow_directory_still_falls_back_to_the_native_config() {
        let (temp, sha) = commit_repo(&[(
            ".forgekeep-ci.yml",
            b"build:\n  script: [echo native]\n" as &[u8],
        )]);

        let config = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap();
        assert!(config.jobs.contains_key("build"));
    }

    #[test]
    fn workflow_not_matching_the_event_falls_back_to_the_native_config() {
        // Valid workflow, wrong event: still a legitimate fallback, not an error.
        let (temp, sha) = commit_repo(&[
            (
                ".gitea/workflows/tags.yml",
                b"on:\n  push:\n    tags: [v*]\njobs:\n  build:\n    steps:\n      - run: echo tagged\n" as &[u8],
            ),
            (".forgekeep-ci.yml", b"build:\n  script: [echo native]\n"),
        ]);

        let config = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap();
        assert!(config.jobs.contains_key("build"));
        assert!(!config.jobs.keys().any(|name| name.starts_with("tags/")));
    }

    #[test]
    fn untriggered_workflow_without_native_config_says_so_instead_of_no_config() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/tags.yml",
            b"on:\n  push:\n    tags: [v*]\njobs:\n  build:\n    steps:\n      - run: echo tagged\n"
                as &[u8],
        )]);

        let error = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap_err();
        let rendered = format!("{error}");
        assert!(
            rendered.contains(".gitea/workflows") && rendered.contains("refs/heads/main"),
            "error must point at the untriggered workflows and the ref: {rendered}"
        );
        assert!(
            !rendered.contains("no CI config found"),
            "a workflow that exists but is not triggered is not a missing config: {rendered}"
        );
    }

    #[test]
    fn no_config_at_all_still_reports_no_ci_config_found() {
        let (temp, sha) = commit_repo(&[("README.md", b"nothing to build\n" as &[u8])]);

        let error = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap_err();
        assert!(
            error.to_string().contains("no CI config found"),
            "genuinely missing config keeps its own message: {error:#}"
        );
    }

    #[test]
    fn merged_workflow_stages_are_ordered_deterministically() {
        let workflow = |job: &str| {
            format!("on: push\njobs:\n  {job}:\n    steps:\n      - run: echo {job}\n").into_bytes()
        };
        let (temp, sha) = commit_repo(&[
            (".gitea/workflows/a.yml", &workflow("first")),
            (".gitea/workflows/b.yml", &workflow("second")),
            (".gitea/workflows/c.yml", &workflow("third")),
        ]);

        let expected = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None)
            .unwrap()
            .stages
            .unwrap();
        assert_eq!(expected.len(), 3);
        for _ in 0..8 {
            let stages = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None)
                .unwrap()
                .stages
                .unwrap();
            assert_eq!(stages, expected, "stage order must not depend on hashing");
        }
    }

    /// card_074d93bfe327: the repository the whole defect is about — its entire
    /// CI is one `on: pull_request` workflow. Under the `push` event it has
    /// nothing to run (that is not a bug, it asked for PRs); under
    /// `pull_request` it must produce its jobs.
    #[test]
    fn a_pull_request_only_workflow_runs_for_the_pull_request_event() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/pr.yml",
            b"on: pull_request\njobs:\n  verify:\n    steps:\n      - run: echo reviewed\n"
                as &[u8],
        )]);

        let config = read_ci_config(
            temp.path(),
            &sha,
            "refs/pull/1/head",
            "pull_request",
            Some("main"),
        )
        .expect("an on: pull_request workflow must be selected by the pull_request event");
        assert!(
            config.jobs.contains_key("pr/verify"),
            "the workflow's job must be in the config: {:?}",
            config.jobs.keys().collect::<Vec<_>>()
        );

        // The same repository under `push` has nothing to offer, and says so
        // rather than claiming there is no CI config at all.
        let error = read_ci_config(temp.path(), &sha, "refs/heads/main", "push", None).unwrap_err();
        assert!(
            error.to_string().contains("pull_request") || error.to_string().contains("triggered"),
            "the push event must report an untriggered workflow: {error:#}"
        );
    }

    /// A `branches:` filter under `on: pull_request` is matched against the
    /// branch the PR *targets*. Nothing carried that branch into the matcher, so
    /// it fell back to the repository's default — and a PR into `develop` was
    /// judged as if it were a PR into `main`.
    #[test]
    fn the_pull_request_branch_filter_matches_the_base_branch_not_the_default_one() {
        let (temp, sha) = commit_repo(&[(
            ".gitea/workflows/pr.yml",
            b"on:\n  pull_request:\n    branches: [develop]\njobs:\n  verify:\n    steps:\n      - run: echo reviewed\n" as &[u8],
        )]);

        let config = read_ci_config(
            temp.path(),
            &sha,
            "refs/pull/7/head",
            "pull_request",
            Some("develop"),
        )
        .expect("a PR into develop must select the workflow filtered on develop");
        assert!(config.jobs.contains_key("pr/verify"));

        // The fixture's default branch is whatever `git init` produced, never
        // `develop`, so without the base branch the filter used to reject this.
        let error = read_ci_config(
            temp.path(),
            &sha,
            "refs/pull/7/head",
            "pull_request",
            Some("main"),
        )
        .unwrap_err();
        assert!(
            !format!("{error:#}").contains("no CI config found"),
            "a PR into another branch is untriggered, not configuration-less: {error:#}"
        );
    }

    /// The gate the producer asks before creating anything. It must answer for
    /// the *event*, not for "is there any CI here at all" — a repository with a
    /// native config and no workflow would otherwise get a duplicate pipeline on
    /// every PR open and every PR sync.
    #[test]
    fn the_event_gate_answers_per_event_not_per_repository() {
        let (workflow_repo, workflow_sha) = commit_repo(&[(
            ".gitea/workflows/pr.yml",
            b"on: pull_request\njobs:\n  verify:\n    steps:\n      - run: echo reviewed\n"
                as &[u8],
        )]);
        assert!(workflow_matches_event(
            workflow_repo.path(),
            &workflow_sha,
            "pull_request",
            "refs/pull/1/head",
            Some("main")
        )
        .unwrap());
        assert!(!workflow_matches_event(
            workflow_repo.path(),
            &workflow_sha,
            "push",
            "refs/heads/main",
            None
        )
        .unwrap());

        let (native_repo, native_sha) = commit_repo(&[(
            ".forgekeep-ci.yml",
            b"build:\n  script: [echo native]\n" as &[u8],
        )]);
        assert!(
            rg_core::ci::has_ci_config(native_repo.path(), &native_sha),
            "the fixture must look like a repository with CI"
        );
        assert!(
            !workflow_matches_event(
                native_repo.path(),
                &native_sha,
                "pull_request",
                "refs/pull/1/head",
                Some("main")
            )
            .unwrap(),
            "a native config declares no events — it must not answer for pull_request"
        );
    }

    #[test]
    fn manual_jobs_are_supported_and_invalid_execution_policies_fail_closed() {
        let mut job = config(BTreeMap::new());
        job.when = Some("manual".into());
        let config = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job.clone())]),
        };
        validate_execution_semantics(&config).unwrap();
        job.when = Some("delayed".into());
        let invalid_when = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job.clone())]),
        };
        assert!(validate_execution_semantics(&invalid_when).is_err());
        job.when = None;
        job.timeout_seconds = Some(0);
        let config = CiConfig {
            stages: Some(vec!["test".into()]),
            concurrency: None,
            jobs: HashMap::from([("deploy".into(), job)]),
        };
        assert!(validate_execution_semantics(&config).is_err());
    }
}
