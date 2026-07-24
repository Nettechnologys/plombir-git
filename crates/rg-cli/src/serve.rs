//! `forgekeep serve` subcommand: TOML configuration model, config resolution,
//! validation, and the HTTP + SSH server bootstrap.

use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::Context;

use crate::admin::validate_jwt_secret;
use crate::telemetry;

/// TOML configuration file structure.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct ConfigFile {
    #[serde(default)]
    server: ServerConfig,
    #[serde(default)]
    database: DatabaseConfig,
    #[serde(default)]
    auth: AuthConfig,
    #[serde(default)]
    ci: CiConfig,
    #[serde(default)]
    releases: ReleasesConfig,
    #[serde(default)]
    rate_limit: RateLimitConfig,
    #[serde(default)]
    smtp: SmtpConfig,
    #[serde(default)]
    tls: TlsConfig,
    #[serde(default)]
    logging: LoggingConfig,
    #[serde(default)]
    audit: AuditConfig,
    #[serde(default)]
    timeouts: TimeoutConfig,
    #[serde(default)]
    webhooks: WebhooksConfig,
    #[serde(default)]
    observability: ObservabilityConfig,
    /// Server external URL (e.g., "https://git.example.com"). Used for SSO callbacks.
    #[serde(default)]
    external_url: Option<String>,
}

// No `#[allow(dead_code)]` here on purpose: every field below must actually be
// consumed by `resolve_settings` / `run_serve`. If a key is added to the struct
// (and to `forgekeep.example.toml`) but never wired up, the dead-code lint says
// so at build time instead of the operator finding out that their setting is
// silently ignored.
#[derive(Debug, serde::Deserialize, Default)]
struct ServerConfig {
    repo_root: Option<String>,
    http_addr: Option<String>,
    ssh_addr: Option<String>,
    host_key: Option<String>,
    /// External-facing URL for SSO callbacks and links (e.g., "https://git.example.com")
    external_url: Option<String>,
    /// Grace window (seconds) for draining in-flight requests and the CI-log
    /// queue on SIGTERM/ctrl_c before the process is forced down (default: 30).
    shutdown_grace_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct DatabaseConfig {
    url: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[allow(dead_code)]
struct AuthConfig {
    jwt_secret: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct CiConfig {
    #[serde(default)]
    docker: Option<bool>,
    #[serde(default)]
    external_runners: Option<bool>,
    /// Allow imageless CI jobs to run as a shell on the host (default false).
    #[serde(default)]
    allow_host_runner: Option<bool>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[allow(dead_code)]
struct ReleasesConfig {
    /// Enable opt-in Ed25519 provenance attestation of release assets (default
    /// false). Also settable via `FORGEKEEP_ATTESTATION_ENABLED=1`, which wins.
    #[serde(default)]
    attestation_enabled: Option<bool>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct RateLimitConfig {
    max: Option<u32>,
    window_secs: Option<u64>,
    #[serde(default)]
    trusted_proxies: Vec<String>,
    /// Hard cap on distinct client keys the limiter tracks (memory guard).
    max_keys: Option<usize>,
    /// Stricter per-IP cap for credential endpoints (register/login).
    auth_max: Option<u32>,
    /// Window (seconds) for the credential-endpoint limiter.
    auth_window_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct SmtpConfig {
    host: Option<String>,
    port: Option<u16>,
    user: Option<String>,
    pass: Option<String>,
    from: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct TlsConfig {
    cert: Option<String>,
    key: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct LoggingConfig {
    file: Option<String>,
    max_size_mb: Option<u64>,
    max_files: Option<usize>,
}

#[derive(Debug, serde::Deserialize, Default)]
struct AuditConfig {
    enabled: Option<bool>,
    archive_dir: Option<String>,
    archive_after_days: Option<i64>,
    interval_minutes: Option<u64>,
    batch_size: Option<u64>,
}

/// `[observability]` — OpenTelemetry distributed-tracing (OTLP) export. All
/// fields optional; with no endpoint set (here or via the `OTEL_EXPORTER_OTLP_*`
/// env vars) OTel tracing stays off and only Prometheus `/metrics` + logs run.
#[derive(Debug, serde::Deserialize, Default)]
#[allow(dead_code)]
struct ObservabilityConfig {
    /// OTLP/HTTP endpoint, e.g. "http://localhost:4318" (the `/v1/traces` path is
    /// appended automatically). Overridden by `OTEL_EXPORTER_OTLP_ENDPOINT` /
    /// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`. Unset ⇒ tracing disabled.
    otlp_endpoint: Option<String>,
    /// `service.name` resource attribute (default "forgekeep"). Overridden by
    /// `OTEL_SERVICE_NAME`.
    service_name: Option<String>,
    /// Head sampling ratio in 0.0..=1.0 (default 1.0 = sample every trace).
    sample_ratio: Option<f64>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[allow(dead_code)]
struct WebhooksConfig {
    /// Shared secret for verifying HMAC-SHA256 signatures on *inbound* external
    /// webhooks (`/webhooks/external/*`). Unset = signature checking disabled
    /// (endpoints rely on JWT/PAT auth alone). Also settable via the
    /// `FORGEKEEP_EXTERNAL_WEBHOOK_SECRET` environment variable, which wins.
    external_secret: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct TimeoutConfig {
    /// CI job timeout in seconds (default: 3600 = 1 hour).
    #[serde(default = "default_job_timeout")]
    job_secs: u64,
    /// Git CLI command timeout in seconds (default: 120).
    #[serde(default = "default_git_timeout")]
    git_cmd_secs: u64,
    /// Wall-clock timeout in seconds for the streaming git transport —
    /// upload-pack (clone/fetch) and receive-pack (push). Bounds a hung or
    /// pathologically slow `git` subprocess so it can't hold a connection +
    /// process indefinitely. More generous than `git_cmd_secs` because pack
    /// generation over a large repo is legitimately slower than a metadata
    /// command. 0 disables the bound (default: 300).
    #[serde(default = "default_git_stream_timeout")]
    git_stream_secs: u64,
    /// Idle timeout in seconds for the streaming git transport, layered on top
    /// of `git_stream_secs`. The git stream is killed if it makes no read/write
    /// progress for this long — catching a slow-drip push/fetch that dribbles
    /// bytes to stay under the wall-clock budget. Applies to SSH (stream
    /// wrapper) and HTTP (request-body buffering). 0 disables the idle watchdog
    /// (default: 30).
    #[serde(default = "default_git_idle_timeout")]
    git_idle_secs: u64,
    /// Database connect timeout in seconds (default: 10).
    #[serde(default = "default_db_connect_timeout")]
    db_connect_secs: u64,
    /// Database idle timeout in seconds (default: 600).
    #[serde(default = "default_db_idle_timeout")]
    db_idle_secs: u64,
}

// Hand-written so an *entirely omitted* `[timeouts]` table (which routes through
// `TimeoutConfig::default()` via the parent `#[serde(default)]`, NOT through the
// per-field `#[serde(default = ...)]` functions) still lands on the documented
// non-zero defaults. A `#[derive(Default)]` here would silently zero every knob
// — a 0 acquire/idle timeout makes the DB pool churn/unusable and a 0 git
// timeout makes every git command elapse instantly.
impl Default for TimeoutConfig {
    fn default() -> Self {
        Self {
            job_secs: default_job_timeout(),
            git_cmd_secs: default_git_timeout(),
            git_stream_secs: default_git_stream_timeout(),
            git_idle_secs: default_git_idle_timeout(),
            db_connect_secs: default_db_connect_timeout(),
            db_idle_secs: default_db_idle_timeout(),
        }
    }
}

fn default_job_timeout() -> u64 {
    3600
}
fn default_git_timeout() -> u64 {
    120
}
fn default_git_stream_timeout() -> u64 {
    300
}
fn default_git_idle_timeout() -> u64 {
    30
}
fn default_db_connect_timeout() -> u64 {
    10
}
fn default_db_idle_timeout() -> u64 {
    600
}

/// Check that `path` is an existing, regular file *before* something tries to
/// read it, so a bad path fails with a message that names the path and says
/// what to do about it.
///
/// The motivating incident: a Docker bind-mount whose source `forgekeep.toml`
/// did not exist made the daemon auto-create a **directory** at the mount
/// point, and `read_to_string` reported nothing but `Is a directory (os error
/// 21)` — no path, no cause. The container crash-looped 268 times on it.
fn ensure_regular_file(path: &std::path::Path, what: &str, hint: &str) -> anyhow::Result<()> {
    let shown = path.display();
    match std::fs::metadata(path) {
        Ok(md) if md.is_dir() => anyhow::bail!(
            "{what} path `{shown}` is a directory, not a file — a Docker bind-mount \
             likely auto-created it because the source file was missing; {hint}"
        ),
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            anyhow::bail!("{what} `{shown}` does not exist — {hint}")
        }
        Err(e) => Err(anyhow::Error::new(e))
            .with_context(|| format!("failed to stat {what} `{shown}`")),
    }
}

/// Extract the on-disk file a SQLite URL points at, or `None` for a
/// non-SQLite/in-memory URL. Used only to turn an opaque "unable to open
/// database file" into a message naming the directory that has to be writable.
fn sqlite_file_path(db_url: &str) -> Option<PathBuf> {
    let rest = db_url
        .strip_prefix("sqlite://")
        .or_else(|| db_url.strip_prefix("sqlite:"))?;
    let path = rest.split('?').next().unwrap_or("");
    if path.is_empty() || path == ":memory:" {
        return None;
    }
    Some(PathBuf::from(path))
}

/// True when the process can create a file in `dir` right now.
fn dir_is_writable(dir: &std::path::Path) -> bool {
    let probe = dir.join(".forgekeep_db_write_test");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Attach the uid/ownership diagnostic to a SQLite connection failure caused by
/// an unwritable data directory.
///
/// SQLite reports that case as `unable to open database file` with no path and
/// no reason, and it is the single most likely first-boot failure of a
/// container whose `/data` bind-mount is owned by the host uid: the WAL and
/// `-shm` sidecar files need write access to the *directory*, not just the
/// database file. Non-permission failures (corrupt file, bad URL, Postgres,
/// MySQL) are returned untouched.
fn annotate_db_open_error(error: anyhow::Error, db_url: &str) -> anyhow::Error {
    let Some(path) = sqlite_file_path(db_url) else {
        return error;
    };
    let dir = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    if dir_is_writable(dir) {
        return error;
    }

    let mut context = format!(
        "SQLite database `{}` could not be opened: `{}` is not writable by the server, and SQLite \
         needs to create the `-wal` / `-shm` sidecar files next to the database",
        path.display(),
        dir.display()
    );
    if let Some(hint) = rg_core::platform::fs::ownership_hint(dir) {
        context.push_str(&format!("\n  hint: {hint}"));
    }
    error.context(context)
}

/// Remediation appended to every `--config` failure: the file the deployer was
/// supposed to create in the first place.
const CONFIG_FILE_HINT: &str =
    "create it first: `cp forgekeep.example.toml forgekeep.toml` (and bind-mount that file, \
     not a directory)";

fn load_config_file(path: &str) -> anyhow::Result<ConfigFile> {
    ensure_regular_file(std::path::Path::new(path), "config file", CONFIG_FILE_HINT)?;
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file `{path}`"))?;
    let config: ConfigFile = toml::from_str(&content)
        .with_context(|| format!("failed to parse config file `{path}` as TOML"))?;
    tracing::info!(path = %path, "Loaded configuration file");
    Ok(config)
}

/// Wait for the first OS shutdown signal: ctrl_c (SIGINT) on all platforms,
/// plus SIGTERM on Unix (the signal `kill`/systemd/Docker send on stop).
async fn wait_for_shutdown_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::error!(%e, "failed to install ctrl_c handler");
            std::future::pending::<()>().await;
        }
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::error!(%e, "failed to install SIGTERM handler");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("received ctrl_c — initiating graceful shutdown"),
        _ = terminate => tracing::info!("received SIGTERM — initiating graceful shutdown"),
    }
}

fn parse_rate_limit_trusted_proxies(values: &[String]) -> anyhow::Result<Vec<IpAddr>> {
    values
        .iter()
        .map(|value| {
            value
                .parse::<IpAddr>()
                .with_context(|| format!("invalid rate_limit trusted proxy IP: {value}"))
        })
        .collect()
}

/// Validate critical configuration before starting servers.
/// Refuses to start with dangerous defaults or invalid settings.
fn validate_config(
    jwt_secret: &str,
    repo_root: &std::path::Path,
    tls_config: &Option<(PathBuf, PathBuf)>,
) -> anyhow::Result<()> {
    // 1. Validate JWT secret (refuse default, warn if too short)
    validate_jwt_secret(jwt_secret, "config")?;

    // 2. Verify repo_root is writable. A bind-mounted host directory owned by
    //    the wrong uid fails exactly here, so the message carries the uid/chown
    //    diagnostic rather than a bare `Permission denied (os error 13)`.
    let test_file = repo_root.join(".write_test");
    std::fs::write(&test_file, "test").map_err(|e| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "repo_root",
            repo_root,
            &e,
            "the server must be able to write into repo_root",
        ))
    })?;
    std::fs::remove_file(&test_file)?;

    // 3. Verify TLS files exist if configured. `exists()` alone is not enough:
    //    a bind-mount of a missing source file leaves a *directory* behind,
    //    which passes an existence check and then fails deep inside rustls.
    if let Some((ref cert, ref key)) = tls_config {
        ensure_regular_file(
            cert,
            "TLS certificate",
            "point `--tls-cert` / `[tls].cert` at an existing PEM file",
        )?;
        ensure_regular_file(
            key,
            "TLS private key",
            "point `--tls-key` / `[tls].key` at an existing PEM file",
        )?;
    }

    tracing::info!("Configuration validation passed");
    Ok(())
}

/// Reject a numeric config knob left at `0` when every downstream consumer
/// treats `0` as broken rather than as a meaningful "disabled" sentinel:
/// a zero DB acquire/idle timeout makes the SQLite/MySQL pool churn or become
/// unusable, a zero rate-limit window is nonsensical, a zero archiver interval
/// busy-loops, etc.
///
/// Knobs that *do* document `0` as "disable the bound" are deliberately never
/// routed through here: `timeouts.git_stream_secs` (see `with_git_timeout`),
/// `timeouts.job_secs` (see `PipelineRunner::set_job_timeout`),
/// `rate_limit.max`, `rate_limit.max_keys`, and `rate_limit.auth_max`.
fn require_positive(key: &str, value: u64) -> anyhow::Result<()> {
    if value == 0 {
        anyhow::bail!(
            "config `{key}` must be >= 1 (got 0); remove the key to use its default \
             or set a positive value"
        );
    }
    Ok(())
}

/// Range-validate the always-consumed numeric timeout / rate-limit knobs, whose
/// `0` values are silently accepted by serde `#[serde(default)]` but break the
/// consumer. Extracted as a pure function so the boundary behaviour stays
/// unit-testable without booting the whole server. Audit-archiver and SMTP-port
/// ranges are validated closer to their (conditional) consumers.
fn validate_numeric_ranges(
    git_cmd_secs: u64,
    db_connect_secs: u64,
    db_idle_secs: u64,
    rate_limit_window_secs: u64,
    rate_limit_auth_window_secs: u64,
) -> anyhow::Result<()> {
    require_positive("timeouts.git_cmd_secs", git_cmd_secs)?;
    require_positive("timeouts.db_connect_secs", db_connect_secs)?;
    require_positive("timeouts.db_idle_secs", db_idle_secs)?;
    require_positive("rate_limit.window_secs", rate_limit_window_secs)?;
    require_positive("rate_limit.auth_window_secs", rate_limit_auth_window_secs)?;
    Ok(())
}

/// Built-in defaults for the settings that exist both as a CLI flag and as a
/// config-file key. They live here rather than in clap's `default_value` on
/// purpose: a clap default is indistinguishable from a value the operator
/// typed, so with one the config file could never win over "the flag was not
/// passed" — which is exactly why `[server].repo_root` and `[database].url`
/// were silently ignored for every `forgekeep serve --config …` deployment.
const DEFAULT_REPO_ROOT: &str = "./repos";
const DEFAULT_HTTP_ADDR: &str = "0.0.0.0:8080";
const DEFAULT_SSH_ADDR: &str = "0.0.0.0:2222";
const DEFAULT_DB_URL: &str = "sqlite://./forgekeep.db?mode=rwc";
const DEFAULT_SMTP_PORT: u16 = 587;
const DEFAULT_RATE_LIMIT_MAX: u32 = 0;
const DEFAULT_RATE_LIMIT_WINDOW: u64 = 60;
const DEFAULT_LOG_MAX_SIZE_MB: u64 = 10;
const DEFAULT_LOG_MAX_FILES: usize = 5;

/// The CLI half of every dual-source knob, resolved against the config file by
/// [`resolve_settings`]. `None` means "flag not passed" — never a default.
#[derive(Debug, Default)]
struct CliSettings {
    repo_root: Option<String>,
    http_addr: Option<String>,
    ssh_addr: Option<String>,
    host_key: Option<String>,
    db_url: Option<String>,
    rate_limit_max: Option<u32>,
    rate_limit_window: Option<u64>,
    smtp_port: Option<u16>,
    log_max_size_mb: Option<u64>,
    log_max_files: Option<usize>,
}

/// The same knobs after `CLI arg > config file > built-in default` has been
/// applied.
#[derive(Debug, PartialEq, Eq)]
struct ResolvedSettings {
    repo_root: String,
    http_addr: String,
    ssh_addr: String,
    host_key: Option<String>,
    db_url: String,
    rate_limit_max: u32,
    rate_limit_window: u64,
    smtp_port: u16,
    log_max_size_mb: u64,
    log_max_files: usize,
}

/// Apply the documented `CLI arg > config file > built-in default` precedence
/// to every setting that has both a flag and a config key.
///
/// Extracted as a pure function so the wiring (which config key feeds which
/// flag) is unit-testable without booting a server — the original bug was a
/// missing wire, not a bad value.
fn resolve_settings(cli: CliSettings, cfg: Option<&ConfigFile>) -> ResolvedSettings {
    let server = cfg.map(|c| &c.server);
    ResolvedSettings {
        repo_root: cli
            .repo_root
            .or_else(|| server.and_then(|s| s.repo_root.clone()))
            .unwrap_or_else(|| DEFAULT_REPO_ROOT.to_string()),
        http_addr: cli
            .http_addr
            .or_else(|| server.and_then(|s| s.http_addr.clone()))
            .unwrap_or_else(|| DEFAULT_HTTP_ADDR.to_string()),
        ssh_addr: cli
            .ssh_addr
            .or_else(|| server.and_then(|s| s.ssh_addr.clone()))
            .unwrap_or_else(|| DEFAULT_SSH_ADDR.to_string()),
        host_key: cli
            .host_key
            .or_else(|| server.and_then(|s| s.host_key.clone())),
        db_url: cli
            .db_url
            .or_else(|| cfg.and_then(|c| c.database.url.clone()))
            .unwrap_or_else(|| DEFAULT_DB_URL.to_string()),
        rate_limit_max: cli
            .rate_limit_max
            .or_else(|| cfg.and_then(|c| c.rate_limit.max))
            .unwrap_or(DEFAULT_RATE_LIMIT_MAX),
        rate_limit_window: cli
            .rate_limit_window
            .or_else(|| cfg.and_then(|c| c.rate_limit.window_secs))
            .unwrap_or(DEFAULT_RATE_LIMIT_WINDOW),
        smtp_port: cli
            .smtp_port
            .or_else(|| cfg.and_then(|c| c.smtp.port))
            .unwrap_or(DEFAULT_SMTP_PORT),
        log_max_size_mb: cli
            .log_max_size_mb
            .or_else(|| cfg.and_then(|c| c.logging.max_size_mb))
            .unwrap_or(DEFAULT_LOG_MAX_SIZE_MB),
        log_max_files: cli
            .log_max_files
            .or_else(|| cfg.and_then(|c| c.logging.max_files))
            .unwrap_or(DEFAULT_LOG_MAX_FILES),
    }
}

/// Initialise and run the ForgeKeep server (HTTP + SSH).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_serve(
    repo_root: Option<String>,
    http_addr: Option<String>,
    ssh_addr: Option<String>,
    host_key: Option<String>,
    db_url: Option<String>,
    jwt_secret: Option<String>,
    docker: bool,
    external_runners: bool,
    allow_host_runner: bool,
    rate_limit_max: Option<u32>,
    rate_limit_window: Option<u64>,
    rate_limit_trusted_proxies: Vec<String>,
    smtp_host: Option<String>,
    smtp_port: Option<u16>,
    smtp_user: Option<String>,
    smtp_pass: Option<String>,
    smtp_from: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    config: Option<String>,
    log_file: Option<String>,
    log_max_size_mb: Option<u64>,
    log_max_files: Option<usize>,
) -> anyhow::Result<()> {
    // ── Load config file (if specified) ────────────────────────
    let cfg = if let Some(config_path) = &config {
        Some(load_config_file(config_path.as_str())?)
    } else {
        None
    };

    // Resolve JWT secret: env var > CLI args > config file > error
    let resolved_jwt_secret = if let Some(env_secret) =
        rg_core::env_compat::env_var_compat("FORGEKEEP_JWT_SECRET", "IRONFORGE_JWT_SECRET")
    {
        validate_jwt_secret(&env_secret, "environment variable FORGEKEEP_JWT_SECRET")?;
        tracing::info!("Using JWT secret from environment variable FORGEKEEP_JWT_SECRET");
        env_secret
    } else if let Some(cli_secret) = jwt_secret {
        validate_jwt_secret(&cli_secret, "--jwt-secret CLI argument")?;
        cli_secret
    } else if let Some(cfg_secret) = cfg.as_ref().and_then(|c| c.auth.jwt_secret.clone()) {
        validate_jwt_secret(&cfg_secret, "config file [auth].jwt_secret")?;
        cfg_secret
    } else {
        anyhow::bail!(
            "No JWT secret provided. Set FORGEKEEP_JWT_SECRET, use --jwt-secret, or configure [auth].jwt_secret in config file"
        );
    };

    // Resolve every dual-source knob in one place: CLI args > config file >
    // built-in default.
    let ResolvedSettings {
        repo_root: resolved_repo_root,
        http_addr: resolved_http_addr,
        ssh_addr: resolved_ssh_addr,
        host_key: resolved_host_key,
        db_url: resolved_db_url,
        rate_limit_max: resolved_rate_limit_max,
        rate_limit_window: resolved_rate_limit_window,
        smtp_port: resolved_smtp_port,
        log_max_size_mb: resolved_log_max_size_mb,
        log_max_files: resolved_log_max_files,
    } = resolve_settings(
        CliSettings {
            repo_root,
            http_addr,
            ssh_addr,
            host_key,
            db_url,
            rate_limit_max,
            rate_limit_window,
            smtp_port,
            log_max_size_mb,
            log_max_files,
        },
        cfg.as_ref(),
    );

    let resolved_docker = docker || cfg.as_ref().and_then(|c| c.ci.docker).unwrap_or(false);
    let resolved_external_runners = external_runners
        || cfg
            .as_ref()
            .and_then(|c| c.ci.external_runners)
            .unwrap_or(false);
    let resolved_allow_host_runner = allow_host_runner
        || cfg
            .as_ref()
            .and_then(|c| c.ci.allow_host_runner)
            .unwrap_or(false);
    // Env var wins over config file; both default off (opt-in).
    let resolved_attestation_enabled = match std::env::var("FORGEKEEP_ATTESTATION_ENABLED") {
        Ok(v) => matches!(v.trim(), "1" | "true" | "yes" | "on"),
        Err(_) => cfg
            .as_ref()
            .and_then(|c| c.releases.attestation_enabled)
            .unwrap_or(false),
    };
    let resolved_rate_limit_trusted_proxy_values = if !rate_limit_trusted_proxies.is_empty() {
        rate_limit_trusted_proxies
    } else {
        cfg.as_ref()
            .map(|c| c.rate_limit.trusted_proxies.clone())
            .unwrap_or_default()
    };
    let resolved_rate_limit_trusted_proxies =
        parse_rate_limit_trusted_proxies(&resolved_rate_limit_trusted_proxy_values)?;
    // Config-file-only knobs (no CLI flag): memory cap + credential-endpoint
    // limiter. 0 = use the library default cap; auth defaults are always-on so
    // registration spam is throttled even when the global limiter is disabled.
    let resolved_rate_limit_max_keys = cfg
        .as_ref()
        .and_then(|c| c.rate_limit.max_keys)
        .unwrap_or(0);
    let resolved_rate_limit_auth_max = cfg
        .as_ref()
        .and_then(|c| c.rate_limit.auth_max)
        .unwrap_or(10);
    let resolved_rate_limit_auth_window = cfg
        .as_ref()
        .and_then(|c| c.rate_limit.auth_window_secs)
        .unwrap_or(60);

    // SMTP: CLI takes precedence, fallback to config (the port is resolved
    // alongside the other dual-source knobs above).
    let (resolved_smtp_host, resolved_smtp_user, resolved_smtp_pass, resolved_smtp_from) = {
        let h = smtp_host.or_else(|| cfg.as_ref().and_then(|c| c.smtp.host.clone()));
        let u = smtp_user.or_else(|| cfg.as_ref().and_then(|c| c.smtp.user.clone()));
        let pw = smtp_pass.or_else(|| cfg.as_ref().and_then(|c| c.smtp.pass.clone()));
        let f = smtp_from.or_else(|| cfg.as_ref().and_then(|c| c.smtp.from.clone()));
        (h, u, pw, f)
    };

    // TLS: CLI takes precedence, fallback to config
    let resolved_tls_cert = tls_cert.or_else(|| cfg.as_ref().and_then(|c| c.tls.cert.clone()));
    let resolved_tls_key = tls_key.or_else(|| cfg.as_ref().and_then(|c| c.tls.key.clone()));

    // Logging: CLI takes precedence, fallback to config (rotation sizes are
    // resolved alongside the other dual-source knobs above).
    let resolved_log_file = log_file.or_else(|| cfg.as_ref().and_then(|c| c.logging.file.clone()));

    // External URL: CLI takes precedence, fallback to config
    let resolved_external_url = cfg
        .as_ref()
        .and_then(|c| c.external_url.clone())
        .or_else(|| cfg.as_ref().and_then(|c| c.server.external_url.clone()));

    // Inbound-webhook HMAC secret: env var wins, fallback to config file.
    // Unset ⇒ signature verification stays off (endpoints are auth-gated).
    let resolved_external_webhook_secret = rg_core::env_compat::env_var_compat(
        "FORGEKEEP_EXTERNAL_WEBHOOK_SECRET",
        "IRONFORGE_EXTERNAL_WEBHOOK_SECRET",
    )
    .or_else(|| cfg.as_ref().and_then(|c| c.webhooks.external_secret.clone()));
    if resolved_external_webhook_secret.is_some() {
        tracing::info!("Inbound external-webhook HMAC-SHA256 verification enabled");
    }

    // Timeouts from config (with defaults)
    let resolved_job_timeout = cfg.as_ref().map(|c| c.timeouts.job_secs).unwrap_or(3600);
    let resolved_git_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.git_cmd_secs)
        .unwrap_or_else(default_git_timeout);
    let resolved_git_stream_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.git_stream_secs)
        .unwrap_or_else(default_git_stream_timeout);
    let resolved_git_idle_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.git_idle_secs)
        .unwrap_or_else(default_git_idle_timeout);
    let resolved_db_connect_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.db_connect_secs)
        .unwrap_or_else(default_db_connect_timeout);
    let resolved_db_idle_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.db_idle_secs)
        .unwrap_or_else(default_db_idle_timeout);

    // Range-check the numeric knobs before anything consumes them: reject a
    // silently-accepted `0` (e.g. `db_connect_secs = 0`) with a clear message
    // rather than booting into a churning pool or a nonsensical rate window.
    validate_numeric_ranges(
        resolved_git_timeout,
        resolved_db_connect_timeout,
        resolved_db_idle_timeout,
        resolved_rate_limit_window,
        resolved_rate_limit_auth_window,
    )?;

    // ── Initialize logging + tracing ───────────────────────────
    // Build the log writer (rolling file or stdout), then let `telemetry::init`
    // compose the fmt layer with an optional OTLP export layer. The returned
    // guard owns the non-blocking appender worker and the OTLP tracer provider;
    // it is flushed on shutdown at the end of this function.
    use tracing_subscriber::fmt::writer::BoxMakeWriter;
    let (log_writer, appender_guard): (BoxMakeWriter, Option<tracing_appender::non_blocking::WorkerGuard>) =
        if let Some(ref log_path) = resolved_log_file {
            let log_dir = std::path::Path::new(log_path)
                .parent()
                .unwrap_or(std::path::Path::new("."));
            let log_prefix = std::path::Path::new(log_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("forgekeep");
            let log_suffix = std::path::Path::new(log_path)
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("log");

            let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
                .rotation(tracing_appender::rolling::Rotation::DAILY)
                .filename_prefix(log_prefix)
                .filename_suffix(log_suffix)
                .max_log_files(resolved_log_max_files)
                .build(log_dir)
                .map_err(|e| {
                    // The appender error names neither the directory nor the
                    // reason, and an unwritable bind-mounted log directory is
                    // the usual cause — carry both.
                    let mut message = format!(
                        "failed to create log appender in {}: {e}\n  \
                         hint: point `--log-file` / `[logging].file` at a path the server can write to",
                        log_dir.display()
                    );
                    if let Some(hint) = rg_core::platform::fs::ownership_hint(log_dir) {
                        message.push_str(&format!("\n  hint: {hint}"));
                    }
                    anyhow::anyhow!(message)
                })?;

            let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
            (BoxMakeWriter::new(non_blocking), Some(guard))
        } else {
            (BoxMakeWriter::new(std::io::stdout), None)
        };

    let otel_config = telemetry::resolve_otel_config(
        cfg.as_ref()
            .and_then(|c| c.observability.otlp_endpoint.clone()),
        cfg.as_ref()
            .and_then(|c| c.observability.service_name.clone()),
        cfg.as_ref().and_then(|c| c.observability.sample_ratio),
    );

    let telemetry_guard = telemetry::init(log_writer, appender_guard, otel_config)?;

    if let Some(ref log_path) = resolved_log_file {
        tracing::info!(file = %log_path, "Logging to file with rotation");
        if resolved_log_max_size_mb != DEFAULT_LOG_MAX_SIZE_MB {
            tracing::warn!(
                max_size_mb = resolved_log_max_size_mb,
                "log_max_size_mb is not enforced: the file appender rotates daily (not by size). Use log_max_files to cap the number of retained files."
            );
        }
    }

    let repo_root = PathBuf::from(&resolved_repo_root);
    std::fs::create_dir_all(&repo_root).map_err(|e| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "repo_root",
            &repo_root,
            &e,
            "point `--repo-root` / `[server].repo_root` at a directory the server can create",
        ))
    })?;

    // ── Git CLI gateway (seed configured command timeout) ─────────
    if let Err(e) = rg_git::cli_gateway::init_global_gateway(std::time::Duration::from_secs(
        resolved_git_timeout,
    )) {
        tracing::warn!(%e, "git gateway init failed — git-dependent features may be unavailable");
    } else {
        tracing::info!(git_cmd_secs = resolved_git_timeout, "Git CLI gateway ready");
    }

    // ── Database ──────────────────────────────────────────────────
    tracing::info!(
        "Connecting to database: {}",
        rg_db::redact_database_url(&resolved_db_url)
    );
    let db = rg_db::connect_with_timeouts(
        &resolved_db_url,
        resolved_db_connect_timeout,
        resolved_db_idle_timeout,
    )
    .await
    .map_err(|e| annotate_db_open_error(e, &resolved_db_url))?;
    rg_db::run_migrations(&db).await?;
    tracing::info!("Database ready");

    // ── Graceful shutdown signal ──────────────────────────────────
    // A single `watch` channel fans the SIGTERM/ctrl_c signal out to the HTTP
    // server and every background worker so they can drain and exit cleanly.
    let resolved_shutdown_grace = cfg
        .as_ref()
        .and_then(|c| c.server.shutdown_grace_secs)
        .unwrap_or(30);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });

    let audit_config = cfg.as_ref().map(|config| &config.audit);
    let _audit_archiver_handle = if audit_config
        .and_then(|config| config.enabled)
        .unwrap_or(true)
    {
        // Note: the archiver's numeric knobs (archive_after_days /
        // interval_minutes / batch_size) are range-checked by
        // `AuditArchiveConfig::validate()`, invoked at the top of
        // `spawn_archiver_with_shutdown` below, so a 0 there also fails at start.
        let archive_config = rg_core::audit::archiver::AuditArchiveConfig {
            archive_dir: PathBuf::from(
                audit_config
                    .and_then(|config| config.archive_dir.as_deref())
                    .unwrap_or("./data/audit-archive"),
            ),
            archive_after_days: audit_config
                .and_then(|config| config.archive_after_days)
                .unwrap_or(90),
            interval_minutes: audit_config
                .and_then(|config| config.interval_minutes)
                .unwrap_or(60),
            batch_size: audit_config
                .and_then(|config| config.batch_size)
                .unwrap_or(1_000),
        };
        Some(rg_core::audit::archiver::spawn_archiver_with_shutdown(
            db.clone(),
            archive_config,
            Some(shutdown_rx.clone()),
        )?)
    } else {
        tracing::info!("Audit log archival disabled by configuration");
        None
    };

    // ── HTTP server ───────────────────────────────────────────────
    let smtp_config =
        match (
            resolved_smtp_host,
            resolved_smtp_user,
            resolved_smtp_pass,
            resolved_smtp_from,
        ) {
            (Some(host), Some(user), Some(pass), Some(from)) => {
                if resolved_smtp_port == 0 {
                    anyhow::bail!(
                        "config `smtp.port` must be 1-65535 (got 0) when SMTP is configured"
                    );
                }
                Some(rg_core::email::SmtpConfig::new(
                    &host,
                    resolved_smtp_port,
                    &user,
                    &pass,
                    &from,
                ))
            }
            _ => None,
        };

    let tls_config = match (resolved_tls_cert, resolved_tls_key) {
        (Some(cert), Some(key)) => {
            tracing::info!("TLS enabled: cert={}, key={}", cert, key);
            Some((PathBuf::from(cert), PathBuf::from(key)))
        }
        (Some(_), None) => {
            tracing::warn!("TLS cert specified but no key — running HTTP only");
            None
        }
        (None, Some(_)) => {
            tracing::warn!("TLS key specified but no cert — running HTTP only");
            None
        }
        _ => None,
    };

    validate_config(&resolved_jwt_secret, &repo_root, &tls_config)?;

    let http_config = rg_http::HttpServerConfig {
        listen_addr: resolved_http_addr,
        repo_root: repo_root.clone(),
        db: db.clone(),
        jwt_secret: resolved_jwt_secret.clone(),
        external_webhook_secret: resolved_external_webhook_secret,
        docker_enabled: resolved_docker,
        external_runners: resolved_external_runners,
        allow_host_runner: resolved_allow_host_runner,
        rate_limit_max: resolved_rate_limit_max,
        rate_limit_window_secs: resolved_rate_limit_window,
        rate_limit_trusted_proxies: resolved_rate_limit_trusted_proxies,
        rate_limit_max_keys: resolved_rate_limit_max_keys,
        rate_limit_auth_max: resolved_rate_limit_auth_max,
        rate_limit_auth_window_secs: resolved_rate_limit_auth_window,
        smtp_config,
        tls_config,
        oci_storage_path: None,
        external_url: resolved_external_url,
        job_timeout_secs: resolved_job_timeout,
        git_stream_timeout_secs: resolved_git_stream_timeout,
        git_idle_timeout_secs: resolved_git_idle_timeout,
        // M-14: Inject CiEngine via trait object, decoupling rg-http from rg-ci.
        ci_engine: std::sync::Arc::new(rg_ci::CiEngine),
        shutdown_rx: shutdown_rx.clone(),
        shutdown_grace_secs: resolved_shutdown_grace,
        attestation_enabled: resolved_attestation_enabled,
    };

    // ── SSH server ────────────────────────────────────────────────
    let host_key_path = resolved_host_key.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{}/.ssh/id_ed25519", home)
    });

    let ssh_config = rg_ssh::SshServerConfig {
        host_key_path: PathBuf::from(&host_key_path),
        listen_addr: resolved_ssh_addr,
        repo_root: repo_root.clone(),
        db: Some(db.clone()),
        git_stream_timeout_secs: resolved_git_stream_timeout,
        git_idle_timeout_secs: resolved_git_idle_timeout,
    };

    let http_handle = tokio::spawn(async move {
        if let Err(e) = rg_http::run(http_config).await {
            tracing::error!("HTTP server error: {:#}", e);
        }
    });

    let _ssh_handle = tokio::spawn(async move {
        if let Err(e) = rg_ssh::start_ssh_server(ssh_config).await {
            tracing::error!("SSH server error (HTTP unaffected): {:#}", e);
        }
    });

    tracing::info!("ForgeKeep server started (Phase 20)");

    if let Err(e) = http_handle.await {
        tracing::error!("HTTP server task terminated: {:#}", e);
    }

    // Flush the OTLP exporter (and the non-blocking log appender) before exit so
    // the final batch of spans reaches the collector.
    telemetry_guard.shutdown();

    Ok(())
}

#[cfg(test)]
mod config_tests {
    use super::{CliSettings, ConfigFile};

    /// `[server].repo_root` / `[database].url` from the config file were parsed
    /// and then thrown away: `run_serve` assigned the clap value verbatim. A
    /// `forgekeep serve --config …` with no flags therefore wrote repos and the
    /// SQLite file into `./repos` / `./forgekeep.db` relative to the working
    /// directory (inside the container: an ephemeral `/app`), not where the
    /// config said.
    #[test]
    fn config_file_locations_apply_when_no_cli_flag_is_passed() {
        let config: ConfigFile = toml::from_str(
            r#"
[server]
repo_root = "/srv/forgekeep/repos"
http_addr = "127.0.0.1:9000"
ssh_addr = "127.0.0.1:2323"
host_key = "/srv/forgekeep/ssh_host_key"

[database]
url = "sqlite:////srv/forgekeep/forgekeep.db?mode=rwc"

[smtp]
port = 2525

[rate_limit]
max = 500
window_secs = 30

[logging]
max_size_mb = 42
max_files = 7
"#,
        )
        .unwrap();

        let resolved = super::resolve_settings(CliSettings::default(), Some(&config));

        assert_eq!(resolved.repo_root, "/srv/forgekeep/repos");
        assert_eq!(resolved.db_url, "sqlite:////srv/forgekeep/forgekeep.db?mode=rwc");
        assert_eq!(resolved.http_addr, "127.0.0.1:9000");
        assert_eq!(resolved.ssh_addr, "127.0.0.1:2323");
        assert_eq!(
            resolved.host_key.as_deref(),
            Some("/srv/forgekeep/ssh_host_key")
        );
        assert_eq!(resolved.smtp_port, 2525);
        assert_eq!(resolved.rate_limit_max, 500);
        assert_eq!(resolved.rate_limit_window, 30);
        assert_eq!(resolved.log_max_size_mb, 42);
        assert_eq!(resolved.log_max_files, 7);
    }

    /// The documented order is `CLI args > config file > defaults`
    /// (ARCHITECTURE.md §8). A passed flag must beat the config file even when
    /// the value it carries happens to equal the built-in default — which is
    /// why none of these flags may have a clap `default_value`.
    #[test]
    fn cli_flags_win_over_the_config_file() {
        let config: ConfigFile = toml::from_str(
            r#"
[server]
repo_root = "/from/config"
http_addr = "10.0.0.1:1111"
ssh_addr = "10.0.0.1:2222"
host_key = "/from/config/key"

[database]
url = "postgres://config/db"

[smtp]
port = 2525

[rate_limit]
max = 500
window_secs = 30

[logging]
max_size_mb = 42
max_files = 7
"#,
        )
        .unwrap();

        let cli = CliSettings {
            repo_root: Some("/from/cli".to_string()),
            http_addr: Some(super::DEFAULT_HTTP_ADDR.to_string()),
            ssh_addr: Some("0.0.0.0:9999".to_string()),
            host_key: Some("/from/cli/key".to_string()),
            db_url: Some("mysql://cli/db".to_string()),
            // Exactly the built-in defaults: an explicit `--rate-limit-max 0`
            // disables the limiter even though the config file enables it.
            rate_limit_max: Some(super::DEFAULT_RATE_LIMIT_MAX),
            rate_limit_window: Some(super::DEFAULT_RATE_LIMIT_WINDOW),
            smtp_port: Some(super::DEFAULT_SMTP_PORT),
            log_max_size_mb: Some(super::DEFAULT_LOG_MAX_SIZE_MB),
            log_max_files: Some(super::DEFAULT_LOG_MAX_FILES),
        };

        let resolved = super::resolve_settings(cli, Some(&config));

        assert_eq!(resolved.repo_root, "/from/cli");
        assert_eq!(resolved.db_url, "mysql://cli/db");
        assert_eq!(resolved.http_addr, super::DEFAULT_HTTP_ADDR);
        assert_eq!(resolved.ssh_addr, "0.0.0.0:9999");
        assert_eq!(resolved.host_key.as_deref(), Some("/from/cli/key"));
        assert_eq!(resolved.smtp_port, super::DEFAULT_SMTP_PORT);
        assert_eq!(resolved.rate_limit_max, super::DEFAULT_RATE_LIMIT_MAX);
        assert_eq!(resolved.rate_limit_window, super::DEFAULT_RATE_LIMIT_WINDOW);
        assert_eq!(resolved.log_max_size_mb, super::DEFAULT_LOG_MAX_SIZE_MB);
        assert_eq!(resolved.log_max_files, super::DEFAULT_LOG_MAX_FILES);
    }

    /// Bottom of the chain: no flag, no config file at all.
    #[test]
    fn built_in_defaults_apply_without_cli_or_config() {
        let resolved = super::resolve_settings(CliSettings::default(), None);

        assert_eq!(resolved.repo_root, super::DEFAULT_REPO_ROOT);
        assert_eq!(resolved.http_addr, super::DEFAULT_HTTP_ADDR);
        assert_eq!(resolved.ssh_addr, super::DEFAULT_SSH_ADDR);
        assert_eq!(resolved.host_key, None);
        assert_eq!(resolved.db_url, super::DEFAULT_DB_URL);
        assert_eq!(resolved.smtp_port, super::DEFAULT_SMTP_PORT);
        assert_eq!(resolved.rate_limit_max, super::DEFAULT_RATE_LIMIT_MAX);
        assert_eq!(resolved.rate_limit_window, super::DEFAULT_RATE_LIMIT_WINDOW);
        assert_eq!(resolved.log_max_size_mb, super::DEFAULT_LOG_MAX_SIZE_MB);
        assert_eq!(resolved.log_max_files, super::DEFAULT_LOG_MAX_FILES);
    }

    /// A config file that sets none of these keys must not shadow the defaults
    /// with empty values.
    #[test]
    fn an_empty_config_file_falls_through_to_the_defaults() {
        let config: ConfigFile = toml::from_str("[ci]\ndocker = true\n").unwrap();
        let resolved = super::resolve_settings(CliSettings::default(), Some(&config));

        assert_eq!(resolved.repo_root, super::DEFAULT_REPO_ROOT);
        assert_eq!(resolved.db_url, super::DEFAULT_DB_URL);
        assert_eq!(resolved.http_addr, super::DEFAULT_HTTP_ADDR);
        assert_eq!(resolved.ssh_addr, super::DEFAULT_SSH_ADDR);
    }

    /// The shipped example is the file operators copy — the values it advertises
    /// must be the values the server actually boots with.
    #[test]
    fn example_config_locations_are_actually_applied() {
        let config: ConfigFile =
            toml::from_str(include_str!("../../../forgekeep.example.toml")).unwrap();
        let resolved = super::resolve_settings(CliSettings::default(), Some(&config));

        assert_eq!(resolved.repo_root, "./repos");
        assert_eq!(resolved.db_url, "sqlite://./forgekeep.db?mode=rwc");
        assert_eq!(resolved.http_addr, "0.0.0.0:8080");
        assert_eq!(resolved.ssh_addr, "0.0.0.0:2222");
    }

    #[test]
    fn config_path_pointing_at_a_directory_names_path_and_remediation() {
        // The exact crash-loop shape: `docker compose` bind-mounted a missing
        // `forgekeep.toml`, so the daemon created a directory there and the
        // server died with a bare `Is a directory (os error 21)`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forgekeep.toml");
        std::fs::create_dir(&path).unwrap();

        let err = super::load_config_file(path.to_str().unwrap())
            .unwrap_err()
            .to_string();

        assert!(err.contains(path.to_str().unwrap()), "no path: {err}");
        assert!(err.contains("is a directory"), "no cause: {err}");
        assert!(err.contains("bind-mount"), "no diagnosis: {err}");
        assert!(
            err.contains("cp forgekeep.example.toml forgekeep.toml"),
            "no remediation: {err}"
        );
    }

    #[test]
    fn missing_config_file_names_path_and_remediation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.toml");

        let err = super::load_config_file(path.to_str().unwrap())
            .unwrap_err()
            .to_string();

        assert!(err.contains(path.to_str().unwrap()), "no path: {err}");
        assert!(err.contains("does not exist"), "no cause: {err}");
        assert!(
            err.contains("cp forgekeep.example.toml forgekeep.toml"),
            "no remediation: {err}"
        );
    }

    #[test]
    fn malformed_config_file_names_the_path_it_failed_to_parse() {
        // A TOML syntax error otherwise surfaces as a bare parser message with
        // no clue about *which* file the operator has to fix.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        std::fs::write(&path, "[server\nrepo_root = \"/data/repos\"\n").unwrap();

        let err = format!(
            "{:#}",
            super::load_config_file(path.to_str().unwrap()).unwrap_err()
        );

        assert!(err.contains(path.to_str().unwrap()), "no path: {err}");
        assert!(err.contains("as TOML"), "no parse context: {err}");
    }

    #[test]
    fn a_readable_config_file_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forgekeep.toml");
        std::fs::write(&path, "[rate_limit]\nmax = 0\n").unwrap();

        let config = super::load_config_file(path.to_str().unwrap()).unwrap();
        assert_eq!(config.timeouts.db_connect_secs, 10);
    }

    #[test]
    fn tls_paths_that_are_directories_are_rejected_before_boot() {
        // Same bind-mount trap, different knob: `exists()` is true for a
        // directory, so the old check waved it through into rustls.
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("fullchain.pem");
        let key = dir.path().join("privkey.pem");
        std::fs::create_dir(&cert).unwrap();
        std::fs::write(&key, "key").unwrap();

        let secret = "a-sufficiently-long-test-jwt-secret-value";
        let err = super::validate_config(secret, dir.path(), &Some((cert.clone(), key.clone())))
            .unwrap_err()
            .to_string();
        assert!(err.contains("TLS certificate"), "unexpected: {err}");
        assert!(err.contains("is a directory"), "unexpected: {err}");

        // And a missing key is reported by name too.
        std::fs::remove_file(&key).unwrap();
        std::fs::remove_dir(&cert).unwrap();
        std::fs::write(&cert, "cert").unwrap();
        let err = super::validate_config(secret, dir.path(), &Some((cert, key)))
            .unwrap_err()
            .to_string();
        assert!(err.contains("TLS private key"), "unexpected: {err}");
        assert!(err.contains("does not exist"), "unexpected: {err}");
    }

    /// The shipped container config must keep every persistent path inside the
    /// one bind-mounted directory — a stray relative path would write into the
    /// image's ephemeral `/app` and vanish on the next `docker compose up`.
    #[test]
    fn docker_example_config_keeps_all_state_in_the_data_directory() {
        let config: ConfigFile =
            toml::from_str(include_str!("../../../deploy/forgekeep.docker.toml")).unwrap();

        let resolved = super::resolve_settings(CliSettings::default(), Some(&config));
        assert_eq!(resolved.repo_root, "/data/repos");
        assert_eq!(resolved.db_url, "sqlite:///data/forgekeep.db?mode=rwc");
        assert_eq!(resolved.host_key.as_deref(), Some("/data/ssh_host_key"));
        // No log file: a container logs to stdout, otherwise `docker compose
        // logs` shows nothing and the operator debugs a silent box.
        assert!(config.logging.file.is_none());
        assert_eq!(config.audit.archive_dir.as_deref(), Some("/data/audit-archive"));
        // The JWT secret belongs in deploy/.env, never in a file that may be
        // committed.
        assert!(config.auth.jwt_secret.is_none());
    }

    #[test]
    fn sqlite_urls_resolve_to_the_file_the_directory_check_needs() {
        use std::path::PathBuf;

        assert_eq!(
            super::sqlite_file_path("sqlite:///data/forgekeep.db?mode=rwc"),
            Some(PathBuf::from("/data/forgekeep.db"))
        );
        assert_eq!(
            super::sqlite_file_path("sqlite://./forgekeep.db"),
            Some(PathBuf::from("./forgekeep.db"))
        );
        assert_eq!(super::sqlite_file_path("sqlite::memory:"), None);
        assert_eq!(
            super::sqlite_file_path("postgres://user:pw@localhost/forgekeep"),
            None
        );
    }

    /// The `/data` bind-mount case: the directory is unwritable, so the opaque
    /// SQLite failure gains the path plus the uid to `chown` to.
    #[cfg(unix)]
    #[test]
    fn unwritable_sqlite_directory_is_named_in_the_connect_error() {
        use std::os::unix::fs::PermissionsExt;

        if unsafe { libc::geteuid() } == 0 {
            return; // root writes through the mode bits
        }
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o500)).unwrap();
        let url = format!("sqlite://{}/forgekeep.db?mode=rwc", data.display());

        let annotated = format!(
            "{:#}",
            super::annotate_db_open_error(anyhow::anyhow!("unable to open database file"), &url)
        );

        assert!(annotated.contains("unable to open database file"), "{annotated}");
        assert!(annotated.contains("is not writable"), "{annotated}");
        assert!(annotated.contains("this process runs as uid="), "{annotated}");
        assert!(
            annotated.contains("chmod") || annotated.contains("chown"),
            "{annotated}"
        );

        std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    /// A corrupt database or a bad URL must not be blamed on permissions.
    #[test]
    fn writable_sqlite_directory_leaves_the_connect_error_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite://{}/forgekeep.db?mode=rwc", dir.path().display());

        let annotated = format!(
            "{:#}",
            super::annotate_db_open_error(anyhow::anyhow!("file is not a database"), &url)
        );

        assert_eq!(annotated, "file is not a database");
    }

    #[test]
    fn example_config_includes_valid_audit_archive_settings() {
        let config: ConfigFile =
            toml::from_str(include_str!("../../../forgekeep.example.toml")).unwrap();
        assert_eq!(config.audit.enabled, Some(true));
        assert_eq!(config.audit.archive_after_days, Some(90));
        assert_eq!(config.audit.interval_minutes, Some(60));
        assert_eq!(config.audit.batch_size, Some(1_000));
    }

    #[test]
    fn numeric_ranges_accept_defaults_and_minimum_boundary() {
        // Shipped defaults pass.
        assert!(super::validate_numeric_ranges(120, 10, 600, 60, 60).is_ok());
        // The lower boundary (1) is the smallest valid value for every knob.
        assert!(super::validate_numeric_ranges(1, 1, 1, 1, 1).is_ok());
    }

    #[test]
    fn numeric_ranges_reject_zero_db_connect_timeout() {
        // The card's flagship case: `db_connect_secs = 0` must fail at start
        // with a message that names the offending key.
        let err = super::validate_numeric_ranges(120, 0, 600, 60, 60)
            .unwrap_err()
            .to_string();
        assert!(err.contains("db_connect_secs"), "unexpected message: {err}");
    }

    #[test]
    fn numeric_ranges_reject_each_zero_knob() {
        assert!(super::validate_numeric_ranges(0, 10, 600, 60, 60).is_err()); // git_cmd_secs
        assert!(super::validate_numeric_ranges(120, 0, 600, 60, 60).is_err()); // db_connect_secs
        assert!(super::validate_numeric_ranges(120, 10, 0, 60, 60).is_err()); // db_idle_secs
        assert!(super::validate_numeric_ranges(120, 10, 600, 0, 60).is_err()); // rate_limit.window_secs
        assert!(super::validate_numeric_ranges(120, 10, 600, 60, 0).is_err()); // rate_limit.auth_window_secs
    }

    #[test]
    fn omitted_timeouts_table_uses_documented_defaults_not_zeros() {
        // A config file that provides *some* section but omits `[timeouts]`
        // entirely must still land on the non-zero documented defaults — a
        // `#[derive(Default)]` on TimeoutConfig would zero every knob and the
        // range validator would (rightly) refuse to boot.
        let config: ConfigFile = toml::from_str("[rate_limit]\nmax = 0\n").unwrap();
        assert_eq!(config.timeouts.job_secs, 3600);
        assert_eq!(config.timeouts.git_cmd_secs, 120);
        assert_eq!(config.timeouts.git_stream_secs, 300);
        assert_eq!(config.timeouts.git_idle_secs, 30);
        assert_eq!(config.timeouts.db_connect_secs, 10);
        assert_eq!(config.timeouts.db_idle_secs, 600);
        // And those defaults pass the range gate.
        assert!(super::validate_numeric_ranges(
            config.timeouts.git_cmd_secs,
            config.timeouts.db_connect_secs,
            config.timeouts.db_idle_secs,
            60,
            60,
        )
        .is_ok());
    }
}
