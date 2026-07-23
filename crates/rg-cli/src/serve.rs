//! `forgekeep serve` subcommand: TOML configuration model, config resolution,
//! validation, and the HTTP + SSH server bootstrap.

use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

use crate::admin::validate_jwt_secret;

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
struct RateLimitConfig {
    max: Option<u32>,
    window_secs: Option<u64>,
    #[serde(default)]
    trusted_proxies: Vec<String>,
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

#[derive(Debug, serde::Deserialize, Default)]
struct TimeoutConfig {
    /// CI job timeout in seconds (default: 3600 = 1 hour).
    #[serde(default = "default_job_timeout")]
    job_secs: u64,
    /// Git CLI command timeout in seconds (default: 120).
    #[serde(default = "default_git_timeout")]
    git_cmd_secs: u64,
    /// Database connect timeout in seconds (default: 10).
    #[serde(default = "default_db_connect_timeout")]
    db_connect_secs: u64,
    /// Database idle timeout in seconds (default: 600).
    #[serde(default = "default_db_idle_timeout")]
    db_idle_secs: u64,
}

fn default_job_timeout() -> u64 {
    3600
}
fn default_git_timeout() -> u64 {
    120
}
fn default_db_connect_timeout() -> u64 {
    10
}
fn default_db_idle_timeout() -> u64 {
    600
}

fn load_config_file(path: &str) -> anyhow::Result<ConfigFile> {
    let content = std::fs::read_to_string(path)?;
    let config: ConfigFile = toml::from_str(&content)?;
    tracing::info!(path = %path, "Loaded configuration file");
    Ok(config)
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

    // 3. Verify TLS files exist if configured
    if let Some((ref cert, ref key)) = tls_config {
        if !cert.exists() {
            anyhow::bail!("TLS certificate not found: {:?}", cert);
        }
        if !key.exists() {
            anyhow::bail!("TLS private key not found: {:?}", key);
        }
    }

    tracing::info!("Configuration validation passed");
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

    // Timeouts from config (with defaults)
    let resolved_job_timeout = cfg.as_ref().map(|c| c.timeouts.job_secs).unwrap_or(3600);
    let resolved_git_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.git_cmd_secs)
        .unwrap_or_else(default_git_timeout);
    let resolved_db_connect_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.db_connect_secs)
        .unwrap_or_else(default_db_connect_timeout);
    let resolved_db_idle_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.db_idle_secs)
        .unwrap_or_else(default_db_idle_timeout);

    // ── Initialize logging ─────────────────────────────────────
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

        let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .with_target(false)
            .with_writer(non_blocking)
            .init();

        std::mem::forget(_guard);

        tracing::info!(file = %log_path, "Logging to file with rotation");
        if resolved_log_max_size_mb != 10 {
            tracing::warn!(
                max_size_mb = resolved_log_max_size_mb,
                "log_max_size_mb is not enforced: the file appender rotates daily (not by size). Use log_max_files to cap the number of retained files."
            );
        }
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
            )
            .with_target(false)
            .init();
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

    let audit_config = cfg.as_ref().map(|config| &config.audit);
    let _audit_archiver_handle = if audit_config
        .and_then(|config| config.enabled)
        .unwrap_or(true)
    {
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
        Some(rg_core::audit::archiver::spawn_archiver_with_config(
            db.clone(),
            archive_config,
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
            (Some(host), Some(user), Some(pass), Some(from)) => Some(
                rg_core::email::SmtpConfig::new(&host, resolved_smtp_port, &user, &pass, &from),
            ),
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
        docker_enabled: resolved_docker,
        external_runners: resolved_external_runners,
        allow_host_runner: resolved_allow_host_runner,
        rate_limit_max: resolved_rate_limit_max,
        rate_limit_window_secs: resolved_rate_limit_window,
        rate_limit_trusted_proxies: resolved_rate_limit_trusted_proxies,
        smtp_config,
        tls_config,
        oci_storage_path: None,
        external_url: resolved_external_url,
        job_timeout_secs: resolved_job_timeout,
        // M-14: Inject CiEngine via trait object, decoupling rg-http from rg-ci.
        ci_engine: std::sync::Arc::new(rg_ci::CiEngine),
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

    Ok(())
}

#[cfg(test)]
mod config_tests {
    use super::ConfigFile;

    #[test]
    fn example_config_includes_valid_audit_archive_settings() {
        let config: ConfigFile =
            toml::from_str(include_str!("../../../forgekeep.example.toml")).unwrap();
        assert_eq!(config.audit.enabled, Some(true));
        assert_eq!(config.audit.archive_after_days, Some(90));
        assert_eq!(config.audit.interval_minutes, Some(60));
        assert_eq!(config.audit.batch_size, Some(1_000));
    }
}
