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

    /// Values of the `inputs` context after the declaring trigger schema has
    /// supplied defaults and checked the caller. They are kept typed while a
    /// reusable workflow is expanded, then exposed to executable jobs through
    /// the runner's existing `INPUT_*` vocabulary.
    #[serde(skip)]
    resolved_inputs: HashMap<String, GiteaInputValue>,
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
    #[serde(default, deserialize_with = "deserialize_present_trigger")]
    pub workflow_dispatch: Option<GiteaTriggerDeclaration>,
    #[serde(default, deserialize_with = "deserialize_present_trigger")]
    pub workflow_call: Option<GiteaTriggerDeclaration>,
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

/// The schema-bearing body shared by `workflow_dispatch` and `workflow_call`.
///
/// The two triggers allow different input types, which is checked below. A
/// flattened remainder is deliberate: [`WorkflowTriggers`] is untagged, so a
/// nested `deny_unknown_fields` error would otherwise collapse into serde's
/// context-free "did not match any variant" instead of naming the author's
/// qualified key.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct GiteaTriggerDeclaration {
    #[serde(default)]
    inputs: HashMap<String, GiteaInputDefinition>,
    #[serde(flatten)]
    other: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GiteaInputDefinition {
    /// Metadata is preserved and used in a missing-required diagnostic. It does
    /// not change the value delivered to a job.
    description: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(rename = "type")]
    input_type: Option<GiteaInputType>,
    default: Option<GiteaInputValue>,
    options: Option<Vec<String>>,
    #[serde(flatten)]
    other: HashMap<String, serde_yaml::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GiteaInputType {
    Boolean,
    Choice,
    Number,
    Environment,
    String,
    Unknown(String),
}

impl GiteaInputType {
    fn as_str(&self) -> &str {
        match self {
            Self::Boolean => "boolean",
            Self::Choice => "choice",
            Self::Number => "number",
            Self::Environment => "environment",
            Self::String => "string",
            Self::Unknown(value) => value,
        }
    }

    fn implicit_default(&self, options: Option<&[String]>) -> GiteaInputValue {
        match self {
            Self::Boolean => GiteaInputValue::Boolean(false),
            Self::Number => GiteaInputValue::Number(serde_yaml::Number::from(0)),
            Self::Choice => GiteaInputValue::String(
                options
                    .and_then(|values| values.first())
                    .cloned()
                    .unwrap_or_default(),
            ),
            Self::Environment | Self::String | Self::Unknown(_) => {
                GiteaInputValue::String(String::new())
            }
        }
    }

    fn parses_manual_value(&self, value: &str) -> Option<GiteaInputValue> {
        match self {
            Self::Boolean => match value {
                "true" => Some(GiteaInputValue::Boolean(true)),
                "false" => Some(GiteaInputValue::Boolean(false)),
                _ => None,
            },
            Self::Number => value
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .and_then(|_| serde_yaml::from_str::<serde_yaml::Number>(value).ok())
                .map(GiteaInputValue::Number),
            Self::Choice | Self::Environment | Self::String => {
                Some(GiteaInputValue::String(value.to_owned()))
            }
            Self::Unknown(_) => None,
        }
    }
}

impl<'de> Deserialize<'de> for GiteaInputType {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "boolean" => Self::Boolean,
            "choice" => Self::Choice,
            "number" => Self::Number,
            "environment" => Self::Environment,
            "string" => Self::String,
            _ => Self::Unknown(value),
        })
    }
}

/// Scalar input values retain their YAML type until the called schema has
/// checked it. Conversion to strings happens only at the runner environment
/// boundary, where every job variable is a string by contract.
#[derive(Debug, Clone, PartialEq)]
pub enum GiteaInputValue {
    String(String),
    Boolean(bool),
    Number(serde_yaml::Number),
}

impl GiteaInputValue {
    fn from_yaml(value: serde_yaml::Value) -> std::result::Result<Self, String> {
        match value {
            serde_yaml::Value::String(value) => Ok(Self::String(value)),
            serde_yaml::Value::Bool(value) => Ok(Self::Boolean(value)),
            serde_yaml::Value::Number(value) => Ok(Self::Number(value)),
            _ => Err("must be a string, number, or boolean".into()),
        }
    }

    fn as_string(&self) -> String {
        match self {
            Self::String(value) => value.clone(),
            Self::Boolean(value) => value.to_string(),
            Self::Number(value) => value.to_string(),
        }
    }
}

impl<'de> Deserialize<'de> for GiteaInputValue {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_yaml::Value::deserialize(deserializer)?;
        Self::from_yaml(value).map_err(serde::de::Error::custom)
    }
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

fn input_environment_name(name: &str) -> String {
    format!("INPUT_{}", name.to_ascii_uppercase().replace('-', "_"))
}

fn valid_input_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

fn input_value_matches_type(value: &GiteaInputValue, input_type: &GiteaInputType) -> bool {
    matches!(
        (value, input_type),
        (GiteaInputValue::Boolean(_), GiteaInputType::Boolean)
            | (GiteaInputValue::Number(_), GiteaInputType::Number)
            | (
                GiteaInputValue::String(_),
                GiteaInputType::Choice | GiteaInputType::Environment | GiteaInputType::String
            )
    )
}

fn collect_trigger_input_schema_issues(
    trigger_name: &str,
    declaration: &GiteaTriggerDeclaration,
    is_dispatch: bool,
    issues: &mut Vec<String>,
) {
    issues.extend(
        declaration
            .other
            .keys()
            .map(|key| format!("{trigger_name}.{key}")),
    );
    if is_dispatch && declaration.inputs.len() > 25 {
        issues.push(format!(
            "{trigger_name}.inputs declares {} entries (maximum: 25)",
            declaration.inputs.len()
        ));
    }

    let mut environment_names: HashMap<String, String> = HashMap::new();
    let mut names = declaration.inputs.keys().collect::<Vec<_>>();
    names.sort();
    for name in names {
        let definition = &declaration.inputs[name];
        let prefix = format!("{trigger_name}.inputs.{name}");
        if !valid_input_name(name) {
            issues.push(format!(
                "{prefix} has an invalid input name (expected letter/_ followed by letters, numbers, _ or -)"
            ));
        }
        let environment_name = input_environment_name(name);
        if let Some(previous) = environment_names.insert(environment_name.clone(), name.clone()) {
            issues.push(format!(
                "{prefix} collides with {trigger_name}.inputs.{previous} as {environment_name}"
            ));
        }
        issues.extend(definition.other.keys().map(|key| format!("{prefix}.{key}")));

        let Some(input_type) = &definition.input_type else {
            issues.push(format!("{prefix}.type (required)"));
            continue;
        };
        let allowed = match input_type {
            GiteaInputType::Boolean | GiteaInputType::Number | GiteaInputType::String => true,
            GiteaInputType::Choice | GiteaInputType::Environment => is_dispatch,
            GiteaInputType::Unknown(_) => false,
        };
        if !allowed {
            let supported = if is_dispatch {
                "boolean, choice, number, environment, string"
            } else {
                "boolean, number, string"
            };
            issues.push(format!(
                "{prefix}.type={} (supported: {supported})",
                input_type.as_str()
            ));
            continue;
        }

        match (input_type, definition.options.as_deref()) {
            (GiteaInputType::Choice, Some([])) => {
                issues.push(format!(
                    "{prefix}.options (choice requires a non-empty list)"
                ));
            }
            (GiteaInputType::Choice, None) => {
                issues.push(format!("{prefix}.options (required for choice)"));
            }
            (GiteaInputType::Choice, Some(options)) => {
                let unique = options.iter().collect::<std::collections::HashSet<_>>();
                if unique.len() != options.len() {
                    issues.push(format!("{prefix}.options contains duplicate values"));
                }
            }
            (_, Some(_)) => issues.push(format!(
                "{prefix}.options (supported only when type is choice)"
            )),
            (_, None) => {}
        }

        if let Some(default) = &definition.default {
            if !input_value_matches_type(default, input_type) {
                issues.push(format!(
                    "{prefix}.default must have type {}",
                    input_type.as_str()
                ));
            } else if let (GiteaInputType::Choice, GiteaInputValue::String(value), Some(options)) =
                (input_type, default, definition.options.as_ref())
            {
                if !options.contains(value) {
                    issues.push(format!("{prefix}.default must be one of its options"));
                }
            }
        }
    }
}

fn undeclared_input_names<'a>(
    definitions: Option<&HashMap<String, GiteaInputDefinition>>,
    provided: impl Iterator<Item = &'a String>,
) -> Vec<String> {
    let mut unknown = provided
        .filter(|name| definitions.is_none_or(|definitions| !definitions.contains_key(*name)))
        .cloned()
        .collect::<Vec<_>>();
    unknown.sort();
    unknown
}

fn resolve_declared_inputs(
    trigger_name: &str,
    definitions: Option<&HashMap<String, GiteaInputDefinition>>,
    provided: &HashMap<String, GiteaInputValue>,
    implicit_defaults: bool,
) -> Result<HashMap<String, GiteaInputValue>> {
    let Some(definitions) = definitions else {
        return Ok(HashMap::new());
    };
    let mut names = definitions.keys().collect::<Vec<_>>();
    names.sort();
    let mut resolved = HashMap::new();
    for name in names {
        let definition = &definitions[name];
        let input_type = definition
            .input_type
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("{trigger_name}.inputs.{name}.type is required"))?;
        let value = if let Some(value) = provided.get(name) {
            value.clone()
        } else if let Some(default) = &definition.default {
            default.clone()
        } else if definition.required {
            let description = definition
                .description
                .as_deref()
                .map(|description| format!(" ({description})"))
                .unwrap_or_default();
            anyhow::bail!("{trigger_name} required input '{name}'{description} was not provided");
        } else if implicit_defaults {
            input_type.implicit_default(definition.options.as_deref())
        } else {
            continue;
        };

        if !input_value_matches_type(&value, input_type) {
            anyhow::bail!(
                "{trigger_name} input '{name}' must have type {}",
                input_type.as_str()
            );
        }
        if let (GiteaInputType::Choice, GiteaInputValue::String(value), Some(options)) =
            (input_type, &value, definition.options.as_ref())
        {
            if !options.contains(value) {
                anyhow::bail!(
                    "{trigger_name} input '{name}' must be one of: {}",
                    options.join(", ")
                );
            }
        }
        resolved.insert(name.clone(), value);
    }
    Ok(resolved)
}

fn resolve_reusable_input_expression(
    value: &GiteaInputValue,
    caller_inputs: &HashMap<String, GiteaInputValue>,
) -> std::result::Result<GiteaInputValue, String> {
    let GiteaInputValue::String(value) = value else {
        return Ok(value.clone());
    };
    if !value.contains("${{") {
        return Ok(GiteaInputValue::String(value.clone()));
    }
    let trimmed = value.trim();
    let Some(expression) = trimmed
        .strip_prefix("${{")
        .and_then(|value| value.strip_suffix("}}"))
        .map(str::trim)
        .and_then(|key| context_member(key, "inputs"))
    else {
        return Err("only a complete `${{ inputs.<name> }}` expression is supported here".into());
    };
    caller_inputs
        .get(expression)
        .cloned()
        .ok_or_else(|| format!("references undeclared caller input '{expression}'"))
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
    pub with: HashMap<String, GiteaInputValue>,

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

/// The `uses:` values this engine implements natively.
///
/// A constant rather than two literals inside the filter below, because the
/// list is half of what `docs/gitea-actions.md` promises an author: the page is
/// held to it by `every_boundary_the_engine_enforces_is_named_in_the_documentation`,
/// so an action gained or lost cannot leave the page behind.
const SUPPORTED_ACTIONS: &[&str] = &["actions/checkout@", "actions/cache@"];

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

    fn trigger_input_definitions(
        &self,
        trigger_name: &str,
    ) -> Option<&HashMap<String, GiteaInputDefinition>> {
        let WorkflowTriggers::Single(trigger) = &self.on else {
            return None;
        };
        match trigger_name {
            "workflow_dispatch" => trigger
                .workflow_dispatch
                .as_ref()
                .map(|declaration| &declaration.inputs),
            WORKFLOW_CALL_TRIGGER => trigger
                .workflow_call
                .as_ref()
                .map(|declaration| &declaration.inputs),
            _ => None,
        }
    }

    fn validate_trigger_input_schemas(&self) -> Result<()> {
        let WorkflowTriggers::Single(trigger) = &self.on else {
            return Ok(());
        };
        let mut issues = Vec::new();
        if let Some(declaration) = &trigger.workflow_dispatch {
            collect_trigger_input_schema_issues(
                "workflow_dispatch",
                declaration,
                true,
                &mut issues,
            );
        }
        if let Some(declaration) = &trigger.workflow_call {
            collect_trigger_input_schema_issues(
                WORKFLOW_CALL_TRIGGER,
                declaration,
                false,
                &mut issues,
            );
        }
        issues.sort();
        if issues.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("invalid trigger input schema: {}", issues.join(", "))
        }
    }

    /// The manual-run form declared by this workflow, if it is dispatchable.
    ///
    /// Simple/array spellings can only declare a bare trigger and therefore
    /// produce an empty form. Mapping declarations retain the validated schema
    /// that [`resolve_dispatch_inputs`](Self::resolve_dispatch_inputs) consumes,
    /// so the web form and the runner do not grow separate type vocabularies.
    pub(crate) fn workflow_dispatch_input_schema(
        &self,
    ) -> Result<Option<Vec<rg_core::ci::WorkflowDispatchInput>>> {
        let definitions = match &self.on {
            WorkflowTriggers::Simple(name) if name == rg_core::ci::WORKFLOW_DISPATCH_EVENT => {
                return Ok(Some(Vec::new()));
            }
            WorkflowTriggers::Array(names)
                if names
                    .iter()
                    .any(|name| name == rg_core::ci::WORKFLOW_DISPATCH_EVENT) =>
            {
                return Ok(Some(Vec::new()));
            }
            WorkflowTriggers::Single(trigger) => trigger
                .workflow_dispatch
                .as_ref()
                .map(|declaration| &declaration.inputs),
            _ => None,
        };
        let Some(definitions) = definitions else {
            return Ok(None);
        };

        let mut names = definitions.keys().collect::<Vec<_>>();
        names.sort();
        let mut inputs = Vec::with_capacity(names.len());
        for name in names {
            let definition = &definitions[name];
            let input_type = definition.input_type.as_ref().ok_or_else(|| {
                anyhow::anyhow!("workflow_dispatch.inputs.{name}.type is required")
            })?;
            inputs.push(rg_core::ci::WorkflowDispatchInput {
                name: name.clone(),
                description: definition.description.clone(),
                required: definition.required,
                input_type: input_type.as_str().to_owned(),
                default: definition.default.as_ref().map(GiteaInputValue::as_string),
                options: definition.options.clone().unwrap_or_default(),
            });
        }
        Ok(Some(inputs))
    }

    pub(crate) fn resolve_dispatch_inputs(
        &mut self,
        provided: &HashMap<String, String>,
    ) -> Result<()> {
        let definitions = self.trigger_input_definitions("workflow_dispatch");
        let unknown = undeclared_input_names(definitions, provided.keys());
        if !unknown.is_empty() {
            anyhow::bail!(
                "workflow_dispatch received undeclared input(s): {}",
                unknown.join(", ")
            );
        }

        let mut typed = HashMap::new();
        if let Some(definitions) = definitions {
            for (name, value) in provided {
                let definition = &definitions[name];
                let input_type = definition.input_type.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("workflow_dispatch.inputs.{name}.type is required")
                })?;
                let value = input_type.parses_manual_value(value).ok_or_else(|| {
                    anyhow::anyhow!(
                        "workflow_dispatch input '{name}' must have type {}",
                        input_type.as_str()
                    )
                })?;
                typed.insert(name.clone(), value);
            }
        }
        let resolved = resolve_declared_inputs("workflow_dispatch", definitions, &typed, true)?;
        self.install_inputs(resolved);
        Ok(())
    }

    fn resolve_reusable_inputs(
        &mut self,
        job_name: &str,
        provided: &HashMap<String, GiteaInputValue>,
        caller_inputs: &HashMap<String, GiteaInputValue>,
    ) -> Result<()> {
        let definitions = self.trigger_input_definitions(WORKFLOW_CALL_TRIGGER);
        let unknown = undeclared_input_names(definitions, provided.keys());
        if !unknown.is_empty() {
            anyhow::bail!(
                "reusable workflow job '{job_name}' passes undeclared input(s): {}",
                unknown.join(", ")
            );
        }

        let provided = provided
            .iter()
            .map(|(name, value)| {
                resolve_reusable_input_expression(value, caller_inputs).map_or_else(
                    |error| Err(anyhow::anyhow!("jobs.{job_name}.with.{name}: {error}")),
                    |value| Ok((name.clone(), value)),
                )
            })
            .collect::<Result<HashMap<_, _>>>()?;
        let resolved =
            resolve_declared_inputs(WORKFLOW_CALL_TRIGGER, definitions, &provided, true)?;
        self.install_inputs(resolved);
        Ok(())
    }

    fn install_inputs(&mut self, inputs: HashMap<String, GiteaInputValue>) {
        for job in self.jobs.values_mut() {
            for (name, value) in &inputs {
                job.env
                    .insert(input_environment_name(name), value.as_string());
            }
        }
        self.resolved_inputs = inputs;
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

    /// The `(trigger, filter)` pairs this workflow declares.
    ///
    /// The destructuring is exhaustive for the reason [`Self::declared_triggers`]
    /// gives: a filter-carrying trigger added to [`WorkflowTriggerSingle`]
    /// without a line here would be validated by nobody.
    fn event_filters(&self) -> Vec<(&'static str, &EventFilter)> {
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

        [
            ("push", push.as_ref()),
            (PULL_REQUEST_TRIGGER, pull_request.as_ref()),
            (PULL_REQUEST_TARGET_TRIGGER, pull_request_target.as_ref()),
            (MERGE_GROUP_TRIGGER, merge_group.as_ref()),
        ]
        .into_iter()
        .filter_map(|(trigger_name, filter)| filter.map(|filter| (trigger_name, filter)))
        .collect()
    }

    /// Event-filter declarations that parsed but have no consumer.
    fn unsupported_event_filter_keys(&self) -> Vec<String> {
        let mut unsupported = self
            .event_filters()
            .into_iter()
            .flat_map(|(trigger_name, filter)| {
                filter
                    .other
                    .keys()
                    .map(move |key| format!("{trigger_name}.{key}"))
            })
            .collect::<Vec<_>>();
        unsupported.sort();
        unsupported
    }

    /// Filter patterns whose spelling this engine cannot honour as written.
    ///
    /// `!` is the one filter metacharacter with a *list*-level meaning: it
    /// excludes what an earlier pattern of the same list selected, so two
    /// shapes of it cannot mean anything, and GitHub's own documentation
    /// refuses both. A list of nothing but negations has nothing to subtract
    /// from — `branches: ['!main']` selects no branch and the workflow runs
    /// nowhere — and a negation inside a `-ignore` list is a second negative on
    /// a key that is already an exclusion. The third shape is local to this
    /// matcher: a pattern ending in a lone `\` escapes nothing and can match
    /// no ref or path at all.
    ///
    /// The fourth shape is not about `!` at all: `+` and `[…]` are
    /// metacharacters of the dialect this file's documentation points at, and
    /// [`glob_segments`] implements neither — see
    /// [`unimplemented_metacharacters`]. Refusing them by name costs the
    /// patterns that use one as an ordinary character today, which is why the
    /// message names `\+` / `\[` as the spelling that keeps working.
    ///
    /// Naming them here is the answer [`Self::unsupported_event_filter_keys`]
    /// already gives an unknown key. The alternative is what this tree did
    /// before: the pattern parsed, matched nothing anybody meant, and the
    /// author's only evidence was a job running on the wrong pushes — or on
    /// none (card_8dc2adb75578).
    fn unhonourable_filter_patterns(&self) -> Vec<String> {
        let mut defects = Vec::new();
        for (trigger_name, filter) in self.event_filters() {
            // The third element names the exclusion key to point the author at,
            // and its absence marks the exclusion lists themselves.
            for (key, patterns, ignore_alternative) in [
                (
                    "branches",
                    filter.branches.as_ref(),
                    Some("branches-ignore"),
                ),
                ("tags", filter.tags.as_ref(), Some("tags-ignore")),
                ("paths", filter.paths.as_ref(), Some("paths-ignore")),
                ("branches-ignore", filter.branches_ignore.as_ref(), None),
                ("tags-ignore", filter.tags_ignore.as_ref(), None),
                ("paths-ignore", filter.paths_ignore.as_ref(), None),
            ] {
                let Some(patterns) = patterns else { continue };
                for pattern in patterns {
                    if ends_with_dangling_escape(pattern) {
                        defects.push(format!(
                            "{trigger_name}.{key}: `{pattern}` ends in a lone `\\`, which escapes \
                             nothing and matches nothing"
                        ));
                    }
                    for (metacharacter, meaning) in unimplemented_metacharacters(pattern) {
                        defects.push(format!(
                            "{trigger_name}.{key}: `{pattern}` uses `{metacharacter}`, a filter \
                             metacharacter this engine does not implement — GitHub reads it as \
                             \"{meaning}\", and here it would be matched as the literal \
                             character. Write `\\{metacharacter}` if the literal character is \
                             what you meant"
                        ));
                    }
                    if ignore_alternative.is_none() && negated_pattern(pattern).is_some() {
                        defects.push(format!(
                            "{trigger_name}.{key}: `{pattern}` negates a pattern inside an \
                             exclusion list, which has nothing to exclude it from"
                        ));
                    }
                }
                if let Some(ignore_alternative) = ignore_alternative {
                    if !patterns.is_empty()
                        && patterns
                            .iter()
                            .all(|pattern| negated_pattern(pattern).is_some())
                    {
                        defects.push(format!(
                            "{trigger_name}.{key}: every pattern is negated, so nothing is ever \
                             selected for `!` to exclude and the workflow would run nowhere. Add \
                             one pattern without `!`, or use `{trigger_name}.{ignore_alternative}`"
                        ));
                    }
                }
            }
        }
        defects.sort();
        defects
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
        self.validate_trigger_input_schemas()?;
        let unsupported_filters = self.unsupported_event_filter_keys();
        if !unsupported_filters.is_empty() {
            anyhow::bail!(
                "unsupported event filter key(s): {}. Supported event filters: {}",
                unsupported_filters.join(", "),
                SUPPORTED_EVENT_FILTERS.join(", ")
            );
        }
        let unhonourable_patterns = self.unhonourable_filter_patterns();
        if !unhonourable_patterns.is_empty() {
            anyhow::bail!(
                "event filter pattern(s) this engine cannot honour: {}",
                unhonourable_patterns.join("; ")
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
                            !SUPPORTED_ACTIONS
                                .iter()
                                .any(|action| uses.starts_with(action))
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
        unsupported.extend(
            self.jobs
                .iter()
                .filter(|(_, job)| job.uses.is_none() && !job.with.is_empty())
                .map(|(job_name, _)| format!("{job_name}: with without a reusable workflow uses")),
        );
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

    /// Reject a `needs:` that names a job this workflow does not declare, and a
    /// `needs:` graph that closes on itself.
    ///
    /// [`Self::compute_job_stages`] gives a job the stage
    /// `max(stage of its needs) + 1` and hands whatever it could not resolve to
    /// `job_stage.entry(name).or_insert(0)`. That fallback reads both mistakes
    /// as the same thing — *no dependency at all*. `needs: [buidl]` under a job
    /// spelled `build` did not fail the workflow, it removed the constraint: the
    /// job landed in stage 0 and ran alongside the one it was written to wait
    /// for, so a `deploy` whose `needs: [test]` carried a typo deployed while
    /// the tests were still running. A cycle came out the same way, one stage
    /// holding jobs that each declared they were waiting on the other
    /// (card_85ba100789a8).
    ///
    /// Called after [`Self::expand_local_reusable_workflows`], so the names
    /// judged here are the prefixed, flattened ones the stage pass will see —
    /// `expand_reusable_jobs` rewrites `needs:` entries it recognises and passes
    /// the rest through untouched, which is what makes a typo indistinguishable
    /// from a valid name by the time stages are assigned.
    pub fn validate_job_dependencies(&self) -> Result<()> {
        let mut missing = self
            .jobs
            .iter()
            .flat_map(|(job_name, job)| {
                job.needs
                    .iter()
                    .flatten()
                    .filter(|dependency| !self.jobs.contains_key(*dependency))
                    .map(move |dependency| format!("{job_name}: needs '{dependency}'"))
            })
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            let mut declared = self.jobs.keys().cloned().collect::<Vec<_>>();
            declared.sort();
            missing.sort();
            anyhow::bail!(
                "needs: names job(s) this workflow does not declare: {}. Declared jobs: {}",
                missing.join(", "),
                declared.join(", ")
            )
        }
        let unresolvable = self.jobs_no_stage_pass_can_place();
        if !unresolvable.is_empty() {
            anyhow::bail!(
                "needs: forms a dependency cycle involving job(s): {}. Each one waits, directly or \
                 through another job, on itself, so none of them can ever start",
                unresolvable.join(", ")
            )
        }
        Ok(())
    }

    /// The jobs the stage fixpoint can never place, in name order.
    ///
    /// Deliberately the same fixpoint [`Self::compute_job_stages`] runs: place
    /// every job all of whose dependencies are already placed, until a pass
    /// places nothing. With each `needs:` target known to exist, the leftovers
    /// are exactly the jobs sitting in a `needs:` cycle or downstream of one —
    /// the set the stage pass would otherwise sweep into stage 0.
    fn jobs_no_stage_pass_can_place(&self) -> Vec<String> {
        let mut remaining = self.jobs.keys().map(String::as_str).collect::<Vec<_>>();
        remaining.sort_unstable();
        let mut placed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        loop {
            let mut progressed = false;
            let mut next = Vec::new();
            for name in std::mem::take(&mut remaining) {
                let placeable = self.jobs[name]
                    .needs
                    .iter()
                    .flatten()
                    .all(|dependency| placed.contains(dependency.as_str()));
                if placeable {
                    placed.insert(name);
                    progressed = true;
                } else {
                    next.push(name);
                }
            }
            remaining = next;
            if !progressed || remaining.is_empty() {
                break;
            }
        }
        remaining.into_iter().map(str::to_owned).collect()
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
                        ref_matches_filter(&base_ref, filter) && paths_match_filter(filter, changed)
                    })
                };
                match event {
                    "push" => {
                        if let Some(filter) = push {
                            ref_matches_filter(ref_name, filter)
                                // GitHub/Gitea do not evaluate path filters for
                                // tag pushes: once the tag half selects the ref,
                                // `paths` / `paths-ignore` are satisfied without
                                // asking for a diff. Branch pushes still take the
                                // ordinary path-filter path below
                                // (card_105181820c3b).
                                && (ref_name.starts_with("refs/tags/")
                                    || paths_match_filter(filter, changed))
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
                cancel_in_progress: c
                    .cancel_in_progress
                    .unwrap_or(crate::config::DEFAULT_CANCEL_IN_PROGRESS),
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
        let mut called = GiteaWorkflow::parse(source).map_err(|error| {
            anyhow::anyhow!("failed to parse reusable workflow {target}: {error}")
        })?;
        called.validate_supported_triggers().map_err(|error| {
            anyhow::anyhow!("unsupported trigger in reusable workflow {target}: {error:#}")
        })?;
        if !called.is_reusable() {
            anyhow::bail!("workflow {target} is not reusable; declare `on: workflow_call`");
        }
        called.resolve_reusable_inputs(name, &original.with, &workflow.resolved_inputs)?;
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
            resolved_inputs: _,
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

fn deserialize_present_trigger<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<GiteaTriggerDeclaration>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_yaml::Value::deserialize(deserializer)?;
    if value.is_null() {
        return Ok(Some(GiteaTriggerDeclaration::default()));
    }
    serde_yaml::from_value(value)
        .map(Some)
        .map_err(serde::de::Error::custom)
}

/// Check if a ref matches an event filter.
///
/// The filter has two halves and each one is scoped to a *kind* of ref:
/// `branches:` / `branches-ignore:` speak about `refs/heads/…` and nothing
/// else, `tags:` / `tags-ignore:` about `refs/tags/…`. Three consequences, and
/// they are the whole rule — GitHub states it as "if you define only `tags` /
/// `tags-ignore` or only `branches` / `branches-ignore`, the workflow won't run
/// for events affecting the undefined Git ref", and Gitea's `matchPushEvent`
/// (`modules/actions/workflows.go`) implements the same one:
/// - neither half declared → every push matches, branch or tag alike;
/// - one half declared → the workflow is about that kind of ref only, and a
///   push of the other kind does not select it at all;
/// - both halves declared → one of them matching is enough.
///
/// This used to be a flat chain of four `if let`s over a single name computed
/// as `strip_prefix("refs/heads/").unwrap_or(ref_name)`, so for a tag push the
/// *whole ref* was handed to the `branches:` patterns: `refs/tags/v1` matched
/// `branches: ['**']` while the `tags:` half — `None`, since the author never
/// wrote it — was asked nothing. The sign of that error is the bad half of this
/// class: a workflow its author restricted to branches ran on a tag, so a
/// deploy job hanging off `branches: ['**']` or `['*', '!wip/*']` shipped on an
/// event that was never invited into the file (card_13c2d6a55c3c). Narrow
/// patterns (`main`, `release/*`) do not match `refs/tags/…`, which is why the
/// defect slept until the first wide one.
///
/// The mirror image was there as well, and it was not one bug but two: a
/// `tags-ignore:`-only workflow ran on every branch push, and a workflow that
/// declared `branches:` *and* `tags:` ran on nothing at all — the two halves
/// were `&&`-ed, so a branch push failed the tag half and a tag push the branch
/// half.
fn ref_matches_filter(ref_name: &str, filter: &EventFilter) -> bool {
    let branch_declared = filter.branches.is_some() || filter.branches_ignore.is_some();
    let tag_declared = filter.tags.is_some() || filter.tags_ignore.is_some();
    if !branch_declared && !tag_declared {
        return true;
    }

    match ref_name.strip_prefix("refs/tags/") {
        Some(tag) => tag_declared && tag_filter_selects(filter, tag),
        // Everything that is not a tag is read as a branch: the canonical
        // `refs/heads/<name>` every producer of a push event sends, and the bare
        // short name a pipeline row written before those producers canonicalised
        // their input can still carry into a retry. Only `refs/tags/` is treated
        // as the other kind — pinned by
        // `a_ref_outside_both_namespaces_is_read_as_a_branch`.
        None => {
            let branch = ref_name.strip_prefix("refs/heads/").unwrap_or(ref_name);
            branch_declared && branch_filter_selects(filter, branch)
        }
    }
}

/// The branch half of an event filter, asked only about a branch ref.
fn branch_filter_selects(filter: &EventFilter, branch: &str) -> bool {
    if let Some(ref branches) = filter.branches {
        if !list_selects(branches, |pattern| match_glob(branch, pattern)) {
            return false;
        }
    }

    // A plain `any` rather than an ordered selection: `!` inside an `-ignore`
    // list carries no meaning and is refused where the file is read, so there is
    // no exclusion here for a later pattern to take back.
    if let Some(ref ignored) = filter.branches_ignore {
        if ignored.iter().any(|pattern| match_glob(branch, pattern)) {
            return false;
        }
    }

    true
}

/// The tag half, asked only about a `refs/tags/…` ref.
///
/// `tags-ignore:` was declared and read by nobody: a workflow that asked to skip
/// release candidates ran on every one of them (card_e1e76c3ede65).
fn tag_filter_selects(filter: &EventFilter, tag: &str) -> bool {
    if let Some(ref tags) = filter.tags {
        if !list_selects(tags, |pattern| match_glob(tag, pattern)) {
            return false;
        }
    }

    if let Some(ref ignored) = filter.tags_ignore {
        if ignored.iter().any(|pattern| match_glob(tag, pattern)) {
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
/// This helper is called for branch pushes and PR-shaped events. Tag pushes
/// bypass path filters in [`GiteaWorkflow::matches_event`], matching the
/// upstream Actions dialect without computing a meaningless tag diff.
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
            .any(|path| list_selects(patterns, |p| match_path_pattern(path, p)))
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

/// Does an ordered `branches:` / `tags:` / `paths:` list select this candidate?
///
/// Not `any()`, because GitHub reads such a list in order: a plain pattern that
/// matches selects the ref or path, a `!` pattern that matches deselects it
/// again, and a later plain pattern can select it back. `paths: ['**',
/// '!docs/**']` — the canonical spelling of "everything except the docs" — is
/// exactly the shape `any()` gets wrong: the leading `**` answers first and the
/// exclusion never gets a say, so a commit touching one README paid for the
/// full run. Nor was the `!` pattern inert on its own; it was a literal, and
/// matched only a path physically starting with an exclamation mark
/// (card_8dc2adb75578).
///
/// A list is refused before it reaches here when its negation cannot mean
/// anything — see [`GiteaWorkflow::unhonourable_filter_patterns`].
fn list_selects(patterns: &[String], mut matches: impl FnMut(&str) -> bool) -> bool {
    let mut selected = false;
    for pattern in patterns {
        match negated_pattern(pattern) {
            Some(excluded) => {
                if matches(excluded) {
                    selected = false;
                }
            }
            None => {
                if matches(pattern) {
                    selected = true;
                }
            }
        }
    }
    selected
}

/// The body of a `!` pattern, or `None` when the pattern is a plain one.
///
/// `\!x` is a plain pattern for a name that opens with an exclamation mark —
/// the escape is left in place for the matcher, which is what consumes it.
fn negated_pattern(pattern: &str) -> Option<&str> {
    pattern.strip_prefix('!')
}

/// Does the pattern end in a backslash with nothing left to escape?
///
/// Counted as a run, not as one byte: `a\\` ends in an escaped backslash and is
/// a perfectly good pattern, while `a\` ends in a dangling one.
fn ends_with_dangling_escape(pattern: &str) -> bool {
    pattern
        .bytes()
        .rev()
        .take_while(|byte| *byte == b'\\')
        .count()
        % 2
        == 1
}

/// The filter metacharacters GitHub's dialect defines and this matcher lacks,
/// each with the meaning that dialect gives it, in the order they first appear
/// in the pattern.
///
/// GitHub's cheat sheet gives `+` the meaning "one or more of the preceding
/// character" and `[…]` "one alphanumeric character listed in the brackets or
/// included in ranges". [`glob_segments`] knows neither, so both reach its
/// literal arm: `tags: ['v1.[0-9]']` parses, validates, and then matches only a
/// tag spelled with those five characters. No such tag is ever pushed, the
/// release workflow never runs, and the author's only evidence is the silence
/// (card_61e3349073d7).
///
/// `?` is the third character of that cheat sheet and the one that used to be
/// worse than either, because it did not fall through: this matcher read it as
/// "any one character", the way a shell glob does, while the dialect the page
/// is copied from reads it as "zero or one of the *preceding* character". Two
/// engines, two different pattern languages, one spelling — and the divergence
/// runs both ways. `v1.?` selects `v1.0` here and `v1` / `v1.` there; the
/// direction that matters is the other one, where `release?/**` selected
/// `releaseX/**` — a run on a branch the author never named (card_eeffc067afdd).
/// So `?` is refused by name for the same reason and with the same wording as
/// `+`: this engine implements a glob, not the half-regular expression the
/// cheat sheet documents, and a spelling it cannot honour as written is a
/// refusal rather than a second meaning.
///
/// The escape is honoured, because it is the answer the refusal offers: `\+`,
/// `\[` and `\?` already reach the literal character through
/// [`glob_segments`]'s backslash arm, so a pattern for a ref or file genuinely
/// named with one is still writable and is not reported here.
fn unimplemented_metacharacters(pattern: &str) -> Vec<(char, &'static str)> {
    let mut found: Vec<(char, &'static str)> = Vec::new();
    let mut escaped = false;
    for character in pattern.chars() {
        let meaning = match character {
            _ if escaped => {
                escaped = false;
                continue;
            }
            '\\' => {
                escaped = true;
                continue;
            }
            '+' => "one or more of the character before it",
            '[' => "one character from the set or range in the brackets",
            '?' => "zero or one of the character before it — not \"any one character\"",
            _ => continue,
        };
        if !found.iter().any(|(seen, _)| *seen == character) {
            found.push((character, meaning));
        }
    }
    found
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
/// `/`, `**` matches any run including `/` — and a pattern ending in `/` or
/// `/**` covers everything under that directory.
fn match_path_pattern(path: &str, pattern: &str) -> bool {
    // `docs/` and `docs/**` both mean "everything under docs".
    if let Some(dir) = pattern.strip_suffix("/**").or(pattern.strip_suffix('/')) {
        if path == dir || path.starts_with(&format!("{dir}/")) {
            return true;
        }
    }
    glob_segments(path.as_bytes(), pattern.as_bytes())
}

/// Backtracking matcher for `*` and `**` over a path.
///
/// `?` is deliberately absent, and its absence is the decision recorded in
/// [`unimplemented_metacharacters`]: the character means "zero or one of the
/// preceding character" in the dialect this file's documentation points at, and
/// implementing that would make the pattern language half regular. So it lands
/// on the literal arm below, exactly as `+` and `[` do — which is what the
/// refusal promises the author ("it would be matched as the literal
/// character"), and the promise has to be true for the workflows that never
/// reach the validator.
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
        // GitHub's escape: a backslash makes the next byte a literal, so `\*`
        // reaches the file actually named with a star and `\!` a pattern that
        // opens with an exclamation mark instead of negating. Without this arm
        // the backslash stayed in the pattern as an ordinary byte and could
        // only match a path that physically carried one (card_8dc2adb75578) —
        // the same defect `pattern_matches` had in CODEOWNERS.
        Some(b'\\') => match pattern.get(1) {
            Some(literal) => {
                !path.is_empty() && path[0] == *literal && glob_segments(&path[1..], &pattern[2..])
            }
            // A trailing backslash escapes nothing. `unhonourable_filter_patterns`
            // refuses one where the file is read, so this arm is only reached
            // through the matcher's other callers.
            None => false,
        },
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
    use std::collections::BTreeSet;

    fn test_context() -> WorkflowContext {
        WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc123".into(),
            event: "push".into(),
            repo_owner: "owner".into(),
            repo_name: "repo".into(),
        }
    }

    /// The page a repository author reads to learn which half of Actions this
    /// engine implements.
    ///
    /// `docs/ci.md` used to send them to the Gitea and GitHub projects instead.
    /// That is not a shortcut but a wrong signpost: those documents describe a
    /// surface an order of magnitude wider than this one, every block here is
    /// `deny_unknown_fields`, and a file valid by them is refused whole by this.
    /// Resolved at compile time, so a moved document breaks the build rather
    /// than silently skipping the checks below.
    const ACTIONS_DOCUMENTATION: (&str, &str) = (
        "docs/gitea-actions.md",
        include_str!("../../../docs/gitea-actions.md"),
    );

    /// The production half of this file, with the test modules cut away so a
    /// key that exists only in a fixture cannot pass for a key of the model.
    fn production_source() -> &'static str {
        include_str!("gitea_actions.rs")
            .split_once("\n#[cfg(test)]\n")
            .map(|(production, _)| production)
            .expect("gitea_actions.rs must keep its test modules behind #[cfg(test)]")
    }

    /// A fenced example, with the `<!-- example: … -->` marker that introduced
    /// it — the marker is what says whether the document claims this file is
    /// accepted or refused, so both claims are checked rather than one.
    struct DocExample {
        marker: Option<String>,
        language: String,
        line: usize,
        body: String,
    }

    fn doc_examples(name: &str, content: &str) -> Vec<DocExample> {
        let mut blocks = Vec::new();
        let mut body: Vec<&str> = Vec::new();
        let mut pending: Option<String> = None;
        let mut marker = None;
        let mut language = String::new();
        let mut start = 0usize;
        let mut inside = false;

        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if inside {
                if trimmed == "```" {
                    blocks.push(DocExample {
                        marker: marker.take(),
                        language: language.clone(),
                        line: start,
                        body: body.join("\n"),
                    });
                    body.clear();
                    inside = false;
                } else {
                    body.push(line);
                }
            } else if let Some(fence) = trimmed.strip_prefix("```") {
                inside = true;
                marker = pending.take();
                language = fence.to_owned();
                start = index + 2;
            } else if let Some(rest) = trimmed
                .strip_prefix("<!-- example:")
                .and_then(|rest| rest.strip_suffix("-->"))
            {
                pending = Some(rest.trim().to_owned());
            } else if !trimmed.is_empty() {
                pending = None;
            }
        }

        assert!(
            !inside,
            "{name}:{start}: a ``` block is never closed — the extractor reads the rest of the \
             document as one example"
        );
        blocks
    }

    /// The lines of the fenced block that follows a marker comment.
    ///
    /// The marker, rather than the block's position, ties an inventory in the
    /// document to a list in this file: inserting a paragraph must not silently
    /// re-point a check at somebody else's example.
    fn inventory_after(name: &str, content: &str, marker: &str) -> Vec<String> {
        let marker_line = content
            .lines()
            .position(|line| line.trim() == marker)
            .unwrap_or_else(|| {
                panic!(
                    "{name}: the marker `{marker}` is gone, so the inventory it introduced can no \
                     longer be checked against this file — restore it above the list"
                )
            });
        let example = doc_examples(name, content)
            .into_iter()
            .find(|example| example.line > marker_line + 1)
            .unwrap_or_else(|| panic!("{name}: no fenced block follows the marker `{marker}`"));
        example
            .body
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Every mapping key a YAML document shows, at any depth.
    fn yaml_keys(body: &str) -> BTreeSet<&str> {
        body.lines()
            .filter_map(|line| {
                let line = line.trim();
                let line = line.strip_prefix("- ").unwrap_or(line);
                let (key, rest) = line.split_once(':')?;
                (!key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    && (rest.is_empty() || rest.starts_with(' ')))
                .then_some(key)
            })
            .collect()
    }

    fn documented_keys(name: &str, content: &str) -> BTreeSet<String> {
        doc_examples(name, content)
            .iter()
            .filter(|example| example.language == "yaml")
            .flat_map(|example| {
                yaml_keys(&example.body)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// One field of a serde struct, as the reader of the YAML sees it.
    struct SerdeField {
        field: String,
        key: String,
        type_text: String,
        flattened: bool,
        skipped: bool,
    }

    fn has_serde_flag(attributes: &str, flag: &str) -> bool {
        attributes
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|token| token == flag)
    }

    fn serde_rename(attributes: &str) -> Option<&str> {
        attributes
            .split_once("rename = \"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(name, _)| name)
    }

    /// The body of a `struct` or `enum` declared in `source`.
    fn type_body<'a>(source: &'a str, type_name: &str) -> Option<(&'static str, &'a str)> {
        for keyword in ["struct", "enum"] {
            let declaration = format!("{keyword} {type_name} {{");
            let body = source
                .split_once(declaration.as_str())
                .map(|(_, rest)| rest)
                .and_then(|rest| rest.split_once("\n}").map(|(body, _)| body));
            if let Some(body) = body {
                return Some((keyword, body));
            }
        }
        None
    }

    /// The fields of a struct declared in `source`, in declaration order.
    ///
    /// Reading the declaration rather than keeping a list beside it is the
    /// whole point: a key added to the model joins the contract below by
    /// existing, not by someone remembering to register it. Multi-line
    /// `#[serde(…)]` attributes are accumulated until their brackets balance —
    /// `pull_request_target` spells its `rename` three lines below the `#[`.
    fn serde_fields(source: &str, type_name: &str) -> Vec<SerdeField> {
        let (keyword, body) = type_body(source, type_name)
            .unwrap_or_else(|| panic!("{type_name} must be declared in gitea_actions.rs"));
        assert_eq!(keyword, "struct", "{type_name} is not a struct");

        let mut fields = Vec::new();
        let mut attributes = String::new();
        let mut open = 0i32;

        for line in body.lines() {
            let line = line.trim();
            if line.is_empty() {
                if open == 0 {
                    attributes.clear();
                }
                continue;
            }
            if line.starts_with("//") {
                continue;
            }
            if open > 0 || line.starts_with("#[") {
                attributes.push_str(line);
                open += i32::try_from(line.matches('[').count()).unwrap_or(0)
                    - i32::try_from(line.matches(']').count()).unwrap_or(0);
                continue;
            }
            let declaration = line
                .strip_prefix("pub(crate) ")
                .or_else(|| line.strip_prefix("pub "))
                .unwrap_or(line);
            let Some((field, type_text)) = declaration.split_once(':') else {
                continue;
            };
            fields.push(SerdeField {
                field: field.trim().to_owned(),
                key: serde_rename(&attributes)
                    .unwrap_or_else(|| field.trim())
                    .to_owned(),
                type_text: type_text.trim().trim_end_matches(',').to_owned(),
                flattened: has_serde_flag(&attributes, "flatten"),
                skipped: has_serde_flag(&attributes, "skip"),
            });
            attributes.clear();
        }
        fields
    }

    /// Every type a committed workflow file is parsed into, walked from
    /// [`GiteaWorkflow`].
    ///
    /// Enums are followed as well as structs: `on:` reaches
    /// `WorkflowTriggerSingle` only through the untagged [`WorkflowTriggers`],
    /// and a struct-only walk would leave every trigger key out of the
    /// contract. Returned as `(name, is_struct)` — only structs carry keys.
    fn workflow_model_types(source: &str) -> Vec<(String, bool)> {
        let mut reachable = vec!["GiteaWorkflow".to_owned()];
        let mut kinds = vec![true];
        let mut visited = 0;

        while visited < reachable.len() {
            let type_name = reachable[visited].clone();
            let is_struct = kinds[visited];
            visited += 1;

            // A struct contributes only its field types; an enum has no keys,
            // so every identifier in its body is a candidate payload type.
            let candidates: Vec<String> = if is_struct {
                serde_fields(source, &type_name)
                    .into_iter()
                    .filter(|field| !field.skipped)
                    .map(|field| field.type_text)
                    .collect()
            } else {
                type_body(source, &type_name)
                    .map(|(_, body)| {
                        body.lines()
                            .map(str::trim)
                            .filter(|line| !line.starts_with("//"))
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default()
            };

            for text in candidates {
                for candidate in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
                    if candidate.is_empty() || reachable.iter().any(|known| known == candidate) {
                        continue;
                    }
                    match type_body(source, candidate) {
                        Some((keyword, _)) => {
                            reachable.push(candidate.to_owned());
                            kinds.push(keyword == "struct");
                        }
                        None => continue,
                    }
                }
            }
        }
        reachable.into_iter().zip(kinds).collect()
    }

    /// Fields that carry no key of their own, each with the reason. The list
    /// exists so that a new flattened field is a decision someone made, rather
    /// than a key that quietly stopped needing documentation.
    const NAMED_BY_THE_READER: [(&str, &str, &str); 4] = [
        (
            "WorkflowTriggerSingle",
            "other",
            "flattened: every `on:` key that is not one of the named triggers, kept only so the \
             refusal can name what the author wrote",
        ),
        (
            "EventFilter",
            "other",
            "flattened: every filter key outside SUPPORTED_EVENT_FILTERS, kept only to be refused \
             by name",
        ),
        (
            "GiteaTriggerDeclaration",
            "other",
            "flattened: every key under `workflow_dispatch:` / `workflow_call:` that is not \
             `inputs`, kept only to be refused by name",
        ),
        (
            "GiteaInputDefinition",
            "other",
            "flattened: every input attribute outside the documented five, kept only to be \
             refused by name",
        ),
    ];

    /// Run a documented example through exactly the chain `try_read_gitea_workflows`
    /// puts a committed file through, minus the event match.
    fn judge_example(body: &str, sources: &HashMap<String, String>) -> Result<()> {
        let workflow = GiteaWorkflow::parse(body)?;
        workflow.validate_supported_triggers()?;
        let workflow = workflow.expand_local_reusable_workflows(sources)?;
        workflow.validate_supported_actions()?;
        workflow.validate_job_dependencies()
    }

    /// Both halves of every claim the document makes about an example.
    ///
    /// An accepted example has to be a file this engine runs; a block the page
    /// introduces with `<!-- example: refused -->` has to actually be refused —
    /// otherwise the page teaches an author to avoid something that works, or,
    /// worse, keeps calling something unsupported after it was implemented.
    #[test]
    fn every_example_in_the_actions_documentation_is_judged_as_the_document_says() {
        let (name, content) = ACTIONS_DOCUMENTATION;
        let examples = doc_examples(name, content);

        // A called workflow is a file of its own; the marker carries the name
        // the caller's `uses:` must point at, so renaming it in the document
        // moves both halves together.
        let sources: HashMap<String, String> = examples
            .iter()
            .filter_map(|example| {
                example
                    .marker
                    .as_deref()
                    .and_then(|marker| marker.strip_prefix("reusable-callee "))
                    .map(|file| (file.trim().to_owned(), example.body.clone()))
            })
            .collect();

        let (mut accepted, mut refused, mut callers) = (0, 0, 0);
        for example in examples.iter().filter(|example| example.language == "yaml") {
            let DocExample { line, body, .. } = example;
            match example.marker.as_deref() {
                Some("refused") => {
                    let error = judge_example(body, &sources).err().unwrap_or_else(|| {
                        panic!(
                            "{name}:{line}: this block is presented as refused, but the engine \
                             accepts it — the page is teaching an author to avoid something that \
                             works"
                        )
                    });
                    assert!(
                        !format!("{error:#}").is_empty(),
                        "{name}:{line}: refused with an empty message"
                    );
                    refused += 1;
                }
                Some("reusable-caller") => {
                    judge_example(body, &sources).unwrap_or_else(|error| {
                        panic!("{name}:{line}: this caller does not expand: {error:#}")
                    });
                    callers += 1;
                }
                _ => {
                    judge_example(body, &sources).unwrap_or_else(|error| {
                        panic!(
                            "{name}:{line}: this ```yaml block is what a reader commits to their \
                             own repository, and this engine refuses it: {error:#}"
                        )
                    });
                    accepted += 1;
                }
            }
        }

        // Floors, not counts: they fail loudly if the fence or marker scanner
        // ever stops matching and the test quietly checks nothing.
        assert!(
            accepted >= 6,
            "only {accepted} accepted examples found in {name} — the scanner has stopped matching"
        );
        assert!(
            refused >= 2,
            "only {refused} refused examples found in {name} — the page has stopped showing what \
             the boundary looks like from the outside"
        );
        assert_eq!(
            callers, 1,
            "expected exactly one reusable-workflow caller example in {name}"
        );
    }

    /// The mirror: everything the model accepts has to be shown.
    ///
    /// A key nobody can discover is worse here than in the server's own config.
    /// This file lives in the *author's* repository, every block that takes it
    /// is `deny_unknown_fields`, and the surrounding documentation an author
    /// would reach for describes a different, larger format.
    #[test]
    fn every_key_the_workflow_model_accepts_is_shown_in_the_documentation() {
        let (name, content) = ACTIONS_DOCUMENTATION;
        let documented = documented_keys(name, content);
        let source = production_source();
        let mut excused = BTreeSet::new();
        let mut checked = 0;

        for (type_name, is_struct) in workflow_model_types(source) {
            if !is_struct {
                continue;
            }
            for field in serde_fields(source, &type_name) {
                if field.skipped {
                    continue;
                }
                if field.flattened {
                    let reason =
                        NAMED_BY_THE_READER
                            .iter()
                            .find(|&&(excused_type, excused_field, _)| {
                                excused_type == type_name && excused_field == field.field
                            });
                    assert!(
                        reason.is_some(),
                        "`{}` of {type_name} is flattened into the document and named in no \
                         `NAMED_BY_THE_READER` entry — say in {name} what an author writes there, \
                         then record the reason it has no key of its own",
                        field.field
                    );
                    excused.insert((type_name.clone(), field.field.clone()));
                    continue;
                }
                assert!(
                    documented.contains(&field.key),
                    "no example in {name} shows `{}` of {type_name}, so the only way to learn the \
                     key exists is to read gitea_actions.rs — and `deny_unknown_fields` means an \
                     author who guesses gets the whole workflow refused",
                    field.key
                );
                checked += 1;
            }
        }

        for (type_name, field, reason) in NAMED_BY_THE_READER {
            assert!(
                excused.contains(&(type_name.to_owned(), field.to_owned())),
                "`{field}` of {type_name} is excused from the documentation as {reason:?}, but it \
                 is no longer a flattened field of the model — drop the excuse or restore it"
            );
        }

        assert!(
            checked >= 45,
            "only {checked} keys read off the model — the declaration scanner has stopped matching"
        );
    }

    /// The keys are only half the boundary. The other half is the closed lists:
    /// which events run, which filters exist, which two actions are
    /// implemented, which of their inputs are honoured, and which expressions
    /// resolve. Each is read off the constant that enforces it, so a list that
    /// grows or shrinks cannot leave the page behind.
    #[test]
    fn every_boundary_the_engine_enforces_is_named_in_the_documentation() {
        let (name, content) = ACTIONS_DOCUMENTATION;
        let documented = documented_keys(name, content);

        for event in rg_core::ci::PIPELINE_EVENTS
            .iter()
            .copied()
            .chain(std::iter::once(WORKFLOW_CALL_TRIGGER))
        {
            assert!(
                documented.contains(event),
                "no example in {name} declares `on: {event}`, which is a trigger this engine runs"
            );
        }

        for filter in SUPPORTED_EVENT_FILTERS {
            assert!(
                documented.contains(*filter),
                "no example in {name} shows the event filter `{filter}` this engine honours"
            );
        }

        // Set equality against a marker-anchored inventory, not "the name
        // occurs somewhere": this page names the *refused* inputs too, so a
        // substring check would be satisfied by the sentence explaining why an
        // input is NOT read — and would stay green for an input that has since
        // been implemented.
        for (marker, inputs, action) in [
            (
                "<!-- inventory: checkout-inputs -->",
                CHECKOUT_INPUTS,
                "actions/checkout",
            ),
            (
                "<!-- inventory: cache-inputs -->",
                CACHE_INPUTS,
                "actions/cache",
            ),
        ] {
            let documented_inputs: BTreeSet<String> =
                inventory_after(name, content, marker).into_iter().collect();
            let read: BTreeSet<String> = inputs.iter().map(|input| (*input).to_owned()).collect();

            let undocumented: Vec<&String> = read.difference(&documented_inputs).collect();
            assert!(
                undocumented.is_empty(),
                "{name}: {action} now reads each of {undocumented:?}, and the inventory under \
                 `{marker}` does not list it — the page still tells an author the input is ignored"
            );

            let imagined: Vec<&String> = documented_inputs.difference(&read).collect();
            assert!(
                imagined.is_empty(),
                "{name}: the inventory under `{marker}` promises {action} honours each of \
                 {imagined:?}, and nothing reads it — a workflow setting it is refused"
            );
        }

        for action in SUPPORTED_ACTIONS {
            assert!(
                content.contains(action),
                "{name} does not mention `{action}`, one of the only two actions implemented — \
                 every other `uses:` fails the workflow"
            );
        }

        for (expression, substitution) in GITHUB_RUN_EXPRESSIONS {
            assert!(
                content.contains(expression) && content.contains(substitution),
                "{name} does not show that `${{{{ {expression} }}}}` becomes `{substitution}`"
            );
        }

        // The engine's own upper bound on reusable-workflow nesting: a number
        // an author meets only by hitting it.
        assert!(
            content.contains("four levels"),
            "{name} does not state the depth at which reusable-workflow nesting is refused"
        );
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

    /// `!` only means something a list can give it: it excludes what an
    /// earlier pattern of the same list selected. The three shapes below cannot
    /// carry that meaning, and a pattern that cannot mean anything is refused
    /// by name rather than run as a literal — which is what it used to be,
    /// matching only a ref or path that physically opened with an exclamation
    /// mark (card_8dc2adb75578).
    #[test]
    fn filter_patterns_that_cannot_be_honoured_are_refused_by_name() {
        for (filter, expected) in [
            (
                "branches:\n      - '!main'",
                ["push.branches", "push.branches-ignore"],
            ),
            (
                "paths-ignore:\n      - '!docs/**'",
                ["push.paths-ignore", "!docs/**"],
            ),
            ("paths:\n      - 'docs\\'", ["push.paths", "escapes"]),
        ] {
            let yaml = format!(
                "on:\n  push:\n    {filter}\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n"
            );
            let workflow =
                GiteaWorkflow::parse(&yaml).expect("the pattern parses; the validator judges it");
            let error = workflow
                .validate_supported_triggers()
                .expect_err("a pattern that cannot mean anything must not be run as a literal")
                .to_string();
            for needle in expected {
                assert!(error.contains(needle), "missing {needle:?}: {error}");
            }
        }
    }

    /// The other side of the same gate: the shape `!` *is* for stays accepted.
    /// A refusal that also swallowed `['**', '!docs/**']` would trade a silent
    /// wrong run for a loud wrong rejection.
    #[test]
    fn a_negation_with_something_to_exclude_from_is_accepted() {
        let workflow = GiteaWorkflow::parse(
            "on:\n  push:\n    paths:\n      - '**'\n      - '!docs/**'\n    branches:\n      - 'release/**'\n      - '!release/wip'\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
        )
        .expect("parse");
        workflow
            .validate_supported_triggers()
            .expect("an exclusion with a preceding selection is honoured, not refused");
    }

    /// `+`, `?` and `[…]` carry a meaning in the filter dialect this page's
    /// author is reading, and this matcher has none of the three: they fall
    /// through to its literal arm, so `tags: ['v1.[0-9]']` selects no tag that
    /// anybody pushes and the release workflow silently never runs
    /// (card_61e3349073d7). The refusal has to name the author's key *and* the
    /// character, because the only other evidence of the defect is a job that
    /// did not happen.
    #[test]
    fn filter_metacharacters_this_engine_lacks_are_refused_by_name() {
        for (filter, expected) in [
            (
                "tags:\n      - 'v1.[0-9]'",
                vec!["push.tags", "v1.[0-9]", "`[`", "\\["],
            ),
            (
                "branches:\n      - 'release+'",
                vec!["push.branches", "release+", "`+`", "\\+"],
            ),
            (
                "paths-ignore:\n      - 'c++/[a-z]*'",
                vec!["push.paths-ignore", "`+`", "`[`"],
            ),
            (
                "branches:\n      - 'release?/**'",
                vec!["push.branches", "release?/**", "`?`", "\\?"],
            ),
            (
                "tags-ignore:\n      - 'v1.?'",
                vec!["push.tags-ignore", "v1.?", "`?`"],
            ),
        ] {
            let yaml = format!(
                "on:\n  push:\n    {filter}\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n"
            );
            let workflow =
                GiteaWorkflow::parse(&yaml).expect("the pattern parses; the validator judges it");
            let error = workflow
                .validate_supported_triggers()
                .expect_err(
                    "a pattern whose metacharacter this matcher lacks must not be run as a literal",
                )
                .to_string();
            for needle in expected {
                assert!(error.contains(needle), "missing {needle:?}: {error}");
            }
        }
    }

    /// The escape the refusal offers has to be a real way out, or the gate has
    /// simply banned three bytes from every ref and file name: `\+`, `\[` and
    /// `\?` stay accepted, and they match the character itself and nothing
    /// else.
    #[test]
    fn the_escaped_spelling_of_those_metacharacters_stays_accepted_and_matches() {
        let workflow = GiteaWorkflow::parse(
            "on:\n  push:\n    paths:\n      - 'c\\+\\+/**'\n      - 'src/\\[gen]/**'\n      - 'docs/faq\\?.md'\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
        )
        .expect("parse");
        workflow
            .validate_supported_triggers()
            .expect("an escaped metacharacter is a literal, which this matcher does implement");

        assert!(match_path_pattern("c++/main.cc", "c\\+\\+/**"));
        assert!(!match_path_pattern("cc/main.cc", "c\\+\\+/**"));
        assert!(match_path_pattern("src/[gen]/a.rs", "src/\\[gen]/**"));
        assert!(!match_path_pattern("src/g/a.rs", "src/\\[gen]/**"));
        assert!(match_path_pattern("docs/faq?.md", "docs/faq\\?.md"));
        assert!(!match_path_pattern("docs/faqx.md", "docs/faq\\?.md"));
    }

    /// The decision `card_eeffc067afdd` asked for, written down where a reader
    /// meets it: **both** meanings of `?` are named here, so neither can be
    /// "fixed" from memory by somebody who knows only one of them.
    ///
    /// - GitHub's filter cheat sheet: "zero or one of the **preceding**
    ///   character", which makes `v1.?` match `v1` and `v1.` and nothing else.
    /// - A shell glob, which is what this matcher used to implement: "any one
    ///   character", which makes the same `v1.?` match `v1.0`.
    ///
    /// Neither is implemented now. The first would make the pattern language
    /// half a regular expression — and `+`, its other half, was refused by name
    /// one card earlier (card_61e3349073d7). The second is the one that shipped,
    /// and it is the reason this card exists: the divergence runs both ways, and
    /// the expensive direction is the wide one, where `release?/**` selected
    /// `releaseX/**` and ran a job on a branch nobody named. So `?` is refused,
    /// like the other two, and `\?` is the way to a name that carries one.
    #[test]
    fn both_meanings_of_a_question_mark_are_named_and_neither_is_implemented() {
        // The refusal quotes GitHub's meaning, so the author reading it learns
        // what their pattern would have meant on the engine they copied it from.
        let workflow = GiteaWorkflow::parse(
            "on:\n  push:\n    branches:\n      - 'v1.?'\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo ok\n",
        )
        .expect("the pattern parses; the validator judges it");
        let error = workflow
            .validate_supported_triggers()
            .expect_err("`?` is not this engine's, so it is refused rather than given a meaning")
            .to_string();
        for needle in [
            "push.branches",
            "v1.?",
            "`?`",
            "zero or one of the character before it",
            "any one character",
            "\\?",
        ] {
            assert!(error.contains(needle), "missing {needle:?}: {error}");
        }

        // And the matcher agrees with the refusal's promise: a `?` that never
        // reaches the validator is the literal character, not either meaning.
        assert!(match_glob("v1.?", "v1.?"), "the literal arm still matches");
        assert!(
            !match_glob("v1.0", "v1.?"),
            "the shell-glob meaning is gone: `?` no longer stands for any one character"
        );
        assert!(
            !match_glob("v1", "v1.?"),
            "GitHub's meaning is not implemented either: `?` does not make `.` optional"
        );
        assert!(
            !match_path_pattern("releaseX/deploy.sh", "release?/**"),
            "the wide direction that ran a job on an unnamed branch is closed"
        );
    }

    #[test]
    fn trigger_input_schema_types_defaults_and_options_fail_by_qualified_key() {
        for (trigger, schema, expected) in [
            (
                "workflow_dispatch",
                "target:\n        type: boolean\n        default: staging",
                "workflow_dispatch.inputs.target.default must have type boolean",
            ),
            (
                "workflow_dispatch",
                "target:\n        type: choice",
                "workflow_dispatch.inputs.target.options",
            ),
            (
                "workflow_call",
                "target:\n        required: true",
                "workflow_call.inputs.target.type",
            ),
            (
                "workflow_call",
                "target:\n        type: choice\n        options: [one]",
                "workflow_call.inputs.target.type=choice",
            ),
        ] {
            let workflow = GiteaWorkflow::parse(&format!(
                "on:\n  {trigger}:\n    inputs:\n      {schema}\njobs:\n  build:\n    steps:\n      - run: echo ok\n"
            ))
            .expect("the semantic validator must retain the qualified input path");
            let error = workflow
                .validate_supported_triggers()
                .expect_err("an invalid trigger input schema must be refused")
                .to_string();
            assert!(error.contains(expected), "missing {expected:?}: {error}");
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
    inputs:
      target:
        required: true
        type: string
      dry-run:
        type: boolean
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
        assert_eq!(expanded.jobs["shared/build"].env["INPUT_DRY_RUN"], "false");
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
    fn reusable_workflow_inputs_reject_missing_unknown_and_wrong_typed_values() {
        let source = r#"
on:
  workflow_call:
    inputs:
      target:
        description: Deployment target
        required: true
        type: string
      dry-run:
        type: boolean
jobs:
  build:
    steps:
      - run: echo "${{ inputs.target }} ${{ inputs.dry-run }}"
"#;
        let sources = HashMap::from([("shared.yml".into(), source.into())]);
        for (with, expected) in [
            ("", "required input 'target'"),
            (
                "    with:\n      typo: value\n",
                "undeclared input(s): typo",
            ),
            (
                "    with:\n      target: true\n",
                "input 'target' must have type string",
            ),
        ] {
            let caller = GiteaWorkflow::parse(&format!(
                "on: push\njobs:\n  shared:\n    uses: ./.gitea/workflows/shared.yml\n{with}"
            ))
            .unwrap();
            let error = caller
                .expand_local_reusable_workflows(&sources)
                .expect_err("the called workflow schema must judge jobs.<id>.with");
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
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
        wf.validate_job_dependencies()
            .expect("every needs: names a declared job");
    }

    #[test]
    fn needs_naming_a_job_that_does_not_exist_is_rejected_by_name() {
        let workflow = |dependency: &str| {
            GiteaWorkflow::parse(&format!(
                r#"
on: push
jobs:
  build:
    steps:
      - run: cargo build
  deploy:
    needs: [{dependency}]
    steps:
      - run: deploy.sh
"#
            ))
            .unwrap()
        };

        let error = format!(
            "{:#}",
            workflow("buidl")
                .validate_job_dependencies()
                .expect_err("a needs: target that does not exist must be refused")
        );
        // The typo, the job that made it, and the list to compare against —
        // the author's only other evidence is a deploy that ran too early.
        assert!(error.contains("deploy: needs 'buidl'"), "{error}");
        assert!(error.contains("Declared jobs: build, deploy"), "{error}");

        // Without the gate the typo is not an error but the *removal* of the
        // dependency: `compute_job_stages` cannot resolve `buidl`, so `deploy`
        // falls out of the fixpoint and is swept into stage 0 next to `build`.
        let ctx = WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
        };
        let unguarded = workflow("buidl").to_ci_config(&ctx);
        assert_eq!(unguarded.jobs["build"].stage.as_deref(), Some("stage-0"));
        assert_eq!(unguarded.jobs["deploy"].stage.as_deref(), Some("stage-0"));

        // Spelled correctly, the same file passes the gate and keeps its order.
        let correct = workflow("build");
        correct
            .validate_job_dependencies()
            .expect("needs: [build] names a declared job");
        let ci = correct.to_ci_config(&ctx);
        assert_eq!(ci.jobs["build"].stage.as_deref(), Some("stage-0"));
        assert_eq!(ci.jobs["deploy"].stage.as_deref(), Some("stage-1"));
    }

    #[test]
    fn needs_cycles_are_rejected_instead_of_collapsing_into_one_stage() {
        let cyclic = GiteaWorkflow::parse(
            r#"
on: push
jobs:
  a:
    needs: [c]
    steps:
      - run: echo a
  b:
    needs: [a]
    steps:
      - run: echo b
  c:
    needs: [b]
    steps:
      - run: echo c
  standalone:
    steps:
      - run: echo standalone
"#,
        )
        .unwrap();
        let error = format!(
            "{:#}",
            cyclic
                .validate_job_dependencies()
                .expect_err("a needs: cycle must be refused, not flattened into stage 0")
        );
        assert!(error.contains("dependency cycle"), "{error}");
        assert!(error.contains("a, b, c"), "{error}");
        // The job outside the cycle is not accused of being in it.
        assert!(!error.contains("standalone"), "{error}");

        // Self-reference is the one-job spelling of the same graph.
        let self_referential = GiteaWorkflow::parse(
            r#"
on: push
jobs:
  loop:
    needs: [loop]
    steps:
      - run: echo loop
"#,
        )
        .unwrap();
        let error = format!(
            "{:#}",
            self_referential
                .validate_job_dependencies()
                .expect_err("a job that needs itself must be refused")
        );
        assert!(error.contains("dependency cycle"), "{error}");
        assert!(error.contains("loop"), "{error}");
    }

    #[test]
    fn needs_is_validated_against_the_flattened_reusable_job_names() {
        let sources = HashMap::from([(
            "shared.yml".into(),
            r#"
on: workflow_call
jobs:
  build:
    steps:
      - run: echo build
"#
            .into(),
        )]);
        let caller = |dependency: &str| {
            GiteaWorkflow::parse(&format!(
                r#"
on: push
jobs:
  shared:
    uses: ./.gitea/workflows/shared.yml
    secrets: inherit
  publish:
    needs: [{dependency}]
    steps:
      - run: echo publish
"#
            ))
            .unwrap()
            .expand_local_reusable_workflows(&sources)
            .unwrap()
        };

        // `shared` is rewritten to the called workflow's leaves, so `publish`
        // ends up needing `shared/build` — a name the caller never typed.
        let expanded = caller("shared");
        assert_eq!(
            expanded.jobs["publish"].needs.as_ref().unwrap(),
            &vec!["shared/build".to_string()]
        );
        expanded
            .validate_job_dependencies()
            .expect("the rewritten dependency names a job of the flattened workflow");

        // A name the expansion cannot rewrite is passed through as-is, which is
        // precisely where it stops being distinguishable from a valid one.
        let error = format!(
            "{:#}",
            caller("shard")
                .validate_job_dependencies()
                .expect_err("an unrewritable needs: target must be refused after expansion")
        );
        assert!(error.contains("publish: needs 'shard'"), "{error}");
        assert!(error.contains("shared/build"), "{error}");
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

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn produced_pipeline_events(name: &str, source: &str) -> Vec<(String, String)> {
        rust_source::string_field_literals(source, "trigger_type")
            .into_iter()
            .map(|field| (format!("{name}:{}", field.line), field.value))
            .collect()
    }

    fn assert_pipeline_events_are_canonical(produced: &[(String, String)]) {
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

    #[test]
    fn producer_event_census_reads_only_production_string_fields() {
        const SAMPLE: &str = r####"
// trigger_type: "line_comment",
/* trigger_type: "block_comment", */
let normal = "trigger_type: \"normal_string\",";
let raw = r#"trigger_type: "raw_string","#;
let bytes = b"trigger_type: \"byte_string\",";

let first = Pipeline {
    trigger_type:
        /* the value may be on another line */ "push",
};

#[cfg(test)]
mod early_tests {
    const DECOY: Pipeline = Pipeline { trigger_type: "early_test" };
    const BRACE_DECOY: &str = "}";
}

let second = Pipeline {
    trigger_type: r#"merge_group"#,
};

#[cfg(test)]
mod tail_tests {
    const DECOY: Pipeline = Pipeline { trigger_type: "test_tail" };
}
"####;
        let line_of = |needle: &str| {
            SAMPLE
                .lines()
                .position(|line| line.contains(needle))
                .map(|line| line + 1)
                .unwrap_or_else(|| panic!("sample has no line containing `{needle}`"))
        };

        assert_eq!(
            produced_pipeline_events("fixture.rs", SAMPLE),
            vec![
                (
                    format!("fixture.rs:{}", line_of("let first") + 1),
                    "push".into()
                ),
                (
                    format!("fixture.rs:{}", line_of("merge_group")),
                    "merge_group".into(),
                ),
            ]
        );
    }

    #[test]
    #[should_panic(expected = "invented_event")]
    fn an_unknown_production_event_makes_the_guard_fail() {
        let produced = produced_pipeline_events(
            "fixture.rs",
            "let pipeline = Pipeline { trigger_type: \"invented_event\" };",
        );
        assert_pipeline_events_are_canonical(&produced);
    }

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
                produced.extend(produced_pipeline_events(
                    &path.display().to_string(),
                    &source,
                ));
            }
        }

        assert_pipeline_events_are_canonical(&produced);
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
