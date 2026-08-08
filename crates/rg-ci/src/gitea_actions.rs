//! Gitea Actions / GitHub Actions workflow compatibility layer.
//!
//! Parses `.gitea/workflows/*.yml` (GitHub Actions-compatible format) and
//! translates them into ForgeKeep's internal `CiConfig` model.
//!
//! Supported features:
//! - `on: push`, `on: pull_request` triggers with branch filtering
//! - `jobs.<id>.runs-on` → runner tags
//! - `jobs.<id>.steps[].run` → script commands
//! - workflow/job `defaults.run.working-directory` and per-step
//!   `working-directory` → isolated step directories
//! - `jobs.<id>.steps[].uses` → `actions/checkout` is implicit; other actions are rejected
//! - `jobs.<id>.container.image` → Docker image
//! - `jobs.<id>.env` → environment variables
//! - `jobs.<id>.needs` → stage ordering (implicit via dependency graph)
//! - Repository-local reusable workflows with `on: workflow_call`, inputs, inherited secrets, and dependency rewriting
//! - Basic `${{ }}` expression substitution

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

use crate::config::{CacheConfig, CiConfig, ConcurrencyConfig, JobConfig};

/// A parsed Gitea Actions workflow file.
#[derive(Debug, Clone, Deserialize)]
pub struct GiteaWorkflow {
    /// Workflow name (optional, defaults to filename).
    pub name: Option<String>,

    /// Event triggers.
    pub on: WorkflowTriggers,

    /// Job definitions.
    pub jobs: HashMap<String, GiteaJob>,

    /// Concurrency control (optional).
    pub concurrency: Option<GiteaConcurrency>,

    /// Workflow-level environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Defaults inherited by every `run:` step unless a job or step overrides
    /// them.
    pub defaults: Option<GiteaDefaults>,
}

/// Workflow trigger definitions.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum WorkflowTriggers {
    /// Simple trigger: `on: push`
    Simple(String),
    /// Single event with config: `on: { push: { branches: [main] } }`
    Single(Box<WorkflowTriggerSingle>),
    /// Array of event names: `on: [push, pull_request]`
    Array(Vec<String>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowTriggerSingle {
    /// `on:\n  push:` — a trigger with no filters — is the same declaration as
    /// `on: push`, and a plain `Option<EventFilter>` reads its empty value as
    /// *absent*. The matcher then answered "this workflow is not triggered by
    /// push" for a file that plainly asks for it. Same defect, same line shape,
    /// as the manual trigger in card_e87a1b6f9633; found by the round-trip test
    /// that asks the matcher about every event the canon names.
    #[serde(default, deserialize_with = "deserialize_present_filter")]
    pub push: Option<EventFilter>,
    #[serde(default, deserialize_with = "deserialize_present_filter")]
    pub pull_request: Option<EventFilter>,
    #[serde(
        rename = "pull_request_target",
        default,
        deserialize_with = "deserialize_present_filter"
    )]
    pub pull_request_target: Option<EventFilter>,
    pub schedule: Option<Vec<ScheduleTrigger>>,
    /// Manual runs. Read with the same "present, even if empty" deserializer as
    /// `workflow_call`: the usual spelling is a bare `workflow_dispatch:` with
    /// nothing under it, which plain `Option` turns into `None` — and a trigger
    /// that reads as absent is a Run button that answers "no workflow is
    /// triggered by this event" for a perfectly valid file (card_e87a1b6f9633).
    #[serde(default, deserialize_with = "deserialize_present_yaml")]
    pub workflow_dispatch: Option<serde_yaml::Value>,
    #[serde(default, deserialize_with = "deserialize_present_yaml")]
    pub workflow_call: Option<serde_yaml::Value>,
}

/// Event filter with optional branch/tag/path filtering.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct EventFilter {
    pub branches: Option<Vec<String>>,
    #[serde(rename = "branches-ignore")]
    pub branches_ignore: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
    #[serde(rename = "tags-ignore")]
    pub tags_ignore: Option<Vec<String>>,
    pub paths: Option<Vec<String>>,
    #[serde(rename = "paths-ignore")]
    pub paths_ignore: Option<Vec<String>>,
}

/// Schedule trigger with cron expression.
#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleTrigger {
    pub cron: String,
}

/// A Gitea Actions job definition.
#[derive(Debug, Clone, Deserialize)]
pub struct GiteaJob {
    /// Reusable workflow invocation. Repository-local workflow files are
    /// expanded before conversion; remote targets remain unsupported.
    pub uses: Option<String>,

    /// Inputs passed to a local reusable workflow.
    #[serde(default)]
    pub with: HashMap<String, String>,

    /// Reusable-workflow secret declaration (`inherit` is accepted implicitly
    /// because repository secrets are already scoped to every job).
    pub secrets: Option<serde_yaml::Value>,
    /// Runner label (e.g., `ubuntu-latest`, `self-hosted`).
    #[serde(rename = "runs-on")]
    pub runs_on: Option<serde_yaml::Value>,

    /// Job steps.
    #[serde(default)]
    pub steps: Vec<GiteaStep>,

    /// Defaults applied to `run:` steps.
    pub defaults: Option<GiteaDefaults>,

    /// Container specification.
    pub container: Option<GiteaContainer>,

    /// Job-level environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Dependencies (job names that must complete before this job).
    #[serde(default, deserialize_with = "deserialize_optional_string_or_vec")]
    pub needs: Option<Vec<String>>,

    /// Job condition (if expression).
    #[serde(rename = "if")]
    pub condition: Option<String>,

    /// Job timeout in minutes.
    #[serde(rename = "timeout-minutes")]
    pub timeout_minutes: Option<u64>,

    #[serde(rename = "continue-on-error", default)]
    pub continue_on_error: bool,

    /// Deployment environment, either a name or `{ name, url }` mapping.
    pub environment: Option<serde_yaml::Value>,

    /// Matrix expansion compatible with `strategy.matrix`.
    pub strategy: Option<GiteaStrategy>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GiteaStrategy {
    #[serde(default)]
    pub matrix: std::collections::BTreeMap<String, Vec<serde_yaml::Value>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GiteaDefaults {
    pub run: Option<GiteaRunDefaults>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GiteaRunDefaults {
    #[serde(rename = "working-directory")]
    pub working_directory: Option<String>,
    pub shell: Option<String>,
}

/// A step within a Gitea Actions job.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GiteaStep {
    /// Step name (optional).
    pub name: Option<String>,

    /// The action to use (e.g., `actions/checkout@v4`).
    #[serde(rename = "uses")]
    pub uses: Option<String>,

    /// Shell command to run.
    pub run: Option<String>,

    /// Shell to use (default: bash).
    pub shell: Option<String>,

    /// Directory for this `run:` step, relative to the workspace.
    #[serde(rename = "working-directory")]
    pub working_directory: Option<String>,

    /// Step identifiers and execution policies are parsed so unsupported
    /// semantics fail loudly in `validate_supported_actions`.
    pub id: Option<String>,
    #[serde(rename = "continue-on-error")]
    pub continue_on_error: Option<serde_yaml::Value>,
    #[serde(rename = "timeout-minutes")]
    pub timeout_minutes: Option<serde_yaml::Value>,

    /// Step-level environment variables.
    #[serde(default)]
    pub env: HashMap<String, String>,

    /// Step condition.
    #[serde(rename = "if")]
    pub condition: Option<String>,

    /// Input parameters for the action.
    #[serde(default)]
    pub with: HashMap<String, String>,
}

/// Container specification for a job.
#[derive(Debug, Clone, Deserialize)]
pub struct GiteaContainer {
    /// Docker image.
    pub image: String,

    /// Container options.
    pub options: Option<String>,

    /// Environment variables for the container.
    #[serde(default)]
    pub env: HashMap<String, String>,
}

/// Concurrency configuration (Gitea Actions format).
#[derive(Debug, Clone, Deserialize)]
pub struct GiteaConcurrency {
    pub group: String,
    #[serde(rename = "cancel-in-progress")]
    pub cancel_in_progress: Option<bool>,
}

/// Context for workflow expression evaluation.
pub struct WorkflowContext {
    pub ref_name: String,
    pub sha: String,
    pub event: String,
    pub repo_owner: String,
    pub repo_name: String,
}

impl GiteaWorkflow {
    /// Parse a Gitea Actions workflow YAML string.
    pub fn parse(yaml: &str) -> Result<Self> {
        let wf: GiteaWorkflow = serde_yaml::from_str(yaml)?;
        Ok(wf)
    }

    pub fn expand_local_reusable_workflows(
        &self,
        sources: &HashMap<String, String>,
    ) -> Result<Self> {
        let mut stack = Vec::new();
        let mut expanded = self.clone();
        expanded.jobs = expand_reusable_jobs(self, sources, 0, &mut stack)?;
        Ok(expanded)
    }

    fn is_reusable(&self) -> bool {
        match &self.on {
            WorkflowTriggers::Simple(name) => name == "workflow_call",
            WorkflowTriggers::Array(names) => names.iter().any(|name| name == "workflow_call"),
            WorkflowTriggers::Single(trigger) => trigger.workflow_call.is_some(),
        }
    }

    /// Reject workflows that would otherwise appear successful after silently
    /// dropping an action step. ForgeKeep's native `.forgekeep-ci.yml` format
    /// is the supported escape hatch for commands that do not have an Actions
    /// runtime.
    pub fn validate_supported_actions(&self) -> Result<()> {
        let mut unsupported = self
            .jobs
            .iter()
            .flat_map(|(job_name, job)| {
                job.steps.iter().filter_map(move |step| {
                    step.uses
                        .as_deref()
                        .filter(|uses| {
                            !uses.starts_with("actions/checkout@")
                                && !uses.starts_with("actions/cache@")
                        })
                        .map(|uses| format!("{job_name}: {uses}"))
                })
            })
            .collect::<Vec<_>>();
        if self
            .defaults
            .as_ref()
            .and_then(|defaults| defaults.run.as_ref())
            .and_then(|run| run.shell.as_ref())
            .is_some()
        {
            unsupported.push("defaults.run.shell".into());
        }
        unsupported.extend(self.jobs.iter().filter_map(|(job_name, job)| {
            job.uses
                .as_ref()
                .map(|uses| format!("{job_name}: reusable workflow {uses}"))
        }));
        unsupported.extend(self.jobs.iter().flat_map(|(job_name, job)| {
            let job_condition = job
                .condition
                .as_deref()
                .filter(|condition| !supported_condition(condition, true))
                .map(|condition| format!("{job_name}: unsupported job condition {condition}"));
            let step_conditions = job.steps.iter().filter_map(move |step| {
                step.condition
                    .as_deref()
                    .filter(|condition| !supported_condition(condition, false))
                    .map(|condition| format!("{job_name}: unsupported step condition {condition}"))
            });
            job_condition.into_iter().chain(step_conditions)
        }));
        unsupported.extend(self.jobs.iter().flat_map(|(job_name, job)| {
            let default_shell = job
                .defaults
                .as_ref()
                .and_then(|defaults| defaults.run.as_ref())
                .and_then(|run| run.shell.as_ref())
                .map(|_| format!("{job_name}: defaults.run.shell"));
            let step_features = job.steps.iter().enumerate().flat_map(move |(index, step)| {
                let prefix = format!("{job_name}: step {}", index + 1);
                [
                    step.shell.as_ref().map(|_| format!("{prefix} shell")),
                    step.id.as_ref().map(|_| format!("{prefix} id")),
                    step.continue_on_error
                        .as_ref()
                        .map(|_| format!("{prefix} continue-on-error")),
                    step.timeout_minutes
                        .as_ref()
                        .map(|_| format!("{prefix} timeout-minutes")),
                    (step.working_directory.is_some() && step.run.is_none())
                        .then(|| format!("{prefix} working-directory without run")),
                ]
                .into_iter()
                .flatten()
            });
            default_shell.into_iter().chain(step_features)
        }));
        if unsupported.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "unsupported workflow feature(s): {}. Convert them to run: commands or use .forgekeep-ci.yml",
                unsupported.join(", ")
            )
        }
    }

    /// Check if this workflow should be triggered for the given event and ref.
    ///
    /// `base_branch` is the branch the event targets: the PR's base for
    /// `pull_request` / `merge_group`, and the repository's default branch as
    /// the fallback for a caller that has no better answer. It is deliberately
    /// *not* the head ref — a `branches:` filter under `on: pull_request` is
    /// matched against the target of the PR, which is why `ref_name` is unused
    /// on that arm.
    pub fn matches_event(
        &self,
        event: &str,
        ref_name: &str,
        base_branch: &str,
        changed: &ChangedPaths<'_>,
    ) -> bool {
        match &self.on {
            WorkflowTriggers::Simple(name) => name.as_str() == event,
            WorkflowTriggers::Array(names) => names.iter().any(|n| n.as_str() == event),
            WorkflowTriggers::Single(trigger) => {
                let WorkflowTriggerSingle {
                    push,
                    pull_request,
                    pull_request_target: _,
                    schedule: _,
                    workflow_dispatch,
                    workflow_call: _,
                } = trigger.as_ref();
                match event {
                    "push" => {
                        if let Some(filter) = push {
                            ref_matches_filter(ref_name, filter, base_branch)
                                && paths_match_filter(filter, changed)
                        } else {
                            false
                        }
                    }
                    "pull_request" | "merge_group" => {
                        if let Some(filter) = pull_request {
                            // For pull_request events, GitHub/Gitea `branches` filters
                            // apply to the PR's base (target) branch, not the head ref.
                            let base_ref = format!("refs/heads/{base_branch}");
                            ref_matches_filter(&base_ref, filter, base_branch)
                                && paths_match_filter(filter, changed)
                        } else {
                            false
                        }
                    }
                    // A manual run carries no ref filter of its own — GitHub's
                    // `workflow_dispatch` takes `inputs`, not `branches` — so
                    // declaring the trigger *is* the match.
                    rg_core::ci::WORKFLOW_DISPATCH_EVENT => workflow_dispatch.is_some(),
                    _ => false,
                }
            }
        }
    }

    /// Convert this workflow into an ForgeKeep `CiConfig`.
    ///
    /// The conversion:
    /// - Groups jobs by their dependency order (needs) into stages
    /// - Extracts `run` commands from steps into `script`
    /// - Maps `runs-on` labels to `tags`
    /// - Translates `container.image` to `image`
    pub fn to_ci_config(&self, ctx: &WorkflowContext) -> CiConfig {
        let (job_stage, max_stage) = self.compute_job_stages();

        // Generate stage names
        let stages: Vec<String> = (0..=max_stage).map(|i| format!("stage-{}", i)).collect();

        // Convert each job
        let mut job_configs: HashMap<String, JobConfig> = HashMap::new();
        for (name, job) in &self.jobs {
            let (script, job_vars, cache) = self.build_job_script(name, job, ctx);

            let s = job_stage.get(name).copied().unwrap_or(0);
            let stage_name = format!("stage-{}", s);

            job_configs.insert(
                name.clone(),
                JobConfig {
                    stage: Some(stage_name),
                    script,
                    image: job.container.as_ref().map(|c| c.image.clone()),
                    only: None, // filtering is done at trigger time
                    variables: if job_vars.is_empty() {
                        None
                    } else {
                        Some(job_vars)
                    },
                    when: None,
                    condition: job.condition.clone(),
                    environment: job.environment.as_ref().and_then(environment_name),
                    allow_failure: Some(job.continue_on_error),
                    // Saturating on both hops on purpose: an absurd
                    // `timeout-minutes` has to arrive at the validator as an
                    // absurd number of seconds and be refused by name. A
                    // wrapping `as i64` would have turned it negative, i.e.
                    // into a value the validator used to wave through.
                    timeout_seconds: job.timeout_minutes.map(|minutes| {
                        i64::try_from(minutes.saturating_mul(60)).unwrap_or(i64::MAX)
                    }),
                    tags: runs_on_tags(&job.runs_on),
                    matrix: job.strategy.as_ref().map(|strategy| {
                        strategy
                            .matrix
                            .iter()
                            .map(|(key, values)| {
                                let values = values.iter().filter_map(yaml_scalar_string).collect();
                                (key.clone(), values)
                            })
                            .collect()
                    }),
                    cache,
                },
            );
        }

        CiConfig {
            stages: Some(stages),
            concurrency: self.concurrency.as_ref().map(|c| ConcurrencyConfig {
                group: c.group.clone(),
                cancel_in_progress: c.cancel_in_progress.unwrap_or(false),
            }),
            jobs: job_configs,
        }
    }

    /// Assign each job to a stage index via a simple BFS topological pass:
    /// a job's stage is `max(stage of its `needs`) + 1`, or 0 when it has no
    /// dependencies. Jobs caught in a dependency cycle fall back to stage 0.
    /// Returns the per-job stage map and the highest stage index used.
    fn compute_job_stages(&self) -> (HashMap<String, usize>, usize) {
        // Build dependency graph to determine stage ordering
        let mut job_deps: HashMap<&str, Vec<&str>> = HashMap::new();
        let mut job_order: Vec<&str> = Vec::new();

        for (name, job) in &self.jobs {
            job_order.push(name.as_str());
            if let Some(ref needs) = job.needs {
                job_deps.insert(name.as_str(), needs.iter().map(|s| s.as_str()).collect());
            } else {
                job_deps.insert(name.as_str(), vec![]);
            }
        }

        let mut job_stage: HashMap<String, usize> = HashMap::new();
        let mut max_stage = 0usize;

        // Iterate until all jobs have a stage assigned
        let mut remaining = job_order.clone();
        let mut changed = true;
        while changed && !remaining.is_empty() {
            changed = false;
            let mut next_remaining: Vec<&str> = Vec::new();
            for &name in &remaining {
                let deps = job_deps.get(name).map(|v| v.as_slice()).unwrap_or(&[]);
                if deps.is_empty() || deps.iter().all(|d| job_stage.contains_key(*d)) {
                    let s = if deps.is_empty() {
                        0
                    } else {
                        deps.iter()
                            .map(|d| job_stage.get(*d).copied().unwrap_or(0))
                            .max()
                            .unwrap_or(0)
                            + 1
                    };
                    job_stage.insert(name.to_string(), s);
                    max_stage = max_stage.max(s);
                    changed = true;
                } else {
                    next_remaining.push(name);
                }
            }
            remaining = next_remaining;
        }

        // Assign any remaining jobs to stage 0 (circular deps or self-references)
        for name in remaining {
            job_stage.entry(name.to_string()).or_insert(0);
        }

        (job_stage, max_stage)
    }

    /// Build the shell `script`, resolved job variables, and optional cache
    /// config for a single job: expands workflow/job env, then translates each
    /// step (`uses: checkout` → implicit, `uses: cache` → `CacheConfig`, other
    /// `uses:` → hard failure, `run:` → exported env + command).
    fn build_job_script(
        &self,
        job_name: &str,
        job: &GiteaJob,
        ctx: &WorkflowContext,
    ) -> (Vec<String>, HashMap<String, String>, Option<CacheConfig>) {
        // GitHub's default bash invocation is fail-fast; preserving this
        // prevents a later successful step from masking an earlier failure.
        let mut script: Vec<String> = vec!["set -e".into()];
        let mut job_vars: HashMap<String, String> = HashMap::new();
        let mut cache = None;

        // Copy workflow-level env
        for (k, v) in &self.env {
            job_vars.insert(k.clone(), substitute_expr(v, job_name, &self.env, &job.env));
        }
        // Copy job-level env
        for (k, v) in &job.env {
            job_vars.insert(k.clone(), substitute_expr(v, job_name, &self.env, &job.env));
        }

        if let Some(uses) = &job.uses {
            script.push(format!(
                "echo \"ForgeKeep does not support reusable workflow '{}'; use explicit jobs or .forgekeep-ci.yml\" >&2; exit 78",
                uses
            ));
        }

        // Process steps
        let mut has_checkout = false;
        let default_working_directory = job
            .defaults
            .as_ref()
            .and_then(|defaults| defaults.run.as_ref())
            .and_then(|run| run.working_directory.as_deref())
            .or_else(|| {
                self.defaults
                    .as_ref()
                    .and_then(|defaults| defaults.run.as_ref())
                    .and_then(|run| run.working_directory.as_deref())
            });
        for step in &job.steps {
            if let Some(condition) = step.condition.as_deref() {
                let mut condition_variables = job_vars.clone();
                for (name, value) in &step.env {
                    condition_variables.insert(
                        name.clone(),
                        substitute_expr(value, name, &self.env, &job.env),
                    );
                }
                let context = actions_condition_context(ctx, &condition_variables);
                if !crate::condition::evaluate_condition(condition, &context).unwrap_or(false) {
                    continue;
                }
            }
            // Handle `uses: actions/checkout@vX` — implicit in ForgeKeep, skip
            if let Some(ref uses) = step.uses {
                if uses.starts_with("actions/checkout") {
                    has_checkout = true;
                    continue;
                }
                if uses.starts_with("actions/cache@") {
                    if let (Some(path), Some(key)) = (step.with.get("path"), step.with.get("key")) {
                        let paths = path
                            .lines()
                            .map(str::trim)
                            .filter(|path| !path.is_empty())
                            .map(str::to_owned)
                            .collect::<Vec<_>>();
                        cache = Some(CacheConfig {
                            key: substitute_expr(key, job_name, &self.env, &job_vars),
                            paths,
                        });
                    } else {
                        script.push(
                            "echo \"actions/cache requires both 'path' and 'key'\" >&2; exit 78"
                                .into(),
                        );
                    }
                    continue;
                }
                // Direct callers should still fail visibly even if they
                // skipped `validate_supported_actions`.
                script.push(format!(
                    "echo \"ForgeKeep does not support action '{}'; use run: or .forgekeep-ci.yml\" >&2; exit 78",
                    uses
                ));
                continue;
            }

            // Handle `run:` commands
            if let Some(ref run_cmd) = step.run {
                // Copy step-level env
                for (k, v) in &step.env {
                    let expanded = substitute_expr(v, job_name, &self.env, &job_vars);
                    script.push(format!("export {}={}", k, expanded));
                }

                // Substitute expressions in the command
                let expanded_cmd = substitute_expr(run_cmd, job_name, &self.env, &job_vars);
                let working_directory = step
                    .working_directory
                    .as_deref()
                    .or(default_working_directory)
                    .map(|directory| substitute_expr(directory, job_name, &self.env, &job_vars));
                script.push(match working_directory {
                    Some(directory) => command_in_working_directory(&expanded_cmd, &directory),
                    None => expanded_cmd,
                });
            }
        }

        // If no checkout step was found, add a comment
        if !has_checkout && !script.is_empty() {
            script.insert(
                0,
                "# [ForgeKeep] Repository is already checked out at /workspace".to_string(),
            );
        }

        (script, job_vars, cache)
    }
}

/// Run one Actions step in a subshell so its directory cannot leak into the
/// following step. Expected runtime placeholders such as `${MATRIX_OS}` remain
/// expandable while shell metacharacters in the configured path stay quoted.
fn command_in_working_directory(command: &str, directory: &str) -> String {
    format!("(\ncd -- {}\n{}\n)", shell_double_quote(directory), command)
}

fn shell_double_quote(value: &str) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    let mut index = 0;
    while index < chars.len() {
        match chars[index] {
            '$' if index + 3 < chars.len() && chars[index + 1] == '{' => {
                let Some(relative_end) = chars[index + 2..].iter().position(|c| *c == '}') else {
                    quoted.push_str("\\$");
                    index += 1;
                    continue;
                };
                let end = index + 2 + relative_end;
                let name = &chars[index + 2..end];
                let valid_name = name
                    .first()
                    .is_some_and(|c| c.is_ascii_uppercase() || *c == '_')
                    && name
                        .iter()
                        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == '_');
                if valid_name {
                    quoted.extend(&chars[index..=end]);
                    index = end + 1;
                } else {
                    quoted.push_str("\\$");
                    index += 1;
                }
            }
            '\\' => {
                quoted.push_str("\\\\");
                index += 1;
            }
            '"' => {
                quoted.push_str("\\\"");
                index += 1;
            }
            '`' => {
                quoted.push_str("\\`");
                index += 1;
            }
            '$' => {
                quoted.push_str("\\$");
                index += 1;
            }
            character => {
                quoted.push(character);
                index += 1;
            }
        }
    }
    quoted.push('"');
    quoted
}

/// Map a job's `runs-on` value to ForgeKeep runner tags: a scalar becomes a
/// single tag, a sequence becomes the list of its string entries. Returns
/// `None` when absent, non-string, or an empty sequence.
fn runs_on_tags(runs_on: &Option<serde_yaml::Value>) -> Option<Vec<String>> {
    match runs_on {
        Some(serde_yaml::Value::String(s)) => Some(vec![s.clone()]),
        Some(serde_yaml::Value::Sequence(arr)) => {
            let tags: Vec<String> = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
            if tags.is_empty() {
                None
            } else {
                Some(tags)
            }
        }
        _ => None,
    }
}

fn environment_name(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(name) => Some(name.clone()),
        serde_yaml::Value::Mapping(mapping) => mapping
            .get(serde_yaml::Value::String("name".into()))
            .and_then(serde_yaml::Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

fn expand_reusable_jobs(
    workflow: &GiteaWorkflow,
    sources: &HashMap<String, String>,
    depth: usize,
    stack: &mut Vec<String>,
) -> Result<HashMap<String, GiteaJob>> {
    if depth > 4 {
        anyhow::bail!("reusable workflow nesting exceeds the maximum depth of 4");
    }
    let mut jobs = HashMap::new();
    let mut expansion: HashMap<String, Vec<String>> = HashMap::new();

    for (name, original) in &workflow.jobs {
        let Some(uses) = original.uses.as_deref() else {
            let mut job = original.clone();
            for (key, value) in &workflow.env {
                job.env.entry(key.clone()).or_insert_with(|| value.clone());
            }
            jobs.insert(name.clone(), job);
            expansion.insert(name.clone(), vec![name.clone()]);
            continue;
        };

        let target = uses
            .strip_prefix("./.gitea/workflows/")
            .ok_or_else(|| anyhow::anyhow!("only repository-local reusable workflows under .gitea/workflows/ are supported: {uses}"))?;
        if target.is_empty() || target.contains('/') || target.contains("..") {
            anyhow::bail!("invalid local reusable workflow path: {uses}");
        }
        if stack.iter().any(|entry| entry == target) {
            anyhow::bail!(
                "reusable workflow cycle detected: {} -> {target}",
                stack.join(" -> ")
            );
        }
        if let Some(secrets) = &original.secrets {
            if secrets.as_str() != Some("inherit") {
                anyhow::bail!("reusable workflow '{name}' supports only `secrets: inherit`; named secret remapping is not supported");
            }
        }
        let source = sources
            .get(target)
            .ok_or_else(|| anyhow::anyhow!("local reusable workflow not found: {uses}"))?;
        let called = GiteaWorkflow::parse(source).map_err(|error| {
            anyhow::anyhow!("failed to parse reusable workflow {target}: {error}")
        })?;
        if !called.is_reusable() {
            anyhow::bail!("workflow {target} is not reusable; declare `on: workflow_call`");
        }
        stack.push(target.to_owned());
        let called_jobs = expand_reusable_jobs(&called, sources, depth + 1, stack)?;
        stack.pop();

        let depended_on = called_jobs
            .values()
            .flat_map(|job| job.needs.clone().unwrap_or_default())
            .collect::<std::collections::HashSet<_>>();
        let leaves = called_jobs
            .keys()
            .filter(|job_name| !depended_on.contains(*job_name))
            .cloned()
            .collect::<Vec<_>>();
        let roots = called_jobs
            .iter()
            .filter(|(_, job)| job.needs.as_ref().is_none_or(Vec::is_empty))
            .map(|(job_name, _)| job_name.clone())
            .collect::<std::collections::HashSet<_>>();

        for (child_name, mut child) in called_jobs {
            child.needs = child.needs.map(|needs| {
                needs
                    .into_iter()
                    .map(|dependency| format!("{name}/{dependency}"))
                    .collect()
            });
            if roots.contains(&child_name) {
                child.needs = original.needs.clone();
            }
            for (input, value) in &original.with {
                let env_name = format!("INPUT_{}", input.to_ascii_uppercase().replace('-', "_"));
                child.env.insert(env_name, value.clone());
            }
            jobs.insert(format!("{name}/{child_name}"), child);
        }
        expansion.insert(
            name.clone(),
            leaves
                .into_iter()
                .map(|leaf| format!("{name}/{leaf}"))
                .collect(),
        );
    }

    for job in jobs.values_mut() {
        if let Some(needs) = job.needs.take() {
            let mut rewritten = Vec::new();
            for dependency in needs {
                if let Some(leaves) = expansion.get(&dependency) {
                    rewritten.extend(leaves.clone());
                } else {
                    rewritten.push(dependency);
                }
            }
            rewritten.sort();
            rewritten.dedup();
            job.needs = (!rewritten.is_empty()).then_some(rewritten);
        }
    }
    Ok(jobs)
}

fn deserialize_optional_string_or_vec<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        String(String),
        Vec(Vec<String>),
    }
    Ok(match Option::<StringOrVec>::deserialize(deserializer)? {
        Some(StringOrVec::String(value)) => Some(vec![value]),
        Some(StringOrVec::Vec(values)) => Some(values),
        None => None,
    })
}

/// Read a trigger key that may carry no filters at all.
///
/// `on:\n  push:` is a *present* trigger with an empty body; the default
/// `Option<EventFilter>` deserializer turns that into `None`, which the matcher
/// cannot tell from "this workflow does not ask for push".
fn deserialize_present_filter<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<EventFilter>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Some(
        Option::<EventFilter>::deserialize(deserializer)?.unwrap_or_default(),
    ))
}

fn deserialize_present_yaml<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<serde_yaml::Value>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    serde_yaml::Value::deserialize(deserializer).map(Some)
}

/// Check if a ref matches an event filter.
fn ref_matches_filter(ref_name: &str, filter: &EventFilter, default_branch: &str) -> bool {
    // Extract branch name from ref (e.g., "refs/heads/main" → "main")
    let branch = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);

    // Check branches filter
    if let Some(ref branches) = filter.branches {
        if !branches
            .iter()
            .any(|pattern| match_branch_pattern(branch, pattern, default_branch))
        {
            return false;
        }
    }

    // Check branches-ignore filter
    if let Some(ref ignored) = filter.branches_ignore {
        if ignored
            .iter()
            .any(|pattern| match_branch_pattern(branch, pattern, default_branch))
        {
            return false;
        }
    }

    // Check tags filter
    let is_tag = ref_name.starts_with("refs/tags/");
    let tag = ref_name.strip_prefix("refs/tags/").unwrap_or(ref_name);
    if let Some(ref tags) = filter.tags {
        if !is_tag || !tags.iter().any(|p| match_glob(tag, p)) {
            return false;
        }
    }

    // …and its mirror, which was declared and read by nobody: a workflow that
    // asked to skip release tags ran on every one of them (card_e1e76c3ede65).
    // Only a tag ref can be excluded by it — a branch push is not "a tag that
    // was not ignored".
    if let Some(ref ignored) = filter.tags_ignore {
        if is_tag && ignored.iter().any(|p| match_glob(tag, p)) {
            return false;
        }
    }

    true
}

/// Apply the `paths` / `paths-ignore` filters of one trigger.
///
/// Both were parsed and dropped on the floor, and this half of the defect has
/// the opposite sign from a filter that fails to *widen*: `paths:` is how a
/// workflow says "only when this part of the tree changes", so ignoring it does
/// not skip work the author asked for — it runs work the author asked to skip.
/// On a monorepo with a heavy `paths: [backend/**]` workflow, every README
/// commit paid for a full run (card_e1e76c3ede65).
///
/// A trigger that declares neither filter matches, without ever asking what
/// changed — the diff is computed lazily, so unfiltered workflows cost nothing.
fn paths_match_filter(filter: &EventFilter, changed: &ChangedPaths<'_>) -> bool {
    if filter.paths.is_none() && filter.paths_ignore.is_none() {
        return true;
    }

    let Some(changed) = changed.get() else {
        // The filter cannot be evaluated (no repository to diff, or the diff
        // failed). Running is what this server did before the filters were read
        // at all, and it is the safe direction: a skipped pipeline is a missing
        // required check, while an extra one only costs time.
        tracing::warn!(
            "a path filter could not be evaluated for this commit; running the workflow rather \
             than skipping it"
        );
        return true;
    };

    if let Some(patterns) = &filter.paths {
        if !changed
            .iter()
            .any(|path| patterns.iter().any(|p| match_path_pattern(path, p)))
        {
            return false;
        }
    }

    if let Some(patterns) = &filter.paths_ignore {
        // GitHub's rule: the workflow runs when *at least one* changed file is
        // outside the ignore list. A commit that only touches ignored paths is
        // the one that gets skipped.
        if !changed
            .iter()
            .any(|path| !patterns.iter().any(|p| match_path_pattern(path, p)))
        {
            return false;
        }
    }

    true
}

/// Match a branch name against a pattern (supports `*` wildcard and `**`).
fn match_branch_pattern(branch: &str, pattern: &str, _default_branch: &str) -> bool {
    match pattern {
        // Special case: pattern is empty (shouldn't happen but guard)
        "" => branch.is_empty(),
        // Exact match
        p if !p.contains('*') => branch == p,
        // Glob match
        p => match_glob(branch, p),
    }
}

/// The files a commit changed, computed on demand.
///
/// Lazy because most workflows declare no path filter at all, and a tree diff
/// per push for a question nobody asked would be pure cost. Once computed it is
/// shared by every workflow file in the same run.
///
/// `None` from [`ChangedPaths::get`] means "cannot be determined" — no
/// repository to diff against, or the diff itself failed — and the filter falls
/// back to running, which is what this server did before the filters were read
/// at all.
pub struct ChangedPaths<'a> {
    source: Option<ChangedPathsSource<'a>>,
    resolved: std::cell::OnceCell<Option<Vec<String>>>,
}

struct ChangedPathsSource<'a> {
    repo: &'a gix::Repository,
    /// Where the ref stood before this push. `None` falls back to the commit's
    /// first parent, which is the same answer for a merge commit or a
    /// single-commit push and an under-approximation for a fast-forward of
    /// several commits — the honest limit of what the caller handed over.
    previous_sha: Option<&'a str>,
    commit_sha: &'a str,
}

impl<'a> ChangedPaths<'a> {
    /// For a caller with no repository — a parser test, or a matcher question
    /// asked outside a commit. Path filters fall back to running.
    pub fn unknown() -> Self {
        Self {
            source: None,
            resolved: std::cell::OnceCell::new(),
        }
    }

    pub fn of_commit(
        repo: &'a gix::Repository,
        previous_sha: Option<&'a str>,
        commit_sha: &'a str,
    ) -> Self {
        Self {
            source: Some(ChangedPathsSource {
                repo,
                previous_sha,
                commit_sha,
            }),
            resolved: std::cell::OnceCell::new(),
        }
    }

    fn get(&self) -> Option<&[String]> {
        self.resolved
            .get_or_init(|| {
                let source = self.source.as_ref()?;
                match changed_paths_between(source) {
                    Ok(paths) => Some(paths),
                    Err(error) => {
                        tracing::warn!(
                            commit = source.commit_sha,
                            "cannot list the paths this commit changed, so its workflows' path \
                             filters are not applied: {error:#}"
                        );
                        None
                    }
                }
            })
            .as_deref()
    }
}

fn changed_paths_between(source: &ChangedPathsSource<'_>) -> Result<Vec<String>> {
    use gix::bstr::ByteSlice;

    let repo = source.repo;
    let commit = repo
        .rev_parse_single(source.commit_sha)
        .with_context(|| format!("commit not found: {}", source.commit_sha))?
        .object()?
        .peel_to_commit()
        .with_context(|| format!("{} is not a commit", source.commit_sha))?;
    let new_tree = commit.tree()?;

    // The zero sha is how the git protocol spells "this ref did not exist", so
    // it is a branch being created rather than a revision to diff against.
    let previous = source
        .previous_sha
        .filter(|sha| !sha.chars().all(|c| c == '0'))
        .map(|sha| {
            repo.rev_parse_single(sha)
                .with_context(|| format!("previous commit not found: {sha}"))
                .and_then(|id| Ok(id.object()?.peel_to_commit()?.tree()?))
        })
        .transpose()?
        .or_else(|| {
            commit
                .parent_ids()
                .next()
                .and_then(|id| id.object().ok()?.peel_to_commit().ok()?.tree().ok())
        });

    let Some(old_tree) = previous else {
        // A root commit changed every file it contains.
        let mut recorder = gix::traverse::tree::Recorder::default();
        new_tree.traverse().breadthfirst(&mut recorder)?;
        return Ok(recorder
            .records
            .into_iter()
            .filter(|entry| !entry.mode.is_tree())
            .map(|entry| entry.filepath.to_str_lossy().to_string())
            .collect());
    };

    let mut platform = old_tree.changes()?;
    platform.options(|options| {
        options.track_rewrites(None);
    });

    let mut paths = Vec::new();
    {
        let sink = &mut paths;
        platform.for_each_to_obtain_tree(
            &new_tree,
            |change| -> Result<std::ops::ControlFlow<()>, anyhow::Error> {
                let is_tree = match &change {
                    gix::object::tree::diff::Change::Addition { entry_mode, .. }
                    | gix::object::tree::diff::Change::Deletion { entry_mode, .. } => {
                        entry_mode.is_tree()
                    }
                    gix::object::tree::diff::Change::Modification {
                        previous_entry_mode,
                        entry_mode,
                        ..
                    } => previous_entry_mode.is_tree() || entry_mode.is_tree(),
                    gix::object::tree::diff::Change::Rewrite {
                        source_entry_mode,
                        entry_mode,
                        ..
                    } => source_entry_mode.is_tree() || entry_mode.is_tree(),
                };
                if !is_tree {
                    sink.push(change.location().to_str_lossy().to_string());
                }
                Ok(std::ops::ControlFlow::Continue(()))
            },
        )?;
    }
    Ok(paths)
}

/// Match one repository-relative path against one filter pattern.
///
/// Separate from [`match_glob`], which matches branch and tag names: those have
/// no meaningful path separator, while a path pattern's whole vocabulary is
/// built around one. GitHub's rules — `*` matches any run of characters except
/// `/`, `**` matches any run including `/`, `?` matches a single character —
/// and a pattern ending in `/` or `/**` covers everything under that directory.
fn match_path_pattern(path: &str, pattern: &str) -> bool {
    // `docs/` and `docs/**` both mean "everything under docs".
    if let Some(dir) = pattern.strip_suffix("/**").or(pattern.strip_suffix('/')) {
        if path == dir || path.starts_with(&format!("{dir}/")) {
            return true;
        }
    }
    glob_segments(path.as_bytes(), pattern.as_bytes())
}

/// Backtracking matcher for `*`, `**` and `?` over a path.
fn glob_segments(path: &[u8], pattern: &[u8]) -> bool {
    match pattern.first() {
        None => path.is_empty(),
        Some(b'*') => {
            if pattern.get(1) == Some(&b'*') {
                // `**` spans separators. Skipping an optional `/` right after it
                // is what makes `**/x.rs` match a top-level `x.rs`, exactly as
                // GitHub documents.
                let rest = &pattern[2..];
                let rest = rest.strip_prefix(b"/").unwrap_or(rest);
                (0..=path.len()).any(|skip| glob_segments(&path[skip..], rest))
                    || glob_segments(path, rest)
            } else {
                let rest = &pattern[1..];
                // A single `*` stops at the separator.
                let bound = path.iter().position(|b| *b == b'/').unwrap_or(path.len());
                (0..=bound).any(|skip| glob_segments(&path[skip..], rest))
            }
        }
        Some(b'?') => {
            !path.is_empty() && path[0] != b'/' && glob_segments(&path[1..], &pattern[1..])
        }
        Some(expected) => {
            !path.is_empty() && path[0] == *expected && glob_segments(&path[1..], &pattern[1..])
        }
    }
}

/// Match a branch or tag name against a `branches:` / `tags:` pattern.
///
/// The same backtracking matcher the path filters use, on a ref name instead of
/// a file path. A ref is hierarchical for exactly the same reason a path is
/// (`release/1.0`, `v1/rc`), and GitHub draws the `*` / `**` distinction on
/// `/` in both.
///
/// It used to be a ladder of `starts_with` / `ends_with` special cases, and a
/// pattern with a star anywhere but at one end fell through it to `false`:
/// `releases/**` was tested as `starts_with("releases/*")`, and `*-rc*` — the
/// shape every `tags-ignore` is written in — was tested as
/// `starts_with("*-rc")`. Both are workflows that read as configured and match
/// nothing. (The `*middle*` arm that was meant to catch the second could never
/// be reached: the `strip_suffix('*')` arm above it answered first.)
fn match_glob(s: &str, pattern: &str) -> bool {
    glob_segments(s.as_bytes(), pattern.as_bytes())
}

fn supported_condition(condition: &str, allow_matrix: bool) -> bool {
    crate::condition::validate_condition(condition).is_ok()
        && (allow_matrix || !condition.contains("matrix."))
}

fn actions_condition_context(
    ctx: &WorkflowContext,
    variables: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut context = HashMap::from([
        ("github.ref".into(), ctx.ref_name.clone()),
        (
            "github.ref_name".into(),
            ctx.ref_name
                .strip_prefix("refs/heads/")
                .or_else(|| ctx.ref_name.strip_prefix("refs/tags/"))
                .unwrap_or(&ctx.ref_name)
                .to_string(),
        ),
        ("github.event_name".into(), ctx.event.clone()),
        ("github.sha".into(), ctx.sha.clone()),
    ]);
    for (name, value) in variables {
        context.insert(format!("env.{name}"), value.clone());
    }
    context
}

/// Basic `${{ expression }}` substitution.
fn substitute_expr(
    input: &str,
    _job_name: &str,
    workflow_env: &HashMap<String, String>,
    job_env: &HashMap<String, String>,
) -> String {
    let mut result = input.to_string();

    // Substitute ${{ env.VAR }} and ${{ vars.VAR }}
    for (key, value) in workflow_env.iter().chain(job_env.iter()) {
        let pattern = format!("${{{{ env.{} }}}}", key);
        result = result.replace(&pattern, value);
        let pattern2 = format!("${{{{ vars.{} }}}}", key);
        result = result.replace(&pattern2, value);
    }

    // Handle common built-in expressions
    result = result.replace("${{ github.ref }}", "${CI_REF}");
    result = result.replace("${{ github.sha }}", "${CI_SHA}");
    result = result.replace("${{ github.event_name }}", "${CI_EVENT}");

    result = replace_context_expression(result, "secrets", |name| format!("${{{name}}}"));
    result = replace_context_expression(result, "matrix", |name| {
        format!(
            "${{MATRIX_{}}}",
            name.to_ascii_uppercase().replace('-', "_")
        )
    });
    result = replace_context_expression(result, "inputs", |name| {
        format!("${{INPUT_{}}}", name.to_ascii_uppercase().replace('-', "_"))
    });

    result
}

fn yaml_scalar_string(value: &serde_yaml::Value) -> Option<String> {
    match value {
        serde_yaml::Value::String(v) => Some(v.clone()),
        serde_yaml::Value::Bool(v) => Some(v.to_string()),
        serde_yaml::Value::Number(v) => Some(v.to_string()),
        _ => None,
    }
}

fn replace_context_expression(
    mut input: String,
    context: &str,
    replacement: impl Fn(&str) -> String,
) -> String {
    let prefix = format!("${{{{ {context}.");
    while let Some(start) = input.find(&prefix) {
        let name_start = start + prefix.len();
        let Some(relative_end) = input[name_start..].find(" }}") else {
            break;
        };
        let end = name_start + relative_end;
        let name = input[name_start..end].trim();
        if name.is_empty() {
            break;
        }
        input.replace_range(start..end + 3, &replacement(name));
    }
    input
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> WorkflowContext {
        WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc123".into(),
            event: "push".into(),
            repo_owner: "owner".into(),
            repo_name: "repo".into(),
        }
    }

    #[test]
    fn test_parse_simple_workflow() {
        let yml = r#"
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    environment:
      name: production
      url: https://example.invalid
    steps:
      - uses: actions/checkout@v4
      - name: Build
        run: cargo build --release
      - name: Test
        run: cargo test
"#;
        let wf = GiteaWorkflow::parse(yml).unwrap();
        wf.validate_supported_actions().unwrap();
        assert_eq!(wf.name.as_deref(), Some("CI"));
        assert!(wf.matches_event("push", "refs/heads/main", "main", &ChangedPaths::unknown()));

        let ctx = WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc123".into(),
            event: "push".into(),
            repo_owner: "owner".into(),
            repo_name: "repo".into(),
        };
        let ci_config = wf.to_ci_config(&ctx);
        assert_eq!(ci_config.jobs.len(), 1);

        let build = ci_config.jobs.get("build").unwrap();
        assert_eq!(build.image, None);
        assert_eq!(build.environment.as_deref(), Some("production"));
        assert!(build.script.len() > 1);
        // checkout should be skipped, build and test run commands present
        let script_str = build.script.join("\n");
        assert!(script_str.contains("cargo build --release"));
        assert!(script_str.contains("cargo test"));
    }

    #[test]
    fn unsupported_actions_are_rejected_instead_of_silently_skipped() {
        let yml = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
"#;
        let workflow = GiteaWorkflow::parse(yml).unwrap();
        let error = workflow.validate_supported_actions().unwrap_err();
        assert!(error.to_string().contains("actions/setup-node@v4"));
        assert!(error.to_string().contains(".forgekeep-ci.yml"));
    }

    #[cfg(unix)]
    #[test]
    fn working_directories_execute_in_isolated_step_subshells() {
        let workspace = tempfile::tempdir().unwrap();
        for directory in ["step dir", "default dir", "override dir"] {
            std::fs::create_dir(workspace.path().join(directory)).unwrap();
        }
        std::fs::write(workspace.path().join("workspace-marker"), "ok").unwrap();
        std::fs::write(workspace.path().join("step dir/local-marker"), "ok").unwrap();
        std::fs::write(workspace.path().join("default dir/default-marker"), "ok").unwrap();
        std::fs::write(workspace.path().join("override dir/override-marker"), "ok").unwrap();

        let workflow = GiteaWorkflow::parse(
            r#"
on: push
jobs:
  isolated:
    steps:
      - run: test -f local-marker
        working-directory: step dir
      - run: test -f workspace-marker
  defaults:
    defaults:
      run:
        working-directory: default dir
    steps:
      - run: test -f default-marker
      - run: test -f override-marker
        working-directory: override dir
"#,
        )
        .unwrap();
        workflow.validate_supported_actions().unwrap();
        let config = workflow.to_ci_config(&test_context());

        for job_name in ["isolated", "defaults"] {
            let script = config.jobs[job_name].script.join("\n");
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .current_dir(workspace.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{job_name} script failed:\n{script}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn working_directory_precedence_is_step_then_job_then_workflow() {
        let workspace = tempfile::tempdir().unwrap();
        for directory in ["workflow dir", "job dir", "step dir"] {
            std::fs::create_dir(workspace.path().join(directory)).unwrap();
            std::fs::write(workspace.path().join(directory).join("marker"), "ok").unwrap();
        }

        let workflow = GiteaWorkflow::parse(
            r#"
on: push
defaults:
  run:
    working-directory: workflow dir
jobs:
  inherited:
    steps:
      - run: test -f marker
  overridden:
    defaults:
      run:
        working-directory: job dir
    steps:
      - run: test -f marker
      - run: test -f marker
        working-directory: step dir
"#,
        )
        .unwrap();
        workflow.validate_supported_actions().unwrap();
        let config = workflow.to_ci_config(&test_context());

        for job_name in ["inherited", "overridden"] {
            let script = config.jobs[job_name].script.join("\n");
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .current_dir(workspace.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{job_name} script failed:\n{script}\n{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    #[test]
    fn unsupported_step_semantics_are_rejected_instead_of_ignored() {
        for (key, value) in [
            ("shell", "python"),
            ("id", "compile"),
            ("continue-on-error", "true"),
            ("timeout-minutes", "5"),
        ] {
            let yaml = format!(
                "on: push\njobs:\n  build:\n    steps:\n      - run: echo ok\n        {key}: {value}\n"
            );
            let workflow = GiteaWorkflow::parse(&yaml).unwrap();
            let error = workflow
                .validate_supported_actions()
                .unwrap_err()
                .to_string();
            assert!(error.contains(key), "error must name {key}: {error}");
        }

        let workflow = GiteaWorkflow::parse(
            "on: push\njobs:\n  build:\n    defaults:\n      run:\n        shell: bash\n    steps:\n      - run: echo ok\n",
        )
        .unwrap();
        let error = workflow
            .validate_supported_actions()
            .unwrap_err()
            .to_string();
        assert!(error.contains("defaults.run.shell"));

        let workflow = GiteaWorkflow::parse(
            "on: push\ndefaults:\n  run:\n    shell: bash\njobs:\n  build:\n    steps:\n      - run: echo ok\n",
        )
        .unwrap();
        let error = workflow
            .validate_supported_actions()
            .unwrap_err()
            .to_string();
        assert!(error.contains("defaults.run.shell"));
    }

    #[test]
    fn unknown_step_keys_fail_parsing_instead_of_becoming_noops() {
        let error = GiteaWorkflow::parse(
            "on: push\njobs:\n  build:\n    steps:\n      - run: echo ok\n        typo-key: ignored-before\n",
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("typo-key"),
            "parse error must name the key: {error}"
        );
    }

    #[test]
    fn working_directory_quoting_preserves_runtime_vars_and_blocks_substitution() {
        assert_eq!(
            shell_double_quote("build/${MATRIX_OS}/$(touch escaped)"),
            r#""build/${MATRIX_OS}/\$(touch escaped)""#
        );
    }

    #[test]
    fn reusable_workflow_jobs_are_rejected_instead_of_becoming_empty_successes() {
        let wf = GiteaWorkflow::parse(
            r#"
on: push
jobs:
  delegated:
    uses: ./.gitea/workflows/reusable.yml
"#,
        )
        .unwrap();
        let error = wf.validate_supported_actions().unwrap_err().to_string();
        assert!(error.contains("reusable workflow"));
        assert!(error.contains("delegated"));
    }

    #[test]
    fn actions_cache_maps_to_native_cache_without_executing_an_unknown_action() {
        let wf = GiteaWorkflow::parse(
            r#"
on: push
jobs:
  test:
    steps:
      - uses: actions/cache@v4
        with:
          path: |
            target
            .cargo/registry
          key: build-${{ github.sha }}
      - run: cargo test
"#,
        )
        .unwrap();
        wf.validate_supported_actions().unwrap();
        let ci = wf.to_ci_config(&WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        });
        let cache = ci.jobs["test"].cache.as_ref().unwrap();
        assert_eq!(cache.key, "build-${CI_SHA}");
        assert_eq!(cache.paths, vec!["target", ".cargo/registry"]);
    }

    #[test]
    fn expands_local_reusable_workflow_jobs_inputs_and_dependencies() {
        let caller = GiteaWorkflow::parse(
            r#"
on: push
jobs:
  shared:
    uses: ./.gitea/workflows/shared.yml
    with:
      target: production
    secrets: inherit
  publish:
    needs: shared
    steps:
      - run: echo publish
"#,
        )
        .unwrap();
        let sources = HashMap::from([(
            "shared.yml".into(),
            r#"
on:
  workflow_call:
env:
  SHARED: yes
jobs:
  build:
    steps:
      - run: echo "${{ inputs.target }} $SHARED"
  verify:
    needs: build
    steps:
      - run: echo verify
"#
            .into(),
        )]);
        let expanded = caller.expand_local_reusable_workflows(&sources).unwrap();
        assert!(expanded.jobs.contains_key("shared/build"));
        assert_eq!(
            expanded.jobs["shared/verify"].needs.as_ref().unwrap(),
            &vec!["shared/build"]
        );
        assert_eq!(
            expanded.jobs["publish"].needs.as_ref().unwrap(),
            &vec!["shared/verify"]
        );
        assert_eq!(
            expanded.jobs["shared/build"].env["INPUT_TARGET"],
            "production"
        );
        let ci = expanded.to_ci_config(&WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        });
        assert!(ci.jobs["shared/build"]
            .script
            .iter()
            .any(|line| line.contains("${INPUT_TARGET} $SHARED")));
        assert_eq!(ci.jobs["shared/build"].stage.as_deref(), Some("stage-0"));
        assert_eq!(ci.jobs["shared/verify"].stage.as_deref(), Some("stage-1"));
        assert_eq!(ci.jobs["publish"].stage.as_deref(), Some("stage-2"));
    }

    #[test]
    fn reusable_workflow_cycles_and_remote_targets_fail_closed() {
        let remote = GiteaWorkflow::parse(
            "on: push\njobs:\n  call:\n    uses: owner/repo/.gitea/workflows/x.yml@main\n",
        )
        .unwrap();
        assert!(remote
            .expand_local_reusable_workflows(&HashMap::new())
            .is_err());
        let caller =
            GiteaWorkflow::parse("on: push\njobs:\n  call:\n    uses: ./.gitea/workflows/a.yml\n")
                .unwrap();
        let sources = HashMap::from([(
            "a.yml".into(),
            "on: workflow_call\njobs:\n  again:\n    uses: ./.gitea/workflows/a.yml\n".into(),
        )]);
        assert!(caller.expand_local_reusable_workflows(&sources).is_err());
    }

    #[test]
    fn maps_execution_policy_and_evaluates_static_conditions() {
        let workflow = GiteaWorkflow::parse(
            "on: push\njobs:\n  test:\n    continue-on-error: true\n    timeout-minutes: 3\n    steps:\n      - run: exit 1\n",
        ).unwrap();
        workflow.validate_supported_actions().unwrap();
        let ci = workflow.to_ci_config(&WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        });
        assert_eq!(ci.jobs["test"].allow_failure, Some(true));
        assert_eq!(ci.jobs["test"].timeout_seconds, Some(180));

        let conditional = GiteaWorkflow::parse(
            "on: push\njobs:\n  test:\n    if: github.ref == 'refs/heads/main'\n    steps:\n      - if: startsWith(github.ref, 'refs/heads/')\n        run: echo yes\n      - if: github.event_name == 'schedule'\n        run: echo no\n",
        ).unwrap();
        conditional.validate_supported_actions().unwrap();
        let ci = conditional.to_ci_config(&WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        });
        assert_eq!(
            ci.jobs["test"].condition.as_deref(),
            Some("github.ref == 'refs/heads/main'")
        );
        assert!(ci.jobs["test"].script.iter().any(|line| line == "echo yes"));
        assert!(!ci.jobs["test"].script.iter().any(|line| line == "echo no"));

        let unsupported = GiteaWorkflow::parse(
            "on: push\njobs:\n  test:\n    if: secrets.TOKEN == 'x'\n    steps:\n      - run: echo no\n",
        ).unwrap();
        assert!(unsupported
            .validate_supported_actions()
            .unwrap_err()
            .to_string()
            .contains("unsupported job condition"));
    }

    #[test]
    fn test_parse_push_with_branches() {
        let yml = r#"
on:
  push:
    branches: [main, develop]
  pull_request:
    branches: [main]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - run: echo test
"#;
        let wf = GiteaWorkflow::parse(yml).unwrap();
        assert!(wf.matches_event("push", "refs/heads/main", "main", &ChangedPaths::unknown()));
        assert!(wf.matches_event(
            "push",
            "refs/heads/develop",
            "main",
            &ChangedPaths::unknown()
        ));
        assert!(!wf.matches_event(
            "push",
            "refs/heads/feature",
            "main",
            &ChangedPaths::unknown()
        ));
        assert!(wf.matches_event(
            "pull_request",
            "refs/heads/feature",
            "main",
            &ChangedPaths::unknown()
        ));
    }

    #[test]
    fn test_parse_on_array() {
        let yml = r#"
on: [push, pull_request]
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo hi
"#;
        let wf = GiteaWorkflow::parse(yml).unwrap();
        assert!(wf.matches_event("push", "refs/heads/main", "main", &ChangedPaths::unknown()));
        assert!(wf.matches_event(
            "pull_request",
            "refs/heads/main",
            "main",
            &ChangedPaths::unknown()
        ));
        assert!(!wf.matches_event("schedule", "", "main", &ChangedPaths::unknown()));
    }

    #[test]
    fn test_parse_with_container() {
        let yml = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    container:
      image: rust:1.75
    steps:
      - run: cargo build
"#;
        let wf = GiteaWorkflow::parse(yml).unwrap();
        let ctx = WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        };
        let ci = wf.to_ci_config(&ctx);
        let job = ci.jobs.get("build").unwrap();
        assert_eq!(job.image.as_deref(), Some("rust:1.75"));
    }

    #[test]
    fn test_parse_with_env() {
        let yml = r#"
on: push
env:
  GLOBAL_VAR: global
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      JOB_VAR: job-level
    steps:
      - run: echo $GLOBAL_VAR
      - run: echo $JOB_VAR
"#;
        let wf = GiteaWorkflow::parse(yml).unwrap();
        let ctx = WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        };
        let ci = wf.to_ci_config(&ctx);
        let job = ci.jobs.get("build").unwrap();
        let vars = job.variables.as_ref().unwrap();
        assert_eq!(vars.get("GLOBAL_VAR").unwrap(), "global");
        assert_eq!(vars.get("JOB_VAR").unwrap(), "job-level");
    }

    #[test]
    fn test_parse_with_needs() {
        let yml = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: cargo build
  test:
    runs-on: ubuntu-latest
    needs: [build]
    steps:
      - run: cargo test
  deploy:
    runs-on: ubuntu-latest
    needs: [build, test]
    steps:
      - run: deploy.sh
"#;
        let wf = GiteaWorkflow::parse(yml).unwrap();
        let ctx = WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        };
        let ci = wf.to_ci_config(&ctx);
        assert_eq!(ci.jobs.len(), 3);

        // build should be stage-0, test stage-1, deploy stage-2
        let build_stage = ci.jobs.get("build").unwrap().stage.as_deref();
        let test_stage = ci.jobs.get("test").unwrap().stage.as_deref();
        let deploy_stage = ci.jobs.get("deploy").unwrap().stage.as_deref();
        assert_eq!(build_stage, Some("stage-0"));
        assert_eq!(test_stage, Some("stage-1"));
        assert_eq!(deploy_stage, Some("stage-2"));
    }

    #[test]
    fn test_parse_concurrency() {
        let yml = r#"
on: push
concurrency:
  group: deploy-group
  cancel-in-progress: true
jobs:
  deploy:
    runs-on: ubuntu-latest
    steps:
      - run: deploy.sh
"#;
        let wf = GiteaWorkflow::parse(yml).unwrap();
        let ctx = WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        };
        let ci = wf.to_ci_config(&ctx);
        let cc = ci.concurrency.as_ref().unwrap();
        assert_eq!(cc.group, "deploy-group");
        assert!(cc.cancel_in_progress);
    }

    #[test]
    fn converts_actions_matrix_and_secret_expressions() {
        let yml = r#"
on: push
jobs:
  test:
    strategy:
      matrix:
        os: [linux, macos]
        version: [1, 2]
    steps:
      - run: echo "${{ matrix.os }} ${{ secrets.DEPLOY_TOKEN }}"
"#;
        let wf = GiteaWorkflow::parse(yml).unwrap();
        let ci = wf.to_ci_config(&WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        });
        let job = ci.jobs.get("test").unwrap();
        let matrix = job.matrix.as_ref().unwrap();
        assert_eq!(matrix["os"], vec!["linux", "macos"]);
        assert_eq!(matrix["version"], vec!["1", "2"]);
        assert!(job
            .script
            .iter()
            .any(|line| line.contains("${MATRIX_OS} ${DEPLOY_TOKEN}")));
    }
}

/// The producer side of the event vocabulary (card_e87a1b6f9633,
/// card_074d93bfe327): a pipeline may only be created under a name a workflow
/// can declare.
#[cfg(test)]
mod trigger_event_vocabulary_tests {
    use super::*;

    /// Reads the source of the crates that *create* pipelines. A producer that
    /// invents an event name compiles, runs, answers `201`, and matches no
    /// workflow — there is no failure to observe at runtime, which is why this
    /// is checked by reading rather than by driving.
    #[test]
    fn every_event_a_pipeline_is_created_under_is_one_a_workflow_can_declare() {
        let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crates/ directory");
        let mut produced: Vec<(String, String)> = Vec::new();

        for crate_name in ["rg-core", "rg-http"] {
            let mut files = vec![workspace.join(crate_name).join("src")];
            while let Some(path) = files.pop() {
                if path.is_dir() {
                    files.extend(
                        std::fs::read_dir(&path)
                            .expect("read source directory")
                            .map(|entry| entry.expect("read source entry").path()),
                    );
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let source = std::fs::read_to_string(&path).expect("read source file");
                for (index, line) in source.lines().enumerate() {
                    let Some((_, rest)) = line.split_once("trigger_type: \"") else {
                        continue;
                    };
                    let Some((event, _)) = rest.split_once('"') else {
                        continue;
                    };
                    produced.push((
                        format!("{}:{}", path.display(), index + 1),
                        event.to_string(),
                    ));
                }
            }
        }

        let unknown: Vec<_> = produced
            .iter()
            .filter(|(_, event)| !rg_core::ci::PIPELINE_EVENTS.contains(&event.as_str()))
            .collect();
        assert!(
            unknown.is_empty(),
            "these pipelines are created under an event no `on:` clause can name, so they match \
             no workflow and produce nothing: {unknown:?}"
        );
    }

    /// …and the matcher answers every one of them. A name in the canon that the
    /// matcher does not handle is the same dead end from the other side.
    ///
    /// The declaration a workflow writes is not always the event's own name:
    /// the merge queue runs a *speculative* merge of the PR, so it asks the
    /// workflows the PR itself declares. Everything else is spelled as it is
    /// raised — including the empty-bodied `on:\n  <event>:` form, which read as
    /// "not declared" until this test asked.
    #[test]
    fn the_matcher_answers_yes_to_a_workflow_that_declares_any_canonical_event() {
        for event in rg_core::ci::PIPELINE_EVENTS {
            let declared = match event {
                "merge_group" => "pull_request",
                other => other,
            };
            let yaml = format!(
                "name: W\non:\n  {declared}:\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n"
            );
            let workflow: GiteaWorkflow =
                serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("parse {declared}: {e}"));
            assert!(
                workflow.matches_event(event, "refs/heads/main", "main", &ChangedPaths::unknown()),
                "a workflow declaring `on: {declared}:` is not matched by {event}"
            );
        }
    }
}

/// Test-only re-export: the pattern rules are exercised from `lib.rs`'s
/// filter tests, next to the filters that use them.
#[cfg(test)]
pub(crate) fn match_path_pattern_for_test(path: &str, pattern: &str) -> bool {
    match_path_pattern(path, pattern)
}

#[cfg(test)]
pub(crate) fn match_glob_for_test(name: &str, pattern: &str) -> bool {
    match_glob(name, pattern)
}
