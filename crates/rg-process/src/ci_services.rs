//! `jobs.<id>.services`: the containers a job of steps talks to — a database,
//! a cache, a broker — started before its first step and removed with it.
//!
//! Both runners execute a job of steps through [`crate::ci_steps`], and that
//! module starts the services through this one, so a service means the same
//! thing wherever the job lands.
//!
//! What a job gets, as on GitHub:
//!
//! - one Docker network of its own, `<job container>-net`; every service joins
//!   it under its key as a network alias, and a job in `container:` joins it
//!   too, so `postgres:5432` reaches the service keyed `postgres`;
//! - `ports:` published on the runner's loopback — `127.0.0.1` only — for a job
//!   that runs on the host and reaches a service at `localhost:<port>`;
//! - a wait, before the first step, until every service whose image or
//!   `options:` declares a health check reports `healthy`, bounded by
//!   [`HEALTH_TIMEOUT`].
//!
//! A service is sandboxed like a job container, with the one difference a
//! database image needs: its entrypoint starts as root, takes ownership of its
//! data directory and drops to its own user, so it keeps `SERVICE_CAPABILITIES`
//! instead of none. No workspace is mounted into it, it is never privileged, it
//! never sees the Docker socket, and the only `docker run` flags a workflow can
//! add are the health-check ones in `HEALTH_FLAGS` — anything else in
//! `options:` would be a way around that sandbox, the reason `container.options`
//! is refused.
//!
//! Everything a job's services create carries `OWNER_LABEL` and is removed
//! when the job ends: on the way out of [`crate::ci_steps::run_job_steps`], by
//! `StartedServices`' `Drop` when the job's future is dropped by a timeout or
//! a cancellation, by [`remove_services_of`] wherever a runner removes a job's
//! container by name, and — for a runner that died with a job in hand — by the
//! sweep every later Docker job runs first, and the external runner runs when
//! it starts ([`sweep_orphans`]). The same sweep removes such a runner's steps
//! container, which carries `STEPS_LABEL`.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::time::Duration;

/// Labels every service container and network carries: the job container they
/// belong to, and the process and boot that started them — what
/// `sweep_orphans` reads to tell a dead runner's leftovers from a live one's.
pub(crate) const OWNER_LABEL: &str = "plombir-git.service-of";
const HOST_LABEL: &str = "plombir-git.service-host";
const BOOT_LABEL: &str = "plombir-git.service-boot";
const PID_LABEL: &str = "plombir-git.service-pid";
/// The label a job's steps container carries instead of `OWNER_LABEL`, with
/// its own name: it is not one of its services, and `remove_services_of` must
/// not take it, but a runner that dies with it running leaves it to the same
/// sweep. Host, boot and pid are the labels above.
const STEPS_LABEL: &str = "plombir-git.steps-job";

/// Longest a job waits for its services to become healthy before it fails.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(300);

/// How often a starting service's health is asked.
const HEALTH_POLL: Duration = Duration::from_millis(500);

/// Longest one cleanup call to the daemon is waited on.
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

/// The `options:` flags a service may carry: its health check, and nothing
/// else. Each takes one value.
const HEALTH_FLAGS: &[&str] = &[
    "--health-cmd",
    "--health-interval",
    "--health-retries",
    "--health-start-period",
    "--health-start-interval",
    "--health-timeout",
];

/// What a service keeps of Linux's capabilities: enough for an official
/// database image's entrypoint to `chown` its data directory and switch to its
/// own user (`gosu` / `su-exec`), and to signal and bind as it does. Everything
/// else Docker grants by default — raw sockets, `mknod`, `chroot`, audit
/// writes, file capabilities — is dropped.
const SERVICE_CAPABILITIES: &[&str] = &[
    "CHOWN",
    "DAC_OVERRIDE",
    "FOWNER",
    "SETGID",
    "SETUID",
    "KILL",
    "NET_BIND_SERVICE",
];

/// One `jobs.<id>.services.<key>` entry as a job carries it to a runner.
///
/// `image`, the `env` values and the `credentials` may still hold `${NAME}`
/// references — what `${{ secrets.NAME }}` and `${{ matrix.x }}` become when
/// the workflow is read — which the runner fills in from the job's own
/// variables before it starts the service (`expand`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSpec {
    /// The key under `services:`: the service's name on the job's network.
    pub key: String,
    pub image: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<ServicePort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<ServiceCredentials>,
    /// `--flag=value` arguments, each flag one of `HEALTH_FLAGS`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub health: Vec<String>,
}

/// One `ports:` entry: the container port, and the loopback port of the
/// runner it is published on — Docker picks one when the entry names none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServicePort {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<u16>,
    pub container: u16,
    #[serde(default)]
    pub protocol: PortProtocol,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

/// The registry login a private service image is pulled with.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCredentials {
    pub username: String,
    pub password: String,
}

/// Is `key` a name a service can carry on its network and in its container's
/// name: a letter or digit, then letters, digits, `-` and `_`, at most 63.
pub fn valid_service_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && key.len() <= 63
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Read one `ports:` entry: `<container>`, `<host>:<container>`, either with
/// `/tcp` or `/udp`. An address in front (`0.0.0.0:80:80`) is refused: the
/// port is published on loopback, and a workflow does not get to say otherwise.
pub fn parse_port(entry: &str) -> Result<ServicePort, String> {
    let entry = entry.trim();
    let (ports, protocol) = match entry.rsplit_once('/') {
        Some((ports, "tcp")) => (ports, PortProtocol::Tcp),
        Some((ports, "udp")) => (ports, PortProtocol::Udp),
        Some((_, other)) => return Err(format!("`{entry}`: protocol `{other}` is not tcp or udp")),
        None => (entry, PortProtocol::Tcp),
    };
    let number = |text: &str| {
        text.parse::<u16>()
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| format!("`{entry}`: `{text}` is not a port number"))
    };
    match ports.split(':').collect::<Vec<_>>().as_slice() {
        [container] => Ok(ServicePort {
            host: None,
            container: number(container)?,
            protocol,
        }),
        [host, container] => Ok(ServicePort {
            host: Some(number(host)?),
            container: number(container)?,
            protocol,
        }),
        _ => Err(format!(
            "`{entry}`: write `<container port>` or `<runner port>:<container port>`; a service \
             port is published on the runner's loopback only, so an address cannot be chosen"
        )),
    }
}

/// Read a service's `options:` into health-check arguments, refusing every flag
/// that is not one of `HEALTH_FLAGS` by name.
///
/// Split the way a shell splits a command line — quotes group, a backslash
/// escapes — because that is how GitHub's runner hands the string to
/// `docker create`, and `--health-cmd "redis-cli ping"` is the common shape.
pub fn parse_health_options(options: &str) -> Result<Vec<String>, String> {
    let words = split_words(options)?;
    let mut flags = Vec::new();
    let mut words = words.into_iter();
    while let Some(word) = words.next() {
        let (flag, inline) = match word.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_string(), Some(value.to_string()))
            }
            _ => (word.clone(), None),
        };
        if !HEALTH_FLAGS.contains(&flag.as_str()) {
            return Err(format!(
                "`{word}` is not a health-check flag; a service's options may only be {}, because \
                 any other `docker run` flag could undo the sandbox the service runs in",
                HEALTH_FLAGS.join(", ")
            ));
        }
        let value = match inline {
            Some(value) => value,
            None => words
                .next()
                .ok_or_else(|| format!("`{flag}` needs a value"))?,
        };
        if value.is_empty() {
            return Err(format!("`{flag}` needs a value"));
        }
        flags.push(format!("{flag}={value}"));
    }
    Ok(flags)
}

/// Split `text` into words the way a POSIX shell would, without expanding
/// anything: whitespace separates, `'…'` and `"…"` group, `\` escapes.
fn split_words(text: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("an opening ' is never closed".into()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('"' | '\\' | '$' | '`')) => word.push(c),
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("an opening \" is never closed".into()),
                        },
                        Some(c) => word.push(c),
                        None => return Err("an opening \" is never closed".into()),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(c) => word.push(c),
                    None => return Err("a trailing \\ escapes nothing".into()),
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// Fill the `${NAME}` references in `template` from the job's variables.
///
/// Only a whole `${NAME}` whose name the job carries is replaced; anything
/// else — `$NAME`, `${NAME:-x}`, a name the job does not have — is left as
/// written, the way the value would reach a container that no shell reads.
fn expand(template: &str, job_env: &[(String, String)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("${") {
        let after = &rest[open + 2..];
        let Some(close) = after.find('}') else {
            break;
        };
        let name = &after[..close];
        match job_env.iter().rev().find(|(key, _)| key == name) {
            Some((_, value)) if crate::job_environment::valid_environment_name(name) => {
                out.push_str(&rest[..open]);
                out.push_str(value);
            }
            _ => out.push_str(&rest[..open + 2 + close + 1]),
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// What `start_services` needs to know besides the services themselves.
pub struct ServicesRun<'a> {
    /// The `docker` program the runner calls.
    pub program: &'a OsStr,
    /// The job's container name, which the network and every service container
    /// are named and labelled after — whether or not the job itself runs in a
    /// container.
    pub owner: &'a str,
    pub specs: &'a [ServiceSpec],
    /// The runner operator's ceiling, the same one a job container gets.
    pub limits: &'a crate::docker_job::JobLimits,
    /// How long the services may take to become healthy.
    pub health_timeout: Duration,
}

/// The network a job's services are on: `<owner>-net`.
fn network_name(owner: &str) -> String {
    format!("{owner}-net")
}

fn container_name(owner: &str, key: &str) -> String {
    format!("{owner}-svc-{key}")
}

/// Why a job's services are not running: the exit code and the log to fail
/// the job with.
#[derive(Debug)]
pub(crate) struct ServicesFailed {
    pub(crate) exit_code: i32,
    pub(crate) log: String,
}

/// A job's services, up and healthy. Dropped without `StartedServices::stop`
/// — the job's future was dropped by a timeout or a cancellation — they are
/// removed on a thread of their own.
pub(crate) struct StartedServices {
    program: OsString,
    owner: String,
    /// What starting them printed, for the head of the job log.
    pub(crate) log: String,
    armed: bool,
}

impl StartedServices {
    /// The network a job container joins to reach the services by key.
    pub(crate) fn network(&self) -> String {
        network_name(&self.owner)
    }

    /// Remove every service and the network, and wait for it.
    ///
    /// The job's own container must be gone first, or still running only to be
    /// removed by its caller: the network is disconnected from it either way.
    pub(crate) async fn stop(mut self) {
        self.armed = false;
        remove_services_of(&self.program, &self.owner).await;
    }
}

impl Drop for StartedServices {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let program = self.program.clone();
        let owner = self.owner.clone();
        // `Drop` cannot await, and a timeout or a cancellation that dropped the
        // job is exactly when nobody else will remove what the services hold:
        // a database's memory, a published port, a network.
        let spawned = std::thread::Builder::new()
            .name("plombir-git-service-cleanup".into())
            .spawn(move || remove_services_of_blocking(&program, &owner));
        if let Err(error) = spawned {
            tracing::warn!(
                owner = %self.owner,
                %error,
                "could not start removing a dropped job's services; they stay until the next job \
                 with services sweeps them"
            );
        }
    }
}

/// Remove the services of the job whose container is `owner`, and their
/// network, whatever state the job is in. Never fails: what it cannot remove is
/// logged, and `sweep_orphans` takes it later.
pub async fn remove_services_of(program: &OsStr, owner: &str) {
    let filter = format!("label={OWNER_LABEL}={owner}");
    let listed = bounded(cli(program).args(["ps", "-aq", "--filter", &filter])).await;
    let containers: Vec<String> = listed
        .map(|stdout| stdout.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    if !containers.is_empty() {
        let mut command = cli(program);
        command.args(["rm", "-f"]).args(&containers);
        if bounded(&mut command).await.is_none() {
            tracing::warn!(owner, "failed to remove a job's service containers");
        }
    }
    remove_network(program, owner).await;
}

async fn remove_network(program: &OsStr, owner: &str) {
    let network = network_name(owner);
    let exists = bounded(cli(program).args([
        "network",
        "ls",
        "-q",
        "--filter",
        &format!("label={OWNER_LABEL}={owner}"),
    ]))
    .await
    .is_some_and(|stdout| !stdout.trim().is_empty());
    if !exists {
        return;
    }
    // The job's container may still be on it — its caller removes it by name
    // after this — and a network with an endpoint cannot be removed.
    let _disconnected =
        bounded(cli(program).args(["network", "disconnect", "-f", &network, owner])).await;
    if bounded(cli(program).args(["network", "rm", &network]))
        .await
        .is_none()
    {
        tracing::warn!(owner, network, "failed to remove a job's service network");
    }
}

/// [`remove_services_of`] for a thread that cannot await.
fn remove_services_of_blocking(program: &OsStr, owner: &str) {
    let run = |args: &[&str]| -> Option<String> {
        let output = std::process::Command::new(program)
            .env_clear()
            .envs(crate::job_environment::docker_cli_environment())
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let filter = format!("label={OWNER_LABEL}={owner}");
    let containers: Vec<String> = run(&["ps", "-aq", "--filter", &filter])
        .map(|stdout| stdout.split_whitespace().map(str::to_string).collect())
        .unwrap_or_default();
    if !containers.is_empty() {
        let mut args = vec!["rm", "-f"];
        args.extend(containers.iter().map(String::as_str));
        if run(&args).is_none() {
            tracing::warn!(owner, "failed to remove a dropped job's service containers");
        }
    }
    let network = network_name(owner);
    let _disconnected = run(&["network", "disconnect", "-f", &network, owner]);
    let exists = run(&["network", "ls", "-q", "--filter", &filter])
        .is_some_and(|stdout| !stdout.trim().is_empty());
    if exists && run(&["network", "rm", &network]).is_none() {
        tracing::warn!(
            owner,
            network,
            "failed to remove a dropped job's service network"
        );
    }
}

/// A `docker` client in the operator's environment, never the job's: nothing
/// it runs needs a job variable, and a pull it makes may start a credential
/// helper (see [`crate::docker_job`]).
fn cli(program: &OsStr) -> tokio::process::Command {
    let mut command = crate::docker_job::client(program);
    command
        .env_clear()
        .envs(crate::job_environment::docker_cli_environment())
        .stdin(std::process::Stdio::null());
    command
}

/// Run a cleanup or listing call, bounded; its stdout when it succeeded.
async fn bounded(command: &mut tokio::process::Command) -> Option<String> {
    match tokio::time::timeout(CLEANUP_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            Some(String::from_utf8_lossy(&output.stdout).into_owned())
        }
        _ => None,
    }
}

/// Who is starting services: this machine, this boot, this process.
struct Starter {
    host: String,
    boot: String,
    pid: u32,
}

impl Starter {
    /// `None` where the three cannot be read — off Linux — and then no sweep
    /// runs: a leftover there is better than removing a live job's database.
    fn this_process() -> Option<Self> {
        let read = |path: &str| {
            std::fs::read_to_string(path)
                .ok()
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty())
        };
        Some(Self {
            host: read("/proc/sys/kernel/hostname")?,
            boot: read("/proc/sys/kernel/random/boot_id")?,
            pid: std::process::id(),
        })
    }

    /// Whether the process a leftover's labels name is gone: started on this
    /// machine, and either in an earlier boot or by a process that no longer
    /// runs. A label naming another machine — a daemon several hosts share —
    /// is never this sweep's to judge.
    fn is_gone(&self, host: &str, boot: &str, pid: &str) -> bool {
        if host != self.host {
            return false;
        }
        if boot != self.boot {
            return true;
        }
        match pid.parse::<u32>() {
            Ok(pid) => pid != self.pid && !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            Err(_) => false,
        }
    }

    fn labels(&self, owner: &str) -> Vec<String> {
        [
            (OWNER_LABEL, owner.to_string()),
            (HOST_LABEL, self.host.clone()),
            (BOOT_LABEL, self.boot.clone()),
            (PID_LABEL, self.pid.to_string()),
        ]
        .into_iter()
        .flat_map(|(label, value)| ["--label".to_string(), format!("{label}={value}")])
        .collect()
    }
}

/// The labels a job's steps container is started with
/// ([`crate::docker_job::steps_run_args`]), so [`sweep_orphans`] can remove it
/// once the process that started it is gone. Off Linux only its name: without
/// a host label no sweep ever judges it.
pub(crate) fn steps_container_labels(name: &str) -> Vec<String> {
    let mut labels = vec!["--label".to_string(), format!("{STEPS_LABEL}={name}")];
    if let Some(starter) = Starter::this_process() {
        labels.extend(
            [
                (HOST_LABEL, starter.host),
                (BOOT_LABEL, starter.boot),
                (PID_LABEL, starter.pid.to_string()),
            ]
            .into_iter()
            .flat_map(|(label, value)| ["--label".to_string(), format!("{label}={value}")]),
        );
    }
    labels
}

/// What a dead runner left on this machine, by name.
#[derive(Debug, Default, PartialEq, Eq)]
struct Leftovers {
    /// Jobs whose services and network are to go (`OWNER_LABEL` values).
    services_of: std::collections::BTreeSet<String>,
    /// Steps containers to remove.
    steps: std::collections::BTreeSet<String>,
}

/// The `--format` of a sweep listing: name, then `OWNER_LABEL`, `STEPS_LABEL`,
/// host, boot and pid, tab-separated. `name_field` is `.Names` for
/// `docker ps` and `.Name` for `docker network ls`, which has no `.Names` and
/// refuses a template that asks for it.
fn sweep_format(name_field: &str) -> String {
    format!(
        "{{{{{name_field}}}}}\t{{{{.Label \"{OWNER_LABEL}\"}}}}\t{{{{.Label \"{STEPS_LABEL}\"}}}}\t\
         {{{{.Label \"{HOST_LABEL}\"}}}}\t{{{{.Label \"{BOOT_LABEL}\"}}}}\t\
         {{{{.Label \"{PID_LABEL}\"}}}}"
    )
}

/// Read sweep listings: what among them a process that is gone started.
fn leftovers(starter: &Starter, listings: &[String]) -> Leftovers {
    let mut found = Leftovers::default();
    for line in listings.iter().flat_map(|listing| listing.lines()) {
        let fields: Vec<&str> = line.split('\t').collect();
        let [_, owner, steps, host, boot, pid] = fields.as_slice() else {
            continue;
        };
        if !starter.is_gone(host, boot, pid) {
            continue;
        }
        if !owner.is_empty() {
            found.services_of.insert(owner.to_string());
        }
        if !steps.is_empty() {
            found.steps.insert(steps.to_string());
        }
    }
    found
}

/// Remove the steps containers, services and networks a runner on this machine
/// started and then died without removing: their labels name a process that is
/// gone.
///
/// Run before every job that uses Docker and when the external runner starts,
/// so a crashed runner's job container or database does not outlive it by more
/// than the next such job. Never fails the job.
pub async fn sweep_orphans(program: &OsStr) {
    let Some(starter) = Starter::this_process() else {
        return;
    };
    let filter = format!("label={HOST_LABEL}={}", starter.host);
    let listings: Vec<String> = [
        bounded(cli(program).args([
            "ps",
            "-a",
            "--filter",
            &filter,
            "--format",
            &sweep_format(".Names"),
        ]))
        .await,
        bounded(cli(program).args([
            "network",
            "ls",
            "--filter",
            &filter,
            "--format",
            &sweep_format(".Name"),
        ]))
        .await,
    ]
    .into_iter()
    .flatten()
    .collect();
    let found = leftovers(&starter, &listings);
    for name in &found.steps {
        tracing::info!(
            container = name,
            "removing the steps container of a job whose runner is gone"
        );
        if bounded(cli(program).args(["rm", "-f", name]))
            .await
            .is_none()
        {
            tracing::warn!(
                container = name,
                "failed to remove an orphaned steps container"
            );
        }
    }
    // A steps container is the owner its services are named after.
    for owner in found.services_of.iter().chain(&found.steps) {
        tracing::info!(owner, "removing the services of a job whose runner is gone");
        remove_services_of(program, owner).await;
    }
}

/// Start a job's services on a network of their own and wait until each is
/// healthy. On any failure, everything already started is removed before this
/// returns.
///
/// `scratch` is the job's own step directory: a registry login is written
/// there for the one pull that needs it, so a crash in between leaves it where
/// the job's cleanup removes it, never in the system's temporary directory.
pub(crate) async fn start_services(
    run: &ServicesRun<'_>,
    job_env: &[(String, String)],
    scratch: &std::path::Path,
) -> Result<StartedServices, ServicesFailed> {
    let starter = Starter::this_process();
    let labels = match &starter {
        Some(starter) => starter.labels(run.owner),
        None => vec![
            "--label".to_string(),
            format!("{OWNER_LABEL}={}", run.owner),
        ],
    };
    let mut started = StartedServices {
        program: run.program.to_os_string(),
        owner: run.owner.to_string(),
        log: String::new(),
        armed: true,
    };
    match start_all(run, job_env, scratch, &labels, &mut started.log).await {
        Ok(()) => Ok(started),
        Err(failed) => {
            started.stop().await;
            Err(failed)
        }
    }
}

async fn start_all(
    run: &ServicesRun<'_>,
    job_env: &[(String, String)],
    scratch: &std::path::Path,
    labels: &[String],
    log: &mut String,
) -> Result<(), ServicesFailed> {
    // A job handed back and run again keeps its container name; whatever an
    // interrupted earlier run of it left would make the network below exist
    // already.
    remove_services_of(run.program, run.owner).await;
    let network = network_name(run.owner);
    let mut create = cli(run.program);
    create
        .args(["network", "create"])
        .args(labels)
        .arg(&network);
    let created = crate::output_ci_job_in_process_tree(&mut create)
        .await
        .map_err(|error| ServicesFailed {
            exit_code: -1,
            log: format!(
                "[plombir-git-runner] services: need a Docker daemon, and `docker` could not be \
                 run: {error}"
            ),
        })?;
    if !created.status.success() {
        return Err(ServicesFailed {
            exit_code: created.status.code().unwrap_or(-1),
            log: format!(
                "[plombir-git-runner] services: need a Docker daemon, and creating the job's \
                 network {network} failed:\n{}",
                created.log()
            ),
        });
    }

    for spec in run.specs {
        if let Err(failed) = start_one(run, spec, job_env, scratch, labels, &network, log).await {
            return Err(ServicesFailed {
                exit_code: failed.exit_code,
                log: format!("{log}{}", failed.log),
            });
        }
    }
    let deadline = tokio::time::Instant::now() + run.health_timeout;
    for spec in run.specs {
        let name = container_name(run.owner, &spec.key);
        let begun = std::time::Instant::now();
        match wait_until_ready(run.program, &name, deadline, run.health_timeout).await {
            Ok(Readiness::Healthy) => log.push_str(&format!(
                "[plombir-git-runner] service {} is healthy ({}s)\n",
                spec.key,
                begun.elapsed().as_secs()
            )),
            Ok(Readiness::Running) => log.push_str(&format!(
                "[plombir-git-runner] service {} is running; it declares no health check, so \
                 nothing waited for it to be ready\n",
                spec.key
            )),
            Err(reason) => {
                let tail = bounded(cli(run.program).args(["logs", "--tail", "50", &name]))
                    .await
                    .unwrap_or_default();
                return Err(ServicesFailed {
                    exit_code: 1,
                    log: format!(
                        "{log}[plombir-git-runner] service {} {reason}; the job's steps did not \
                         run.\n{}",
                        spec.key,
                        tail.trim_end()
                    ),
                });
            }
        }
    }
    Ok(())
}

enum Readiness {
    Healthy,
    Running,
}

/// Wait until the service is `healthy`, or merely `running` when it has no
/// health check at all — the two answers GitHub waits for.
async fn wait_until_ready(
    program: &OsStr,
    container: &str,
    deadline: tokio::time::Instant,
    timeout: Duration,
) -> Result<Readiness, String> {
    loop {
        let state = bounded(cli(program).args([
            "inspect",
            "--format",
            "{{.State.Status}} {{if .State.Health}}{{.State.Health.Status}}{{else}}none{{end}}",
            container,
        ]))
        .await
        .ok_or_else(|| "could not be inspected".to_string())?;
        match state.split_whitespace().collect::<Vec<_>>().as_slice() {
            ["running", "healthy"] => return Ok(Readiness::Healthy),
            ["running", "none"] => return Ok(Readiness::Running),
            ["running", "unhealthy"] => return Err("reports unhealthy".into()),
            ["running", _] | ["created", _] | ["restarting", _] => {}
            [status, ..] => return Err(format!("stopped before it was ready (status {status})")),
            [] => return Err("could not be inspected".into()),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("was not healthy within {}s", timeout.as_secs()));
        }
        tokio::time::sleep(HEALTH_POLL).await;
    }
}

async fn start_one(
    run: &ServicesRun<'_>,
    spec: &ServiceSpec,
    job_env: &[(String, String)],
    scratch: &std::path::Path,
    labels: &[String],
    network: &str,
    log: &mut String,
) -> Result<(), ServicesFailed> {
    let refused = |reason: String| ServicesFailed {
        exit_code: -1,
        log: format!("[plombir-git-runner] service {}: {reason}", spec.key),
    };
    if !valid_service_key(&spec.key) {
        return Err(refused(
            "the key is not a name a container can carry".into(),
        ));
    }
    let image = expand(&spec.image, job_env);
    crate::image_reference::validate_image_reference(&image).map_err(refused)?;
    for flag in &spec.health {
        let name = flag.split_once('=').map_or(flag.as_str(), |(name, _)| name);
        if !HEALTH_FLAGS.contains(&name) {
            return Err(refused(format!("`{name}` is not a health-check flag")));
        }
    }

    let resolved = match &spec.credentials {
        None => crate::docker_job::resolve_image(run.program, &image).await,
        Some(credentials) => {
            let username = expand(&credentials.username, job_env);
            let password = expand(&credentials.password, job_env);
            let login = scratch.join(format!("service-login-{}", spec.key));
            resolve_with_credentials(run.program, &image, &username, &password, &login).await
        }
    }
    .map_err(|unavailable| ServicesFailed {
        exit_code: unavailable.exit_code,
        log: format!(
            "[plombir-git-runner] service {}: {}",
            spec.key, unavailable.log
        ),
    })?;
    if !resolved.pull_log.is_empty() {
        log.push_str(&resolved.pull_log);
        if !log.ends_with('\n') {
            log.push('\n');
        }
    }

    let env: Vec<(String, String)> = spec
        .env
        .iter()
        .map(|(name, value)| (name.clone(), expand(value, job_env)))
        .collect();
    let left_out: Vec<&str> = env
        .iter()
        .filter(|(name, _)| !crate::job_environment::reaches_container(name))
        .map(|(name, _)| name.as_str())
        .collect();
    if !left_out.is_empty() {
        return Err(refused(format!(
            "env {} cannot be handed to a container",
            left_out.join(", ")
        )));
    }

    let name = container_name(run.owner, &spec.key);
    let mut command = crate::docker_job::client(run.program);
    command
        .env_clear()
        .envs(crate::job_environment::docker_run_environment())
        .stdin(std::process::Stdio::null())
        .envs(
            env.iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
    command.args(service_run_args(
        run,
        spec,
        &name,
        network,
        labels,
        &env,
        &resolved.id,
    ));
    let output = crate::output_ci_job_in_process_tree(&mut command)
        .await
        .map_err(|error| refused(format!("`docker run` could not be started: {error}")))?;
    if !output.status.success() {
        return Err(ServicesFailed {
            exit_code: output.status.code().unwrap_or(-1),
            log: format!(
                "[plombir-git-runner] service {}: docker run exited with code {}\n{}",
                spec.key,
                output.status.code().unwrap_or(-1),
                output.log()
            ),
        });
    }
    log.push_str(&format!(
        "[plombir-git-runner] service {} started from {image} ({})\n",
        spec.key, resolved.id
    ));
    Ok(())
}

/// The `docker run` of one service container, started detached from
/// `image_id` — the ID the image was resolved to, so this client never pulls.
fn service_run_args(
    run: &ServicesRun<'_>,
    spec: &ServiceSpec,
    name: &str,
    network: &str,
    labels: &[String],
    env: &[(String, String)],
    image_id: &str,
) -> Vec<String> {
    let mut args: Vec<String> = [
        "run",
        "-d",
        "--pull",
        "never",
        "--name",
        name,
        "--network",
        network,
        "--network-alias",
        &spec.key,
        "--cap-drop",
        "ALL",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    for capability in SERVICE_CAPABILITIES {
        args.extend(["--cap-add".to_string(), (*capability).to_string()]);
    }
    args.extend([
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
        "--pids-limit".to_string(),
        run.limits.pids().to_string(),
        "--memory".to_string(),
        run.limits.memory().to_string(),
        "--cpus".to_string(),
        run.limits.cpus().to_string(),
    ]);
    args.extend(labels.iter().cloned());
    args.extend(spec.health.iter().cloned());
    for port in &spec.ports {
        let protocol = match port.protocol {
            PortProtocol::Tcp => "tcp",
            PortProtocol::Udp => "udp",
        };
        let host = port.host.map(|host| host.to_string()).unwrap_or_default();
        args.extend([
            "-p".to_string(),
            format!("127.0.0.1:{host}:{}/{protocol}", port.container),
        ]);
    }
    for (key, _) in env {
        args.extend(["-e".to_string(), key.clone()]);
    }
    args.extend(["--".to_string(), image_id.to_string()]);
    args
}

/// [`crate::docker_job::resolve_image`] for a private image: the pull runs with
/// a Docker configuration of its own, in a directory only this process can
/// read, holding the one login — never `docker login` into the operator's.
async fn resolve_with_credentials(
    program: &OsStr,
    image: &str,
    username: &str,
    password: &str,
    directory: &std::path::Path,
) -> Result<crate::docker_job::ResolvedImage, crate::docker_job::ImageUnavailable> {
    let unavailable = |log: String| crate::docker_job::ImageUnavailable { exit_code: -1, log };
    // The job's own step directory: a leftover here is an earlier run of this
    // job's, and nothing else writes into it.
    match std::fs::remove_dir_all(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(unavailable(format!(
                "could not prepare a registry login: {error}"
            )))
        }
    }
    private_config_dir(directory)
        .map_err(|error| unavailable(format!("could not prepare a registry login: {error}")))?;
    let config = docker_config_json(&registry_of(image), username, password);
    write_private(&directory.join("config.json"), config.as_bytes())
        .map_err(|error| unavailable(format!("could not prepare a registry login: {error}")))?;
    let result = crate::docker_job::resolve_image_with_config(program, image, directory).await;
    if let Err(error) = std::fs::remove_dir_all(directory) {
        tracing::warn!(%error, "failed to remove a service's registry login");
    }
    result
}

/// The registry an image reference is pulled from, in the form a Docker
/// configuration keys its logins by.
fn registry_of(image: &str) -> String {
    match image.split_once('/') {
        Some((first, _)) if first.contains('.') || first.contains(':') || first == "localhost" => {
            first.to_string()
        }
        _ => "https://index.docker.io/v1/".to_string(),
    }
}

fn docker_config_json(registry: &str, username: &str, password: &str) -> String {
    use base64::Engine as _;
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
    format!(
        "{{\"auths\":{{{}:{{\"auth\":{}}}}}}}",
        json_string(registry),
        json_string(&auth)
    )
}

fn json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Create `directory` readable by this process's user only.
fn private_config_dir(directory: &std::path::Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(directory)
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_published_on_loopback_and_nowhere_else() {
        assert_eq!(
            parse_port("5432").unwrap(),
            ServicePort {
                host: None,
                container: 5432,
                protocol: PortProtocol::Tcp
            }
        );
        assert_eq!(
            parse_port("8080:80/udp").unwrap(),
            ServicePort {
                host: Some(8080),
                container: 80,
                protocol: PortProtocol::Udp
            }
        );
        for refused in ["0.0.0.0:80:80", "80/sctp", "0", "http", "70000:1"] {
            assert!(parse_port(refused).is_err(), "{refused} was accepted");
        }
    }

    #[test]
    fn only_health_flags_reach_docker_run() {
        assert_eq!(
            parse_health_options(
                "--health-cmd \"redis-cli ping\" --health-interval=10s --health-retries 5"
            )
            .unwrap(),
            [
                "--health-cmd=redis-cli ping",
                "--health-interval=10s",
                "--health-retries=5"
            ]
        );
        assert_eq!(
            parse_health_options("--health-cmd 'pg_isready -U postgres'").unwrap(),
            ["--health-cmd=pg_isready -U postgres"]
        );
        for (options, named) in [
            ("--privileged", "--privileged"),
            ("--health-cmd true -v /:/host", "-v"),
            ("--network=host", "--network=host"),
            ("--health-cmd", "needs a value"),
            ("--health-cmd 'unterminated", "never closed"),
        ] {
            let error = parse_health_options(options).unwrap_err();
            assert!(error.contains(named), "{options}: {error}");
        }
    }

    #[test]
    fn a_template_takes_only_whole_references_the_job_carries() {
        let env = vec![
            ("PG_PASSWORD".to_string(), "s3cret".to_string()),
            ("MATRIX_PG".to_string(), "16".to_string()),
        ];
        assert_eq!(expand("postgres:${MATRIX_PG}", &env), "postgres:16");
        assert_eq!(expand("${PG_PASSWORD}", &env), "s3cret");
        assert_eq!(
            expand("$PG_PASSWORD ${MISSING} ${", &env),
            "$PG_PASSWORD ${MISSING} ${"
        );
    }

    #[test]
    fn service_keys_are_container_name_safe() {
        for good in ["postgres", "redis-7", "db_1", "A"] {
            assert!(valid_service_key(good), "{good}");
        }
        for bad in ["", "-x", "a b", "a.b", "a/b", &"x".repeat(64)] {
            assert!(!valid_service_key(bad), "{bad}");
        }
    }

    #[test]
    fn a_service_runs_sandboxed_on_the_jobs_network_with_its_health_check() {
        let limits = crate::docker_job::JobLimits::default();
        let spec = ServiceSpec {
            key: "postgres".into(),
            image: "postgres:16".into(),
            env: BTreeMap::from([("POSTGRES_PASSWORD".into(), "x".into())]),
            ports: vec![parse_port("5432:5432").unwrap()],
            credentials: None,
            health: vec!["--health-cmd=pg_isready".into()],
        };
        let run = ServicesRun {
            program: OsStr::new("docker"),
            owner: "job-1",
            specs: std::slice::from_ref(&spec),
            limits: &limits,
            health_timeout: HEALTH_TIMEOUT,
        };
        let env = vec![("POSTGRES_PASSWORD".to_string(), "x".to_string())];
        let args = service_run_args(
            &run,
            &spec,
            "job-1-svc-postgres",
            "job-1-net",
            &[],
            &env,
            "sha256:abc",
        );
        let joined = args.join(" ");
        for expected in [
            "--network job-1-net",
            "--network-alias postgres",
            "--cap-drop ALL",
            "--security-opt no-new-privileges",
            "--health-cmd=pg_isready",
            "-p 127.0.0.1:5432:5432/tcp",
            "-e POSTGRES_PASSWORD",
            "--pull never",
        ] {
            assert!(joined.contains(expected), "missing {expected:?}: {joined}");
        }
        assert!(!joined.contains("=x"), "a value reached the argv: {joined}");
        assert!(!joined.contains("--privileged") && !joined.contains("docker.sock"));
        assert_eq!(args.last().map(String::as_str), Some("sha256:abc"));
    }

    #[test]
    fn a_registry_login_is_keyed_by_the_images_registry() {
        assert_eq!(registry_of("ghcr.io/acme/db:1"), "ghcr.io");
        assert_eq!(registry_of("localhost:5000/db"), "localhost:5000");
        assert_eq!(registry_of("acme/db"), "https://index.docker.io/v1/");
        assert_eq!(registry_of("postgres"), "https://index.docker.io/v1/");
        assert_eq!(
            docker_config_json("ghcr.io", "u", "p"),
            "{\"auths\":{\"ghcr.io\":{\"auth\":\"dTpw\"}}}"
        );
    }

    #[test]
    fn a_leftover_is_judged_only_on_the_machine_that_started_it() {
        let starter = Starter {
            host: "runner-a".into(),
            boot: "boot-1".into(),
            pid: std::process::id(),
        };
        let me = std::process::id().to_string();
        assert!(
            !starter.is_gone("runner-b", "boot-0", "1"),
            "another machine's"
        );
        assert!(
            starter.is_gone("runner-a", "boot-0", &me),
            "an earlier boot's"
        );
        assert!(
            !starter.is_gone("runner-a", "boot-1", &me),
            "this process's own"
        );
        assert!(
            starter.is_gone("runner-a", "boot-1", "4294967295"),
            "a dead process's"
        );
        assert!(!starter.is_gone("runner-a", "boot-1", "garbage"));
    }

    #[test]
    fn a_dead_runners_steps_container_is_swept_and_a_live_or_foreign_one_is_not() {
        let starter = Starter {
            host: "runner-a".into(),
            boot: "boot-1".into(),
            pid: std::process::id(),
        };
        let me = std::process::id().to_string();
        let dead = "4294967295";
        let ps = [
            format!("job-dead\t\tjob-dead\trunner-a\tboot-1\t{dead}"),
            format!("job-live\t\tjob-live\trunner-a\tboot-1\t{me}"),
            format!("job-foreign\t\tjob-foreign\trunner-b\tboot-1\t{dead}"),
            format!("job-rebooted\t\tjob-rebooted\trunner-a\tboot-0\t{me}"),
            format!("job-dead-svc-db\tjob-dead\t\trunner-a\tboot-1\t{dead}"),
            format!("job-live-svc-db\tjob-live\t\trunner-a\tboot-1\t{me}"),
        ]
        .join("\n");
        let networks = format!("job-gone-net\tjob-gone\t\trunner-a\tboot-0\t{me}");
        let found = leftovers(&starter, &[ps, networks]);
        assert_eq!(
            found.steps.iter().map(String::as_str).collect::<Vec<_>>(),
            ["job-dead", "job-rebooted"]
        );
        assert_eq!(
            found
                .services_of
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["job-dead", "job-gone"]
        );
    }

    #[test]
    fn the_sweep_asks_each_listing_for_the_name_field_it_has() {
        // `docker network ls` has `.Name` and refuses a template naming
        // `.Names`, which left every orphaned network unlisted.
        assert!(sweep_format(".Name").starts_with("{{.Name}}\t"));
        assert!(sweep_format(".Names").starts_with("{{.Names}}\t"));
        assert_eq!(sweep_format(".Name").split('\t').count(), 6);
    }

    /// The sweep on a real daemon: a dead process's steps container and its
    /// lone network go, a live process's container stays.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "needs a Docker daemon and the alpine:3.20 image"]
    async fn the_sweep_removes_a_dead_runners_container_and_network_on_a_real_daemon() {
        let Some(starter) = Starter::this_process() else {
            return;
        };
        let docker = OsStr::new("docker");
        let tag = format!("plombir-git-sweep-test-{}", std::process::id());
        let dead = format!("{tag}-dead");
        let live = format!("{tag}-live");
        let lone = format!("{tag}-lone");
        let labels = |steps: Option<&str>, owner: Option<&str>, pid: &str| {
            let mut labels = vec![
                format!("{HOST_LABEL}={}", starter.host),
                format!("{BOOT_LABEL}={}", starter.boot),
                format!("{PID_LABEL}={pid}"),
            ];
            labels.extend(steps.map(|name| format!("{STEPS_LABEL}={name}")));
            labels.extend(owner.map(|name| format!("{OWNER_LABEL}={name}")));
            labels
                .into_iter()
                .flat_map(|label| ["--label".to_string(), label])
                .collect::<Vec<_>>()
        };
        let me = std::process::id().to_string();
        for (name, pid) in [(&dead, "4294967295"), (&live, me.as_str())] {
            let started = bounded(
                cli(docker)
                    .args(["run", "-d", "--name", name])
                    .args(labels(Some(name), None, pid))
                    .args(["alpine:3.20", "sleep", "600"]),
            )
            .await;
            assert!(started.is_some(), "could not start {name}");
        }
        let created = bounded(
            cli(docker)
                .args(["network", "create"])
                .args(labels(None, Some(&lone), "4294967295"))
                .arg(network_name(&lone)),
        )
        .await;
        assert!(created.is_some(), "could not create the lone network");

        sweep_orphans(docker).await;

        let exists = |args: Vec<String>| async move {
            bounded(cli(docker).args(&args))
                .await
                .is_some_and(|stdout| !stdout.trim().is_empty())
        };
        let container = |name: &str| {
            vec![
                "ps".to_string(),
                "-aq".to_string(),
                "--filter".to_string(),
                format!("name=^{name}$"),
            ]
        };
        let dead_left = exists(container(&dead)).await;
        let live_left = exists(container(&live)).await;
        let network_left = exists(vec![
            "network".into(),
            "ls".into(),
            "-q".into(),
            "--filter".into(),
            format!("name=^{}$", network_name(&lone)),
        ])
        .await;
        let _ = bounded(cli(docker).args(["rm", "-f", &dead, &live])).await;
        let _ = bounded(cli(docker).args(["network", "rm", &network_name(&lone)])).await;
        assert!(
            !dead_left,
            "a dead runner's steps container survived the sweep"
        );
        assert!(live_left, "a live runner's steps container was swept");
        assert!(
            !network_left,
            "a dead runner's lone network survived the sweep"
        );
    }

    #[test]
    fn a_steps_container_is_labelled_for_the_sweep_and_not_as_a_service() {
        let labels = steps_container_labels("job-7").join(" ");
        assert!(
            labels.contains(&format!("--label {STEPS_LABEL}=job-7")),
            "{labels}"
        );
        assert!(!labels.contains(OWNER_LABEL), "{labels}");
        if Starter::this_process().is_some() {
            for label in [HOST_LABEL, BOOT_LABEL, PID_LABEL] {
                assert!(labels.contains(&format!("--label {label}=")), "{labels}");
            }
        }
    }
}
