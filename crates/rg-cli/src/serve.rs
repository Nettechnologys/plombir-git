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

#[derive(Debug, serde::Deserialize, Default)]
#[allow(dead_code)]
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
#[allow(dead_code)]
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

    // 2. Verify repo_root is writable
    let test_file = repo_root.join(".write_test");
    std::fs::write(&test_file, "test")
        .with_context(|| format!("repo_root is not writable: {:?}", repo_root))?;
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

/// Initialise and run the ForgeKeep server (HTTP + SSH).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_serve(
    repo_root: String,
    http_addr: String,
    ssh_addr: String,
    host_key: Option<String>,
    db_url: String,
    jwt_secret: Option<String>,
    docker: bool,
    external_runners: bool,
    allow_host_runner: bool,
    rate_limit_max: u32,
    rate_limit_window: u64,
    rate_limit_trusted_proxies: Vec<String>,
    smtp_host: Option<String>,
    smtp_port: u16,
    smtp_user: Option<String>,
    smtp_pass: Option<String>,
    smtp_from: Option<String>,
    tls_cert: Option<String>,
    tls_key: Option<String>,
    config: Option<String>,
    log_file: Option<String>,
    log_max_size_mb: u64,
    log_max_files: usize,
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

    // Resolve other values: CLI args > config file
    let resolved_repo_root = repo_root;
    let resolved_http_addr = http_addr;
    let resolved_ssh_addr = ssh_addr;
    let resolved_host_key =
        host_key.or_else(|| cfg.as_ref().and_then(|c| c.server.host_key.clone()));
    let resolved_db_url = db_url;
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
    let resolved_rate_limit_max = if rate_limit_max > 0 {
        rate_limit_max
    } else {
        cfg.as_ref().and_then(|c| c.rate_limit.max).unwrap_or(0_u32)
    };
    let resolved_rate_limit_window = if rate_limit_window != 60 {
        rate_limit_window
    } else {
        cfg.as_ref()
            .and_then(|c| c.rate_limit.window_secs)
            .unwrap_or(60)
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

    // SMTP: CLI takes precedence, fallback to config
    let (
        resolved_smtp_host,
        resolved_smtp_port,
        resolved_smtp_user,
        resolved_smtp_pass,
        resolved_smtp_from,
    ) = {
        let h = smtp_host.or_else(|| cfg.as_ref().and_then(|c| c.smtp.host.clone()));
        let p = cfg.as_ref().and_then(|c| c.smtp.port).unwrap_or(smtp_port);
        let u = smtp_user.or_else(|| cfg.as_ref().and_then(|c| c.smtp.user.clone()));
        let pw = smtp_pass.or_else(|| cfg.as_ref().and_then(|c| c.smtp.pass.clone()));
        let f = smtp_from.or_else(|| cfg.as_ref().and_then(|c| c.smtp.from.clone()));
        (h, p, u, pw, f)
    };

    // TLS: CLI takes precedence, fallback to config
    let resolved_tls_cert = tls_cert.or_else(|| cfg.as_ref().and_then(|c| c.tls.cert.clone()));
    let resolved_tls_key = tls_key.or_else(|| cfg.as_ref().and_then(|c| c.tls.key.clone()));

    // Logging: CLI takes precedence, fallback to config
    let resolved_log_file = log_file.or_else(|| cfg.as_ref().and_then(|c| c.logging.file.clone()));
    let resolved_log_max_files = if log_max_files != 5 {
        log_max_files
    } else {
        cfg.as_ref().and_then(|c| c.logging.max_files).unwrap_or(5)
    };
    let resolved_log_max_size_mb = if log_max_size_mb != 10 {
        log_max_size_mb
    } else {
        cfg.as_ref()
            .and_then(|c| c.logging.max_size_mb)
            .unwrap_or(10)
    };

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
                .map_err(|e| anyhow::anyhow!("failed to create log appender: {}", e))?;

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
        if resolved_log_max_size_mb != 10 {
            tracing::warn!(
                max_size_mb = resolved_log_max_size_mb,
                "log_max_size_mb is not enforced: the file appender rotates daily (not by size). Use log_max_files to cap the number of retained files."
            );
        }
    }

    let repo_root = PathBuf::from(&resolved_repo_root);
    std::fs::create_dir_all(&repo_root)?;

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
    .await?;
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
    use super::ConfigFile;

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
