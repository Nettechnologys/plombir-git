//! Gitea Actions / GitHub Actions workflow compatibility layer.
//!
//! Parses `.gitea/workflows/*.yml` (GitHub Actions-compatible format) and
//! translates them into IronForge's internal `CiConfig` model.
//!
//! Supported features:
//! - `on: push`, `on: pull_request` triggers with branch filtering
//! - `jobs.<id>.runs-on` → runner tags
//! - `jobs.<id>.steps[].run` → script commands
//! - `jobs.<id>.steps[].uses` → `actions/checkout` is implicit; other actions are rejected
//! - `jobs.<id>.container.image` → Docker image
//! - `jobs.<id>.env` → environment variables
//! - `jobs.<id>.needs` → stage ordering (implicit via dependency graph)
//! - Repository-local reusable workflows with `on: workflow_call`, inputs, inherited secrets, and dependency rewriting
//! - Basic `${{ }}` expression substitution

use anyhow::Result;
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
    pub push: Option<EventFilter>,
    pub pull_request: Option<EventFilter>,
    #[serde(rename = "pull_request_target")]
    pub pull_request_target: Option<EventFilter>,
    pub schedule: Option<Vec<ScheduleTrigger>>,
    pub workflow_dispatch: Option<serde_yaml::Value>,
    #[serde(default, deserialize_with = "deserialize_present_yaml")]
    pub workflow_call: Option<serde_yaml::Value>,
}

/// Event filter with optional branch/tag/path filtering.
#[derive(Debug, Clone, Deserialize)]
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

/// A step within a Gitea Actions job.
#[derive(Debug, Clone, Deserialize)]
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
    /// dropping an action step. IronForge's native `.ironforge-ci.yml` format
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
        if unsupported.is_empty() {
            Ok(())
        } else {
            anyhow::bail!(
                "unsupported action step(s): {}. Convert them to run: commands or use .ironforge-ci.yml",
                unsupported.join(", ")
            )
        }
    }

    /// Check if this workflow should be triggered for the given event and ref.
    pub fn matches_event(&self, event: &str, ref_name: &str, default_branch: &str) -> bool {
        match &self.on {
            WorkflowTriggers::Simple(name) => name.as_str() == event,
            WorkflowTriggers::Array(names) => names.iter().any(|n| n.as_str() == event),
            WorkflowTriggers::Single(trigger) => {
                let WorkflowTriggerSingle {
                    push,
                    pull_request,
                    pull_request_target: _,
                    schedule: _,
                    workflow_dispatch: _,
                    workflow_call: _,
                } = trigger.as_ref();
                match event {
                    "push" => {
                        if let Some(filter) = push {
                            ref_matches_filter(ref_name, filter, default_branch)
                        } else {
                            false
                        }
                    }
                    "pull_request" | "merge_group" => {
                        if let Some(filter) = pull_request {
                            // For pull_request events, GitHub/Gitea `branches` filters
                            // apply to the PR's base (target) branch, not the head ref.
                            // The base branch is conveyed via `default_branch`.
                            let base_ref = format!("refs/heads/{default_branch}");
                            ref_matches_filter(&base_ref, filter, default_branch)
                        } else {
                            false
                        }
                    }
                    _ => false,
                }
            }
        }
    }

    /// Convert this workflow into an IronForge `CiConfig`.
    ///
    /// The conversion:
    /// - Groups jobs by their dependency order (needs) into stages
    /// - Extracts `run` commands from steps into `script`
    /// - Maps `runs-on` labels to `tags`
    /// - Translates `container.image` to `image`
    pub fn to_ci_config(&self, ctx: &WorkflowContext) -> CiConfig {
        let mut job_configs: HashMap<String, JobConfig> = HashMap::new();
        let mut stages: Vec<String> = Vec::new();

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

        // Topological sort: simple BFS (each job goes to stage = max(deps_stage) + 1)
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

        // Generate stage names
        for i in 0..=max_stage {
            stages.push(format!("stage-{}", i));
        }

        // Convert each job
        for (name, job) in &self.jobs {
            // GitHub's default bash invocation is fail-fast; preserving this
            // prevents a later successful step from masking an earlier failure.
            let mut script: Vec<String> = vec!["set -e".into()];
            let mut job_vars: HashMap<String, String> = HashMap::new();
            let mut cache = None;

            // Copy workflow-level env
            for (k, v) in &self.env {
                job_vars.insert(k.clone(), substitute_expr(v, name, &self.env, &job.env));
            }
            // Copy job-level env
            for (k, v) in &job.env {
                job_vars.insert(k.clone(), substitute_expr(v, name, &self.env, &job.env));
            }

            if let Some(uses) = &job.uses {
                script.push(format!(
                    "echo \"IronForge does not support reusable workflow '{}'; use explicit jobs or .ironforge-ci.yml\" >&2; exit 78",
                    uses
                ));
            }

            // Process steps
            let mut has_checkout = false;
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
                // Handle `uses: actions/checkout@vX` — implicit in IronForge, skip
                if let Some(ref uses) = step.uses {
                    if uses.starts_with("actions/checkout") {
                        has_checkout = true;
                        continue;
                    }
                    if uses.starts_with("actions/cache@") {
                        if let (Some(path), Some(key)) =
                            (step.with.get("path"), step.with.get("key"))
                        {
                            let paths = path
                                .lines()
                                .map(str::trim)
                                .filter(|path| !path.is_empty())
                                .map(str::to_owned)
                                .collect::<Vec<_>>();
                            cache = Some(CacheConfig {
                                key: substitute_expr(key, name, &self.env, &job_vars),
                                paths,
                            });
                        } else {
                            script.push("echo \"actions/cache requires both 'path' and 'key'\" >&2; exit 78".into());
                        }
                        continue;
                    }
                    // Direct callers should still fail visibly even if they
                    // skipped `validate_supported_actions`.
                    script.push(format!(
                        "echo \"IronForge does not support action '{}'; use run: or .ironforge-ci.yml\" >&2; exit 78",
                        uses
                    ));
                    continue;
                }

                // Handle `run:` commands
                if let Some(ref run_cmd) = step.run {
                    // Copy step-level env
                    for (k, v) in &step.env {
                        let expanded = substitute_expr(v, name, &self.env, &job_vars);
                        script.push(format!("export {}={}", k, expanded));
                    }

                    // Substitute expressions in the command
                    let expanded_cmd = substitute_expr(run_cmd, name, &self.env, &job_vars);
                    script.push(expanded_cmd);
                }
            }

            // If no checkout step was found, add a comment
            if !has_checkout && !script.is_empty() {
                script.insert(
                    0,
                    "# [IronForge] Repository is already checked out at /workspace".to_string(),
                );
            }

            // Determine tags from runs-on
            let tags = match &job.runs_on {
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
            };

            // Determine stage
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
                    timeout_seconds: job
                        .timeout_minutes
                        .map(|minutes| minutes.saturating_mul(60)),
                    tags,
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

/// Simple glob matching supporting `*` (single segment) and `**` (multi segment).
fn match_glob(s: &str, pattern: &str) -> bool {
    // Simple cases
    if pattern == "*" {
        return true;
    }
    if pattern == "**" {
        return true;
    }
    // `**/*` matches anything with at least one path segment
    if pattern == "**/*" {
        return s.contains('/') || !s.is_empty();
    }
    // Simple prefix/suffix glob
    if let Some(prefix) = pattern.strip_suffix('*') {
        return s.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return s.ends_with(suffix);
    }
    // `*middle*` case
    if pattern.starts_with('*') && pattern.ends_with('*') && pattern.len() > 2 {
        let middle = &pattern[1..pattern.len() - 1];
        return s.contains(middle);
    }
    s == pattern
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
        assert!(wf.matches_event("push", "refs/heads/main", "main"));

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
        assert!(error.to_string().contains(".ironforge-ci.yml"));
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
        assert!(wf.matches_event("push", "refs/heads/main", "main"));
        assert!(wf.matches_event("push", "refs/heads/develop", "main"));
        assert!(!wf.matches_event("push", "refs/heads/feature", "main"));
        assert!(wf.matches_event("pull_request", "refs/heads/feature", "main"));
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
        assert!(wf.matches_event("push", "refs/heads/main", "main"));
        assert!(wf.matches_event("pull_request", "refs/heads/main", "main"));
        assert!(!wf.matches_event("schedule", "", "main"));
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
