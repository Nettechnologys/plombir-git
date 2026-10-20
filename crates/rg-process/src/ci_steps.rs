//! The step model: one Actions step, one process.
//!
//! A job translated from an Actions workflow is a list of [`StepSpec`]s, and a
//! runner executes them one by one. Each step gets its own process — on the
//! host, or through `docker exec` in the one container the job runs in — with
//! the shell it asked for, its own working directory, its own `env:` and its
//! own deadline. What a step leaves for the next one goes through the file
//! commands GitHub defines, read by the runner after the step exits:
//!
//! - `$GITHUB_ENV` — `NAME=value` lines (or the `NAME<<DELIMITER` form) that
//!   become variables of every later step;
//! - `$GITHUB_PATH` — directories prepended to `PATH` for every later step;
//! - `$GITHUB_STEP_SUMMARY` — Markdown the step wants shown; it is appended to
//!   the step's own log.
//!
//! Nothing else crosses: a variable a step `export`s, a directory it `cd`s
//! into, a shell option it sets, all end with it.
//!
//! Between two steps the runner reads the next one's `if:` with
//! [`crate::ci_condition::evaluate_step_condition`]: `success()` is "no earlier
//! step failed", `failure()` its opposite, `always()` runs regardless. A step
//! that fails with `continue_on_error` is recorded as failed (its *outcome*)
//! and counts as a success for the job and for the steps after it (its
//! *conclusion*), exactly the two words GitHub uses.
//!
//! Both runners — the one embedded in the server and `plombir-git-runner` —
//! execute through this module, so a step means the same thing wherever it
//! lands.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::ci_condition::{evaluate_step_condition, StepStatus};

/// Where a containerised job sees the runner's per-job directory of step
/// scripts and file commands.
///
/// Outside `/workspace` on purpose: GitHub keeps them in `RUNNER_TEMP`, and a
/// file the runner writes into the checkout would show up in every `git
/// status`, every artifact glob and every cache a job packs.
pub const CONTAINER_TEMP: &str = "/plombir-git/temp";

/// The header a runner sends with every poll to say which protocol it speaks.
pub const RUNNER_PROTOCOL_HEADER: &str = "x-plombir-git-runner-protocol";

/// The protocol in which a runner executes a job's [`StepSpec`]s one by one
/// and reports each of them. A runner that does not send at least this is
/// never handed a job of steps.
pub const STEPS_PROTOCOL: u32 = 2;

/// The protocol in which a runner also starts a job's `services:`
/// ([`crate::ci_services`]). A runner below it is never handed a job that
/// declares any: it would run the steps against services that are not there.
pub const SERVICES_PROTOCOL: u32 = 3;

/// The protocol in which a runner hands each step only the secrets that step
/// names ([`StepSpec::secrets`], [`JobContext::secrets`]). A runner below it
/// would give every step all the secrets the job was given — a downloaded
/// action's steps included — so it is never handed a job that reads any.
pub const SCOPED_SECRETS_PROTOCOL: u32 = 4;

/// What this build's runner speaks, and sends with every poll.
pub const RUNNER_PROTOCOL: u32 = SCOPED_SECRETS_PROTOCOL;

/// Where a containerised job's workspace is mounted.
pub const CONTAINER_WORKSPACE: &str = "/workspace";

/// The longest `$GITHUB_ENV` / `$GITHUB_PATH` / `$GITHUB_STEP_SUMMARY` file a
/// step may leave. GitHub caps the summary at 1 MiB; the same ceiling keeps a
/// runaway `>>` loop from being read into the runner's memory whole.
pub const MAX_FILE_COMMAND_BYTES: u64 = 1024 * 1024;

/// One step of a job, as the translation hands it to a runner.
///
/// Every value is final: `${{ }}` expressions that have a value before the job
/// starts were substituted when the workflow was read. The `if:` is the one
/// field read at run time, because it can ask how the earlier steps went.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepSpec {
    /// What the step is called in the log and the UI.
    pub name: String,
    /// The step's `id:`, when it declares one.
    #[serde(default)]
    pub id: Option<String>,
    /// The script.
    pub run: String,
    /// The `shell:` as written, or the job's / workflow's `defaults.run.shell`.
    /// `None` is GitHub's unspecified shell: `bash -e`, or `sh -e` where there
    /// is no bash. Checked by [`Shell::parse`] when the workflow is read.
    #[serde(default)]
    pub shell: Option<String>,
    /// Relative to the workspace.
    #[serde(default)]
    pub working_directory: Option<String>,
    /// The step's own `env:`, already resolved.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub continue_on_error: bool,
    /// The step's `timeout-minutes:`.
    #[serde(default)]
    pub timeout_minutes: Option<u64>,
    /// The step's `if:`, read between steps. `None` is `success()`.
    #[serde(default)]
    pub condition: Option<String>,
    /// The secrets this step's process gets, by name: those the step's own
    /// text names, those the job's and the workflow's `env:` name, and — for a
    /// step of an action — only what the workflow's step passed the action
    /// in `with:` / `env:`. Every other secret of [`JobContext::secrets`] is
    /// left out of its environment. Empty, and absent from the JSON, when the
    /// step may read none.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub secrets: BTreeSet<String>,
}

/// The shells a step can ask for, and how each one is started — GitHub's own
/// command lines for each name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shell {
    /// No `shell:` at all: `bash -e {0}`, falling back to `sh -e {0}` where
    /// bash is not installed.
    Unspecified,
    /// `bash --noprofile --norc -eo pipefail {0}`.
    Bash,
    /// `sh -e {0}`.
    Sh,
    /// `python {0}`.
    Python,
    /// `pwsh -command ". '{0}'"`.
    Pwsh,
    /// A command line of the author's own with `{0}` where the script goes.
    Custom(Vec<String>),
}

impl Shell {
    /// Read a `shell:` value, or say why it cannot be honoured.
    pub fn parse(declared: Option<&str>) -> Result<Self, String> {
        let Some(declared) = declared else {
            return Ok(Self::Unspecified);
        };
        match declared.trim() {
            "bash" => Ok(Self::Bash),
            "sh" => Ok(Self::Sh),
            "python" => Ok(Self::Python),
            "pwsh" => Ok(Self::Pwsh),
            "cmd" | "powershell" => Err(format!(
                "shell `{declared}` is a Windows shell, and Plombir Git runs steps on Linux; use \
                 `pwsh` for PowerShell Core"
            )),
            custom if custom.contains("{0}") => {
                let argv = custom
                    .split_whitespace()
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                if argv.first().is_none_or(|program| program.contains("{0}")) {
                    return Err(format!(
                        "shell `{declared}` must start with the program that runs the script"
                    ));
                }
                Ok(Self::Custom(argv))
            }
            other => Err(format!(
                "shell `{other}` is neither bash, sh, python nor pwsh, and has no `{{0}}` to say \
                 where the script goes"
            )),
        }
    }

    /// The extension the script file is written with: `python` and `pwsh`
    /// look at it.
    fn extension(&self) -> &'static str {
        match self {
            Self::Python => "py",
            Self::Pwsh => "ps1",
            _ => "sh",
        }
    }

    /// The command line that runs `script`, a path as the step's process sees
    /// it.
    pub fn argv(&self, script: &str) -> Vec<String> {
        let owned = |parts: &[&str]| parts.iter().map(|part| part.to_string()).collect();
        match self {
            Self::Unspecified => {
                let mut argv: Vec<String> = owned(&[
                    "sh",
                    "-c",
                    "if command -v bash >/dev/null 2>&1; then exec bash -e \"$1\"; fi; exec sh -e \"$1\"",
                    "sh",
                ]);
                argv.push(script.to_string());
                argv
            }
            Self::Bash => {
                let mut argv: Vec<String> =
                    owned(&["bash", "--noprofile", "--norc", "-eo", "pipefail"]);
                argv.push(script.to_string());
                argv
            }
            Self::Sh => vec!["sh".into(), "-e".into(), script.into()],
            Self::Python => vec!["python".into(), script.into()],
            Self::Pwsh => vec!["pwsh".into(), "-command".into(), format!(". '{script}'")],
            Self::Custom(argv) => argv
                .iter()
                .map(|part| part.replace("{0}", script))
                .collect(),
        }
    }
}

/// How a step ended, in GitHub's vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepOutcome {
    Success,
    Failure,
    /// Its `if:` said no.
    Skipped,
    /// The job was canceled before or while it ran.
    Cancelled,
}

impl StepOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }
}

/// What one step left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepResult {
    /// 1-based, in the order the workflow lists them.
    pub number: usize,
    pub name: String,
    /// What happened to the step itself.
    pub outcome: StepOutcome,
    /// What it counts as for the job: a failure under `continue-on-error` is
    /// a success here.
    pub conclusion: StepOutcome,
    pub exit_code: Option<i32>,
    /// Masked. Includes the step's summary, if it wrote one.
    pub log: String,
    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
    /// What the step wrote to `$GITHUB_OUTPUT` — unmasked, because a later
    /// step reads it as written; it leaves the job only through the job's own
    /// `outputs:`, which are masked.
    pub outputs: BTreeMap<String, String>,
}

/// What a whole job of steps left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepsResult {
    /// `0` when every step concluded in success or was skipped; otherwise the
    /// exit code of the first step that failed the job (`-1` when it never got
    /// one — a timeout, or a step that could not be started).
    pub exit_code: i32,
    /// Every step's log under a header naming it, capped to the job log size.
    pub log: String,
    pub steps: Vec<StepResult>,
    /// The job's `outputs:`, resolved after its last step and masked. Empty
    /// when the job declared none — or when they could not be produced, in
    /// which case the job failed saying why.
    pub outputs: BTreeMap<String, String>,
}

/// One job a job `needs:`, as its steps see it: `needs.<id>.result` and
/// `needs.<id>.outputs.<name>`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NeededJob {
    /// `success`, `failure`, `cancelled` or `skipped`.
    pub result: String,
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
}

/// The largest total of `$GITHUB_OUTPUT` values one job may write, and of its
/// own `outputs:` — GitHub's 1 MiB.
pub const MAX_JOB_OUTPUT_BYTES: usize = 1024 * 1024;

/// A context only a runner has, between two steps: what the earlier steps of
/// the job left, and what the jobs it needs left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeKey {
    /// `steps.<id>.outputs.<name>`
    StepOutput { step: String, name: String },
    /// `steps.<id>.outcome`
    StepOutcome(String),
    /// `steps.<id>.conclusion`
    StepConclusion(String),
    /// `needs.<job>.outputs.<name>`
    NeedsOutput { job: String, name: String },
    /// `needs.<job>.result`
    NeedsResult(String),
}

/// A step id, an output name or a job id as GitHub accepts them: a letter or
/// `_`, then letters, digits, `-` and `_`.
pub fn valid_runtime_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
}

impl RuntimeKey {
    /// Read one `${{ … }}` key, or `None` when it is not a runtime context.
    pub fn parse(key: &str) -> Option<Self> {
        let parts = key.split('.').collect::<Vec<_>>();
        let named = |names: &[&str]| names.iter().all(|name| valid_runtime_name(name));
        match parts.as_slice() {
            ["steps", step, "outputs", name] if named(&[step, name]) => Some(Self::StepOutput {
                step: step.to_string(),
                name: name.to_string(),
            }),
            ["steps", step, "outcome"] if named(&[step]) => {
                Some(Self::StepOutcome(step.to_string()))
            }
            ["steps", step, "conclusion"] if named(&[step]) => {
                Some(Self::StepConclusion(step.to_string()))
            }
            ["needs", job, "outputs", name] if named(&[job, name]) => Some(Self::NeedsOutput {
                job: job.to_string(),
                name: name.to_string(),
            }),
            ["needs", job, "result"] if named(&[job]) => Some(Self::NeedsResult(job.to_string())),
            _ => None,
        }
    }
}

/// What an earlier step of the job left, by its id.
#[derive(Debug, Clone)]
struct StepState {
    outcome: StepOutcome,
    conclusion: StepOutcome,
    outputs: BTreeMap<String, String>,
}

/// What the runtime contexts answer at one moment of a job.
struct RuntimeContext<'a> {
    steps: &'a HashMap<String, StepState>,
    needs: &'a BTreeMap<String, NeededJob>,
}

impl RuntimeContext<'_> {
    fn value(&self, key: &RuntimeKey) -> String {
        match key {
            RuntimeKey::StepOutput { step, name } => self
                .steps
                .get(step)
                .and_then(|state| state.outputs.get(name))
                .cloned()
                .unwrap_or_default(),
            RuntimeKey::StepOutcome(step) => self
                .steps
                .get(step)
                .map(|state| state.outcome.as_str().to_string())
                .unwrap_or_default(),
            RuntimeKey::StepConclusion(step) => self
                .steps
                .get(step)
                .map(|state| state.conclusion.as_str().to_string())
                .unwrap_or_default(),
            RuntimeKey::NeedsOutput { job, name } => self
                .needs
                .get(job)
                .and_then(|needed| needed.outputs.get(name))
                .cloned()
                .unwrap_or_default(),
            RuntimeKey::NeedsResult(job) => self
                .needs
                .get(job)
                .map(|needed| needed.result.clone())
                .unwrap_or_default(),
        }
    }

    /// Replace every `${{ <runtime key> }}` in `text`, as GitHub does: the
    /// value goes in as text. Anything else between `${{` and `}}` was
    /// resolved — or refused — when the workflow was read, and is left alone.
    fn resolve(&self, text: &str) -> String {
        if !text.contains("${{") {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find("${{") {
            let after = &rest[open + 3..];
            let Some(close) = after.find("}}") else {
                break;
            };
            out.push_str(&rest[..open]);
            match RuntimeKey::parse(after[..close].trim()) {
                Some(key) => out.push_str(&self.value(&key)),
                None => out.push_str(&rest[open..open + 3 + close + 2]),
            }
            rest = &after[close + 2..];
        }
        out.push_str(rest);
        out
    }

    /// The runtime keys a step condition reads, by their spelling.
    fn condition_entries(&self) -> Vec<(String, String)> {
        let mut entries = Vec::new();
        for (id, state) in self.steps {
            entries.push((format!("steps.{id}.outcome"), state.outcome.as_str().into()));
            entries.push((
                format!("steps.{id}.conclusion"),
                state.conclusion.as_str().into(),
            ));
            for (name, value) in &state.outputs {
                entries.push((format!("steps.{id}.outputs.{name}"), value.clone()));
            }
        }
        for (job, needed) in self.needs {
            entries.push((format!("needs.{job}.result"), needed.result.clone()));
            for (name, value) in &needed.outputs {
                entries.push((format!("needs.{job}.outputs.{name}"), value.clone()));
            }
        }
        entries
    }

    /// `step` with every runtime expression in what it runs resolved.
    fn resolve_step(&self, step: &StepSpec) -> StepSpec {
        let mut resolved = step.clone();
        resolved.run = self.resolve(&step.run);
        resolved.working_directory = step
            .working_directory
            .as_deref()
            .map(|directory| self.resolve(directory));
        resolved.env = step
            .env
            .iter()
            .map(|(name, value)| (name.clone(), self.resolve(value)))
            .collect();
        resolved
    }
}

/// Where the steps run.
pub enum StepBackend<'a> {
    /// A process on this machine, in the workspace.
    Local,
    /// `docker exec` into the job's container, already running.
    Docker {
        program: &'a std::ffi::OsStr,
        container: &'a str,
        /// The container's own `PATH`, which `$GITHUB_PATH` entries are put in
        /// front of.
        path: String,
    },
}

/// Told about each step as it starts and as it ends, so a runner can show
/// progress while the job is still running.
pub trait StepReporter {
    fn started(&mut self, number: usize) -> impl std::future::Future<Output = ()>;
    fn finished(&mut self, result: &StepResult) -> impl std::future::Future<Output = ()>;
}

/// A reporter for a caller that only wants the result.
pub struct Silent;

impl StepReporter for Silent {
    async fn started(&mut self, _number: usize) {}
    async fn finished(&mut self, _result: &StepResult) {}
}

/// Everything one job's steps run with.
pub struct StepRun<'a> {
    pub steps: &'a [StepSpec],
    /// The job's environment: the runner's own variables, the repository's
    /// secrets, the job's declared variables — and, on the host, `PATH`,
    /// `LANG` and `HOME`.
    pub job_env: &'a [(String, String)],
    /// The workspace on this machine.
    pub workspace: &'a Path,
    /// A directory on this machine for this job's step scripts and file
    /// commands; created by the caller, emptied and removed by it.
    pub temp: &'a Path,
    pub backend: StepBackend<'a>,
    /// Applied to every step log before it leaves this module.
    pub mask: &'a (dyn Fn(&str) -> String + Sync),
    /// How long one minute of `timeout-minutes` is: [`MINUTE`], except in a
    /// test that cannot wait for one.
    pub minute: std::time::Duration,
    /// The jobs this one `needs:`, by the id its expressions name them with.
    pub needs: &'a BTreeMap<String, NeededJob>,
    /// The job's `outputs:`, as templates over `steps.*` and `needs.*`.
    pub outputs: &'a BTreeMap<String, String>,
    /// [`JobContext::secrets`]: the names in `job_env` that are secrets, of
    /// which each step gets only its own. `None` hides none.
    pub secrets: Option<&'a BTreeSet<String>>,
}

/// One minute of a step's `timeout-minutes`.
pub const MINUTE: std::time::Duration = std::time::Duration::from_secs(60);

/// The names a step's process gets from the runner, never from the job.
const FILE_COMMANDS: [&str; 4] = [
    "GITHUB_ENV",
    "GITHUB_PATH",
    "GITHUB_STEP_SUMMARY",
    "GITHUB_OUTPUT",
];

/// Variables a `$GITHUB_ENV` line may not set: the runner's own, and the two
/// that `$GITHUB_PATH` and the runner answer for.
fn reserved_for_the_runner(name: &str) -> bool {
    crate::job_environment::RUNNER_OWNED_VARIABLES.contains(&name)
        || FILE_COMMANDS.contains(&name)
        || matches!(
            name,
            "PATH" | "HOME" | "GITHUB_WORKSPACE" | "RUNNER_TEMP" | "GITHUB_ACTIONS"
        )
}

/// Parse a `$GITHUB_ENV` file: `NAME=value` lines, and the
/// `NAME<<DELIMITER` … `DELIMITER` form for a value with newlines.
pub fn parse_env_file(text: &str) -> Result<Vec<(String, String)>, String> {
    parse_file_command(text, |name| {
        if !crate::job_environment::valid_environment_name(name) {
            return Err(format!("`{name}` is not a valid variable name"));
        }
        if reserved_for_the_runner(name) {
            return Err(format!(
                "`{name}` is set by the runner and cannot be changed by a step{}",
                if name == "PATH" {
                    "; append a directory to $GITHUB_PATH instead"
                } else {
                    ""
                }
            ));
        }
        Ok(())
    })
}

/// Parse a `$GITHUB_OUTPUT` file — the format of `$GITHUB_ENV`, with GitHub's
/// rule for an output's name.
pub fn parse_output_file(text: &str) -> Result<Vec<(String, String)>, String> {
    parse_file_command(text, |name| {
        if valid_runtime_name(name) {
            Ok(())
        } else {
            Err(format!(
                "`{name}` is not a valid output name (a letter or `_`, then letters, digits, \
                 `-` and `_`)"
            ))
        }
    })
}

fn parse_file_command(
    text: &str,
    check: impl Fn(&str) -> Result<(), String>,
) -> Result<Vec<(String, String)>, String> {
    let mut pairs = Vec::new();
    let mut lines = text.lines().enumerate();
    while let Some((index, line)) = lines.next() {
        if line.trim().is_empty() {
            continue;
        }
        let (name, value) = if let Some((name, delimiter)) = line
            .split_once("<<")
            .filter(|(name, _)| !name.contains('='))
        {
            if delimiter.is_empty() {
                return Err(format!("line {}: `{name}<<` names no delimiter", index + 1));
            }
            let mut value = Vec::new();
            let mut closed = false;
            for (_, line) in lines.by_ref() {
                if line == delimiter {
                    closed = true;
                    break;
                }
                value.push(line);
            }
            if !closed {
                return Err(format!(
                    "line {}: the value of `{name}` is never closed by `{delimiter}`",
                    index + 1
                ));
            }
            (name.to_string(), value.join("\n"))
        } else if let Some((name, value)) = line.split_once('=') {
            (name.to_string(), value.to_string())
        } else {
            return Err(format!(
                "line {}: expected `NAME=value` or `NAME<<DELIMITER`",
                index + 1
            ));
        };
        check(&name).map_err(|reason| format!("line {}: {reason}", index + 1))?;
        pairs.push((name, value));
    }
    Ok(pairs)
}

/// The `github.*` and `env.*` a step condition reads, from the environment its
/// process would get.
///
/// `github.*` comes from the runner's own `CI_*` variables — the same values
/// the translation-time context gives a job condition — and `env.*` from every
/// variable that is not the runner's.
pub fn step_condition_context(env: &[(String, String)]) -> HashMap<String, String> {
    let value = |name: &str| {
        env.iter()
            .rev()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
            .unwrap_or_default()
    };
    let ref_name = value("CI_REF");
    let short_ref = ref_name
        .strip_prefix("refs/heads/")
        .or_else(|| ref_name.strip_prefix("refs/tags/"))
        .unwrap_or(&ref_name)
        .to_string();
    let mut context = HashMap::from([
        ("github.ref".to_string(), ref_name.clone()),
        ("github.ref_name".to_string(), short_ref),
        ("github.event_name".to_string(), value("CI_EVENT")),
        ("github.sha".to_string(), value("CI_SHA")),
        ("github.repository".to_string(), value("CI_REPOSITORY")),
        (
            "github.repository_owner".to_string(),
            value("CI_REPOSITORY_OWNER"),
        ),
        (
            "github.event.schedule".to_string(),
            value("CI_EVENT_SCHEDULE"),
        ),
    ]);
    for (name, value) in env {
        if !reserved_for_the_runner(name) && !matches!(name.as_str(), "LANG") {
            context.insert(format!("env.{name}"), value.clone());
        }
    }
    context
}

/// Read a file command a step wrote, refusing one over the size cap.
fn read_file_command(path: &Path, name: &str) -> Result<String, String> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(error) => return Err(format!("${name} could not be read: {error}")),
    };
    if metadata.len() > MAX_FILE_COMMAND_BYTES {
        return Err(format!(
            "${name} is {} bytes, more than the {MAX_FILE_COMMAND_BYTES} a step may write",
            metadata.len()
        ));
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("${name} could not be read: {error}"))?;
    String::from_utf8(bytes).map_err(|_| format!("${name} is not UTF-8"))
}

/// Paths a step's process sees: the host path for a local step, the mount
/// point for a containerised one.
struct StepPaths {
    script: String,
    env: String,
    path: String,
    summary: String,
    output: String,
    workspace: String,
    temp: String,
}

/// The working directory a step runs in, relative to the workspace, refused
/// when it would leave it.
fn working_directory(declared: Option<&str>) -> Result<Option<&str>, String> {
    let Some(declared) = declared else {
        return Ok(None);
    };
    let path = Path::new(declared);
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "working-directory `{declared}` must stay inside the workspace"
        ));
    }
    Ok(Some(declared))
}

/// Run every step of a job and report each one as it goes.
pub async fn run_steps(run: &StepRun<'_>, reporter: &mut impl StepReporter) -> StepsResult {
    let mut results = Vec::with_capacity(run.steps.len());
    let mut status = StepStatus::default();
    let mut exit_code = 0;
    // What earlier steps left: `$GITHUB_ENV` pairs in the order they were
    // written, and `$GITHUB_PATH` entries newest first.
    let mut carried_env: Vec<(String, String)> = Vec::new();
    let mut carried_path: Vec<String> = Vec::new();
    // What each step with an `id:` left, for `steps.<id>.*`.
    let mut states: HashMap<String, StepState> = HashMap::new();
    // `$GITHUB_OUTPUT` bytes written so far, against the job's 1 MiB.
    let mut output_bytes = 0usize;

    for (index, declared) in run.steps.iter().enumerate() {
        let number = index + 1;
        let runtime = RuntimeContext {
            steps: &states,
            needs: run.needs,
        };
        let resolved = runtime.resolve_step(declared);
        let step = &resolved;
        let env = step_environment(run, step, &carried_env, &carried_path, number);
        let condition = step.condition.as_deref().unwrap_or("success()");
        let mut context = step_condition_context(&env);
        context.extend(runtime.condition_entries());
        let runs = match evaluate_step_condition(condition, &context, status) {
            Ok(runs) => runs,
            Err(error) => {
                let result = StepResult {
                    number,
                    name: step.name.clone(),
                    outcome: StepOutcome::Failure,
                    conclusion: StepOutcome::Failure,
                    exit_code: None,
                    log: format!(
                        "[plombir-git-runner] the step's if: could not be read: {error:#}\n"
                    ),
                    started_at: None,
                    finished_at: None,
                    outputs: BTreeMap::new(),
                };
                reporter.finished(&result).await;
                status.failed = true;
                if exit_code == 0 {
                    exit_code = -1;
                }
                remember(&mut states, step, &result);
                results.push(result);
                continue;
            }
        };
        if !runs {
            let result = StepResult {
                number,
                name: step.name.clone(),
                outcome: StepOutcome::Skipped,
                conclusion: StepOutcome::Skipped,
                exit_code: None,
                log: String::new(),
                started_at: None,
                finished_at: None,
                outputs: BTreeMap::new(),
            };
            reporter.finished(&result).await;
            remember(&mut states, step, &result);
            results.push(result);
            continue;
        }

        reporter.started(number).await;
        let started_at = SystemTime::now();
        let (code, log, file_commands) = execute_step(run, step, number, &env).await;
        let finished_at = SystemTime::now();

        let mut log = log;
        let mut failed = code != Some(0);
        let mut outputs = BTreeMap::new();
        if let Some(line) = log
            .lines()
            .find(|line| line.trim_start().starts_with("::set-output "))
            .map(str::to_string)
        {
            push_line(
                &mut log,
                &format!(
                    "[plombir-git-runner] `{}` uses the `::set-output` workflow command, which \
                     GitHub disabled; write `name=value` to $GITHUB_OUTPUT instead",
                    line.trim()
                ),
            );
            failed = true;
        }
        if !failed {
            match file_commands.and_then(|commands| {
                within_output_budget(&commands.outputs, &mut output_bytes).map(|()| commands)
            }) {
                Ok(FileCommands {
                    env: written,
                    path,
                    summary,
                    outputs: written_outputs,
                }) => {
                    outputs = written_outputs;
                    carried_env.extend(written);
                    for directory in path.into_iter().rev() {
                        carried_path.insert(0, directory);
                    }
                    if !summary.trim().is_empty() {
                        push_line(&mut log, "[plombir-git-runner] step summary:");
                        log.push_str(&summary);
                        if !log.ends_with('\n') {
                            log.push('\n');
                        }
                    }
                }
                Err(reason) => {
                    push_line(
                        &mut log,
                        &format!(
                            "[plombir-git-runner] the step's file commands were refused: {reason}"
                        ),
                    );
                    failed = true;
                }
            }
        }
        let outcome = if failed {
            StepOutcome::Failure
        } else {
            StepOutcome::Success
        };
        let conclusion = if failed && step.continue_on_error {
            push_line(
                &mut log,
                "[plombir-git-runner] the step failed; continue-on-error lets the job go on",
            );
            StepOutcome::Success
        } else {
            outcome
        };
        if conclusion == StepOutcome::Failure {
            status.failed = true;
            if exit_code == 0 {
                exit_code = code.filter(|code| *code != 0).unwrap_or(-1);
            }
        }
        let result = StepResult {
            number,
            name: step.name.clone(),
            outcome,
            conclusion,
            exit_code: code,
            log: (run.mask)(&log),
            started_at: Some(started_at),
            finished_at: Some(finished_at),
            outputs,
        };
        reporter.finished(&result).await;
        remember(&mut states, step, &result);
        results.push(result);
    }

    let mut log = combined_log(&results, run.steps.len());
    let runtime = RuntimeContext {
        steps: &states,
        needs: run.needs,
    };
    let mut outputs = BTreeMap::new();
    let mut total = 0usize;
    for (name, template) in run.outputs {
        let value = (run.mask)(&runtime.resolve(template));
        total += name.len() + value.len();
        if total > MAX_JOB_OUTPUT_BYTES {
            push_line(
                &mut log,
                &format!(
                    "[plombir-git-runner] the job's outputs pass {MAX_JOB_OUTPUT_BYTES} bytes at \
                     `{name}`; none of them are kept"
                ),
            );
            outputs.clear();
            if exit_code == 0 {
                exit_code = -1;
            }
            break;
        }
        outputs.insert(name.clone(), value);
    }

    StepsResult {
        exit_code,
        log,
        steps: results,
        outputs,
    }
}

/// Keep what a step with an `id:` left, for the steps after it.
fn remember(states: &mut HashMap<String, StepState>, step: &StepSpec, result: &StepResult) {
    if let Some(id) = &step.id {
        states.insert(
            id.clone(),
            StepState {
                outcome: result.outcome,
                conclusion: result.conclusion,
                outputs: result.outputs.clone(),
            },
        );
    }
}

/// Count a step's outputs against the job's budget, naming the key that
/// crosses it — an output is refused whole rather than cut.
fn within_output_budget(
    outputs: &BTreeMap<String, String>,
    used: &mut usize,
) -> Result<(), String> {
    for (name, value) in outputs {
        *used += name.len() + value.len();
        if *used > MAX_JOB_OUTPUT_BYTES {
            return Err(format!(
                "$GITHUB_OUTPUT `{name}` takes the job's outputs past {MAX_JOB_OUTPUT_BYTES} bytes"
            ));
        }
    }
    Ok(())
}

fn push_line(log: &mut String, line: &str) {
    if !log.is_empty() && !log.ends_with('\n') {
        log.push('\n');
    }
    log.push_str(line);
    log.push('\n');
}

/// The environment one step's process gets, lowest precedence first: the
/// job's, then what earlier steps wrote to `$GITHUB_ENV`, then the step's own
/// `env:`, then the runner's file-command paths and the `PATH` with every
/// `$GITHUB_PATH` entry in front.
///
/// A secret of the job the step does not name ([`StepSpec::secrets`]) is not
/// in it, and no `${NAME}` in it reads one.
fn step_environment(
    run: &StepRun<'_>,
    step: &StepSpec,
    carried_env: &[(String, String)],
    carried_path: &[String],
    number: usize,
) -> Vec<(String, String)> {
    let paths = step_paths(run, number, step);
    let hidden = |name: &str| {
        run.secrets
            .is_some_and(|secrets| secrets.contains(name) && !step.secrets.contains(name))
    };
    // A job variable may read what a needed job left (`env: VERSION:
    // ${{ needs.build.outputs.version }}`); nothing else in it is a runtime
    // expression.
    let needs_only = RuntimeContext {
        steps: &HashMap::new(),
        needs: run.needs,
    };
    let job_env: BTreeMap<String, String> = run
        .job_env
        .iter()
        .filter(|(name, _)| !hidden(name))
        .map(|(name, value)| (name.clone(), needs_only.resolve(value)))
        .collect();
    // What `${NAME}` in a declared value reads: what the step may see.
    let mut visible = job_env.clone();
    visible.extend(carried_env.iter().cloned());
    let mut env: BTreeMap<String, String> = job_env
        .iter()
        .map(|(name, value)| {
            // A secret's own value is data, never a template; a job that
            // predates scoped secrets cannot tell its secrets from its
            // variables, and keeps its values as they were.
            let declared = run
                .secrets
                .is_some_and(|secrets| !secrets.contains(name) && !reserved_for_the_runner(name));
            let value = if declared {
                expand_placeholders(value, &visible)
            } else {
                value.clone()
            };
            (name.clone(), value)
        })
        .collect();
    env.extend(carried_env.iter().cloned());
    env.extend(
        step.env
            .iter()
            .map(|(name, value)| (name.clone(), expand_placeholders(value, &visible))),
    );
    let base_path = match &run.backend {
        StepBackend::Local => env.get("PATH").cloned().unwrap_or_default(),
        StepBackend::Docker { path, .. } => path.clone(),
    };
    if !carried_path.is_empty() {
        let mut path = carried_path.join(":");
        if !base_path.is_empty() {
            path.push(':');
            path.push_str(&base_path);
        }
        env.insert("PATH".into(), path);
    } else if matches!(run.backend, StepBackend::Docker { .. }) {
        // The container keeps its own `PATH`; the host's never reaches it.
        env.remove("PATH");
    }
    env.insert("GITHUB_ENV".into(), paths.env);
    env.insert("GITHUB_PATH".into(), paths.path);
    env.insert("GITHUB_STEP_SUMMARY".into(), paths.summary);
    env.insert("GITHUB_OUTPUT".into(), paths.output);
    env.insert("GITHUB_WORKSPACE".into(), paths.workspace);
    env.insert("RUNNER_TEMP".into(), paths.temp);
    env.insert("GITHUB_ACTIONS".into(), "true".into());
    env.into_iter().collect()
}

/// Replace each `${NAME}` in a declared value — what `${{ secrets.NAME }}`,
/// `${{ matrix.x }}` and `${{ inputs.x }}` became when the workflow was read —
/// with what `NAME` holds in `visible`, empty when it holds nothing: what the
/// shell did with the same text when steps were one script. Only names the
/// translation writes, `[A-Z_][A-Z0-9_]*`, are read; any other `$` is text.
fn expand_placeholders(value: &str, visible: &BTreeMap<String, String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(open) = rest.find("${") {
        let after = &rest[open + 2..];
        let name_len = after
            .bytes()
            .take_while(|byte| matches!(byte, b'A'..=b'Z' | b'0'..=b'9' | b'_'))
            .count();
        let named = after[..name_len]
            .bytes()
            .next()
            .is_some_and(|first| !first.is_ascii_digit());
        if named && after[name_len..].starts_with('}') {
            out.push_str(&rest[..open]);
            if let Some(found) = visible.get(&after[..name_len]) {
                out.push_str(found);
            }
            rest = &after[name_len + 1..];
        } else {
            out.push_str(&rest[..open + 2]);
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

fn step_paths(run: &StepRun<'_>, number: usize, step: &StepSpec) -> StepPaths {
    let extension = Shell::parse(step.shell.as_deref())
        .map(|shell| shell.extension())
        .unwrap_or("sh");
    let (temp, workspace) = match run.backend {
        StepBackend::Local => (
            run.temp.to_string_lossy().into_owned(),
            run.workspace.to_string_lossy().into_owned(),
        ),
        StepBackend::Docker { .. } => (CONTAINER_TEMP.to_string(), CONTAINER_WORKSPACE.to_string()),
    };
    StepPaths {
        script: format!("{temp}/step-{number}.{extension}"),
        env: format!("{temp}/env-{number}"),
        path: format!("{temp}/path-{number}"),
        summary: format!("{temp}/summary-{number}"),
        output: format!("{temp}/output-{number}"),
        workspace,
        temp,
    }
}

struct FileCommands {
    env: Vec<(String, String)>,
    path: Vec<String>,
    summary: String,
    outputs: BTreeMap<String, String>,
}

/// Run one step to its end. Returns its exit code (`None` when it never
/// started or ran out of time), its log, and what it wrote to its file
/// commands.
async fn execute_step(
    run: &StepRun<'_>,
    step: &StepSpec,
    number: usize,
    env: &[(String, String)],
) -> (Option<i32>, String, Result<FileCommands, String>) {
    let no_commands = || Err(String::new());
    let shell = match Shell::parse(step.shell.as_deref()) {
        Ok(shell) => shell,
        Err(reason) => {
            return (
                None,
                format!("[plombir-git-runner] {reason}\n"),
                no_commands(),
            )
        }
    };
    let directory = match working_directory(step.working_directory.as_deref()) {
        Ok(directory) => directory,
        Err(reason) => {
            return (
                None,
                format!("[plombir-git-runner] {reason}\n"),
                no_commands(),
            )
        }
    };
    let host = |name: &str| run.temp.join(name);
    let extension = shell.extension();
    if let Err(error) = std::fs::write(host(&format!("step-{number}.{extension}")), &step.run) {
        return (
            None,
            format!("[plombir-git-runner] the step script could not be written: {error}\n"),
            no_commands(),
        );
    }
    for file in [
        format!("env-{number}"),
        format!("path-{number}"),
        format!("summary-{number}"),
        format!("output-{number}"),
    ] {
        if let Err(error) = std::fs::write(host(&file), b"") {
            return (
                None,
                format!(
                    "[plombir-git-runner] the step's file commands could not be created: {error}\n"
                ),
                no_commands(),
            );
        }
    }
    let paths = step_paths(run, number, step);
    let argv = shell.argv(&paths.script);
    let timeout = step.timeout_minutes.map(|minutes| {
        run.minute
            .saturating_mul(u32::try_from(minutes).unwrap_or(u32::MAX))
    });

    let (code, log) = match &run.backend {
        StepBackend::Local => {
            let cwd = match directory {
                Some(directory) => run.workspace.join(directory),
                None => run.workspace.to_path_buf(),
            };
            let mut command = tokio::process::Command::new(&argv[0]);
            command.args(&argv[1..]).current_dir(cwd).env_clear();
            for (name, value) in env {
                command.env(name, value);
            }
            let output = crate::output_ci_step_in_process_tree(&mut command);
            match timeout {
                // Dropping the output future kills the step's whole process
                // group, which is the timeout.
                Some(limit) => match tokio::time::timeout(limit, output).await {
                    Ok(output) => step_output(output),
                    Err(_) => (None, timed_out(step)),
                },
                None => step_output(output.await),
            }
        }
        StepBackend::Docker {
            program, container, ..
        } => {
            let workdir = match directory {
                Some(directory) => format!("{CONTAINER_WORKSPACE}/{directory}"),
                None => CONTAINER_WORKSPACE.to_string(),
            };
            let pid_file = format!("{CONTAINER_TEMP}/step-{number}.pid");
            // The step writes its own process id before it becomes the shell,
            // and `docker exec` makes it the leader of its own process group,
            // so a timeout can end the step and everything it started without
            // ending the container the next steps run in.
            let mut wrapped = vec![
                "sh".to_string(),
                "-c".to_string(),
                "echo $$ > \"$0\"; exec \"$@\"".to_string(),
                pid_file.clone(),
            ];
            wrapped.extend(argv);
            let mut command =
                crate::docker_job::exec_command(program, container, &workdir, env, &wrapped);
            let output = crate::output_ci_step_in_process_tree(&mut command);
            match timeout {
                Some(limit) => match tokio::time::timeout(limit, output).await {
                    Ok(output) => step_output(output),
                    Err(_) => {
                        crate::docker_job::kill_exec_group(program, container, &pid_file).await;
                        (None, timed_out(step))
                    }
                },
                None => step_output(output.await),
            }
        }
    };
    let commands = (|| {
        let written = read_file_command(&host(&format!("env-{number}")), "GITHUB_ENV")?;
        let env = parse_env_file(&written).map_err(|reason| format!("$GITHUB_ENV {reason}"))?;
        let path = read_file_command(&host(&format!("path-{number}")), "GITHUB_PATH")?
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        let summary =
            read_file_command(&host(&format!("summary-{number}")), "GITHUB_STEP_SUMMARY")?;
        let written = read_file_command(&host(&format!("output-{number}")), "GITHUB_OUTPUT")?;
        let outputs = parse_output_file(&written)
            .map_err(|reason| format!("$GITHUB_OUTPUT {reason}"))?
            .into_iter()
            .collect();
        Ok(FileCommands {
            env,
            path,
            summary,
            outputs,
        })
    })();
    (code, log, commands)
}

fn step_output(output: std::io::Result<crate::CappedJobOutput>) -> (Option<i32>, String) {
    match output {
        Ok(output) => (Some(output.status.code().unwrap_or(-1)), output.log()),
        Err(error) => (
            None,
            format!("[plombir-git-runner] the step could not be started: {error}\n"),
        ),
    }
}

fn timed_out(step: &StepSpec) -> String {
    format!(
        "[plombir-git-runner] the step ran out of its timeout-minutes: {} and was stopped\n",
        step.timeout_minutes.unwrap_or_default()
    )
}

/// The job log: every step's log under a header naming it, so the log of a
/// stepped job reads the way the log of a single-script job always has.
///
/// Capped to [`crate::MAX_JOB_LOG_BYTES`] the way one stream is — the first
/// part and the last part kept, the middle marked — because each step's own
/// log is already kept whole beside it.
fn combined_log(results: &[StepResult], total: usize) -> String {
    let mut log = String::new();
    for result in results {
        let verdict = match (result.outcome, result.conclusion) {
            (StepOutcome::Skipped, _) => "skipped".to_string(),
            (StepOutcome::Failure, StepOutcome::Success) => {
                "failed, continued on error".to_string()
            }
            (outcome, _) => match result.exit_code {
                Some(code) if outcome == StepOutcome::Failure => format!("failure (exit {code})"),
                _ => outcome.as_str().to_string(),
            },
        };
        push_line(
            &mut log,
            &format!(
                "▶ step {}/{total}: {} — {verdict}",
                result.number, result.name
            ),
        );
        log.push_str(&result.log);
    }
    cap_log(log, crate::MAX_JOB_LOG_BYTES)
}

fn cap_log(log: String, cap: usize) -> String {
    if log.len() <= cap {
        return log;
    }
    let head_end = floor_char_boundary(&log, cap / 4);
    let tail_start = ceil_char_boundary(&log, log.len() - (cap - cap / 4));
    format!(
        "{}\n[plombir-git-runner] {} bytes of the job log omitted here; each step keeps its own log whole\n{}",
        &log[..head_end],
        tail_start - head_end,
        &log[tail_start..]
    )
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn ceil_char_boundary(text: &str, mut index: usize) -> usize {
    while index < text.len() && !text.is_char_boundary(index) {
        index += 1;
    }
    index
}

/// The per-job directory of step scripts and file commands, beside the
/// workspace rather than in it.
pub fn job_temp_dir(workspace: &Path) -> PathBuf {
    let mut name = workspace
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(".steps");
    workspace.with_file_name(name)
}

/// The container a job of steps runs in, when it names an image.
pub struct StepsImage<'a> {
    /// The `docker` program both runners call.
    pub program: &'a std::ffi::OsStr,
    /// `--name`, so a timeout or a cancellation can remove it by name.
    pub name: &'a str,
    pub image: &'a str,
    pub user: Option<String>,
    pub limits: &'a crate::docker_job::JobLimits,
}

/// Removes the job's step directory when the job is done with it — including
/// when the job's future is dropped by a timeout or a cancellation.
struct TempGuard(PathBuf);

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %self.0.display(),
                    %error,
                    "failed to remove a job's step directory"
                );
            }
        }
    }
}

/// Run a whole job of steps: make its step directory beside the workspace,
/// start its container when it has an image, run every step, and remove both.
///
/// Returns the job's exit code and its log, the way a single-script job's
/// execution does, so a runner settles the job the same way whichever shape
/// it had. The container is removed here on the way out; a caller whose
/// future is dropped first (a timeout, a cancellation) removes it by name, as
/// it always has.
/// What a job of steps leaves for the runner to report: its exit code and log,
/// as a single-script job's execution does, and its `outputs:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRun {
    pub exit_code: i32,
    pub log: String,
    pub outputs: BTreeMap<String, String>,
}

/// The job-level half of a job of steps: what it needs and what it outputs.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobContext {
    /// The id each `needs:` entry is named by in the job's expressions → the
    /// job it is in the pipeline.
    #[serde(default)]
    pub needs: BTreeMap<String, String>,
    /// The job's `outputs:`: name → template over `steps.*` / `needs.*`.
    #[serde(default)]
    pub outputs: BTreeMap<String, String>,
    /// The job's `services:`, started before its first step
    /// ([`crate::ci_services`]). Absent from a job that declares none, so a row
    /// written before services existed reads the same.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub services: Vec<crate::ci_services::ServiceSpec>,
    /// Every secret the job names anywhere — its steps, its `env:`, its
    /// services, its container — and so all the job is handed: the server
    /// gives the job no other. Each step gets the part its
    /// [`StepSpec::secrets`] names. `None` — a job translated before secrets
    /// were scoped — is the old reading: every secret, to every step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<BTreeSet<String>>,
    /// What the job's `CI_JOB_TOKEN` may do, as `permissions:` left it — the
    /// space-separated scopes the server signs into it. `None` — a native job,
    /// or one translated before `permissions:` was read — keeps the native
    /// token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_scopes: Option<String>,
}

///
/// `services`, when the job declares any, are started first, on a network the
/// job's container joins; they are removed after the job's container, and the
/// job fails without running a step when one of them does not become healthy.
#[allow(clippy::too_many_arguments)]
pub async fn run_job_steps(
    steps: &[StepSpec],
    job_env: &[(String, String)],
    workspace: &Path,
    image: Option<StepsImage<'_>>,
    services: Option<crate::ci_services::ServicesRun<'_>>,
    mask: &(dyn Fn(&str) -> String + Sync),
    needs: &BTreeMap<String, NeededJob>,
    outputs: &BTreeMap<String, String>,
    secrets: Option<&BTreeSet<String>>,
    reporter: &mut impl StepReporter,
) -> JobRun {
    let failed = failed_job;
    let temp = job_temp_dir(workspace);
    if let Err(error) = std::fs::create_dir_all(&temp) {
        return failed(
            -1,
            format!(
                "[plombir-git-runner] the job's step directory `{}` could not be created: {error}",
                temp.display()
            ),
        );
    }
    let _guard = TempGuard(temp.clone());

    // What a killed runner on this machine left running — a steps container,
    // a service, a network — goes before this job adds its own.
    let docker = image.as_ref().map(|image| image.program).or_else(|| {
        services
            .as_ref()
            .filter(|services| !services.specs.is_empty())
            .map(|services| services.program)
    });
    if let Some(program) = docker {
        crate::ci_services::sweep_orphans(program).await;
    }

    // Before the job's container: it is started on the services' network.
    let services = match services.filter(|services| !services.specs.is_empty()) {
        None => None,
        Some(services) => match crate::ci_services::start_services(&services, job_env, &temp).await
        {
            Ok(started) => Some(started),
            Err(failed) => return failed_job(failed.exit_code, mask(&failed.log)),
        },
    };
    let network = services
        .as_ref()
        .map(crate::ci_services::StartedServices::network);
    let services_log = services
        .as_ref()
        .map(|started| started.log.clone())
        .unwrap_or_default();

    let Some(image) = image else {
        let result = run_steps(
            &StepRun {
                steps,
                job_env,
                workspace,
                temp: &temp,
                backend: StepBackend::Local,
                mask,
                minute: MINUTE,
                needs,
                outputs,
                secrets,
            },
            reporter,
        )
        .await;
        if let Some(services) = services {
            services.stop().await;
        }
        return JobRun {
            exit_code: result.exit_code,
            log: format!("{}{}", mask(&services_log), result.log),
            outputs: result.outputs,
        };
    };

    let job = crate::docker_job::JobContainer {
        name: image.name,
        image: image.image,
        script: "",
        workspace,
        user: image.user.clone(),
        variables: job_env,
        limits: image.limits,
        temp: Some(&temp),
        network: network.as_deref(),
    };
    let container = match crate::docker_job::start_steps_container(image.program, &job).await {
        Ok(container) => container,
        Err(failed) => {
            remove_container(image.program, image.name).await;
            if let Some(services) = services {
                services.stop().await;
            }
            return JobRun {
                exit_code: failed.exit_code,
                log: mask(&format!("{services_log}{}", failed.log)),
                outputs: BTreeMap::new(),
            };
        }
    };
    let result = run_steps(
        &StepRun {
            steps,
            job_env,
            workspace,
            temp: &temp,
            backend: StepBackend::Docker {
                program: image.program,
                container: image.name,
                path: container.path,
            },
            mask,
            minute: MINUTE,
            needs,
            outputs,
            secrets,
        },
        reporter,
    )
    .await;
    remove_container(image.program, image.name).await;
    if let Some(services) = services {
        services.stop().await;
    }
    let mut log = mask(&services_log);
    log.push_str(&container.pull_log);
    if !log.is_empty() && !log.ends_with('\n') {
        log.push('\n');
    }
    if let Some(notice) = crate::docker_job::left_out_variables_notice(job_env) {
        push_line(&mut log, &format!("[plombir-git-runner] {notice}"));
    }
    log.push_str(&result.log);
    JobRun {
        exit_code: result.exit_code,
        log,
        outputs: result.outputs,
    }
}

fn failed_job(exit_code: i32, log: String) -> JobRun {
    JobRun {
        exit_code,
        log,
        outputs: BTreeMap::new(),
    }
}

/// `docker rm -f` a job's container, bounded: a daemon that does not answer
/// costs a pause, not the job.
async fn remove_container(program: &std::ffi::OsStr, name: &str) {
    let mut command = crate::docker_job::client(program);
    command.args(["rm", "-f", name]);
    match tokio::time::timeout(std::time::Duration::from_secs(10), command.output()).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            tracing::warn!(container = name, %error, "failed to remove a job's container")
        }
        Err(_) => tracing::warn!(container = name, "`docker rm` did not answer in time"),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn step(name: &str, run: &str) -> StepSpec {
        StepSpec {
            name: name.into(),
            id: None,
            run: run.into(),
            shell: None,
            working_directory: None,
            env: BTreeMap::new(),
            continue_on_error: false,
            timeout_minutes: None,
            condition: None,
            secrets: BTreeSet::new(),
        }
    }

    fn job_env() -> Vec<(String, String)> {
        let mut env = vec![
            ("CI".to_string(), "true".to_string()),
            ("CI_REF".to_string(), "refs/heads/main".to_string()),
            ("CI_EVENT".to_string(), "push".to_string()),
            ("DEPLOY".to_string(), "staging".to_string()),
        ];
        if let Ok(path) = std::env::var("PATH") {
            env.push(("PATH".to_string(), path));
        }
        env
    }

    #[derive(Default)]
    struct Recorder {
        events: Vec<String>,
    }

    impl StepReporter for Recorder {
        async fn started(&mut self, number: usize) {
            self.events.push(format!("start {number}"));
        }
        async fn finished(&mut self, result: &StepResult) {
            self.events.push(format!(
                "end {} {}/{}",
                result.number,
                result.outcome.as_str(),
                result.conclusion.as_str()
            ));
        }
    }

    async fn run(steps: &[StepSpec]) -> (StepsResult, Vec<String>, tempfile::TempDir) {
        run_with_minute(steps, MINUTE).await
    }

    async fn run_with_minute(
        steps: &[StepSpec],
        minute: std::time::Duration,
    ) -> (StepsResult, Vec<String>, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let temp = root.path().join("temp");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&temp).unwrap();
        let env = job_env();
        let mask = |log: &str| log.replace("hunter2", "***");
        let mut recorder = Recorder::default();
        let result = run_steps(
            &StepRun {
                steps,
                job_env: &env,
                workspace: &workspace,
                temp: &temp,
                backend: StepBackend::Local,
                mask: &mask,
                minute,
                needs: &BTreeMap::new(),
                outputs: &BTreeMap::new(),
                secrets: None,
            },
            &mut recorder,
        )
        .await;
        (result, recorder.events, root)
    }

    /// card_e42a6d0af271: a step sees the secrets it names and none of the
    /// job's others — a step of an action, which names none, sees none — while
    /// a job variable built from a secret the job's `env:` names reaches every
    /// step, as on GitHub. `${NAME}` in a declared value is filled from what
    /// the step may see, never from a secret it may not.
    #[tokio::test]
    async fn each_step_gets_only_the_secrets_it_names() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let temp = root.path().join("temp");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&temp).unwrap();
        let mut env = job_env();
        env.extend([
            ("ALPHA".to_string(), "alpha-value".to_string()),
            ("BRAVO".to_string(), "bravo-value".to_string()),
            ("SHARED".to_string(), "shared-value".to_string()),
            // A job variable from `env: FROM_JOB: ${{ secrets.SHARED }}`, and
            // one a needed job's output filled with a secret's placeholder.
            ("FROM_JOB".to_string(), "${SHARED}".to_string()),
            ("MATRIX_X".to_string(), "${BRAVO}".to_string()),
        ]);
        let secrets = BTreeSet::from(["ALPHA".into(), "BRAVO".into(), "SHARED".into()]);
        let print = "echo \"a=${ALPHA:-none} b=${BRAVO:-none} s=${SHARED:-none} \
                     j=$FROM_JOB m=$MATRIX_X t=${TOKEN:-none}\"";
        let mut workflow = step("Workflow", print);
        workflow.secrets = BTreeSet::from(["ALPHA".into(), "SHARED".into()]);
        workflow
            .env
            .insert("TOKEN".into(), "pre-${ALPHA}-${lower}-$ALPHA".into());
        let mut action = step("Action / Run", print);
        action.secrets = BTreeSet::from(["SHARED".into()]);
        let mask = |log: &str| log.to_string();
        let result = run_steps(
            &StepRun {
                steps: &[workflow, action],
                job_env: &env,
                workspace: &workspace,
                temp: &temp,
                backend: StepBackend::Local,
                mask: &mask,
                minute: MINUTE,
                needs: &BTreeMap::new(),
                outputs: &BTreeMap::new(),
                secrets: Some(&secrets),
            },
            &mut Recorder::default(),
        )
        .await;
        assert!(
            result.steps[0].log.contains(
                "a=alpha-value b=none s=shared-value j=shared-value m= \
                 t=pre-alpha-value-${lower}-$ALPHA"
            ),
            "{}",
            result.steps[0].log
        );
        assert!(
            result.steps[1]
                .log
                .contains("a=none b=none s=shared-value j=shared-value m= t=none"),
            "{}",
            result.steps[1].log
        );
    }

    /// A job translated before secrets were scoped carries no list, and every
    /// step keeps all of them, values untouched.
    #[test]
    fn a_job_without_a_secret_list_hides_nothing() {
        let env = vec![("ALPHA".to_string(), "${KEPT}".to_string())];
        let mask = |log: &str| log.to_string();
        let root = tempfile::tempdir().unwrap();
        let run = StepRun {
            steps: &[],
            job_env: &env,
            workspace: root.path(),
            temp: root.path(),
            backend: StepBackend::Local,
            mask: &mask,
            minute: MINUTE,
            needs: &BTreeMap::new(),
            outputs: &BTreeMap::new(),
            secrets: None,
        };
        let built = step_environment(&run, &step("S", "true"), &[], &[], 1);
        assert!(built.contains(&("ALPHA".to_string(), "${KEPT}".to_string())));
    }

    /// The card's acceptance: a failing step under `continue-on-error` lets
    /// the next one run and the job stay green; without it the next one is
    /// skipped and the job is red.
    #[tokio::test]
    async fn continue_on_error_decides_whether_the_job_goes_on() {
        let mut failing = step("Fail", "echo failing; exit 3");
        failing.continue_on_error = true;
        let steps = [
            step("One", "echo one"),
            failing,
            step("Three", "echo three"),
        ];
        let (result, events, _root) = run(&steps).await;
        assert_eq!(result.exit_code, 0, "{}", result.log);
        assert_eq!(
            events,
            [
                "start 1",
                "end 1 success/success",
                "start 2",
                "end 2 failure/success",
                "start 3",
                "end 3 success/success",
            ]
        );
        assert_eq!(result.steps[1].exit_code, Some(3));
        assert!(result.steps[2].log.contains("three"));

        let steps = [
            step("One", "echo one"),
            step("Fail", "echo failing; exit 3"),
            step("Three", "echo three"),
        ];
        let (result, events, _root) = run(&steps).await;
        assert_eq!(result.exit_code, 3, "the job fails with the step's code");
        assert_eq!(
            events,
            [
                "start 1",
                "end 1 success/success",
                "start 2",
                "end 2 failure/failure",
                "end 3 skipped/skipped",
            ]
        );
        assert!(
            result.log.contains("▶ step 2/3: Fail — failure (exit 3)"),
            "{}",
            result.log
        );
        assert!(
            result.log.contains("▶ step 3/3: Three — skipped"),
            "{}",
            result.log
        );
    }

    /// `if:` is read between steps: `failure()` runs only after a failure,
    /// `always()` regardless, and a condition naming no status function is
    /// an implicit `success() &&`.
    #[tokio::test]
    async fn conditions_are_read_between_steps() {
        let mut on_failure = step("On failure", "echo cleanup");
        on_failure.condition = Some("failure()".into());
        let mut always = step("Always", "echo report");
        always.condition = Some("always()".into());
        let mut plain = step("Plain", "echo plain");
        plain.condition = Some("env.DEPLOY == 'staging'".into());
        let mut matched = step("Matched", "echo matched");
        matched.condition =
            Some("github.ref == 'refs/heads/main' && env.DEPLOY == 'staging'".into());

        let steps = [matched.clone(), on_failure.clone(), plain.clone()];
        let (result, events, _root) = run(&steps).await;
        assert_eq!(result.exit_code, 0);
        assert_eq!(
            events,
            [
                "start 1",
                "end 1 success/success",
                "end 2 skipped/skipped",
                "start 3",
                "end 3 success/success",
            ]
        );

        let steps = [step("Fail", "exit 1"), on_failure, plain, always];
        let (result, events, _root) = run(&steps).await;
        assert_eq!(result.exit_code, 1);
        assert_eq!(
            events,
            [
                "start 1",
                "end 1 failure/failure",
                "start 2",
                "end 2 success/success",
                "end 3 skipped/skipped",
                "start 4",
                "end 4 success/success",
            ]
        );
    }

    /// What a step leaves for the next one goes through the file commands;
    /// what it exports, or where it `cd`s, does not.
    #[tokio::test]
    async fn only_the_file_commands_cross_from_one_step_to_the_next() {
        let steps = [
            step(
                "Set",
                "export LEAKED=yes\nmkdir -p sub && cd sub\necho \"FROM_ENV=first\" >> \"$GITHUB_ENV\"\nprintf 'MULTI<<EOF\\nline one\\nline two\\nEOF\\n' >> \"$GITHUB_ENV\"\nmkdir -p \"$RUNNER_TEMP/bin\" && printf '#!/bin/sh\\necho tool-ran\\n' > \"$RUNNER_TEMP/bin/my-tool\" && chmod +x \"$RUNNER_TEMP/bin/my-tool\"\necho \"$RUNNER_TEMP/bin\" >> \"$GITHUB_PATH\"\necho '## done' >> \"$GITHUB_STEP_SUMMARY\"",
            ),
            step(
                "Read",
                "echo \"leaked=${LEAKED:-no} from=$FROM_ENV pwd=$(basename \"$PWD\")\"\necho \"$MULTI\"\nmy-tool",
            ),
        ];
        let (result, _, _root) = run(&steps).await;
        assert_eq!(result.exit_code, 0, "{}", result.log);
        let read = &result.steps[1].log;
        assert!(
            read.contains("leaked=no from=first pwd=workspace"),
            "{read}"
        );
        assert!(read.contains("line one\nline two"), "{read}");
        assert!(read.contains("tool-ran"), "{read}");
        assert!(
            result.steps[0].log.contains("## done"),
            "the summary is shown: {}",
            result.steps[0].log
        );
    }

    /// A step's `env:` overrides what an earlier step wrote to `$GITHUB_ENV`,
    /// and a `$GITHUB_ENV` line naming a runner variable fails the step.
    #[tokio::test]
    async fn env_precedence_and_runner_variables() {
        let mut overriding = step("Override", "echo \"value=$NAME\"");
        overriding.env.insert("NAME".into(), "from-step".into());
        let steps = [
            step("Write", "echo NAME=from-file >> \"$GITHUB_ENV\""),
            overriding,
            step("Read", "echo \"plain=$NAME\""),
        ];
        let (result, _, _root) = run(&steps).await;
        assert!(result.steps[1].log.contains("value=from-step"));
        assert!(result.steps[2].log.contains("plain=from-file"));

        let steps = [step(
            "Forge",
            "echo CI_REF=refs/heads/evil >> \"$GITHUB_ENV\"",
        )];
        let (result, _, _root) = run(&steps).await;
        assert_eq!(result.steps[0].outcome, StepOutcome::Failure);
        assert!(
            result.steps[0]
                .log
                .contains("`CI_REF` is set by the runner"),
            "{}",
            result.steps[0].log
        );
    }

    /// A step that outlives its `timeout-minutes` is stopped, with everything
    /// it started, and the job carries on into an `always()` step.
    #[tokio::test]
    async fn a_step_timeout_stops_the_step_and_only_the_step() {
        let mut slow = step("Slow", "sleep 600 & sleep 600");
        slow.timeout_minutes = Some(1);
        let mut after = step("After", "echo after");
        after.condition = Some("always()".into());
        let steps = [slow, after];
        let started = std::time::Instant::now();
        // A one-second minute: the step is stopped a second in, not ten minutes.
        let (result, _, _root) = run_with_minute(&steps, std::time::Duration::from_secs(1)).await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(30),
            "the step's background sleeps were not killed with it"
        );
        assert_eq!(result.steps[0].outcome, StepOutcome::Failure);
        assert_eq!(result.steps[0].exit_code, None);
        assert!(
            result.steps[0].log.contains("timeout-minutes: 1"),
            "{}",
            result.steps[0].log
        );
        assert_eq!(result.steps[1].outcome, StepOutcome::Success);
        assert_eq!(result.exit_code, -1);
    }

    /// The shells start the way GitHub starts them; `bash` is fail-fast with
    /// `pipefail`, the unspecified shell is `bash -e`, and a custom one puts the
    /// script where `{0}` is.
    #[tokio::test]
    async fn each_shell_runs_the_way_github_runs_it() {
        let mut bash = step("Bash", "false | true\necho survived");
        bash.shell = Some("bash".into());
        let mut sh = step("Sh", "false | true\necho survived");
        sh.shell = Some("sh".into());
        let mut custom = step("Custom", "print('from python')");
        custom.shell = Some("python3 {0}".into());
        let (result, _, _root) = run(&[bash]).await;
        assert_eq!(
            result.steps[0].outcome,
            StepOutcome::Failure,
            "pipefail: {}",
            result.log
        );
        let (result, _, _root) = run(&[sh]).await;
        assert!(result.steps[0].log.contains("survived"), "{}", result.log);
        if std::process::Command::new("python3")
            .arg("-V")
            .output()
            .is_ok()
        {
            let (result, _, _root) = run(&[custom]).await;
            assert!(
                result.steps[0].log.contains("from python"),
                "{}",
                result.log
            );
        }
        let (result, _, _root) = run(&[step("Default", "[[ 1 == 1 ]] && echo bashism")]).await;
        assert!(
            result.steps[0].log.contains("bashism"),
            "the default shell is bash: {}",
            result.log
        );

        assert_eq!(
            Shell::parse(Some("pwsh")).unwrap().argv("/t/s.ps1"),
            ["pwsh", "-command", ". '/t/s.ps1'"]
        );
        assert_eq!(
            Shell::parse(Some("python")).unwrap().argv("/t/s.py"),
            ["python", "/t/s.py"]
        );
    }

    /// The step log is masked before it leaves the engine, in the step's own
    /// record and in the job log.
    #[tokio::test]
    async fn step_logs_are_masked() {
        let (result, _, _root) = run(&[step("Leak", "echo hunter2")]).await;
        assert!(!result.steps[0].log.contains("hunter2"));
        assert!(!result.log.contains("hunter2"));
        assert!(result.steps[0].log.contains("***"));
    }

    /// The container half, on a real daemon: the steps of a job share one
    /// container — what one installs the next finds — and its workspace; a
    /// `$GITHUB_PATH` entry reaches the next step; a step that runs out of its
    /// timeout is killed with what it started while the container lives on for
    /// the `always()` step after it.
    #[tokio::test]
    #[ignore = "needs a Docker daemon and the alpine:3.20 image"]
    async fn steps_share_one_container_and_a_timeout_spares_it() {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let temp = job_temp_dir(&workspace);
        std::fs::create_dir_all(&temp).unwrap();
        let name = format!("plombir-git-steps-test-{}", std::process::id());
        let limits = crate::docker_job::JobLimits::default();
        let env = vec![("CI".to_string(), "true".to_string())];
        // A rootless daemon maps the container's root onto this user, and this
        // user onto a subordinate id the workspace does not belong to.
        let rootless = std::process::Command::new("docker")
            .args(["info", "--format", "{{.SecurityOptions}}"])
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("rootless"));
        let job = crate::docker_job::JobContainer {
            name: &name,
            image: "alpine:3.20",
            script: "",
            workspace: &workspace,
            user: if rootless {
                Some("0:0".to_string())
            } else {
                crate::container_user()
            },
            variables: &env,
            limits: &limits,
            temp: Some(&temp),
            network: None,
        };
        let started = match crate::docker_job::start_steps_container("docker", &job).await {
            Ok(started) => started,
            Err(failed) => panic!("the container did not start: {}", failed.log),
        };
        let mut slow = step("Slow", "echo $$ > /tmp/slow.pid; sleep 600 & sleep 600");
        slow.timeout_minutes = Some(1);
        let mut after = step(
            "After",
            "if kill -0 \"$(cat /tmp/slow.pid)\" 2>/dev/null; then echo alive; else echo gone; fi",
        );
        after.condition = Some("always()".into());
        let steps = [
            step("Install", "echo installed > /tmp/marker\necho artefact > out.txt\nmkdir -p /tmp/bin && printf '#!/bin/sh\\necho tool-ran\\n' > /tmp/bin/tool && chmod +x /tmp/bin/tool\necho /tmp/bin >> \"$GITHUB_PATH\""),
            step("Find", "cat /tmp/marker\ntool\necho \"workspace=$GITHUB_WORKSPACE\""),
            slow,
            after,
        ];
        let mask = |log: &str| log.to_string();
        let mut recorder = Recorder::default();
        let begun = std::time::Instant::now();
        let result = run_steps(
            &StepRun {
                steps: &steps,
                job_env: &env,
                workspace: &workspace,
                temp: &temp,
                backend: StepBackend::Docker {
                    program: std::ffi::OsStr::new("docker"),
                    container: &name,
                    path: started.path,
                },
                mask: &mask,
                minute: std::time::Duration::from_secs(2),
                needs: &BTreeMap::new(),
                outputs: &BTreeMap::new(),
                secrets: None,
            },
            &mut recorder,
        )
        .await;
        remove_container(std::ffi::OsStr::new("docker"), &name).await;

        assert!(result.steps[1].log.contains("installed"), "{}", result.log);
        assert!(result.steps[1].log.contains("tool-ran"), "{}", result.log);
        assert!(
            result.steps[1].log.contains("workspace=/workspace"),
            "{}",
            result.log
        );
        assert_eq!(
            std::fs::read_to_string(workspace.join("out.txt"))
                .unwrap()
                .trim(),
            "artefact",
            "the workspace is the one the host holds"
        );
        assert_eq!(result.steps[2].outcome, StepOutcome::Failure);
        assert!(
            result.steps[2].log.contains("timeout-minutes"),
            "{}",
            result.log
        );
        assert_eq!(
            result.steps[3].outcome,
            StepOutcome::Success,
            "{}",
            result.log
        );
        assert!(
            result.steps[3].log.contains("gone"),
            "the timed-out step's process group was killed: {}",
            result.log
        );
        assert!(begun.elapsed() < std::time::Duration::from_secs(120));
    }

    async fn run_with(
        steps: &[StepSpec],
        needs: &BTreeMap<String, NeededJob>,
        outputs: &BTreeMap<String, String>,
        job_env: Vec<(String, String)>,
    ) -> (StepsResult, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        let temp = root.path().join("temp");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&temp).unwrap();
        let mask = |log: &str| log.replace("hunter2", "***");
        let result = run_steps(
            &StepRun {
                steps,
                job_env: &job_env,
                workspace: &workspace,
                temp: &temp,
                backend: StepBackend::Local,
                mask: &mask,
                minute: MINUTE,
                needs,
                outputs,
                secrets: None,
            },
            &mut Silent,
        )
        .await;
        (result, root)
    }

    fn with_id(mut spec: StepSpec, id: &str) -> StepSpec {
        spec.id = Some(id.into());
        spec
    }

    /// card_5013aa8fca9e: what a step writes to `$GITHUB_OUTPUT` is what a
    /// later step reads as `steps.<id>.outputs.*` — in its script, its `env:`
    /// and its `if:` — and the job's own `outputs:` are resolved from them
    /// after the last step, masked.
    #[tokio::test]
    async fn step_outputs_reach_later_steps_and_the_jobs_outputs() {
        let mut gate = step("Gate", "echo gated-in");
        gate.condition = Some("steps.version.outputs.channel == 'stable'".into());
        let mut skipped = step("Skipped", "echo never");
        skipped.condition = Some("steps.version.outputs.channel == 'nightly'".into());
        let mut reader = step("Read", "echo \"script=${{ steps.version.outputs.number }}\"\necho \"env=$FROM_ENV\"\necho \"outcome=${{ steps.version.outcome }}\"");
        reader.env.insert(
            "FROM_ENV".into(),
            "${{ steps.version.outputs.notes }}".into(),
        );
        let steps = [
            with_id(
                step(
                    "Version",
                    "echo number=1.2.3 >> \"$GITHUB_OUTPUT\"\necho channel=stable >> \"$GITHUB_OUTPUT\"\nprintf 'notes<<END\\nline one\\nline two\\nEND\\n' >> \"$GITHUB_OUTPUT\"\necho secret=hunter2 >> \"$GITHUB_OUTPUT\"",
                ),
                "version",
            ),
            reader,
            gate,
            skipped,
        ];
        let outputs = BTreeMap::from([
            (
                "version".to_string(),
                "v${{ steps.version.outputs.number }}".to_string(),
            ),
            (
                "leak".to_string(),
                "${{ steps.version.outputs.secret }}".to_string(),
            ),
        ]);
        let (result, _root) = run_with(&steps, &BTreeMap::new(), &outputs, job_env()).await;
        assert_eq!(result.exit_code, 0, "{}", result.log);
        let read = &result.steps[1].log;
        assert!(read.contains("script=1.2.3"), "{read}");
        assert!(read.contains("env=line one\nline two"), "{read}");
        assert!(read.contains("outcome=success"), "{read}");
        assert_eq!(result.steps[2].outcome, StepOutcome::Success);
        assert_eq!(result.steps[3].outcome, StepOutcome::Skipped);
        assert_eq!(
            result.outputs,
            BTreeMap::from([
                ("leak".to_string(), "***".to_string()),
                ("version".to_string(), "v1.2.3".to_string()),
            ]),
            "the job's outputs are resolved and masked"
        );
    }

    /// What a needed job left is read in a step and in a job variable.
    #[tokio::test]
    async fn needs_outputs_reach_the_steps_and_the_job_variables() {
        let needs = BTreeMap::from([(
            "build".to_string(),
            NeededJob {
                result: "success".into(),
                outputs: BTreeMap::from([("version".to_string(), "4.5.6".to_string())]),
            },
        )]);
        let mut env = job_env();
        env.push((
            "JOB_VERSION".into(),
            "${{ needs.build.outputs.version }}".into(),
        ));
        let mut deploy = step(
            "Deploy",
            "echo \"step=${{ needs.build.outputs.version }} job=$JOB_VERSION result=${{ needs.build.result }}\"",
        );
        deploy.condition = Some("needs.build.result == 'success'".into());
        let (result, _root) = run_with(&[deploy], &needs, &BTreeMap::new(), env).await;
        assert!(
            result.steps[0]
                .log
                .contains("step=4.5.6 job=4.5.6 result=success"),
            "{}",
            result.log
        );
    }

    /// An output that takes the job past its 1 MiB fails the step that wrote
    /// it, naming the key — it is refused, never cut.
    #[tokio::test]
    async fn outputs_past_the_budget_fail_the_step_by_name() {
        let steps = [
            with_id(step("Half", "head -c 600000 /dev/zero | tr '\\0' a | sed 's/^/first=/' >> \"$GITHUB_OUTPUT\"; echo >> \"$GITHUB_OUTPUT\""), "half"),
            with_id(step("Over", "head -c 600000 /dev/zero | tr '\\0' b | sed 's/^/second=/' >> \"$GITHUB_OUTPUT\"; echo >> \"$GITHUB_OUTPUT\""), "over"),
        ];
        let (result, _root) = run_with(&steps, &BTreeMap::new(), &BTreeMap::new(), job_env()).await;
        assert_eq!(
            result.steps[0].outcome,
            StepOutcome::Success,
            "{}",
            result.log
        );
        assert_eq!(result.steps[1].outcome, StepOutcome::Failure);
        assert!(
            result.steps[1]
                .log
                .contains("$GITHUB_OUTPUT `second` takes the job's outputs past"),
            "{}",
            result.steps[1].log
        );
    }

    /// `::set-output` is the workflow command GitHub disabled; a step that
    /// prints one fails saying so instead of setting nothing silently.
    #[tokio::test]
    async fn the_set_output_command_fails_its_step_by_name() {
        let (result, _root) = run_with(
            &[step("Old", "echo '::set-output name=version::1.0'")],
            &BTreeMap::new(),
            &BTreeMap::new(),
            job_env(),
        )
        .await;
        assert_eq!(result.steps[0].outcome, StepOutcome::Failure);
        assert!(
            result.steps[0].log.contains("$GITHUB_OUTPUT instead"),
            "{}",
            result.log
        );
    }

    #[test]
    fn runtime_keys_are_read_by_their_github_spelling() {
        assert_eq!(
            RuntimeKey::parse("steps.my-step.outputs.my_value"),
            Some(RuntimeKey::StepOutput {
                step: "my-step".into(),
                name: "my_value".into()
            })
        );
        assert_eq!(
            RuntimeKey::parse("needs.build.result"),
            Some(RuntimeKey::NeedsResult("build".into()))
        );
        for not_runtime in [
            "steps.x",
            "steps.1x.outcome",
            "needs.a.outputs",
            "env.X",
            "steps.x.outputs.a.b",
        ] {
            assert_eq!(RuntimeKey::parse(not_runtime), None, "{not_runtime}");
        }
    }

    #[test]
    fn env_files_parse_both_forms_and_refuse_what_they_cannot_mean() {
        assert_eq!(
            parse_env_file("A=1\n\nB=x=y\nC<<END\none\ntwo\nEND\n").unwrap(),
            [
                ("A".to_string(), "1".to_string()),
                ("B".to_string(), "x=y".to_string()),
                ("C".to_string(), "one\ntwo".to_string()),
            ]
        );
        for (text, named) in [
            ("nonsense\n", "expected `NAME=value`"),
            ("1A=x\n", "not a valid variable name"),
            ("A<<END\nnever closed\n", "never closed"),
            ("PATH=/evil\n", "$GITHUB_PATH instead"),
            ("CI_JOB_TOKEN=x\n", "set by the runner"),
        ] {
            let error = parse_env_file(text).expect_err(text);
            assert!(error.contains(named), "{text:?}: {error}");
        }
    }
}

/// `services:` on a real daemon (card_bc6676ec0ae3). Ignored like the other
/// container tests: they need Docker and the `postgres:17-alpine` image, and
/// run with `--ignored` where both are there.
#[cfg(all(test, target_os = "linux"))]
mod services_docker_tests {
    use super::*;
    use crate::ci_services::{ServiceSpec, ServicesRun, OWNER_LABEL};

    const IMAGE: &str = "postgres:17-alpine";

    fn step(name: &str, run: &str) -> StepSpec {
        StepSpec {
            name: name.into(),
            id: None,
            run: run.into(),
            shell: Some("sh".into()),
            working_directory: None,
            env: BTreeMap::new(),
            continue_on_error: false,
            timeout_minutes: None,
            condition: None,
            secrets: BTreeSet::new(),
        }
    }

    fn postgres() -> ServiceSpec {
        ServiceSpec {
            key: "postgres".into(),
            image: IMAGE.into(),
            env: BTreeMap::from([("POSTGRES_PASSWORD".into(), "${PG_PASSWORD}".into())]),
            ports: Vec::new(),
            credentials: None,
            health: crate::ci_services::parse_health_options(
                "--health-cmd 'pg_isready -U postgres' --health-interval 1s --health-timeout 5s \
                 --health-retries 60",
            )
            .unwrap(),
        }
    }

    fn rootless() -> bool {
        std::process::Command::new("docker")
            .args(["info", "--format", "{{.SecurityOptions}}"])
            .output()
            .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("rootless"))
    }

    /// What the daemon still holds of `owner`'s services: containers and
    /// networks carrying its label.
    fn leftovers(owner: &str) -> Vec<String> {
        let filter = format!("label={OWNER_LABEL}={owner}");
        let list = |args: &[&str]| {
            let output = std::process::Command::new("docker")
                .args(args)
                .output()
                .unwrap();
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>()
        };
        let mut left = list(&["ps", "-aq", "--filter", &filter]);
        left.extend(list(&["network", "ls", "-q", "--filter", &filter]));
        left
    }

    struct Job {
        _root: tempfile::TempDir,
        workspace: PathBuf,
        name: String,
        limits: crate::docker_job::JobLimits,
        env: Vec<(String, String)>,
    }

    fn job(tag: &str) -> Job {
        let root = tempfile::tempdir().unwrap();
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        Job {
            _root: root,
            workspace,
            name: format!("plombir-git-services-test-{tag}-{}", std::process::id()),
            limits: crate::docker_job::JobLimits::default(),
            env: vec![
                ("CI".to_string(), "true".to_string()),
                ("PG_PASSWORD".to_string(), "hunter2".to_string()),
            ],
        }
    }

    async fn run(
        job: &Job,
        steps: &[StepSpec],
        specs: &[ServiceSpec],
        reporter: &mut impl StepReporter,
    ) -> JobRun {
        let mask = |log: &str| log.replace("hunter2", "***");
        run_job_steps(
            steps,
            &job.env,
            &job.workspace,
            Some(StepsImage {
                program: std::ffi::OsStr::new("docker"),
                name: &job.name,
                image: IMAGE,
                user: if rootless() {
                    Some("0:0".to_string())
                } else {
                    crate::container_user()
                },
                limits: &job.limits,
            }),
            Some(ServicesRun {
                program: std::ffi::OsStr::new("docker"),
                owner: &job.name,
                specs,
                limits: &job.limits,
                health_timeout: std::time::Duration::from_secs(120),
            }),
            &mask,
            &BTreeMap::new(),
            &BTreeMap::new(),
            None,
            reporter,
        )
        .await
    }

    /// The acceptance of the card: the job's first step reaches the service by
    /// its key, because the job waited for it to be healthy — `pg_isready`
    /// answers "no response" to a database still running `initdb`, so a job
    /// that did not wait fails here. Nothing is left on the daemon after.
    #[tokio::test]
    #[ignore = "needs a Docker daemon and the postgres:17-alpine image"]
    async fn a_step_reaches_a_healthy_service_by_its_key_and_nothing_is_left() {
        let job = job("healthy");
        let result = run(
            &job,
            &[step("Ready", "pg_isready -h postgres -U postgres")],
            &[postgres()],
            &mut Silent,
        )
        .await;

        assert_eq!(result.exit_code, 0, "{}", result.log);
        assert!(
            result.log.contains("service postgres is healthy"),
            "{}",
            result.log
        );
        assert!(!result.log.contains("hunter2"), "{}", result.log);
        assert_eq!(
            leftovers(&job.name),
            Vec::<String>::new(),
            "services outlived the job"
        );
    }

    /// Reports when the first step starts, so the test can cancel the job while
    /// its services are up.
    struct StartSignal(Option<tokio::sync::oneshot::Sender<()>>);

    impl StepReporter for StartSignal {
        async fn started(&mut self, _number: usize) {
            if let Some(signal) = self.0.take() {
                // The test may already have stopped listening; nothing to do.
                match signal.send(()) {
                    Ok(()) | Err(()) => {}
                }
            }
        }
        async fn finished(&mut self, _result: &StepResult) {}
    }

    /// A job cancelled mid-step — its future dropped, as a timeout or a
    /// cancellation does in both runners — leaves neither its services nor
    /// their network once the runner has removed the job's container by name.
    #[tokio::test]
    #[ignore = "needs a Docker daemon and the postgres:17-alpine image"]
    async fn a_cancelled_job_leaves_no_service_and_no_network() {
        let job = job("cancelled");
        let (signal, started) = tokio::sync::oneshot::channel();
        let mut reporter = StartSignal(Some(signal));
        let steps = [step("Forever", "sleep 600")];
        let specs = [postgres()];
        tokio::select! {
            result = run(&job, &steps, &specs, &mut reporter) => {
                panic!("the job ended before it was cancelled: {}", result.log)
            }
            _ = started => {}
        }
        assert!(
            !leftovers(&job.name).is_empty(),
            "the services were not running while the step was"
        );
        // What both runners do after dropping the job's future.
        remove_container(std::ffi::OsStr::new("docker"), &job.name).await;

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            let left = leftovers(&job.name);
            if left.is_empty() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "a cancelled job left {left:?} behind"
            );
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}
