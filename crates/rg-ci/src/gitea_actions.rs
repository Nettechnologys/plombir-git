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

use crate::config::{
    ActionExpression, ActionJobTemplates, ActionTemplate, ActionTemplatePart, CacheConfig,
    CiConfig, ConcurrencyConfig, JobConfig,
};

/// A parsed Gitea Actions workflow file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
    /// The merge queue's speculative-merge event.
    ///
    /// The queue runs a trial merge of the pull request, so it takes the
    /// workflows the PR itself declares — `on: pull_request` has always matched
    /// it. What did not work was the spelling GitHub's own documentation uses:
    /// `on:\n  merge_group:` had no field to land in, so it fell into `other`,
    /// passed validation (`merge_group` *is* an event a producer emits), and
    /// then matched nothing — a repository whose CI is written that way got no
    /// merge-queue checks at all, with no error anywhere (card_69d4f18b0b23).
    #[serde(default, deserialize_with = "deserialize_present_filter")]
    pub merge_group: Option<EventFilter>,
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
    /// Every other key under `on:`.
    ///
    /// Serde drops unknown fields by default, which for an `on:` clause means
    /// `on:\n  release:\n    types: [published]` deserialized into a trigger set
    /// that declares *nothing* — indistinguishable from an empty `on:`. Keeping
    /// the keys is what lets `validate_supported_triggers` refuse the workflow
    /// by the name its author actually wrote (card_c8f24edaee89).
    #[serde(flatten)]
    pub other: HashMap<String, serde_yaml::Value>,
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
    /// Keys the compatibility layer cannot honour.
    ///
    /// This cannot be `deny_unknown_fields`: [`WorkflowTriggers`] is untagged,
    /// so serde can collapse the useful nested error into "did not match any
    /// variant". Keeping the spelling lets `validate_supported_triggers` name
    /// the exact declaration the author must fix (card_444a8741da37).
    #[serde(flatten)]
    pub other: HashMap<String, serde_yaml::Value>,
}

const SUPPORTED_EVENT_FILTERS: &[&str] = &[
    "branches",
    "branches-ignore",
    "tags",
    "tags-ignore",
    "paths",
    "paths-ignore",
];

/// Schedule trigger with cron expression.
#[derive(Debug, Clone, Deserialize)]
pub struct ScheduleTrigger {
    pub cron: String,
}

/// `on:` names that are not events a producer emits.
///
/// `workflow_call` is the odd one out and the only one that is *allowed*: it
/// declares the file reusable rather than asking for a run, so a workflow that
/// names only it is correct even though nothing will ever trigger it directly.
const WORKFLOW_CALL_TRIGGER: &str = "workflow_call";
/// The `on:` spelling of [`rg_core::ci::pull_request::PULL_REQUEST_EVENT`].
const PULL_REQUEST_TRIGGER: &str = "pull_request";
/// The `on:` spelling of the merge queue's speculative-merge event.
const MERGE_GROUP_TRIGGER: &str = "merge_group";
/// Parsed, matched by nothing: the workflow would be taken from the base branch
/// rather than from the PR's head, which no producer here does (card_c8f24edaee89).
const PULL_REQUEST_TARGET_TRIGGER: &str = "pull_request_target";
/// Parsed, matched by nothing, and there is no scheduler in the tree to emit it.
const SCHEDULE_TRIGGER: &str = "schedule";

/// Whether an `on:` name is something this engine can actually run.
fn is_runnable_trigger(name: &str) -> bool {
    rg_core::ci::PIPELINE_EVENTS.contains(&name) || name == WORKFLOW_CALL_TRIGGER
}

/// The supported `on:` names, for an error message that tells the author what
/// to write instead.
fn supported_trigger_list() -> String {
    rg_core::ci::PIPELINE_EVENTS
        .iter()
        .copied()
        .chain(std::iter::once(WORKFLOW_CALL_TRIGGER))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A Gitea Actions job definition.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
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
    pub runs_on: Option<GiteaRunsOn>,

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

    /// Deployment environment, either a scalar name or `{ name: ... }`.
    pub environment: Option<GiteaEnvironment>,

    /// Matrix expansion compatible with `strategy.matrix`.
    pub strategy: Option<GiteaStrategy>,
}

/// The two `runs-on` forms ForgeKeep can preserve without weakening runner
/// selection: one label, or a non-empty list in which every entry is a label.
#[derive(Debug, Clone)]
pub enum GiteaRunsOn {
    Label(String),
    Labels(Vec<String>),
}

impl GiteaRunsOn {
    fn from_yaml(value: serde_yaml::Value) -> std::result::Result<Self, String> {
        match value {
            serde_yaml::Value::String(label) => Ok(Self::Label(label)),
            serde_yaml::Value::Sequence(labels) if labels.is_empty() => {
                Err("runs-on must be a string or a non-empty list of strings".into())
            }
            serde_yaml::Value::Sequence(labels) => {
                let labels = labels
                    .into_iter()
                    .enumerate()
                    .map(|(index, label)| match label {
                        serde_yaml::Value::String(label) => Ok(label),
                        _ => Err(format!("runs-on[{index}] must be a string")),
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                Ok(Self::Labels(labels))
            }
            _ => Err("runs-on must be a string or a non-empty list of strings".into()),
        }
    }

    fn tags(&self) -> Vec<String> {
        match self {
            Self::Label(label) => vec![label.clone()],
            Self::Labels(labels) => labels.clone(),
        }
    }
}

impl<'de> Deserialize<'de> for GiteaRunsOn {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        Self::from_yaml(value).map_err(serde::de::Error::custom)
    }
}

/// The two environment forms ForgeKeep can honour end to end.
///
/// The mapping intentionally contains only `name`: Actions also defines `url`,
/// but ForgeKeep has no model or UI consumer for it. [`GiteaWorkflow::parse`]
/// validates the raw mapping first so a refusal can name the job and qualified
/// key instead of serde's context-free "untagged enum" error.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum GiteaEnvironment {
    Name(String),
    Details(GiteaEnvironmentDetails),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GiteaEnvironmentDetails {
    pub name: String,
}

impl GiteaEnvironment {
    fn name(&self) -> &str {
        match self {
            Self::Name(name) => name,
            Self::Details(details) => &details.name,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GiteaStrategy {
    #[serde(default)]
    pub matrix: std::collections::BTreeMap<String, Vec<GiteaMatrixValue>>,
}

/// A matrix value ForgeKeep can preserve as one concrete job variant.
///
/// Keeping this closed after the raw-YAML diagnostic pass makes loss impossible
/// in `to_ci_config`: every value in the typed workflow has a string form, so
/// conversion cannot use `filter_map` and silently reduce the Cartesian product.
#[derive(Debug, Clone)]
pub enum GiteaMatrixValue {
    String(String),
    Bool(bool),
    Number(serde_yaml::Number),
}

impl GiteaMatrixValue {
    fn from_yaml(value: serde_yaml::Value) -> std::result::Result<Self, String> {
        match value {
            serde_yaml::Value::String(value) => Ok(Self::String(value)),
            serde_yaml::Value::Bool(value) => Ok(Self::Bool(value)),
            serde_yaml::Value::Number(value) => Ok(Self::Number(value)),
            _ => Err("must be a string, number, or boolean".into()),
        }
    }

    fn as_string(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Bool(value) => value.to_string(),
            Self::Number(value) => value.to_string(),
        }
    }
}

impl<'de> Deserialize<'de> for GiteaMatrixValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        Self::from_yaml(value).map_err(serde::de::Error::custom)
    }
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct GiteaConcurrency {
    pub group: String,
    #[serde(rename = "cancel-in-progress")]
    pub cancel_in_progress: Option<bool>,
}

/// The `with:` inputs `actions/checkout` has an honest answer for.
///
/// ForgeKeep does not run the action; the pipeline workspace is a
/// `git worktree add --detach <pipeline sha>` of the repository the workflow
/// lives in. That satisfies exactly one of the action's inputs and no others:
/// `fetch-depth` asks for *at least* that much history, and the worktree always
/// carries all of it, so any value is met — including the `0` that workflows
/// spell to ask for a full clone.
///
/// Everything else the action accepts changes what ends up in the workspace —
/// `ref`, `repository`, `path`, `submodules`, `lfs`, `sparse-checkout`,
/// `clean`, `persist-credentials` — and the workspace answers to none of them.
/// Accepting those keys silently is how a job that asked to check out a
/// different branch ran green against the pipeline's commit.
const CHECKOUT_INPUTS: &[&str] = &["fetch-depth"];

/// The `with:` inputs `actions/cache` is translated from.
///
/// `build_job_script` reads these two into a [`CacheConfig`]. The rest change
/// behaviour that ForgeKeep's cache does not implement — `restore-keys` turns a
/// miss into a fallback hit, `fail-on-cache-miss` turns a miss into a job
/// failure, `lookup-only` skips the restore — so accepting them would report the
/// opposite of what the workflow asked for.
const CACHE_INPUTS: &[&str] = &["path", "key"];

/// `with:` keys on a natively-implemented action that nothing consumes.
///
/// Returns them sorted so the same workflow always produces the same message;
/// `with` is a `HashMap` and its iteration order is not stable.
fn unsupported_action_inputs(job_name: &str, index: usize, step: &GiteaStep) -> Vec<String> {
    let Some(uses) = step.uses.as_deref() else {
        return Vec::new();
    };
    let supported = if uses.starts_with("actions/checkout") {
        CHECKOUT_INPUTS
    } else if uses.starts_with("actions/cache@") {
        CACHE_INPUTS
    } else {
        // Any other action is already refused whole, inputs and all.
        return Vec::new();
    };

    let mut unknown = step
        .with
        .keys()
        .filter(|key| !supported.contains(&key.as_str()))
        .map(|key| {
            format!(
                "{job_name}: step {} {uses} input '{key}' (supported: {})",
                index + 1,
                supported.join(", ")
            )
        })
        .collect::<Vec<_>>();
    unknown.sort();
    unknown
}

const GITHUB_RUN_EXPRESSIONS: [(&str, &str); 5] = [
    ("github.ref", "${CI_REF}"),
    ("github.sha", "${CI_SHA}"),
    ("github.event_name", "${CI_EVENT}"),
    ("github.repository", "${CI_REPOSITORY}"),
    ("github.repository_owner", "${CI_REPOSITORY_OWNER}"),
];

fn context_member<'a>(key: &'a str, context: &str) -> Option<&'a str> {
    key.strip_prefix(context)
        .and_then(|rest| rest.strip_prefix('.'))
        .filter(|name| {
            !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        })
}

fn supported_run_expression(key: &str) -> bool {
    GITHUB_RUN_EXPRESSIONS
        .iter()
        .any(|(supported, _)| key == *supported)
        // `vars.*` is deliberately absent. ForgeKeep has no repository,
        // organization, or environment configuration-variable source, and an
        // `env.*` value with the same name is a different Actions context.
        || ["env", "secrets", "matrix", "inputs"]
            .iter()
            .any(|context| context_member(key, context).is_some())
}

fn workflow_expressions(input: &str) -> Vec<String> {
    let mut expressions = Vec::new();
    let mut rest = input;
    while let Some(open) = rest.find("${{") {
        let after_open = &rest[open + 3..];
        let Some(close) = after_open.find("}}") else {
            expressions.push("<missing closing `}}`>".into());
            break;
        };
        let key = after_open[..close].trim();
        expressions.push(if key.is_empty() {
            "<empty expression>".into()
        } else {
            key.to_owned()
        });
        rest = &after_open[close + 2..];
    }
    expressions
}

fn unsupported_run_expressions(input: &str) -> Vec<String> {
    workflow_expressions(input)
        .into_iter()
        .filter(|expression| !supported_run_expression(expression))
        .collect()
}

fn job_field_expression(key: &str) -> Option<ActionExpression> {
    match key {
        "github.ref" => Some(ActionExpression::GithubRef),
        "github.sha" => Some(ActionExpression::GithubSha),
        "github.event_name" => Some(ActionExpression::GithubEventName),
        "github.repository" => Some(ActionExpression::GithubRepository),
        "github.repository_owner" => Some(ActionExpression::GithubRepositoryOwner),
        _ => context_member(key, "matrix")
            .map(|name| ActionExpression::Matrix(name.to_owned()))
            .or_else(|| {
                context_member(key, "inputs").map(|name| ActionExpression::Input(name.to_owned()))
            }),
    }
}

fn compile_action_template(input: &str) -> Option<ActionTemplate> {
    let mut parts = Vec::new();
    let mut rest = input;
    let mut found = false;
    while let Some(open) = rest.find("${{") {
        found = true;
        if open > 0 {
            parts.push(ActionTemplatePart::Literal(rest[..open].to_owned()));
        }
        let after_open = &rest[open + 3..];
        let Some(close) = after_open.find("}}") else {
            parts.push(ActionTemplatePart::Expression(
                ActionExpression::Unsupported("<missing closing `}}`>".into()),
            ));
            rest = "";
            break;
        };
        let key = after_open[..close].trim();
        let expression = job_field_expression(key).unwrap_or_else(|| {
            ActionExpression::Unsupported(if key.is_empty() {
                "<empty expression>".into()
            } else {
                key.to_owned()
            })
        });
        parts.push(ActionTemplatePart::Expression(expression));
        rest = &after_open[close + 2..];
    }
    if !found {
        return None;
    }
    if !rest.is_empty() {
        parts.push(ActionTemplatePart::Literal(rest.to_owned()));
    }
    Some(ActionTemplate::new(parts))
}

fn split_action_template(value: Option<String>) -> (Option<String>, Option<ActionTemplate>) {
    match value {
        Some(value) => match compile_action_template(&value) {
            Some(template) => (None, Some(template)),
            None => (Some(value), None),
        },
        None => (None, None),
    }
}

fn split_action_template_list(
    values: Option<Vec<String>>,
) -> (Option<Vec<String>>, Option<Vec<ActionTemplate>>) {
    let Some(values) = values else {
        return (None, None);
    };
    let compiled = values
        .iter()
        .map(|value| compile_action_template(value))
        .collect::<Vec<_>>();
    if compiled.iter().all(Option::is_none) {
        return (Some(values), None);
    }
    let templates = values
        .into_iter()
        .zip(compiled)
        .map(|(value, template)| template.unwrap_or_else(|| ActionTemplate::literal(value)))
        .collect();
    (None, Some(templates))
}

/// Every user-authored string passed through [`substitute_expr`].
///
/// An expression that survives that function lands either in `sh -c` (where
/// `${{` is a `bad substitution`) or in a runner environment as a convincing
/// but false literal. Reject it while reading the workflow and name the exact
/// site instead.
fn unsupported_run_expression_sites(workflow: &GiteaWorkflow) -> Vec<String> {
    let mut unsupported = Vec::new();
    let mut inspect = |site: String, value: &str| {
        unsupported.extend(
            unsupported_run_expressions(value)
                .into_iter()
                .map(|expression| format!("{site} expression `{expression}`")),
        );
    };

    for (name, value) in &workflow.env {
        inspect(format!("env.{name}"), value);
    }
    if let Some(directory) = workflow
        .defaults
        .as_ref()
        .and_then(|defaults| defaults.run.as_ref())
        .and_then(|run| run.working_directory.as_deref())
    {
        inspect("defaults.run.working-directory".into(), directory);
    }

    for (job_name, job) in &workflow.jobs {
        for (name, value) in &job.env {
            inspect(format!("{job_name}: env.{name}"), value);
        }
        if let Some(container) = &job.container {
            for (name, value) in &container.env {
                inspect(format!("{job_name}: container.env.{name}"), value);
            }
        }
        if let Some(directory) = job
            .defaults
            .as_ref()
            .and_then(|defaults| defaults.run.as_ref())
            .and_then(|run| run.working_directory.as_deref())
        {
            inspect(
                format!("{job_name}: defaults.run.working-directory"),
                directory,
            );
        }
        for (index, step) in job.steps.iter().enumerate() {
            let site = format!("{job_name}: step {}", index + 1);
            for (name, value) in &step.env {
                inspect(format!("{site} env.{name}"), value);
            }
            if let Some(run) = &step.run {
                inspect(format!("{site} run"), run);
            }
            if let Some(directory) = &step.working_directory {
                inspect(format!("{site} working-directory"), directory);
            }
            if let Some(key) = step.with.get("key") {
                inspect(format!("{site} with.key"), key);
            }
        }
    }

    unsupported
}

/// Expressions in job fields resolved before a runner starts.
///
/// Supported GitHub and matrix members are compiled into typed templates and
/// rendered for each matrix variant while the pipeline graph is written. Any
/// other context has no value at that boundary, so refuse it by field and
/// expression instead of leaving `${{ ... }}` in `CiConfig`.
fn unsupported_job_field_expression_sites(workflow: &GiteaWorkflow) -> Vec<String> {
    let mut unsupported = Vec::new();
    let mut inspect = |site: String, value: &str| {
        if let Some(template) = compile_action_template(value) {
            unsupported.extend(
                template
                    .unsupported_expressions()
                    .into_iter()
                    .map(|expression| format!("{site} expression `{expression}`")),
            );
        }
    };

    for (job_name, job) in &workflow.jobs {
        match &job.runs_on {
            Some(GiteaRunsOn::Label(value)) => {
                inspect(format!("{job_name}: runs-on"), value);
            }
            Some(GiteaRunsOn::Labels(values)) => {
                for value in values {
                    inspect(format!("{job_name}: runs-on"), value);
                }
            }
            _ => {}
        }
        if let Some(container) = &job.container {
            inspect(format!("{job_name}: container.image"), &container.image);
        }
        if let Some(environment) = job.environment.as_ref().map(GiteaEnvironment::name) {
            inspect(format!("{job_name}: environment.name"), environment);
        }
    }

    unsupported
}

/// Context for workflow expression evaluation.
///
/// `repo_owner` / `repo_name` are the `<owner>/<name>` the pipeline is running
/// for. They were declared here and filled with `String::new() // filled later`
/// by the only producer, and nothing read them either — so `${{ github.repository }}`
/// and `${{ github.repository_owner }}` in an `if:` condition resolved to the
/// empty string, which an expression like `github.repository == 'acme/api'`
/// reports as a plain false (card_054e997a46e6).
pub struct WorkflowContext {
    pub ref_name: String,
    pub sha: String,
    pub event: String,
    pub repo_owner: String,
    pub repo_name: String,
}

impl WorkflowContext {
    /// `<owner>/<name>` — the value GitHub calls `github.repository`.
    pub fn repository(&self) -> String {
        format!("{}/{}", self.repo_owner, self.repo_name)
    }
}

/// Validate `jobs.<name>.environment` while the job-map key is still visible.
///
/// Deserializing the final untagged type alone is fail-closed, but serde then
/// reports only that no enum variant matched. That loses the job and author key
/// which a committed workflow must name in its client-facing refusal.
fn validate_job_environments(raw: &serde_yaml::Value) -> Result<()> {
    let Some(jobs) = raw
        .as_mapping()
        .and_then(|root| root.get(serde_yaml::Value::String("jobs".into())))
        .and_then(serde_yaml::Value::as_mapping)
    else {
        return Ok(());
    };

    for (job_name, job) in jobs {
        let Some(job_name) = job_name.as_str() else {
            continue;
        };
        let Some(environment) = job
            .as_mapping()
            .and_then(|job| job.get(serde_yaml::Value::String("environment".into())))
        else {
            continue;
        };

        match environment {
            serde_yaml::Value::String(_) => {}
            serde_yaml::Value::Mapping(mapping) => {
                let mut unsupported = mapping
                    .keys()
                    .filter_map(serde_yaml::Value::as_str)
                    .filter(|key| *key != "name")
                    .map(|key| format!("environment.{key}"))
                    .collect::<Vec<_>>();
                unsupported.sort();
                if !unsupported.is_empty() {
                    anyhow::bail!(
                        "job '{job_name}' uses unsupported environment key(s): {}. Supported field: environment.name",
                        unsupported.join(", ")
                    );
                }
                if mapping.keys().any(|key| key.as_str().is_none()) {
                    anyhow::bail!(
                        "job '{job_name}' environment has a non-string key. Supported field: environment.name"
                    );
                }
                let Some(name) = mapping.get(serde_yaml::Value::String("name".into())) else {
                    anyhow::bail!(
                        "job '{job_name}' is missing required environment.name. Supported field: environment.name"
                    );
                };
                if name.as_str().is_none() {
                    anyhow::bail!(
                        "job '{job_name}' environment.name must be a string. Supported field: environment.name"
                    );
                }
            }
            _ => anyhow::bail!(
                "job '{job_name}' environment must be a string or a mapping with string environment.name"
            ),
        }
    }

    Ok(())
}

/// Validate `jobs.<name>.strategy.matrix.<dimension>[index]` while the job and
/// author-written path are still available for the client-facing refusal.
fn validate_job_matrix_values(raw: &serde_yaml::Value) -> Result<()> {
    let Some(jobs) = raw
        .as_mapping()
        .and_then(|root| root.get(serde_yaml::Value::String("jobs".into())))
        .and_then(serde_yaml::Value::as_mapping)
    else {
        return Ok(());
    };

    for (job_name, job) in jobs {
        let Some(job_name) = job_name.as_str() else {
            continue;
        };
        let Some(matrix) = job
            .as_mapping()
            .and_then(|job| job.get(serde_yaml::Value::String("strategy".into())))
            .and_then(serde_yaml::Value::as_mapping)
            .and_then(|strategy| strategy.get(serde_yaml::Value::String("matrix".into())))
        else {
            continue;
        };
        let Some(matrix) = matrix.as_mapping() else {
            anyhow::bail!("job '{job_name}' strategy.matrix must be a mapping");
        };

        for (dimension, values) in matrix {
            let Some(dimension) = dimension.as_str() else {
                anyhow::bail!("job '{job_name}' strategy.matrix has a non-string dimension");
            };
            let Some(values) = values.as_sequence() else {
                anyhow::bail!(
                    "job '{job_name}' strategy.matrix.{dimension} must be a list of strings, numbers, or booleans"
                );
            };
            for (index, value) in values.iter().enumerate() {
                GiteaMatrixValue::from_yaml(value.clone()).map_err(|reason| {
                    anyhow::anyhow!(
                        "job '{job_name}' strategy.matrix.{dimension}[{index}] {reason}"
                    )
                })?;
            }
        }
    }

    Ok(())
}

/// Validate `jobs.<name>.runs-on` while both the job name and the author's key
/// are still available for the client-facing refusal.
fn validate_job_runs_on(raw: &serde_yaml::Value) -> Result<()> {
    let Some(jobs) = raw
        .as_mapping()
        .and_then(|root| root.get(serde_yaml::Value::String("jobs".into())))
        .and_then(serde_yaml::Value::as_mapping)
    else {
        return Ok(());
    };

    for (job_name, job) in jobs {
        let Some(job_name) = job_name.as_str() else {
            continue;
        };
        let Some(runs_on) = job
            .as_mapping()
            .and_then(|job| job.get(serde_yaml::Value::String("runs-on".into())))
        else {
            continue;
        };

        GiteaRunsOn::from_yaml(runs_on.clone())
            .map_err(|reason| anyhow::anyhow!("job '{job_name}' {reason}"))?;
    }

    Ok(())
}

impl GiteaWorkflow {
    /// Parse a Gitea Actions workflow YAML string.
    pub fn parse(yaml: &str) -> Result<Self> {
        let raw: serde_yaml::Value = serde_yaml::from_str(yaml)?;
        validate_job_runs_on(&raw)?;
        validate_job_environments(&raw)?;
        validate_job_matrix_values(&raw)?;
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
        // Every executable job now carries the working-directory default of
        // the workflow file that declared it. Leaving the caller's fallback on
        // the flattened workflow would make jobs from a nested workflow with
        // no default inherit the caller's directory.
        if let Some(run) = expanded
            .defaults
            .as_mut()
            .and_then(|defaults| defaults.run.as_mut())
        {
            run.working_directory = None;
        }
        Ok(expanded)
    }

    fn is_reusable(&self) -> bool {
        match &self.on {
            WorkflowTriggers::Simple(name) => name == WORKFLOW_CALL_TRIGGER,
            WorkflowTriggers::Array(names) => {
                names.iter().any(|name| name == WORKFLOW_CALL_TRIGGER)
            }
            WorkflowTriggers::Single(trigger) => trigger.workflow_call.is_some(),
        }
    }

    /// The trigger names this workflow declares, whichever shape `on:` took.
    ///
    /// The `Single` arm destructures exhaustively on purpose: a trigger field
    /// added to [`WorkflowTriggerSingle`] without a line here stops compiling,
    /// which is the whole mechanism that keeps a parsed-but-unrunnable trigger
    /// from going unnoticed a fourth time.
    fn declared_triggers(&self) -> Vec<String> {
        match &self.on {
            WorkflowTriggers::Simple(name) => vec![name.clone()],
            WorkflowTriggers::Array(names) => names.clone(),
            WorkflowTriggers::Single(trigger) => {
                let WorkflowTriggerSingle {
                    push,
                    pull_request,
                    pull_request_target,
                    merge_group,
                    schedule,
                    workflow_dispatch,
                    workflow_call,
                    other,
                } = trigger.as_ref();
                [
                    push.is_some().then_some("push"),
                    pull_request.is_some().then_some(PULL_REQUEST_TRIGGER),
                    pull_request_target
                        .is_some()
                        .then_some(PULL_REQUEST_TARGET_TRIGGER),
                    merge_group.is_some().then_some(MERGE_GROUP_TRIGGER),
                    schedule.is_some().then_some(SCHEDULE_TRIGGER),
                    workflow_dispatch
                        .is_some()
                        .then_some(rg_core::ci::WORKFLOW_DISPATCH_EVENT),
                    workflow_call.is_some().then_some(WORKFLOW_CALL_TRIGGER),
                ]
                .into_iter()
                .flatten()
                .map(str::to_string)
                // Sorted so the refusal below reads the same on every run —
                // `HashMap` iteration order is not stable across processes.
                .chain({
                    let mut rest = other.keys().cloned().collect::<Vec<_>>();
                    rest.sort();
                    rest
                })
                .collect()
            }
        }
    }

    /// Event-filter declarations that parsed but have no consumer.
    fn unsupported_event_filter_keys(&self) -> Vec<String> {
        let WorkflowTriggers::Single(trigger) = &self.on else {
            return Vec::new();
        };
        let WorkflowTriggerSingle {
            push,
            pull_request,
            pull_request_target,
            merge_group,
            schedule: _,
            workflow_dispatch: _,
            workflow_call: _,
            other: _,
        } = trigger.as_ref();

        let mut unsupported = Vec::new();
        for (trigger_name, filter) in [
            ("push", push.as_ref()),
            (PULL_REQUEST_TRIGGER, pull_request.as_ref()),
            (PULL_REQUEST_TARGET_TRIGGER, pull_request_target.as_ref()),
            (MERGE_GROUP_TRIGGER, merge_group.as_ref()),
        ] {
            if let Some(filter) = filter {
                unsupported.extend(
                    filter
                        .other
                        .keys()
                        .map(|key| format!("{trigger_name}.{key}")),
                );
            }
        }
        unsupported.sort();
        unsupported
    }

    /// Reject a workflow whose `on:` clause names something nothing here emits.
    ///
    /// `on: schedule`, `on: pull_request_target` and `on: release` all parse.
    /// None of them ever ran: `matches_event` answers `false` for every event a
    /// producer in this tree can name, and for `schedule` there is no scheduler
    /// in the tree at all. The file was accepted, reported as valid, and never
    /// produced a pipeline — the author's only evidence being that nothing
    /// happened (card_c8f24edaee89).
    ///
    /// So the declaration is refused where the file is read, rather than
    /// half-honoured at match time. That is the same answer this module already
    /// gives to a step it cannot run (see [`validate_supported_actions`]) and to
    /// a workflow that does not parse: a repository whose CI cannot do what its
    /// file asks for finds out at the next push, not after a week of wondering
    /// why the nightly build never fired.
    ///
    /// The supported set is [`rg_core::ci::PIPELINE_EVENTS`] — the events
    /// producers actually create pipelines under — plus `workflow_call`, which
    /// is not an event at all but the declaration that a file is reusable. That
    /// makes this check self-maintaining in the other direction too: a producer
    /// inventing an event name of its own is already caught by
    /// `every_event_a_pipeline_is_created_under_is_one_a_workflow_can_declare`.
    ///
    /// [`validate_supported_actions`]: GiteaWorkflow::validate_supported_actions
    pub fn validate_supported_triggers(&self) -> Result<()> {
        let unsupported_filters = self.unsupported_event_filter_keys();
        if !unsupported_filters.is_empty() {
            anyhow::bail!(
                "unsupported event filter key(s): {}. Supported event filters: {}",
                unsupported_filters.join(", "),
                SUPPORTED_EVENT_FILTERS.join(", ")
            );
        }

        let declared = self.declared_triggers();
        if declared.is_empty() {
            anyhow::bail!(
                "its `on:` clause declares no trigger at all, so nothing can ever run it. \
                 Declare one of: {}",
                supported_trigger_list()
            );
        }
        let unrunnable = declared
            .iter()
            .filter(|name| !is_runnable_trigger(name))
            .cloned()
            .collect::<Vec<_>>();
        if unrunnable.is_empty() {
            return Ok(());
        }
        anyhow::bail!(
            "no producer emits trigger(s): {}. A workflow declaring one is accepted and never runs. \
             Supported: {}",
            unrunnable.join(", "),
            supported_trigger_list()
        )
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
        // `container.options` is raw `docker run` flags. Passing them through
        // would hand a committed workflow the ability to undo the sandbox the
        // runner builds around every containerised job — `--cap-drop=ALL`,
        // `--security-opt=no-new-privileges`, the pids/memory/cpu limits, and
        // the deliberate absence of `--privileged` and the Docker socket. There
        // is no honest partial support here, so the key is refused by name
        // rather than parsed and dropped (card_e949057aaa0d).
        //
        // `container.env` is not in this list: it is honoured, as the job's
        // variables, which is what the runner turns into the container's
        // environment.
        unsupported.extend(self.jobs.iter().filter_map(|(job_name, job)| {
            job.container
                .as_ref()
                .and_then(|container| container.options.as_ref())
                .map(|_| format!("{job_name}: container.options"))
        }));

        // An action ForgeKeep implements natively still has to be honest about
        // *which* of its inputs it implements. `actions/checkout` was skipped
        // whole — `has_checkout = true; continue` — so every `with:` key on it
        // was accepted and ignored, and a job asking for `ref:` or `submodules:`
        // ran green against a tree that had neither. Same for `actions/cache`,
        // where only `path` and `key` are read.
        unsupported.extend(self.jobs.iter().flat_map(|(job_name, job)| {
            job.steps
                .iter()
                .enumerate()
                .flat_map(move |(index, step)| unsupported_action_inputs(job_name, index, step))
        }));
        unsupported.extend(unsupported_run_expression_sites(self));
        unsupported.extend(unsupported_job_field_expression_sites(self));

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
        // The merge queue's alias, in the two shapes that carry no filters. It
        // lived only in the mapped arm below, so a one-line `on: pull_request`
        // — or the same name inside `on: [push, pull_request]` — matched
        // nothing for `merge_group` and the queue merged with no checks of the
        // speculative commit at all, while the very same declaration written as
        // `on:\n  pull_request:` did run them (card_69d4f18b0b23).
        let declares = |name: &str| {
            name == event || (event == MERGE_GROUP_TRIGGER && name == PULL_REQUEST_TRIGGER)
        };
        match &self.on {
            WorkflowTriggers::Simple(name) => declares(name),
            WorkflowTriggers::Array(names) => names.iter().any(|name| declares(name)),
            WorkflowTriggers::Single(trigger) => {
                // The three `_` bindings are triggers no producer emits, so no
                // arm below could ever be reached through them. They are not
                // silently dropped any more: `validate_supported_triggers` — run
                // where the file is read, before this — refuses a workflow that
                // declares `pull_request_target` or `schedule` outright, and
                // `workflow_call` marks a reusable file that is expanded into
                // its caller rather than triggered on its own.
                let WorkflowTriggerSingle {
                    push,
                    pull_request,
                    pull_request_target: _,
                    merge_group,
                    schedule: _,
                    workflow_dispatch,
                    workflow_call: _,
                    other: _,
                } = trigger.as_ref();
                // For PR-shaped events, GitHub/Gitea `branches` filters apply to
                // the PR's base (target) branch, not the head ref.
                let base_branch_matches = |filter: Option<&EventFilter>| {
                    filter.is_some_and(|filter| {
                        let base_ref = format!("refs/heads/{base_branch}");
                        ref_matches_filter(&base_ref, filter, base_branch)
                            && paths_match_filter(filter, changed)
                    })
                };
                match event {
                    "push" => {
                        if let Some(filter) = push {
                            ref_matches_filter(ref_name, filter, base_branch)
                                && paths_match_filter(filter, changed)
                        } else {
                            false
                        }
                    }
                    PULL_REQUEST_TRIGGER => base_branch_matches(pull_request.as_ref()),
                    // Two spellings, one event. The queue merges the PR
                    // speculatively, so the workflows the PR declares are the
                    // ones it has to run — that alias stays. But `merge_group`
                    // also has to be declarable by its own name, which is what
                    // GitHub's documentation tells an author to write and what
                    // used to match nothing at all (card_69d4f18b0b23).
                    MERGE_GROUP_TRIGGER => {
                        base_branch_matches(merge_group.as_ref().or(pull_request.as_ref()))
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
            let (image, image_template) = split_action_template(
                job.container
                    .as_ref()
                    .map(|container| container.image.clone()),
            );
            let (environment, environment_template) = split_action_template(
                job.environment
                    .as_ref()
                    .map(GiteaEnvironment::name)
                    .map(str::to_owned),
            );
            let (tags, tag_templates) = split_action_template_list(runs_on_tags(&job.runs_on));
            let action_templates = ActionJobTemplates {
                image: image_template,
                tags: tag_templates,
                environment: environment_template,
            };
            let action_templates = (!action_templates.is_empty()).then_some(action_templates);

            let s = job_stage.get(name).copied().unwrap_or(0);
            let stage_name = format!("stage-{}", s);

            job_configs.insert(
                name.clone(),
                JobConfig {
                    stage: Some(stage_name),
                    script,
                    image,
                    only: None, // filtering is done at trigger time
                    variables: if job_vars.is_empty() {
                        None
                    } else {
                        Some(job_vars)
                    },
                    when: None,
                    condition: job.condition.clone(),
                    environment,
                    allow_failure: Some(job.continue_on_error),
                    // Saturating on both hops on purpose: an absurd
                    // `timeout-minutes` has to arrive at the validator as an
                    // absurd number of seconds and be refused by name. A
                    // wrapping `as i64` would have turned it negative, i.e.
                    // into a value the validator used to wave through.
                    timeout_seconds: job.timeout_minutes.map(|minutes| {
                        i64::try_from(minutes.saturating_mul(60)).unwrap_or(i64::MAX)
                    }),
                    tags,
                    matrix: job.strategy.as_ref().map(|strategy| {
                        strategy
                            .matrix
                            .iter()
                            .map(|(key, values)| {
                                let values =
                                    values.iter().map(GiteaMatrixValue::as_string).collect();
                                (key.clone(), values)
                            })
                            .collect()
                    }),
                    cache,
                    action_templates,
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

        // Container env first, so workflow- and job-level `env:` override it.
        // The runner turns a job's variables into the container's environment
        // (`docker run -e KEY`), which is what `container.env` means — it was
        // deserialized and then read by nothing, so a workflow declaring it ran
        // without those variables and said so nowhere (card_e949057aaa0d).
        if let Some(container) = &job.container {
            // Destructured exhaustively on purpose, the same device the trigger
            // match uses: a field added to `GiteaContainer` stops compiling here
            // until somebody decides whether it is honoured or refused by name.
            // That decision not being forced is how `options` and `env` came to
            // be deserialized and read by nothing.
            //
            // `image` is read in `to_ci_config`; `options` cannot be honoured
            // and is refused in `validate_supported_actions`.
            let GiteaContainer {
                image: _,
                options: _,
                env,
            } = container;
            for (k, v) in env {
                job_vars.insert(
                    k.clone(),
                    substitute_expr(v, job_name, &self.env, &job.env, None),
                );
            }
        }
        // Copy workflow-level env
        for (k, v) in &self.env {
            job_vars.insert(
                k.clone(),
                substitute_expr(v, job_name, &self.env, &job.env, None),
            );
        }
        // Copy job-level env
        for (k, v) in &job.env {
            job_vars.insert(
                k.clone(),
                substitute_expr(v, job_name, &self.env, &job.env, None),
            );
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
            // Resolve a step's env against the enclosing scopes first, then
            // expose the resolved values to every expression surface in this
            // step. Entries in one `env:` map do not depend on HashMap order or
            // see their siblings while they are being resolved.
            let resolved_step_env = step
                .env
                .iter()
                .map(|(name, value)| {
                    (
                        name.clone(),
                        substitute_expr(value, job_name, &self.env, &job_vars, None),
                    )
                })
                .collect::<HashMap<_, _>>();
            let mut step_vars = job_vars.clone();
            step_vars.extend(resolved_step_env.clone());

            if let Some(condition) = step.condition.as_deref() {
                let context = actions_condition_context(ctx, &step_vars);
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
                            key: substitute_expr(
                                key,
                                job_name,
                                &self.env,
                                &job_vars,
                                Some(&resolved_step_env),
                            ),
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
                // Substitute expressions in the command
                let expanded_cmd = substitute_expr(
                    run_cmd,
                    job_name,
                    &self.env,
                    &job_vars,
                    Some(&resolved_step_env),
                );
                let working_directory = step
                    .working_directory
                    .as_deref()
                    .or(default_working_directory)
                    .map(|directory| {
                        substitute_expr(
                            directory,
                            job_name,
                            &self.env,
                            &job_vars,
                            Some(&resolved_step_env),
                        )
                    });
                script.push(step_command(
                    &expanded_cmd,
                    &resolved_step_env,
                    working_directory.as_deref(),
                ));
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

fn pin_workflow_working_directory(job: &mut GiteaJob, defaults: &Option<GiteaDefaults>) {
    let Some(working_directory) = defaults
        .as_ref()
        .and_then(|defaults| defaults.run.as_ref())
        .and_then(|run| run.working_directory.as_ref())
    else {
        return;
    };
    let run = job
        .defaults
        .get_or_insert(GiteaDefaults { run: None })
        .run
        .get_or_insert(GiteaRunDefaults {
            working_directory: None,
            shell: None,
        });
    run.working_directory
        .get_or_insert_with(|| working_directory.clone());
}

/// Emit one Actions step as a single subshell, so nothing the step sets up for
/// itself can reach the step after it.
///
/// Actions gives every step its own shell process: a step's `env:`, its
/// working directory, and anything its own script exports or `cd`s into all
/// end with the step. Appending the pieces to one flat job script broke that
/// boundary in the direction nobody declares — the *next* step inherited an
/// `export` the workflow only asked for on the previous one, and it had no way
/// to say otherwise.
///
/// Values are quoted the same way the directory is: expected runtime
/// placeholders such as `${MATRIX_OS}` stay expandable, while a space or a
/// shell metacharacter inside a declared value stays part of the value instead
/// of splitting into a second word (`env: MSG: hello world` used to export
/// `MSG=hello`).
fn step_command(
    command: &str,
    env: &HashMap<String, String>,
    working_directory: Option<&str>,
) -> String {
    let mut prologue = String::new();
    // Sorted, so the same workflow always renders the same script text —
    // `HashMap` order would otherwise churn the stored script between runs.
    let mut names = env.keys().collect::<Vec<_>>();
    names.sort();
    for name in names {
        prologue.push_str(&format!(
            "export {}={}\n",
            name,
            shell_double_quote(&env[name])
        ));
    }
    if let Some(directory) = working_directory {
        prologue.push_str(&format!("cd -- {}\n", shell_double_quote(directory)));
    }
    format!("(\n{}{}\n)", prologue, command)
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

/// Map a validated `runs-on` declaration to the runner tags persisted on the
/// job. Invalid or empty declarations cannot inhabit [`GiteaRunsOn`].
fn runs_on_tags(runs_on: &Option<GiteaRunsOn>) -> Option<Vec<String>> {
    runs_on.as_ref().map(GiteaRunsOn::tags)
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
            pin_workflow_working_directory(&mut job, &workflow.defaults);
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
        // This boundary flattens one workflow into another, so every new
        // workflow-level field must make an explicit survive-or-refuse choice.
        // `on` is checked above, `jobs`, `env`, and working-directory defaults
        // are consumed by the recursion, and `concurrency` is refused below.
        let GiteaWorkflow {
            name: _,
            on: _,
            jobs: _,
            concurrency,
            env: _,
            defaults: _,
        } = &called;
        if concurrency.is_some() {
            anyhow::bail!(
                "reusable workflow '{target}' declares `concurrency`; declare `concurrency` in the calling workflow instead"
            );
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

pub(crate) fn actions_condition_context(
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
        ("github.repository".into(), ctx.repository()),
        ("github.repository_owner".into(), ctx.repo_owner.clone()),
    ]);
    for (name, value) in variables {
        context.insert(format!("env.{name}"), value.clone());
    }
    context
}

/// Expand the `${{ … }}` expressions a `concurrency.group` may be built from.
///
/// The group is not a shell string — it is a database key that decides which
/// pipelines wait for or cancel each other — so it is expanded *here*, against
/// the same context the `if:` evaluator uses, rather than deferred to the job
/// environment the way `substitute_expr` defers `${{ github.ref }}` to
/// `${CI_REF}`. `github.workflow` is added on top: it exists only at this level
/// and the canonical GitHub group (`${{ github.workflow }}-${{ github.ref }}`)
/// is built from it.
///
/// Anything not in the context is left **verbatim**, expression braces and all.
/// That is the signal the caller refuses on: a group that still carries an
/// expression is a literal shared by every ref of the repository, which turns
/// `cancel-in-progress` into "cancel whatever else is running".
pub(crate) fn expand_concurrency_group(
    template: &str,
    ctx: &WorkflowContext,
    workflow_label: &str,
) -> String {
    let mut context = actions_condition_context(ctx, &HashMap::new());
    context.insert("github.workflow".into(), workflow_label.to_string());
    expand_expressions(template, |key| context.get(key).cloned())
}

/// Replace every `${{ key }}` the lookup answers for, leaving the rest as-is.
///
/// Written as a scan rather than a list of `replace("${{ github.ref }}", …)`
/// calls so that the un-spaced `${{github.ref}}` — which Actions accepts and a
/// fixed-string replace misses — expands too. A missed expansion here is not a
/// cosmetic difference; it is the difference between a per-ref group and one
/// global one.
fn expand_expressions(template: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("${{") {
        let after_open = &rest[open + 3..];
        let Some(close) = after_open.find("}}") else {
            break;
        };
        let key = after_open[..close].trim();
        match lookup(key) {
            Some(value) => {
                out.push_str(&rest[..open]);
                out.push_str(&value);
            }
            // Kept verbatim, braces included, so the caller can tell that this
            // group was not fully resolved.
            None => out.push_str(&rest[..open + 3 + close + 2]),
        }
        rest = &after_open[close + 2..];
    }
    out.push_str(rest);
    out
}

/// Basic `${{ expression }}` substitution.
fn substitute_expr(
    input: &str,
    _job_name: &str,
    workflow_env: &HashMap<String, String>,
    job_env: &HashMap<String, String>,
    step_env: Option<&HashMap<String, String>>,
) -> String {
    expand_expressions(input, |key| {
        if let Some((_, replacement)) = GITHUB_RUN_EXPRESSIONS
            .iter()
            .find(|(supported, _)| key == *supported)
        {
            return Some((*replacement).to_owned());
        }
        if let Some(name) = context_member(key, "env") {
            return Some(
                step_env
                    .and_then(|env| env.get(name))
                    .or_else(|| job_env.get(name))
                    .or_else(|| workflow_env.get(name))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        if let Some(name) = context_member(key, "secrets") {
            return Some(format!("${{{name}}}"));
        }
        if let Some(name) = context_member(key, "matrix") {
            return Some(format!(
                "${{MATRIX_{}}}",
                name.to_ascii_uppercase().replace('-', "_")
            ));
        }
        context_member(key, "inputs")
            .map(|name| format!("${{INPUT_{}}}", name.to_ascii_uppercase().replace('-', "_")))
    })
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
    fn run_repository_expressions_use_runner_variables_and_unknown_ones_are_rejected() {
        let workflow = GiteaWorkflow::parse(
            r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo "${{ github.repository }} ${{github.repository_owner}}"
"#,
        )
        .unwrap();
        workflow.validate_supported_actions().unwrap();
        let script = workflow.to_ci_config(&test_context()).jobs["build"]
            .script
            .join("\n");
        assert!(
            script.contains("${CI_REPOSITORY} ${CI_REPOSITORY_OWNER}"),
            "repository expressions did not reach the runner vocabulary: {script}"
        );

        let unknown = GiteaWorkflow::parse(
            r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo ${{ foo.bar }}
"#,
        )
        .unwrap();
        let error = unknown
            .validate_supported_actions()
            .expect_err("an unknown run expression must not reach the shell")
            .to_string();
        assert!(error.contains("build: step 1 run"), "{error}");
        assert!(error.contains("foo.bar"), "{error}");
    }

    #[test]
    fn unknown_event_filter_keys_are_rejected_by_name() {
        for (event, filter, expected) in [
            ("pull_request", "types: [labeled]", "pull_request.types"),
            ("push", "branch: [main]", "push.branch"),
        ] {
            let yaml = format!(
                "on:\n  {event}:\n    {filter}\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n"
            );
            let workflow = GiteaWorkflow::parse(&yaml).expect("the validator names the bad key");
            let error = workflow
                .validate_supported_triggers()
                .expect_err("an unknown event filter must not become an absent filter")
                .to_string();
            assert!(error.contains(expected), "missing {expected:?}: {error}");
            for &supported in SUPPORTED_EVENT_FILTERS {
                assert!(
                    error.contains(supported),
                    "the refusal must list supported filter {supported:?}: {error}"
                );
            }
        }
    }

    #[test]
    fn every_supported_event_filter_key_stays_accepted() {
        let workflow = GiteaWorkflow::parse(
            r#"
on:
  push:
    branches: [main]
    branches-ignore: [legacy]
    tags: ['v*']
    tags-ignore: ['v0.*']
    paths: ['src/**']
    paths-ignore: ['docs/**']
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - run: echo ok
"#,
        )
        .expect("all supported event filters parse");

        workflow
            .validate_supported_triggers()
            .expect("all supported event filters validate");
        let WorkflowTriggers::Single(trigger) = workflow.on else {
            panic!("the mapped `on:` form must stay mapped");
        };
        let filter = trigger.push.expect("push filter");
        assert!(
            [
                filter.branches,
                filter.branches_ignore,
                filter.tags,
                filter.tags_ignore,
                filter.paths,
                filter.paths_ignore,
            ]
            .into_iter()
            .all(|value| value.is_some()),
            "all six supported filters must survive deserialization"
        );
        assert!(filter.other.is_empty());
    }

    /// card_e949057aaa0d: a natively-implemented action must be honest about
    /// which of its inputs it implements.
    ///
    /// `actions/checkout` was skipped whole, so every `with:` key on it was
    /// accepted and dropped — a job asking to check out a different ref ran
    /// green against the pipeline's commit and reported nothing.
    #[test]
    fn checkout_inputs_the_workspace_cannot_honour_are_rejected_by_name() {
        let yml = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          ref: release
          submodules: true
      - run: cargo build
"#;
        let workflow = GiteaWorkflow::parse(yml).unwrap();
        let error = workflow
            .validate_supported_actions()
            .unwrap_err()
            .to_string();
        assert!(error.contains("input 'ref'"), "{error}");
        assert!(error.contains("input 'submodules'"), "{error}");
        assert!(
            error.contains("supported: fetch-depth"),
            "the message must say what IS honoured: {error}"
        );
    }

    /// The one checkout input the workspace does satisfy: the worktree carries
    /// the repository's full history at the pipeline commit, so any requested
    /// depth is already met.
    #[test]
    fn checkout_fetch_depth_is_accepted() {
        let yml = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - run: cargo build
"#;
        GiteaWorkflow::parse(yml)
            .unwrap()
            .validate_supported_actions()
            .expect("fetch-depth is met by the pipeline worktree");
    }

    /// `actions/cache` reads `path` and `key`. `restore-keys` would turn a miss
    /// into a fallback hit and `fail-on-cache-miss` would turn one into a job
    /// failure — accepting them reports the opposite of what was asked.
    #[test]
    fn cache_inputs_beyond_path_and_key_are_rejected_by_name() {
        let yml = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/cache@v4
        with:
          path: target
          key: build-${{ github.sha }}
          restore-keys: build-
          fail-on-cache-miss: true
      - run: cargo build
"#;
        let workflow = GiteaWorkflow::parse(yml).unwrap();
        let error = workflow
            .validate_supported_actions()
            .unwrap_err()
            .to_string();
        assert!(error.contains("input 'restore-keys'"), "{error}");
        assert!(error.contains("input 'fail-on-cache-miss'"), "{error}");
        assert!(error.contains("supported: path, key"), "{error}");

        // The two it does read stay accepted, or the rejection above would only
        // mean "cache is unsupported".
        let supported = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/cache@v4
        with:
          path: target
          key: build-key
      - run: cargo build
"#;
        GiteaWorkflow::parse(supported)
            .unwrap()
            .validate_supported_actions()
            .expect("path and key are the inputs the translation reads");
    }

    /// `container.options` is raw `docker run` flags, and the runner's sandbox
    /// is built out of exactly those. There is no partial support to offer.
    #[test]
    fn container_options_are_rejected_rather_than_dropped() {
        let yml = r#"
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    container:
      image: rust:1.75
      options: --privileged -v /var/run/docker.sock:/var/run/docker.sock
    steps:
      - run: cargo build
"#;
        let workflow = GiteaWorkflow::parse(yml).unwrap();
        let error = workflow
            .validate_supported_actions()
            .unwrap_err()
            .to_string();
        assert!(error.contains("container.options"), "{error}");
    }

    /// `container.env` is honoured instead: the runner turns a job's variables
    /// into the container's environment, which is what the key means.
    #[test]
    fn container_env_reaches_the_job_variables() {
        let yml = r#"
on: push
env:
  SHARED: workflow
jobs:
  build:
    runs-on: ubuntu-latest
    container:
      image: rust:1.75
      env:
        RUSTFLAGS: "-D warnings"
        SHARED: container
    steps:
      - run: cargo build
"#;
        let workflow = GiteaWorkflow::parse(yml).unwrap();
        workflow
            .validate_supported_actions()
            .expect("container.env is supported, not refused");
        let ctx = WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        };
        let job = workflow.to_ci_config(&ctx).jobs.remove("build").unwrap();
        let variables = job.variables.expect("container env must become variables");
        assert_eq!(
            variables.get("RUSTFLAGS").map(String::as_str),
            Some("-D warnings")
        );
        assert_eq!(
            variables.get("SHARED").map(String::as_str),
            Some("workflow"),
            "workflow-level env wins over the container's"
        );
    }

    #[test]
    fn env_expressions_use_the_narrowest_declared_scope() {
        let yml = r#"
on: push
env:
  MODE: workflow
jobs:
  job-override:
    runs-on: ubuntu-latest
    env:
      MODE: job
    steps:
      - run: echo "job=${{ env.MODE }}"
  step-override:
    runs-on: ubuntu-latest
    env:
      MODE: job
    steps:
      - env:
          MODE: step
        run: echo "step=${{ env.MODE }}"
"#;
        let workflow = GiteaWorkflow::parse(yml).unwrap();
        workflow
            .validate_supported_actions()
            .expect("workflow, job, and step env expressions are supported");
        let config = workflow.to_ci_config(&test_context());

        let job_script = config.jobs["job-override"].script.join("\n");
        assert!(job_script.contains("echo \"job=job\""), "{job_script}");

        let step_script = config.jobs["step-override"].script.join("\n");
        assert!(
            step_script.contains("export MODE=\"step\""),
            "{step_script}"
        );
        assert!(step_script.contains("echo \"step=step\""), "{step_script}");
    }

    /// A step's `env:` is a declaration about *that* step. The flat job script
    /// used to append its `export`s and leave them standing, so the next step
    /// ran with variables no workflow line ever gave it — and the only way the
    /// author could find out was a command behaving differently than written.
    #[cfg(unix)]
    #[test]
    fn step_env_ends_with_the_step_that_declared_it() {
        let workflow = GiteaWorkflow::parse(
            r#"
on: push
env:
  MODE: workflow
jobs:
  build:
    runs-on: ubuntu-latest
    env:
      MODE: job
    steps:
      - env:
          MODE: step
          MESSAGE: hello world
        run: |
          test "$MODE" = step
          test "$MESSAGE" = "hello world"
          export ESCAPEE=leaked
          cd /
      - run: |
          test "$MODE" = job
          test -z "${MESSAGE-}"
          test -z "${ESCAPEE-}"
          test -f workspace-marker
"#,
        )
        .unwrap();
        workflow.validate_supported_actions().unwrap();
        let config = workflow.to_ci_config(&test_context());
        let job = &config.jobs["build"];

        // What the runner hands the shell as the process environment: the job
        // scope lives here, so a leaking step scope is the only way step two
        // could see anything else.
        let variables = job
            .variables
            .clone()
            .expect("job-level env becomes job variables");
        assert_eq!(variables.get("MODE").map(String::as_str), Some("job"));

        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("workspace-marker"), "ok").unwrap();
        let script = job.script.join("\n");
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&script)
            .current_dir(workspace.path())
            .env_clear()
            .envs(&variables)
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "script leaked step state into the following step:\n{script}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
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

    #[cfg(unix)]
    #[test]
    fn reusable_workflow_defaults_stay_scoped_to_their_declaring_file() {
        let workspace = tempfile::tempdir().unwrap();
        for (directory, marker) in [
            ("caller dir", "caller-marker"),
            ("called dir", "called-marker"),
            ("job dir", "job-marker"),
            ("nested dir", "nested-marker"),
        ] {
            let directory = workspace.path().join(directory);
            std::fs::create_dir(&directory).unwrap();
            std::fs::write(directory.join(marker), "ok").unwrap();
        }
        std::fs::write(workspace.path().join("workspace-marker"), "ok").unwrap();

        let caller = GiteaWorkflow::parse(
            r#"
on: push
defaults:
  run:
    working-directory: caller dir
jobs:
  local:
    steps:
      - run: test -f caller-marker
  shared:
    uses: ./.gitea/workflows/shared.yml
"#,
        )
        .unwrap();
        let sources = HashMap::from([
            (
                "shared.yml".into(),
                r#"
on: workflow_call
defaults:
  run:
    working-directory: called dir
jobs:
  inherited:
    steps:
      - run: test -f called-marker
  overridden:
    defaults:
      run:
        working-directory: job dir
    steps:
      - run: test -f job-marker
  nested-own:
    uses: ./.gitea/workflows/nested-own.yml
  nested-root:
    uses: ./.gitea/workflows/nested-root.yml
"#
                .into(),
            ),
            (
                "nested-own.yml".into(),
                r#"
on: workflow_call
defaults:
  run:
    working-directory: nested dir
jobs:
  check:
    steps:
      - run: test -f nested-marker
"#
                .into(),
            ),
            (
                "nested-root.yml".into(),
                r#"
on: workflow_call
jobs:
  check:
    steps:
      - run: test -f workspace-marker
"#
                .into(),
            ),
        ]);
        let expanded = caller.expand_local_reusable_workflows(&sources).unwrap();
        let config = expanded.to_ci_config(&test_context());

        for job_name in [
            "local",
            "shared/inherited",
            "shared/overridden",
            "shared/nested-own/check",
            "shared/nested-root/check",
        ] {
            let script = config.jobs[job_name].script.join("\n");
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(&script)
                .current_dir(workspace.path())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{job_name} used defaults from another workflow:\n{script}\n{}",
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
        // Each step is emitted as its own subshell, so the command is a line
        // inside a script entry rather than the whole entry.
        let script = ci.jobs["test"].script.join("\n");
        assert!(script.contains("\necho yes\n"), "{script}");
        assert!(!script.contains("echo no"), "{script}");

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
        wf.validate_supported_triggers()
            .expect("push and pull_request both have producers");
    }

    /// card_c8f24edaee89 — the three triggers that parsed and never ran.
    ///
    /// Asserting the refusal *and* the message: "this workflow never fires" is
    /// exactly the state the author could already observe, so a bare `is_err()`
    /// would be satisfied by a refusal that says nothing about which trigger is
    /// the problem or what to write instead.
    #[test]
    fn a_trigger_no_producer_emits_is_refused_by_name() {
        for (label, on) in [
            (
                "schedule, mapped shape",
                "on:\n  schedule:\n    - cron: '0 3 * * *'\n",
            ),
            ("schedule, bare name", "on: schedule\n"),
            ("schedule, in an array", "on: [push, schedule]\n"),
            ("pull_request_target", "on:\n  pull_request_target:\n"),
            ("an event this engine has never heard of", "on: release\n"),
            // Serde drops unknown fields, so before `other` this shape declared
            // *nothing* and the refusal could not have named `release`.
            (
                "an unknown event in the mapped shape",
                "on:\n  release:\n    types: [published]\n",
            ),
        ] {
            let yml = format!("{on}jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n");
            let workflow =
                GiteaWorkflow::parse(&yml).unwrap_or_else(|e| panic!("{label} must parse: {e}"));
            let error = workflow
                .validate_supported_triggers()
                .expect_err(&format!("{label} must be refused, not silently accepted"))
                .to_string();
            let named = on
                .contains("schedule")
                .then_some("schedule")
                .or(on
                    .contains("pull_request_target")
                    .then_some("pull_request_target"))
                .unwrap_or("release");
            assert!(
                error.contains(named),
                "{label}: the refusal must name the offending trigger, got: {error}"
            );
            assert!(
                error.contains("workflow_dispatch"),
                "{label}: the refusal must list what IS supported, got: {error}"
            );
        }
    }

    /// The other half: everything with a producer still loads, and so does a
    /// reusable file, whose `workflow_call` is a declaration rather than an
    /// event and must not be mistaken for an unrunnable trigger.
    #[test]
    fn every_trigger_with_a_producer_is_accepted() {
        let body =
            "jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo hi\n";
        for event in rg_core::ci::PIPELINE_EVENTS {
            let mapped = format!("on:\n  {event}:\n{body}");
            GiteaWorkflow::parse(&mapped)
                .unwrap()
                .validate_supported_triggers()
                .unwrap_or_else(|e| panic!("mapped `on: {event}` must be accepted: {e}"));
            let bare = format!("on: {event}\n{body}");
            GiteaWorkflow::parse(&bare)
                .unwrap()
                .validate_supported_triggers()
                .unwrap_or_else(|e| panic!("bare `on: {event}` must be accepted: {e}"));
        }
        let reusable = format!("on:\n  workflow_call:\n{body}");
        GiteaWorkflow::parse(&reusable)
            .unwrap()
            .validate_supported_triggers()
            .expect("a reusable workflow declares no event and is still valid");
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

    /// …and the matcher answers every one of them, each under **its own name**.
    /// A name in the canon that the matcher does not handle is the same dead end
    /// from the other side, and it does not stop being one because some *other*
    /// key happens to cover the event: `merge_group` was reachable only by
    /// declaring `pull_request`, so the alias branch this loop used to carry was
    /// the bug, written down as if it were the rule (card_69d4f18b0b23).
    ///
    /// Every event is spelled here as it is raised — including the empty-bodied
    /// `on:\n  <event>:` form, which read as "not declared" until this test
    /// asked.
    #[test]
    fn the_matcher_answers_yes_to_a_workflow_that_declares_any_canonical_event() {
        for event in rg_core::ci::PIPELINE_EVENTS {
            let yaml = format!(
                "name: W\non:\n  {event}:\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n"
            );
            let workflow: GiteaWorkflow =
                serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("parse {event}: {e}"));
            workflow
                .validate_supported_triggers()
                .unwrap_or_else(|e| panic!("`on: {event}:` refused by validation: {e:#}"));
            assert!(
                workflow.matches_event(event, "refs/heads/main", "main", &ChangedPaths::unknown()),
                "a workflow declaring `on: {event}:` is not matched by {event}"
            );
        }
    }

    /// `${{ github.repository }}` names the repository the run belongs to, and
    /// for a long while it named nothing: the two fields behind it were declared
    /// on `WorkflowContext`, filled with `String::new() // filled later` by the
    /// only producer, and read by nobody (card_054e997a46e6). A step condition
    /// comparing against them was therefore *always false*, which is
    /// indistinguishable from a condition that correctly did not match.
    #[test]
    fn a_step_condition_can_name_the_repository_the_run_belongs_to() {
        let workflow = GiteaWorkflow::parse(
            "name: W
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - if: github.repository == 'owner/repo'
        run: echo mine
      - if: github.repository_owner == 'owner'
        run: echo my-owner
      - if: github.repository == 'someone/else'
        run: echo theirs
",
        )
        .expect("parse");
        let ci = workflow.to_ci_config(&WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc123".into(),
            event: "push".into(),
            repo_owner: "owner".into(),
            repo_name: "repo".into(),
        });
        let script = ci.jobs["build"].script.join("\n");

        assert!(script.contains("echo mine"), "{script}");
        assert!(script.contains("echo my-owner"), "{script}");
        assert!(
            !script.contains("echo theirs"),
            "a condition naming another repository must not run here: {script}"
        );
    }

    /// The alias is intentional and stays: the queue builds a speculative merge
    /// of the pull request, so a workflow that only ever mentions
    /// `pull_request` is still the workflow that has to gate the merge.
    #[test]
    fn a_pull_request_workflow_still_gates_the_merge_queue() {
        // …in every shape `on:` can take. The filterless spellings are the
        // common ones and were the ones the alias never reached.
        for on in [
            "on: pull_request\n",
            "on: [push, pull_request]\n",
            "on:\n  pull_request:\n",
        ] {
            let workflow = GiteaWorkflow::parse(&format!(
                "name: W\n{on}jobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n"
            ))
            .unwrap_or_else(|e| panic!("parse {on:?}: {e}"));
            assert!(
                workflow.matches_event(
                    "merge_group",
                    "refs/heads/gh-readonly-queue/main/pr-1",
                    "main",
                    &ChangedPaths::unknown()
                ),
                "`{on}` leaves the merge queue with nothing to run"
            );
        }

        let workflow = GiteaWorkflow::parse(
            "name: W\non:\n  pull_request:\n    branches: [main]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
        )
        .expect("parse");
        assert!(workflow.matches_event(
            "merge_group",
            "refs/heads/gh-readonly-queue/main/pr-1",
            "main",
            &ChangedPaths::unknown()
        ));
        assert!(
            !workflow.matches_event(
                "merge_group",
                "refs/heads/gh-readonly-queue/release/pr-1",
                "release",
                &ChangedPaths::unknown()
            ),
            "the `branches:` filter is about the branch the queue merges into"
        );
    }

    /// And a `merge_group:` of its own answers only for the merge queue — the
    /// fix must not turn it into a second way of declaring `pull_request`.
    #[test]
    fn a_merge_group_declaration_does_not_also_match_pull_request() {
        let workflow = GiteaWorkflow::parse(
            "name: W\non:\n  merge_group:\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
        )
        .expect("parse");
        assert!(workflow.matches_event(
            "merge_group",
            "refs/heads/main",
            "main",
            &ChangedPaths::unknown()
        ));
        assert!(!workflow.matches_event(
            "pull_request",
            "refs/heads/feature",
            "main",
            &ChangedPaths::unknown()
        ));
        assert!(!workflow.matches_event(
            "push",
            "refs/heads/main",
            "main",
            &ChangedPaths::unknown()
        ));
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
