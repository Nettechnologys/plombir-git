//! Actions from other repositories: `uses: owner/repo[/path]@ref` and
//! `uses: ./path`, run as composite actions.
//!
//! An action is loaded when the workflow is read, on the server, and its steps
//! are written into the job in place of the `uses:` step. Neither runner learns
//! anything new: what reaches them is the step model they already execute, so
//! an action means the same thing on the embedded runner and on
//! `plombir-git-runner`.
//!
//! Where an action may come from is the operator's decision, not the
//! workflow's ([`ActionsPolicy`], `[ci]` `actions_sources`): a repository of
//! this instance, an upstream forge (`https://github.com` unless configured
//! otherwise), or neither. A `uses:` the policy does not allow is refused by
//! the name of the source it would have come from.
//!
//! Only `runs.using: composite` is executed. A JavaScript action
//! (`runs.using: node*`) or a Docker action is refused by name, and so is
//! anything in `action.yml` this engine would otherwise have to ignore — the
//! promise every other key of a workflow keeps: it runs, or it is refused.
//!
//! The three actions the engine implements natively — `actions/checkout`,
//! `actions/cache`, `actions/upload-artifact` — are never loaded: they keep
//! their own translation (`gitea_actions::SUPPORTED_ACTIONS`).

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::gitea_actions::{GiteaStep, GiteaWorkflow};

/// The forge `uses: owner/repo@ref` is fetched from when the instance does not
/// hold the repository — Gitea's `DEFAULT_ACTIONS_URL`.
pub const DEFAULT_ACTIONS_UPSTREAM_URL: &str = "https://github.com";

/// The sources a workflow may load actions from unless the operator says
/// otherwise: this instance's own repositories, and no network.
pub const DEFAULT_ACTIONS_SOURCES: &[&str] = &["instance"];

/// Whether a `uses:` must pin a full commit SHA unless the operator says
/// otherwise.
pub const DEFAULT_ACTIONS_REQUIRE_FULL_SHA: bool = false;

/// The built-in ceiling, in MiB, on the cache of actions fetched from the
/// upstream: one fetch may not write more, and the cache evicts the actions
/// used least recently once it holds more.
pub const DEFAULT_ACTIONS_CACHE_MAX_MB: u64 = 1024;

/// How deep composite actions may call composite actions — GitHub's limit.
const MAX_ACTION_DEPTH: usize = 10;

/// How many steps one job may hold once its actions are written into it.
const MAX_EXPANDED_STEPS: usize = 1000;

/// The largest `action.yml` read, the same ceiling a workflow file has.
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

/// Where `uses: owner/repo@ref` may be loaded from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionSource {
    /// A repository of this instance: public, or the workflow's own.
    Instance,
    /// The configured upstream forge.
    Upstream,
}

impl ActionSource {
    /// The spelling `[ci]` `actions_sources` uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::Instance => "instance",
            Self::Upstream => "upstream",
        }
    }

    /// Read one `[ci]` `actions_sources` entry.
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "instance" => Ok(Self::Instance),
            "upstream" => Ok(Self::Upstream),
            other => Err(format!(
                "`{other}` is not an action source; write \"instance\" (this instance's \
                 repositories) or \"upstream\" (the forge at ci.actions_upstream_url)"
            )),
        }
    }
}

/// The operator's rules for loading actions — `[ci]` `actions_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionsPolicy {
    sources: Vec<ActionSource>,
    upstream_url: String,
    require_full_sha: bool,
    cache_max_mb: u64,
}

impl Default for ActionsPolicy {
    fn default() -> Self {
        Self {
            sources: DEFAULT_ACTIONS_SOURCES
                .iter()
                .map(|source| ActionSource::parse(source).expect("built-in source"))
                .collect(),
            upstream_url: DEFAULT_ACTIONS_UPSTREAM_URL.to_string(),
            require_full_sha: DEFAULT_ACTIONS_REQUIRE_FULL_SHA,
            cache_max_mb: DEFAULT_ACTIONS_CACHE_MAX_MB,
        }
    }
}

impl ActionsPolicy {
    /// Replace the allowed sources; `setting` names the key in a refusal.
    pub fn with_sources(mut self, setting: &str, values: &[String]) -> Result<Self, String> {
        let mut sources = Vec::new();
        for value in values {
            let source = ActionSource::parse(value).map_err(|why| format!("{setting}: {why}"))?;
            if !sources.contains(&source) {
                sources.push(source);
            }
        }
        self.sources = sources;
        Ok(self)
    }

    /// Replace the upstream forge. `https://` for a forge, `file://` for a
    /// directory of mirrors on an air-gapped host; anything else sends the
    /// fetch in the clear or somewhere git would run a program.
    pub fn with_upstream_url(mut self, setting: &str, value: &str) -> Result<Self, String> {
        let trimmed = value.trim().trim_end_matches('/');
        let scheme_ok = trimmed
            .strip_prefix("https://")
            .or_else(|| trimmed.strip_prefix("file://"))
            .is_some_and(|rest| !rest.is_empty());
        if !scheme_ok
            || trimmed
                .chars()
                .any(|ch| ch.is_whitespace() || ch.is_control())
        {
            return Err(format!(
                "{setting} = {value:?} is not an upstream forge: write an https:// URL such as \
                 \"{DEFAULT_ACTIONS_UPSTREAM_URL}\", or a file:// directory of mirrors"
            ));
        }
        self.upstream_url = trimmed.to_string();
        Ok(self)
    }

    /// Require every `uses: owner/repo@ref` to name a full commit SHA.
    pub fn with_require_full_sha(mut self, require: bool) -> Self {
        self.require_full_sha = require;
        self
    }

    /// Replace the upstream cache ceiling, in MiB; `0` removes it.
    pub fn with_cache_max_mb(mut self, cache_max_mb: u64) -> Self {
        self.cache_max_mb = cache_max_mb;
        self
    }

    pub fn allows(&self, source: ActionSource) -> bool {
        self.sources.contains(&source)
    }

    pub fn upstream_url(&self) -> &str {
        &self.upstream_url
    }

    pub fn require_full_sha(&self) -> bool {
        self.require_full_sha
    }

    pub fn cache_max_mb(&self) -> u64 {
        self.cache_max_mb
    }

    fn allowed_names(&self) -> String {
        if self.sources.is_empty() {
            "none".to_string()
        } else {
            self.sources
                .iter()
                .map(|source| source.name())
                .collect::<Vec<_>>()
                .join(", ")
        }
    }
}

/// A `uses:` value this module loads.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum ActionReference {
    /// `./path`: a directory of the workflow's own repository, at the commit
    /// being built.
    Local { path: String },
    /// `owner/repo[/path]@ref`.
    Remote {
        owner: String,
        repo: String,
        path: String,
        git_ref: String,
    },
}

impl ActionReference {
    /// `None` for a `uses:` this module leaves alone: one of the natively
    /// implemented actions, and `docker://` (refused by the workflow check).
    pub fn parse(uses: &str) -> Result<Option<Self>, String> {
        if crate::gitea_actions::is_native_action(uses) || uses.starts_with("docker://") {
            return Ok(None);
        }
        if let Some(path) = uses.strip_prefix("./") {
            let path = path.trim_end_matches('/');
            validate_path(path, uses)?;
            return Ok(Some(Self::Local {
                path: path.to_string(),
            }));
        }
        let Some((location, git_ref)) = uses.rsplit_once('@') else {
            return Err(format!(
                "uses: {uses} names no version; write owner/repo@<tag, branch or commit SHA>"
            ));
        };
        if git_ref.is_empty()
            || !git_ref.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/')
            })
            || git_ref.starts_with('-')
            || git_ref.contains("..")
        {
            return Err(format!(
                "uses: {uses} — `{git_ref}` is not a tag, branch or commit SHA this engine resolves"
            ));
        }
        let mut parts = location.splitn(3, '/');
        let (Some(owner), Some(repo)) = (parts.next(), parts.next()) else {
            return Err(format!(
                "uses: {uses} is neither ./path nor owner/repo[/path]@ref"
            ));
        };
        let path = parts.next().unwrap_or("").trim_end_matches('/');
        for (what, name) in [("owner", owner), ("repository", repo)] {
            if name.is_empty()
                || name.starts_with('.')
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                return Err(format!(
                    "uses: {uses} — `{name}` is not a valid {what} name"
                ));
            }
        }
        if !path.is_empty() {
            validate_path(path, uses)?;
        }
        Ok(Some(Self::Remote {
            owner: owner.to_string(),
            repo: repo.to_string(),
            path: path.to_string(),
            git_ref: git_ref.to_string(),
        }))
    }

    /// The reference as a workflow spells it.
    pub fn label(&self) -> String {
        match self {
            Self::Local { path } => format!("./{path}"),
            Self::Remote {
                owner,
                repo,
                path,
                git_ref,
            } if path.is_empty() => format!("{owner}/{repo}@{git_ref}"),
            Self::Remote {
                owner,
                repo,
                path,
                git_ref,
            } => format!("{owner}/{repo}/{path}@{git_ref}"),
        }
    }
}

fn validate_path(path: &str, uses: &str) -> Result<(), String> {
    let sound = !path.is_empty()
        && path.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        });
    if sound {
        Ok(())
    } else {
        Err(format!(
            "uses: {uses} — `{path}` is not a directory path inside a repository"
        ))
    }
}

/// An `action.yml` and where it was found.
#[derive(Debug, Clone)]
pub(crate) struct LoadedAction {
    /// The commit the reference resolved to.
    pub sha: String,
    /// Where it came from, for the job log: `this repository`, `this
    /// instance`, or the upstream URL.
    pub origin: String,
    /// The `action.yml` text.
    pub manifest: String,
}

/// Why an action could not be loaded.
#[derive(Debug)]
pub(crate) enum ActionLoadError {
    /// The workflow's to fix: a policy refusal, a missing repository, ref or
    /// file, an upstream that answered no.
    Refused(String),
    /// The server's: storage that would not open, a cache that could not be
    /// written. Kept out of the message a workflow author sees.
    Failed(anyhow::Error),
}

impl From<anyhow::Error> for ActionLoadError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failed(error)
    }
}

/// Where the workflow's actions are read from.
pub(crate) trait ActionLoader {
    fn load(&self, reference: &ActionReference) -> Result<LoadedAction, ActionLoadError>;
}

// ── action.yml ─────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionManifest {
    // What a person reads about the action; nothing to execute.
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    description: Option<String>,
    #[allow(dead_code)]
    author: Option<String>,
    #[allow(dead_code)]
    branding: Option<serde_json::Value>,
    #[serde(default)]
    inputs: BTreeMap<String, ActionInput>,
    #[serde(default)]
    outputs: BTreeMap<String, ActionOutput>,
    runs: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionInput {
    #[allow(dead_code)]
    description: Option<String>,
    #[serde(default)]
    required: Option<serde_json::Value>,
    #[serde(default)]
    default: Option<serde_json::Value>,
    #[serde(rename = "deprecationMessage")]
    #[allow(dead_code)]
    deprecation_message: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionOutput {
    #[allow(dead_code)]
    description: Option<String>,
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompositeRuns {
    #[allow(dead_code)]
    using: String,
    steps: Vec<GiteaStep>,
}

/// A scalar as text: `true`, `3`, `x`. `None` for a mapping or a list.
fn scalar_text(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Bool(flag) => Some(flag.to_string()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Null => Some(String::new()),
        _ => None,
    }
}

struct ParsedAction {
    inputs: BTreeMap<String, ActionInput>,
    outputs: BTreeMap<String, ActionOutput>,
    steps: Vec<GiteaStep>,
}

fn parse_manifest(label: &str, manifest: &str) -> Result<ParsedAction, String> {
    let manifest: ActionManifest = rg_core::yaml::from_str(manifest)
        .map_err(|error| format!("action {label}: action.yml: {error:#}"))?;
    let using = manifest
        .runs
        .get("using")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("action {label}: action.yml has no runs.using"))?;
    match using {
        "composite" => {}
        javascript if javascript.starts_with("node") => {
            return Err(format!(
                "action {label} is a JavaScript action (runs.using: {javascript}); only composite \
                 actions run here"
            ))
        }
        "docker" => {
            return Err(format!(
                "action {label} is a Docker action (runs.using: docker); only composite actions \
                 run here"
            ))
        }
        other => {
            return Err(format!(
                "action {label}: runs.using: {other} is not an action runtime this engine knows"
            ))
        }
    }
    let runs: CompositeRuns = serde_json::from_value(manifest.runs)
        .map_err(|error| format!("action {label}: action.yml runs: {error}"))?;
    for (name, input) in &manifest.inputs {
        if !crate::gitea_actions::valid_input_name(name) {
            return Err(format!(
                "action {label}: input `{name}` is not a valid input name"
            ));
        }
        if let Some(default) = &input.default {
            if scalar_text(default).is_none() {
                return Err(format!(
                    "action {label}: input `{name}` has a default that is not a string"
                ));
            }
        }
        if let Some(required) = &input.required {
            if required_flag(required).is_none() {
                return Err(format!(
                    "action {label}: input `{name}` has required: that is neither true nor false"
                ));
            }
        }
    }
    for (name, output) in &manifest.outputs {
        if !rg_process::ci_steps::valid_runtime_name(name) {
            return Err(format!(
                "action {label}: output `{name}` is not a valid output name"
            ));
        }
        if output.value.is_none() {
            return Err(format!(
                "action {label}: output `{name}` has no value — a composite action's output says \
                 which step it comes from"
            ));
        }
    }
    Ok(ParsedAction {
        inputs: manifest.inputs,
        outputs: manifest.outputs,
        steps: runs.steps,
    })
}

fn required_flag(value: &serde_json::Value) -> Option<bool> {
    match value {
        serde_json::Value::Bool(flag) => Some(*flag),
        serde_json::Value::String(text) => match text.as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

// ── Expansion ──────────────────────────────────────────────────────────

/// Write every action a job of `workflow` uses into the job, in place of the
/// `uses:` step that named it.
///
/// `Err` is the workflow's to fix, and names the job and the action; the
/// server's own failures come back as [`ActionLoadError::Failed`] inside the
/// `Ok(Err)` half so the caller keeps them out of the author's message.
pub(crate) fn expand_actions(
    workflow: &mut GiteaWorkflow,
    loader: &dyn ActionLoader,
) -> Result<Result<(), String>, anyhow::Error> {
    let mut cache: HashMap<ActionReference, LoadedAction> = HashMap::new();
    let mut job_names = workflow.jobs.keys().cloned().collect::<Vec<_>>();
    job_names.sort();
    for job_name in job_names {
        let steps = std::mem::take(
            &mut workflow
                .jobs
                .get_mut(&job_name)
                .expect("listed above")
                .steps,
        );
        let mut expansion = Expansion {
            loader,
            cache: &mut cache,
            job_name: &job_name,
            anonymous: 0,
            out: Vec::new(),
        };
        for step in steps {
            match expansion.step(step, 0) {
                Ok(()) => {}
                Err(StepError::Refused(reason)) => return Ok(Err(format!("{job_name}: {reason}"))),
                Err(StepError::Failed(error)) => return Err(error),
            }
        }
        workflow
            .jobs
            .get_mut(&job_name)
            .expect("listed above")
            .steps = expansion.out;
    }
    Ok(Ok(()))
}

enum StepError {
    Refused(String),
    Failed(anyhow::Error),
}

impl From<String> for StepError {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

struct Expansion<'a> {
    loader: &'a dyn ActionLoader,
    cache: &'a mut HashMap<ActionReference, LoadedAction>,
    job_name: &'a str,
    /// Ids handed to `uses:` steps that declared none, so their inner steps can
    /// still be told apart.
    anonymous: usize,
    out: Vec<GiteaStep>,
}

impl Expansion<'_> {
    fn push(&mut self, step: GiteaStep) -> Result<(), StepError> {
        if self.out.len() >= MAX_EXPANDED_STEPS {
            return Err(StepError::Refused(format!(
                "more than {MAX_EXPANDED_STEPS} steps once its actions are written into it"
            )));
        }
        self.out.push(step);
        Ok(())
    }

    /// Write `step` into the job, expanding it first when it uses an action.
    fn step(&mut self, step: GiteaStep, depth: usize) -> Result<(), StepError> {
        let Some(uses) = step.uses.clone() else {
            return self.push(step);
        };
        let Some(reference) = ActionReference::parse(&uses)? else {
            return self.push(step);
        };
        if depth >= MAX_ACTION_DEPTH {
            return Err(StepError::Refused(format!(
                "uses: {uses} — composite actions nest deeper than {MAX_ACTION_DEPTH}"
            )));
        }
        let refused_keys = [
            ("run", step.run.is_some()),
            ("shell", step.shell.is_some()),
            ("working-directory", step.working_directory.is_some()),
            ("continue-on-error", step.continue_on_error.is_some()),
            ("timeout-minutes", step.timeout_minutes.is_some()),
        ]
        .into_iter()
        .filter_map(|(key, present)| present.then_some(key))
        .collect::<Vec<_>>();
        if !refused_keys.is_empty() {
            return Err(StepError::Refused(format!(
                "uses: {uses} — {} on a step that runs a composite action is not supported",
                refused_keys.join(", ")
            )));
        }

        let loaded = match self.cache.get(&reference) {
            Some(loaded) => loaded.clone(),
            None => {
                let loaded = match self.loader.load(&reference) {
                    Ok(loaded) => loaded,
                    Err(ActionLoadError::Refused(reason)) => {
                        return Err(StepError::Refused(format!("uses: {uses} — {reason}")))
                    }
                    Err(ActionLoadError::Failed(error)) => {
                        return Err(StepError::Failed(error.context(format!(
                            "loading action {uses} for job {}",
                            self.job_name
                        ))))
                    }
                };
                self.cache.insert(reference.clone(), loaded.clone());
                loaded
            }
        };
        let label = reference.label();
        let action = parse_manifest(&label, &loaded.manifest)?;
        let inputs = resolve_inputs(&label, &action.inputs, &step.with)?;

        let caller_id = match &step.id {
            Some(id) => id.clone(),
            None => {
                self.anonymous += 1;
                format!("__action{}", self.anonymous)
            }
        };
        let caller_name = step.name.clone().unwrap_or_else(|| label.clone());
        let inner_ids = action
            .steps
            .iter()
            .filter_map(|inner| inner.id.clone())
            .collect::<Vec<_>>();
        // All of the job's secrets the action's steps may read: what the
        // workflow's step hands it — or, for an action an action calls, what
        // the outermost one was handed. An action has no secrets context of
        // its own, so nothing it writes can ask for more (card_e42a6d0af271).
        let granted = step.granted_secrets.clone().unwrap_or_else(|| {
            let mut names = std::collections::BTreeSet::new();
            for value in step.with.values().chain(step.env.values()) {
                crate::gitea_actions::secrets_named(value, &mut names);
            }
            names
        });
        let context = ActionContext {
            label: &label,
            inputs: &inputs,
            inner_ids: &inner_ids,
            caller_id: &caller_id,
            reference: &reference,
            sha: &loaded.sha,
            granted: &granted,
        };

        // What `uses:` resolved to goes in the job log before anything of the
        // action runs: a tag can move, the SHA cannot.
        self.push(GiteaStep {
            name: Some(format!("Set up {caller_name}")),
            uses: None,
            run: Some(format!(
                "echo 'Action {label} is commit {} from {}'",
                loaded.sha, loaded.origin
            )),
            shell: Some("sh".to_string()),
            working_directory: None,
            id: None,
            continue_on_error: None,
            timeout_minutes: None,
            env: HashMap::new(),
            condition: step.condition.clone(),
            with: HashMap::new(),
            granted_secrets: Some(std::collections::BTreeSet::new()),
        })?;

        for inner in action.steps {
            let inner = context.rewrite(inner, &step, &caller_name)?;
            self.step(inner, depth + 1)?;
        }

        // The caller reads the action's outputs as `steps.<id>.outputs.<name>`:
        // one more step carries that id and writes each output, evaluated
        // after the action's last step, to `$GITHUB_OUTPUT`. The values travel
        // in its environment, so no output can become a line of its script.
        if step.id.is_some() || !action.outputs.is_empty() {
            let mut env = HashMap::new();
            let mut lines = vec![
                "d=\"plombir-git-output-$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \\n')\""
                    .to_string(),
                "[ \"$d\" = plombir-git-output- ] && d=\"plombir-git-output-$$-$(date +%s)\""
                    .to_string(),
            ];
            for (index, (name, output)) in action.outputs.iter().enumerate() {
                let value = context.text(output.value.as_deref().unwrap_or_default())?;
                let variable = format!("PLOMBIR_GIT_ACTION_OUTPUT_{index}");
                lines.push(format!(
                    "printf '%s<<%s\\n%s\\n%s\\n' '{name}' \"$d\" \"${variable}\" \"$d\" >> \"$GITHUB_OUTPUT\""
                ));
                env.insert(variable, value);
            }
            if action.outputs.is_empty() {
                lines = vec![":".to_string()];
            }
            self.push(GiteaStep {
                name: Some(format!("Outputs of {caller_name}")),
                uses: None,
                run: Some(lines.join("\n")),
                shell: Some("sh".to_string()),
                working_directory: None,
                id: Some(caller_id),
                continue_on_error: None,
                timeout_minutes: None,
                env,
                condition: step.condition.clone(),
                with: HashMap::new(),
                granted_secrets: Some(granted.clone()),
            })?;
        }
        Ok(())
    }
}

/// The value of each input the action declares: the caller's `with:`, else
/// the declared default, else empty — and a refusal for a required input the
/// caller left out or a `with:` key the action does not declare.
fn resolve_inputs(
    label: &str,
    declared: &BTreeMap<String, ActionInput>,
    with: &HashMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let declared_names = declared
        .keys()
        .map(|name| name.to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut unknown = with
        .keys()
        .filter(|key| !declared_names.contains(&key.to_ascii_lowercase()))
        .cloned()
        .collect::<Vec<_>>();
    if !unknown.is_empty() {
        unknown.sort();
        return Err(format!(
            "action {label} declares no input {} (it declares: {})",
            unknown.join(", "),
            if declared.is_empty() {
                "none".to_string()
            } else {
                declared.keys().cloned().collect::<Vec<_>>().join(", ")
            }
        ));
    }
    let mut inputs = BTreeMap::new();
    for (name, input) in declared {
        let given = with
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone());
        let value = match given {
            Some(value) => value,
            None => match input.default.as_ref().and_then(scalar_text) {
                Some(default) => default,
                None if input.required.as_ref().and_then(required_flag) == Some(true) => {
                    return Err(format!(
                        "action {label} requires input `{name}`, and the step gives it no value"
                    ))
                }
                None => String::new(),
            },
        };
        inputs.insert(name.to_ascii_lowercase(), value);
    }
    Ok(inputs)
}

/// What one use of an action resolves its steps' text against.
struct ActionContext<'a> {
    label: &'a str,
    /// Lowercased input name → value.
    inputs: &'a BTreeMap<String, String>,
    /// The ids the action's own steps declare.
    inner_ids: &'a [String],
    /// The id the caller's step runs under; inner ids are prefixed with it.
    caller_id: &'a str,
    reference: &'a ActionReference,
    sha: &'a str,
    /// The secrets every step of this use may read.
    granted: &'a std::collections::BTreeSet<String>,
}

impl ActionContext<'_> {
    /// One of the action's steps, as the job will run it.
    fn rewrite(
        &self,
        inner: GiteaStep,
        caller: &GiteaStep,
        caller_name: &str,
    ) -> Result<GiteaStep, String> {
        let label = self.label;
        if inner.timeout_minutes.is_some() {
            return Err(format!(
                "action {label}: timeout-minutes on a composite action's step is not supported"
            ));
        }
        if inner.run.is_some() && inner.shell.is_none() {
            return Err(format!(
                "action {label}: a composite action's run: step must name its shell:"
            ));
        }
        if inner.run.is_none() && inner.uses.is_none() {
            return Err(format!(
                "action {label}: a composite action's step needs run: or uses:"
            ));
        }
        let inner_label = inner.name.clone().unwrap_or_else(|| match &inner.run {
            Some(run) => format!("Run {}", run.lines().next().unwrap_or_default().trim()),
            None => inner.uses.clone().unwrap_or_default(),
        });
        let mut env = HashMap::new();
        for (name, value) in &caller.env {
            env.insert(name.clone(), value.clone());
        }
        for (name, value) in &inner.env {
            env.insert(name.clone(), self.text(value)?);
        }
        let mut with = HashMap::new();
        for (name, value) in &inner.with {
            with.insert(name.clone(), self.text(value)?);
        }
        let condition = match (
            caller.condition.as_deref(),
            inner
                .condition
                .as_deref()
                .map(|condition| self.expression(condition))
                .transpose()?,
        ) {
            (None, None) => None,
            (Some(caller), None) => Some(caller.to_string()),
            (None, Some(inner)) => Some(inner),
            (Some(caller), Some(inner)) => Some(format!(
                "({}) && ({})",
                unwrap_expression(caller),
                unwrap_expression(&inner)
            )),
        };
        Ok(GiteaStep {
            name: Some(format!("{caller_name} / {}", self.text(&inner_label)?)),
            uses: inner
                .uses
                .as_deref()
                .map(|uses| self.text(uses))
                .transpose()?,
            run: inner.run.as_deref().map(|run| self.text(run)).transpose()?,
            shell: inner.shell.clone(),
            working_directory: inner
                .working_directory
                .as_deref()
                .map(|directory| self.text(directory))
                .transpose()?,
            id: inner.id.as_deref().map(|id| self.prefixed(id)),
            continue_on_error: inner.continue_on_error.clone(),
            timeout_minutes: None,
            env,
            condition,
            with,
            granted_secrets: Some(self.granted.clone()),
        })
    }

    fn prefixed(&self, id: &str) -> String {
        format!("{}__{id}", self.caller_id)
    }

    /// A text field: every `${{ inputs.<name> }}` becomes the input's value
    /// as written, every other expression has its `inputs.*` and the
    /// action's own `steps.*` rewritten in place.
    ///
    /// The result holds no `${{` but the ones written here: the caller's input
    /// values and the expressions rewritten in place. Text that would become
    /// an expression only once its own pieces are read — `$${{ '{' }}{
    /// secrets.X }}` — is refused, or an action could name a secret its
    /// expressions are not allowed to.
    fn text(&self, text: &str) -> Result<String, String> {
        let mut written = 0;
        let out = map_groups(text, |expression| {
            if let Some(name) = whole_input(expression) {
                let value = self.input(name)?.to_string();
                written += value.matches("${{").count();
                return Ok(value);
            }
            let body = self.expression_body(expression)?;
            // `${{ github.action_ref }}` became one string literal: in text it
            // is that text, not an expression the job would have to evaluate.
            Ok(match single_literal(&body) {
                Some(literal) => literal,
                None => {
                    written += 1;
                    format!("${{{{ {body} }}}}")
                }
            })
        })?;
        if out.matches("${{").count() != written {
            return Err(format!(
                "action {} writes `${{{{` that is not an expression of its own — text that turns \
                 into an expression only after its pieces are read is not supported",
                self.label
            ));
        }
        Ok(out)
    }

    /// An `if:`, with or without its `${{ }}`. An input is always a string
    /// literal here — even read whole — so its value cannot become operators
    /// of the condition.
    fn expression(&self, condition: &str) -> Result<String, String> {
        if condition.contains("${{") {
            map_groups(condition, |expression| {
                Ok(format!("${{{{ {} }}}}", self.expression_body(expression)?))
            })
        } else {
            self.expression_body(condition)
        }
    }

    fn input(&self, name: &str) -> Result<&str, String> {
        self.inputs
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
            .ok_or_else(|| {
                format!(
                    "action {} reads input `{name}`, which it does not declare",
                    self.label
                )
            })
    }

    /// The inside of one expression: `inputs.<name>` becomes a string
    /// literal, the action's own `steps.<id>` its prefixed id, and
    /// `github.action_repository` / `github.action_ref` what the reference
    /// names. `github.action_path` is refused: an action's files are not
    /// shipped to the runner, only its steps.
    fn expression_body(&self, expression: &str) -> Result<String, String> {
        if expression.contains("github.action_path") {
            return Err(format!(
                "action {} reads github.action_path; the files of an action do not reach the \
                 runner here — only its steps run, so inline what it runs from its directory",
                self.label
            ));
        }
        let (repository, git_ref) = match self.reference {
            ActionReference::Local { .. } => (String::new(), self.sha.to_string()),
            ActionReference::Remote {
                owner,
                repo,
                git_ref,
                ..
            } => (format!("{owner}/{repo}"), git_ref.clone()),
        };
        let mut out = String::with_capacity(expression.len());
        let bytes = expression.as_bytes();
        let mut index = 0;
        let mut in_string = false;
        'scan: while index < bytes.len() {
            let byte = bytes[index];
            if byte == b'\'' {
                in_string = !in_string;
                out.push('\'');
                index += 1;
                continue;
            }
            let boundary = index == 0 || !is_identifier_byte(bytes[index - 1]);
            if !in_string && boundary {
                let rest = &expression[index..];
                if let Some(name) = identifier_after(rest, "inputs.") {
                    let value = self.input(name)?;
                    if value.contains("${{") {
                        return Err(format!(
                            "action {} reads input `{name}` inside an expression, and the step \
                             gives it an expression of its own; pass it a plain value, or read \
                             it as a whole `${{{{ inputs.{name} }}}}`",
                            self.label
                        ));
                    }
                    out.push('\'');
                    out.push_str(&value.replace('\'', "''"));
                    out.push('\'');
                    index += "inputs.".len() + name.len();
                    continue;
                }
                if let Some(name) = identifier_after(rest, "secrets.") {
                    return Err(format!(
                        "action {} reads secrets.{name}; an action has no secrets context, as \
                         on GitHub — the workflow passes what it needs as an input",
                        self.label
                    ));
                }
                if let Some(id) = identifier_after(rest, "steps.") {
                    if self.inner_ids.iter().any(|inner| inner == id) {
                        out.push_str("steps.");
                        out.push_str(&self.prefixed(id));
                        index += "steps.".len() + id.len();
                        continue;
                    }
                }
                for (key, value) in [
                    ("github.action_repository", repository.as_str()),
                    ("github.action_ref", git_ref.as_str()),
                ] {
                    if rest.starts_with(key)
                        && rest[key.len()..]
                            .bytes()
                            .next()
                            .is_none_or(|next| !is_identifier_byte(next) && next != b'.')
                    {
                        out.push('\'');
                        out.push_str(&value.replace('\'', "''"));
                        out.push('\'');
                        index += key.len();
                        continue 'scan;
                    }
                }
            }
            let ch = expression[index..].chars().next().expect("in bounds");
            out.push(ch);
            index += ch.len_utf8();
        }
        Ok(out)
    }
}

/// `text` with every `${{ … }}` group replaced by what `map` makes of its
/// trimmed inside.
fn map_groups(
    text: &str,
    mut map: impl FnMut(&str) -> Result<String, String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("${{") {
        let after = &rest[open + 3..];
        let Some(close) = after.find("}}") else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&map(after[..close].trim())?);
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// The text of `body` when it is exactly one `'…'` string literal.
pub(crate) fn single_literal(body: &str) -> Option<String> {
    let inner = body.strip_prefix('\'')?.strip_suffix('\'')?;
    (!inner.replace("''", "").contains('\'')).then(|| inner.replace("''", "'"))
}

fn is_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

/// `name` when `text` starts with `prefix` followed by an identifier.
fn identifier_after<'t>(text: &'t str, prefix: &str) -> Option<&'t str> {
    let rest = text.strip_prefix(prefix)?;
    let end = rest
        .bytes()
        .position(|byte| !is_identifier_byte(byte))
        .unwrap_or(rest.len());
    (end > 0).then(|| &rest[..end])
}

/// `name` when the whole expression is `inputs.<name>`.
fn whole_input(expression: &str) -> Option<&str> {
    identifier_after(expression, "inputs.").filter(|name| expression == format!("inputs.{name}"))
}

fn unwrap_expression(condition: &str) -> &str {
    let trimmed = condition.trim();
    trimmed
        .strip_prefix("${{")
        .and_then(|inner| inner.strip_suffix("}}"))
        .map(str::trim)
        .unwrap_or(trimmed)
}

// ── Loading ────────────────────────────────────────────────────────────

/// A repository of this instance an action may come from, as the database
/// answers for it.
#[derive(Debug, Clone)]
pub(crate) struct InstanceRepository {
    /// Its directory under the repository root, `<namespace>/<name>.git`.
    pub path: PathBuf,
    pub is_private: bool,
    pub id: i64,
}

/// Looks a repository of this instance up by `owner/repo`.
pub(crate) type InstanceLookup<'a> =
    dyn Fn(&str, &str) -> anyhow::Result<Option<InstanceRepository>> + Send + Sync + 'a;

/// Loads actions for one trigger of one workflow repository.
pub(crate) struct RepositoryActionLoader<'a> {
    pub policy: &'a ActionsPolicy,
    /// The repository the workflow lives in, and the commit being built:
    /// `./path` actions are read from there.
    pub workflow_repo: &'a Path,
    pub workflow_repo_id: i64,
    pub commit_sha: &'a str,
    /// Where the instance keeps repositories; the upstream cache lives under
    /// it in [`ACTIONS_CACHE_DIR`].
    pub repo_root: &'a Path,
    pub lookup: &'a InstanceLookup<'a>,
}

/// The directory under the repository root that holds the upstream cache.
/// Underscore-led like `_ci_cache`, so no owner name can collide with it.
pub(crate) const ACTIONS_CACHE_DIR: &str = "_ci_actions";

/// Serialises every fetch into the upstream cache in this process: two
/// triggers fetching into one cached repository, or an eviction under a fetch,
/// would otherwise race on the same directory.
static UPSTREAM_CACHE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl ActionLoader for RepositoryActionLoader<'_> {
    fn load(&self, reference: &ActionReference) -> Result<LoadedAction, ActionLoadError> {
        match reference {
            ActionReference::Local { path } => {
                let repo = rg_git::repository::open(self.workflow_repo).map_err(|error| {
                    ActionLoadError::Failed(anyhow::anyhow!(
                        "failed to open the workflow repository: {error}"
                    ))
                })?;
                let manifest = read_manifest(&repo, self.commit_sha, path)?;
                Ok(LoadedAction {
                    sha: self.commit_sha.to_string(),
                    origin: "this repository".to_string(),
                    manifest,
                })
            }
            ActionReference::Remote {
                owner,
                repo,
                path,
                git_ref,
            } => {
                let full_sha = is_full_sha(git_ref);
                if self.policy.require_full_sha() && !full_sha {
                    return Err(ActionLoadError::Refused(format!(
                        "`{git_ref}` is not a full commit SHA, and this instance loads actions \
                         only by one (ci.actions_require_full_sha)"
                    )));
                }
                let instance = (self.lookup)(owner, repo)?;
                if let Some(instance) = instance {
                    return self.load_from_instance(owner, repo, path, git_ref, &instance);
                }
                if !self.policy.allows(ActionSource::Upstream) {
                    let mut reason = format!(
                        "no repository {owner}/{repo} on this instance, and source \"upstream\" \
                         ({}) is not allowed (ci.actions_sources = {})",
                        self.policy.upstream_url(),
                        self.policy.allowed_names()
                    );
                    if !self.policy.allows(ActionSource::Instance) {
                        reason = format!(
                            "source \"upstream\" ({}) is not allowed, and neither is \
                             \"instance\" (ci.actions_sources = {})",
                            self.policy.upstream_url(),
                            self.policy.allowed_names()
                        );
                    }
                    return Err(ActionLoadError::Refused(reason));
                }
                self.load_from_upstream(owner, repo, path, git_ref)
            }
        }
    }
}

impl RepositoryActionLoader<'_> {
    fn load_from_instance(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        git_ref: &str,
        instance: &InstanceRepository,
    ) -> Result<LoadedAction, ActionLoadError> {
        if !self.policy.allows(ActionSource::Instance) {
            return Err(ActionLoadError::Refused(format!(
                "source \"instance\" ({owner}/{repo} on this instance) is not allowed \
                 (ci.actions_sources = {})",
                self.policy.allowed_names()
            )));
        }
        // A private repository's actions are its owner's to share, and nothing
        // records that it was shared: another repository's workflow would
        // otherwise run — and print — whatever it holds.
        if instance.is_private && instance.id != self.workflow_repo_id {
            return Err(ActionLoadError::Refused(format!(
                "{owner}/{repo} is a private repository of this instance; actions load from its \
                 public repositories and from the workflow's own"
            )));
        }
        let repository = rg_git::repository::open(&instance.path).map_err(|error| {
            ActionLoadError::Failed(anyhow::anyhow!(
                "failed to open the action repository {owner}/{repo}: {error}"
            ))
        })?;
        let sha = resolve_local_ref(&repository, git_ref)?.ok_or_else(|| {
            ActionLoadError::Refused(format!(
                "{owner}/{repo} on this instance has no tag, branch or commit `{git_ref}`"
            ))
        })?;
        let manifest = read_manifest(&repository, &sha, path)?;
        Ok(LoadedAction {
            sha,
            origin: "this instance".to_string(),
            manifest,
        })
    }

    fn load_from_upstream(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        git_ref: &str,
    ) -> Result<LoadedAction, ActionLoadError> {
        let url = format!("{}/{owner}/{repo}", self.policy.upstream_url());
        let cache_root = self.repo_root.join(ACTIONS_CACHE_DIR);
        let cached = cache_root
            .join(cache_host(self.policy.upstream_url()))
            .join(owner.to_ascii_lowercase())
            .join(format!("{}.git", repo.to_ascii_lowercase()));
        let _guard = UPSTREAM_CACHE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let git = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?;
        if !cached.join("HEAD").exists() {
            std::fs::create_dir_all(&cached).map_err(|error| {
                ActionLoadError::Failed(anyhow::anyhow!(
                    "failed to create the action cache {}: {error}",
                    cached.display()
                ))
            })?;
            rg_git::invocation::local(git)
                .run(&["init", "--bare", "--quiet"], Some(&cached))
                .and_then(|output| output.ensure_success())
                .map_err(|error| {
                    ActionLoadError::Failed(error.context("initialise the action cache"))
                })?;
        }
        // A full SHA already held is the cache hit; a tag or a branch is asked
        // of the upstream every time, because it can move.
        let already = is_full_sha(git_ref)
            && rg_git::repository::open(&cached)
                .ok()
                .and_then(|repository| resolve_local_ref(&repository, git_ref).ok().flatten())
                .is_some();
        let sha = if already {
            git_ref.to_string()
        } else {
            let budget = rg_core::clone_budget::CloneBudget::from_mb(
                "ci.actions_cache_max_mb",
                self.policy.cache_max_mb(),
            );
            let fetched = rg_git::credentials::credential_invocation(None)
                .run_under_disk_budget(
                    git,
                    &[
                        "-c",
                        "protocol.version=2",
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--depth=1",
                        &url,
                        git_ref,
                    ],
                    Some(&cached),
                    budget.for_fetch(&cached)?.as_ref(),
                )
                .map_err(|error| budget.explain(error));
            let output = match fetched {
                Ok(output) => output,
                Err(error) if rg_core::clone_budget::CloneBudget::refused(&error) => {
                    discard(&cached);
                    return Err(ActionLoadError::Refused(format!("{error:#}")));
                }
                Err(error) => return Err(ActionLoadError::Failed(error)),
            };
            if !output.success() {
                let said = output
                    .stderr_str()
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or("no reason given")
                    .to_string();
                return Err(ActionLoadError::Refused(format!(
                    "the upstream {url} did not hand over `{git_ref}`: {}",
                    rg_core::net::mask_url_credentials(&said)
                )));
            }
            let head = rg_git::invocation::local(git)
                .run(
                    &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
                    Some(&cached),
                )
                .map_err(ActionLoadError::Failed)?;
            if !head.success() {
                return Err(ActionLoadError::Refused(format!(
                    "the upstream {url} answered `{git_ref}` with something that is not a commit"
                )));
            }
            let sha = head.stdout_str().trim().to_string();
            // Kept by a ref, so a `gc` in the cache does not prune what a
            // later run of the same SHA expects to find.
            let keep = format!("refs/plombir-git/{sha}");
            rg_git::invocation::local(git)
                .run(&["update-ref", &keep, &sha], Some(&cached))
                .and_then(|output| output.ensure_success())
                .map_err(|error| ActionLoadError::Failed(error.context("pin a cached action")))?;
            sha
        };
        touch(&cached);
        let repository = rg_git::repository::open(&cached).map_err(|error| {
            ActionLoadError::Failed(anyhow::anyhow!("failed to open the action cache: {error}"))
        })?;
        let manifest = read_manifest(&repository, &sha, path)?;
        evict(&cache_root, &cached, self.policy.cache_max_mb());
        Ok(LoadedAction {
            sha,
            origin: url,
            manifest,
        })
    }
}

fn is_full_sha(git_ref: &str) -> bool {
    git_ref.len() == 40 && git_ref.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The commit `git_ref` names: a full SHA held here, a tag, then a branch.
fn resolve_local_ref(
    repository: &gix::Repository,
    git_ref: &str,
) -> Result<Option<String>, ActionLoadError> {
    if is_full_sha(git_ref) {
        let Ok(id) = gix::ObjectId::from_hex(git_ref.to_ascii_lowercase().as_bytes()) else {
            return Ok(None);
        };
        return Ok(repository
            .find_object(id)
            .ok()
            .and_then(|object| object.peel_to_commit().ok())
            .map(|commit| commit.id.to_string()));
    }
    for name in [
        format!("refs/tags/{git_ref}"),
        format!("refs/heads/{git_ref}"),
    ] {
        let Some(mut reference) = repository
            .try_find_reference(name.as_str())
            .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?
        else {
            continue;
        };
        let id = reference
            .peel_to_id()
            .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?;
        let commit = repository
            .find_object(id.detach())
            .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?
            .peel_to_commit()
            .map_err(|_| ActionLoadError::Refused(format!("`{git_ref}` does not name a commit")))?;
        return Ok(Some(commit.id.to_string()));
    }
    Ok(None)
}

/// `<dir>/action.yml` (or `action.yaml`) at `sha`, bounded like a workflow
/// file.
fn read_manifest(
    repository: &gix::Repository,
    sha: &str,
    dir: &str,
) -> Result<String, ActionLoadError> {
    let id = gix::ObjectId::from_hex(sha.as_bytes())
        .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?;
    let tree = repository
        .find_object(id)
        .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?
        .peel_to_tree()
        .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?;
    let at = |name: &str| {
        if dir.is_empty() {
            name.to_string()
        } else {
            format!("{dir}/{name}")
        }
    };
    for name in ["action.yml", "action.yaml"] {
        let path = at(name);
        let Some(entry) = tree
            .lookup_entry_by_path(path.as_str())
            .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?
        else {
            continue;
        };
        if !entry.mode().is_blob() && !entry.mode().is_executable() {
            return Err(ActionLoadError::Refused(format!("{path} is not a file")));
        }
        let header = repository
            .find_header(entry.oid())
            .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?;
        if header.size() > MAX_MANIFEST_BYTES {
            return Err(ActionLoadError::Refused(format!(
                "{path} is larger than {MAX_MANIFEST_BYTES} bytes"
            )));
        }
        let object = entry
            .object()
            .map_err(|error| ActionLoadError::Failed(anyhow::anyhow!("{error}")))?;
        return String::from_utf8(object.data.clone())
            .map_err(|_| ActionLoadError::Refused(format!("{path} is not UTF-8")));
    }
    Err(ActionLoadError::Refused(format!(
        "no action.yml or action.yaml in {} at commit {sha}",
        if dir.is_empty() {
            "the repository root"
        } else {
            dir
        }
    )))
}

/// The upstream's host as a directory name.
fn cache_host(upstream_url: &str) -> String {
    let without_scheme = upstream_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(upstream_url);
    without_scheme
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-') {
                byte as char
            } else {
                '_'
            }
        })
        .collect()
}

fn discard(path: &Path) {
    if let Err(error) = std::fs::remove_dir_all(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(path = %path.display(), %error, "failed to remove an action cache entry");
        }
    }
}

/// Mark `cached` as just used: eviction goes by this.
fn touch(cached: &Path) {
    let marker = cached.join("plombir-git-last-used");
    if let Err(error) = std::fs::write(&marker, b"") {
        tracing::warn!(path = %marker.display(), %error, "failed to mark an action cache entry used");
    }
}

/// Remove the cached repositories used least recently until the cache holds no
/// more than `max_mb`, never the one this trigger just used.
fn evict(cache_root: &Path, keep: &Path, max_mb: u64) {
    if max_mb == 0 {
        return;
    }
    let max_bytes = max_mb.saturating_mul(1024 * 1024);
    let mut entries = Vec::new();
    let Ok(hosts) = std::fs::read_dir(cache_root) else {
        return;
    };
    for host in hosts.flatten() {
        let Ok(owners) = std::fs::read_dir(host.path()) else {
            continue;
        };
        for owner in owners.flatten() {
            let Ok(repos) = std::fs::read_dir(owner.path()) else {
                continue;
            };
            for repo in repos.flatten() {
                let path = repo.path();
                let used = std::fs::metadata(path.join("plombir-git-last-used"))
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(std::time::UNIX_EPOCH);
                let bytes = rg_core::clone_budget::CloneBudget::measure(&path);
                entries.push((used, bytes, path));
            }
        }
    }
    let mut total: u64 = entries.iter().map(|(_, bytes, _)| *bytes).sum();
    entries.sort_by_key(|(used, _, _)| *used);
    for (_, bytes, path) in entries {
        if total <= max_bytes {
            break;
        }
        if path == keep {
            continue;
        }
        discard(&path);
        total = total.saturating_sub(bytes);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(HashMap<String, String>);

    impl ActionLoader for Fixed {
        fn load(&self, reference: &ActionReference) -> Result<LoadedAction, ActionLoadError> {
            self.0
                .get(&reference.label())
                .map(|manifest| LoadedAction {
                    sha: "a".repeat(40),
                    origin: "test".into(),
                    manifest: manifest.clone(),
                })
                .ok_or_else(|| ActionLoadError::Refused("not in the fixture".into()))
        }
    }

    fn expand(workflow: &str, actions: &[(&str, &str)]) -> Result<GiteaWorkflow, String> {
        let mut workflow = GiteaWorkflow::parse(workflow).unwrap();
        let loader = Fixed(
            actions
                .iter()
                .map(|(label, manifest)| (label.to_string(), manifest.to_string()))
                .collect(),
        );
        expand_actions(&mut workflow, &loader).unwrap()?;
        Ok(workflow)
    }

    const GREET: &str = "name: Greet\ninputs:\n  who:\n    required: true\n  greeting:\n    default: Hello\noutputs:\n  said:\n    value: ${{ steps.say.outputs.text }}\nruns:\n  using: composite\n  steps:\n    - id: say\n      shell: sh\n      run: echo \"text=${{ inputs.greeting }} ${{ inputs.who }}\" >> \"$GITHUB_OUTPUT\"\n    - shell: sh\n      if: inputs.greeting == 'Hello'\n      run: echo polite\n";

    #[test]
    fn references_parse_the_way_workflows_spell_them() {
        assert_eq!(ActionReference::parse("actions/checkout@v4").unwrap(), None);
        assert_eq!(ActionReference::parse("docker://alpine").unwrap(), None);
        assert_eq!(
            ActionReference::parse("./.github/actions/x").unwrap(),
            Some(ActionReference::Local {
                path: ".github/actions/x".into()
            })
        );
        assert_eq!(
            ActionReference::parse("acme/tools/lint@v1.2").unwrap(),
            Some(ActionReference::Remote {
                owner: "acme".into(),
                repo: "tools".into(),
                path: "lint".into(),
                git_ref: "v1.2".into()
            })
        );
        for bad in [
            "acme/tools",
            "acme@v1",
            "acme/tools@",
            "./../x",
            "acme/to ols@v1",
            "a/b@-x",
        ] {
            assert!(ActionReference::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_composite_action_becomes_steps_with_its_inputs_and_outputs() {
        let workflow = expand(
            "on: push\njobs:\n  build:\n    steps:\n      - id: hi\n        uses: acme/greet@v1\n        with:\n          who: ${{ matrix.name }}\n      - run: echo ${{ steps.hi.outputs.said }}\n",
            &[("acme/greet@v1", GREET)],
        )
        .unwrap();
        let steps = &workflow.jobs["build"].steps;
        assert_eq!(steps.len(), 5, "{steps:#?}");
        assert!(steps[0].run.as_deref().unwrap().contains(&"a".repeat(40)));
        assert_eq!(steps[1].id.as_deref(), Some("hi__say"));
        assert_eq!(
            steps[1].run.as_deref(),
            Some("echo \"text=Hello ${{ matrix.name }}\" >> \"$GITHUB_OUTPUT\"")
        );
        assert_eq!(steps[2].condition.as_deref(), Some("'Hello' == 'Hello'"));
        assert_eq!(steps[3].id.as_deref(), Some("hi"));
        assert_eq!(
            steps[3].env["PLOMBIR_GIT_ACTION_OUTPUT_0"],
            "${{ steps.hi__say.outputs.text }}"
        );
        assert_eq!(
            steps[4].run.as_deref(),
            Some("echo ${{ steps.hi.outputs.said }}")
        );
    }

    #[test]
    fn what_the_engine_cannot_run_is_refused_by_name() {
        let workflow = |step: &str| format!("on: push\njobs:\n  build:\n    steps:\n{step}");
        let cases = [
            ("      - uses: acme/greet@v1\n", GREET, "requires input `who`"),
            (
                "      - uses: acme/greet@v1\n        with:\n          who: x\n          colour: red\n",
                GREET,
                "declares no input colour",
            ),
            (
                "      - uses: acme/greet@v1\n        continue-on-error: true\n        with:\n          who: x\n",
                GREET,
                "continue-on-error",
            ),
            (
                "      - uses: acme/greet@v1\n",
                "runs:\n  using: node20\n  main: index.js\n",
                "JavaScript action (runs.using: node20)",
            ),
            (
                "      - uses: acme/greet@v1\n",
                "runs:\n  using: docker\n  image: Dockerfile\n",
                "Docker action",
            ),
            (
                "      - uses: acme/greet@v1\n",
                "runs:\n  using: composite\n  steps: []\nunknown: 1\n",
                "unknown",
            ),
            (
                "      - uses: acme/greet@v1\n",
                "runs:\n  using: composite\n  steps:\n    - run: echo\n",
                "must name its shell",
            ),
            (
                "      - uses: acme/greet@v1\n",
                "runs:\n  using: composite\n  steps:\n    - shell: sh\n      run: ${{ github.action_path }}/x.sh\n",
                "github.action_path",
            ),
            (
                "      - uses: acme/greet@v1\n",
                "runs:\n  using: composite\n  steps:\n    - shell: sh\n      run: echo ${{ inputs.nope }}\n",
                "does not declare",
            ),
            ("      - uses: acme/missing@v1\n", GREET, "not in the fixture"),
            (
                "      - uses: acme/greet@v1\n",
                "runs:\n  using: composite\n  steps:\n    - shell: sh\n      run: echo ${{ secrets.TOKEN }}\n",
                "no secrets context",
            ),
            (
                "      - uses: acme/greet@v1\n",
                "runs:\n  using: composite\n  steps:\n    - shell: sh\n      run: echo $${{ '{' }}{ secrets.TOKEN }}\n",
                "not an expression of its own",
            ),
        ];
        for (step, manifest, expected) in cases {
            let error = expand(&workflow(step), &[("acme/greet@v1", manifest)]).unwrap_err();
            assert!(error.contains(expected), "{expected}: {error}");
            assert!(error.starts_with("build: "), "names the job: {error}");
        }
    }

    /// What the action knows about itself is text where text is written, and
    /// an input read by an `if:` stays a string literal even read whole.
    #[test]
    fn the_action_names_itself_and_inputs_stay_literals_in_conditions() {
        let manifest = "inputs:\n  flag:\n    default: \"true || always()\"\nruns:\n  using: composite\n  steps:\n    - shell: sh\n      if: ${{ inputs.flag }}\n      run: echo ${{ github.action_repository }}@${{ github.action_ref }}\n";
        let workflow = expand(
            "on: push\njobs:\n  build:\n    steps:\n      - uses: acme/greet@v1\n",
            &[("acme/greet@v1", manifest)],
        )
        .unwrap();
        let step = &workflow.jobs["build"].steps[1];
        assert_eq!(step.run.as_deref(), Some("echo acme/greet@v1"));
        assert_eq!(
            step.condition.as_deref(),
            Some("${{ 'true || always()' }}"),
            "the input's text must not become operators of the condition"
        );
        assert_eq!(single_literal("'it''s'").as_deref(), Some("it's"));
        assert_eq!(single_literal("'a' == 'b'"), None);
    }

    /// card_e42a6d0af271: each step's process gets the secrets that step
    /// names; an action's steps get only what the workflow's step passed it,
    /// whatever the action's text says; what the workflow's `env:` names
    /// reaches every step; the job is given the union and nothing else.
    #[test]
    fn each_step_is_granted_only_the_secrets_it_is_handed() {
        let workflow = expand(
            "on: push\nenv:\n  SHARED: ${{ secrets.WF }}\njobs:\n  build:\n    steps:\n      - run: echo ${{ secrets.ALPHA }}\n      - id: hi\n        uses: acme/greet@v1\n        with:\n          who: ${{ secrets.BRAVO }}\n      - uses: acme/quiet@v1\n",
            &[
                ("acme/greet@v1", GREET),
                (
                    "acme/quiet@v1",
                    "runs:\n  using: composite\n  steps:\n    - shell: sh\n      run: echo \"$CHARLIE secrets.CHARLIE\"\n",
                ),
            ],
        )
        .unwrap();
        let config = workflow.to_ci_config(&crate::gitea_actions::WorkflowContext {
            ref_name: "refs/heads/main".into(),
            sha: "abc".into(),
            event: "push".into(),
            repo_owner: "o".into(),
            repo_name: "r".into(),
            event_schedule: None,
        });
        let job = &config.jobs["build"];
        let granted: Vec<(String, Vec<&str>)> = job
            .steps
            .as_ref()
            .unwrap()
            .iter()
            .map(|step| {
                (
                    step.name.clone(),
                    step.secrets.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        let names = |step: usize| granted[step].1.clone();
        assert_eq!(names(0), ["ALPHA", "WF"], "{granted:#?}");
        assert_eq!(names(1), ["WF"], "the set-up step: {granted:#?}");
        for step in 2..=4 {
            assert_eq!(names(step), ["BRAVO", "WF"], "{granted:#?}");
        }
        assert_eq!(names(6), ["WF"], "an action handed nothing: {granted:#?}");
        let job_secrets = job.job_context.as_ref().unwrap().secrets.as_ref().unwrap();
        assert_eq!(
            job_secrets.iter().map(String::as_str).collect::<Vec<_>>(),
            ["ALPHA", "BRAVO", "WF"]
        );
    }

    #[test]
    fn nesting_stops_at_the_depth_limit() {
        let looping = "runs:\n  using: composite\n  steps:\n    - uses: acme/loop@v1\n";
        let error = expand(
            "on: push\njobs:\n  build:\n    steps:\n      - uses: acme/loop@v1\n",
            &[("acme/loop@v1", looping)],
        )
        .unwrap_err();
        assert!(error.contains("nest deeper than 10"), "{error}");
    }

    #[test]
    fn the_policy_reads_its_settings_and_refuses_what_it_cannot_honour() {
        let policy = ActionsPolicy::default();
        assert!(policy.allows(ActionSource::Instance));
        assert!(!policy.allows(ActionSource::Upstream));
        assert!(ActionsPolicy::default()
            .with_sources("ci.actions_sources", &["github".into()])
            .unwrap_err()
            .contains("ci.actions_sources"));
        assert!(ActionsPolicy::default()
            .with_upstream_url("ci.actions_upstream_url", "http://example.com")
            .is_err());
        assert_eq!(
            ActionsPolicy::default()
                .with_upstream_url("k", "https://gitea.example/")
                .unwrap()
                .upstream_url(),
            "https://gitea.example"
        );
    }
}
