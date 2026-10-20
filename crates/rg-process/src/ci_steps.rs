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

use std::collections::{BTreeMap, HashMap};
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
}

/// One minute of a step's `timeout-minutes`.
pub const MINUTE: std::time::Duration = std::time::Duration::from_secs(60);

/// The names a step's process gets from the runner, never from the job.
const FILE_COMMANDS: [&str; 3] = ["GITHUB_ENV", "GITHUB_PATH", "GITHUB_STEP_SUMMARY"];

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
        if !crate::job_environment::valid_environment_name(&name) {
            return Err(format!(
                "line {}: `{name}` is not a valid variable name",
                index + 1
            ));
        }
        if reserved_for_the_runner(&name) {
            return Err(format!(
                "line {}: `{name}` is set by the runner and cannot be changed by a step{}",
                index + 1,
                if name == "PATH" {
                    "; append a directory to $GITHUB_PATH instead"
                } else {
                    ""
                }
            ));
        }
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

    for (index, step) in run.steps.iter().enumerate() {
        let number = index + 1;
        let env = step_environment(run, step, &carried_env, &carried_path, number);
        let condition = step.condition.as_deref().unwrap_or("success()");
        let runs = match evaluate_step_condition(condition, &step_condition_context(&env), status) {
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
                };
                reporter.finished(&result).await;
                status.failed = true;
                if exit_code == 0 {
                    exit_code = -1;
                }
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
            };
            reporter.finished(&result).await;
            results.push(result);
            continue;
        }

        reporter.started(number).await;
        let started_at = SystemTime::now();
        let (code, log, file_commands) = execute_step(run, step, number, &env).await;
        let finished_at = SystemTime::now();

        let mut log = log;
        let mut failed = code != Some(0);
        if !failed {
            match file_commands {
                Ok(FileCommands {
                    env: written,
                    path,
                    summary,
                }) => {
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
        };
        reporter.finished(&result).await;
        results.push(result);
    }

    StepsResult {
        exit_code,
        log: combined_log(&results, run.steps.len()),
        steps: results,
    }
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
fn step_environment(
    run: &StepRun<'_>,
    step: &StepSpec,
    carried_env: &[(String, String)],
    carried_path: &[String],
    number: usize,
) -> Vec<(String, String)> {
    let paths = step_paths(run, number, step);
    let mut env: BTreeMap<String, String> = run.job_env.iter().cloned().collect();
    env.extend(carried_env.iter().cloned());
    env.extend(
        step.env
            .iter()
            .map(|(name, value)| (name.clone(), value.clone())),
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
    env.insert("GITHUB_WORKSPACE".into(), paths.workspace);
    env.insert("RUNNER_TEMP".into(), paths.temp);
    env.insert("GITHUB_ACTIONS".into(), "true".into());
    env.into_iter().collect()
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
        workspace,
        temp,
    }
}

struct FileCommands {
    env: Vec<(String, String)>,
    path: Vec<String>,
    summary: String,
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
        Ok(FileCommands { env, path, summary })
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
pub async fn run_job_steps(
    steps: &[StepSpec],
    job_env: &[(String, String)],
    workspace: &Path,
    image: Option<StepsImage<'_>>,
    mask: &(dyn Fn(&str) -> String + Sync),
    reporter: &mut impl StepReporter,
) -> (i32, String) {
    let temp = job_temp_dir(workspace);
    if let Err(error) = std::fs::create_dir_all(&temp) {
        return (
            -1,
            format!(
                "[plombir-git-runner] the job's step directory `{}` could not be created: {error}",
                temp.display()
            ),
        );
    }
    let _guard = TempGuard(temp.clone());

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
            },
            reporter,
        )
        .await;
        return (result.exit_code, result.log);
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
    };
    let container = match crate::docker_job::start_steps_container(image.program, &job).await {
        Ok(container) => container,
        Err(failed) => {
            remove_container(image.program, image.name).await;
            return (failed.exit_code, mask(&failed.log));
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
        },
        reporter,
    )
    .await;
    remove_container(image.program, image.name).await;
    let mut log = container.pull_log;
    if !log.is_empty() && !log.ends_with('\n') {
        log.push('\n');
    }
    if let Some(notice) = crate::docker_job::left_out_variables_notice(job_env) {
        push_line(&mut log, &format!("[plombir-git-runner] {notice}"));
    }
    log.push_str(&result.log);
    (result.exit_code, log)
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
            },
            &mut recorder,
        )
        .await;
        (result, recorder.events, root)
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
