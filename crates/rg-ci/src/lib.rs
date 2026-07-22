//! IronForge CI/CD Engine.
//!
//! Parses `.ironforge-ci.yml` or `.gitea/workflows/*.yml` (Gitea Actions format)
//! from the repository and executes pipelines.
//!
//! ## Native format (`.ironforge-ci.yml`)
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
        params.jwt_secret,
        params.external_url,
    );
    Ok(())
}

/// Trigger a CI pipeline for a push event.
///
/// This function:
/// 1. Reads `.ironforge-ci.yml` from the repo at the given commit
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
        triggered_by,
        docker_enabled,
        external_runners,
        jwt_secret,
        external_url,
    } = params;

    // 1. Read CI config from repo
    let config = read_ci_config(repo_path, commit_sha, ref_name, trigger_type)?;
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

async fn evaluate_initial_success(
    db: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    repo_id: i64,
    commit_sha: &str,
    docker_enabled: bool,
    external_runners: bool,
    jwt_secret: Option<&str>,
    external_url: Option<&str>,
) {
    let Some(repo_root) = repo_path.parent().and_then(std::path::Path::parent) else {
        return;
    };
    if let Err(error) =
        rg_core::pull_request::try_auto_merges_for_head_commit(db, repo_root, repo_id, commit_sha)
            .await
    {
        tracing::warn!(repo_id, %error, "auto-merge evaluation after conditional CI failed");
    }
    let ci_engine = CiEngine;
    if let Err(error) = rg_core::pull_request::merge_queue::process_for_head_commit_with_ci(
        db,
        repo_root,
        repo_id,
        commit_sha,
        &rg_core::pull_request::merge_queue::MergeQueueCi {
            trigger: &ci_engine,
            docker_enabled,
            external_runners,
            jwt_secret,
            external_url,
        },
    )
    .await
    {
        tracing::warn!(repo_id, %error, "merge queue evaluation after conditional CI failed");
    }
}

fn spawn_internal_runner(
    db: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    repo_id: i64,
    pipeline_id: i64,
    docker_enabled: bool,
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
        if let Some(secret) = jwt_secret_owned {
            runner.set_jwt_secret(secret);
        }
        if let Some(url) = oidc_token_url {
            runner.set_oidc_token_url(url);
        }
        if let Err(error) = runner.run().await {
            tracing::error!(pipeline_id, %error, "pipeline runner error");
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
/// 2. `.ironforge-ci.yml` (native format)
///
/// For Gitea Actions workflows, multiple files are merged into a single `CiConfig`.
/// Jobs from different workflow files are placed in separate stages.
fn read_ci_config(
    repo_path: &std::path::Path,
    commit_sha: &str,
    ref_name: &str,
    event: &str,
) -> Result<CiConfig> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    // Try Gitea Actions format first
    if let Ok(config) = try_read_gitea_workflows(&repo, commit_sha, ref_name, event) {
        tracing::info!("Using Gitea Actions workflow from .gitea/workflows/");
        return Ok(config);
    }

    // Fall back to native .ironforge-ci.yml
    let revspec = format!("{}:.ironforge-ci.yml", commit_sha);
    let object_id = repo.rev_parse_single(revspec.as_str()).map_err(|_| {
        anyhow::anyhow!(
            "no CI config found (.gitea/workflows/*.yml or .ironforge-ci.yml) at commit {}",
            commit_sha
        )
    })?;

    let object_id = object_id.object().context("failed to resolve object")?;
    let blob = object_id
        .try_into_blob()
        .context("expected a blob object for .ironforge-ci.yml")?;

    let ci_yml =
        String::from_utf8(blob.data.to_vec()).context(".ironforge-ci.yml is not valid UTF-8")?;

    let config: CiConfig = serde_yaml::from_str(&ci_yml)
        .with_context(|| format!("failed to parse .ironforge-ci.yml: {}", ci_yml))?;

    Ok(config)
}

/// Try to find and parse Gitea Actions workflow files in `.gitea/workflows/`.
fn try_read_gitea_workflows(
    repo: &gix::Repository,
    commit_sha: &str,
    ref_name: &str,
    event: &str,
) -> Result<CiConfig> {
    let tree_revspec = format!("{}:.gitea/workflows", commit_sha);
    let object_id = repo
        .rev_parse_single(tree_revspec.as_str())
        .map_err(|_| anyhow::anyhow!("no .gitea/workflows directory"))?;

    let object = object_id.object()?;
    let tree = object
        .try_into_tree()
        .map_err(|_| anyhow::anyhow!(".gitea/workflows is not a directory"))?;

    let default_branch = get_default_branch(repo)?;

    let mut all_jobs: std::collections::HashMap<String, config::JobConfig> =
        std::collections::HashMap::new();
    let mut all_stages: Vec<String> = Vec::new();
    let mut workflow_sources = std::collections::HashMap::new();

    // Load all workflow sources first so callers can resolve repository-local
    // reusable workflows from the same immutable commit tree.
    for entry in tree.iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let name = entry.filename().to_string();
        if !name.ends_with(".yml") && !name.ends_with(".yaml") {
            continue;
        }

        let entry_id = entry.oid();
        let entry_object = match repo.find_object(entry_id) {
            Ok(o) => o,
            Err(_) => continue,
        };
        let blob = match entry_object.try_into_blob() {
            Ok(b) => b,
            Err(_) => continue,
        };
        let yml = String::from_utf8(blob.data.to_vec()).unwrap_or_default();
        workflow_sources.insert(name, yml);
    }

    for (name, yml) in &workflow_sources {
        let workflow = match gitea_actions::GiteaWorkflow::parse(&yml) {
            Ok(w) => w,
            Err(e) => {
                tracing::debug!("Skipping .gitea/workflows/{} (parse error: {})", name, e);
                continue;
            }
        };

        // Check if this workflow should be triggered
        if !workflow.matches_event(event, ref_name, &default_branch) {
            continue;
        }
        let workflow = workflow
            .expand_local_reusable_workflows(&workflow_sources)
            .with_context(|| format!("failed to expand .gitea/workflows/{name}"))?;
        workflow
            .validate_supported_actions()
            .with_context(|| format!("unsupported workflow .gitea/workflows/{name}"))?;

        tracing::info!("Triggering workflow from .gitea/workflows/{}", name);

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
        anyhow::bail!("no matching workflows found in .gitea/workflows/");
    }

    Ok(CiConfig {
        stages: Some(all_stages),
        concurrency: None, // per-workflow concurrency not merged
        jobs: all_jobs,
    })
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
            temp.path().join(".ironforge-ci.yml"),
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
            .run(&["add", ".ironforge-ci.yml"], Some(temp.path()))
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
            triggered_by: Some(user.id),
            docker_enabled: false,
            external_runners: true,
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
            triggered_by: Some(user.id),
            docker_enabled: false,
            external_runners: true,
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
        let config = read_ci_config(temp.path(), &sha, "refs/heads/main", "push").unwrap();
        let job = config.jobs.get("main/shared/build").unwrap();
        assert!(job
            .script
            .iter()
            .any(|line| line.contains("${INPUT_TARGET}")));
        assert_eq!(job.variables.as_ref().unwrap()["INPUT_TARGET"], "staging");
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
