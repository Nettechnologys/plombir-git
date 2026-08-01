//! `forgekeep serve` subcommand: startup validation and the HTTP + SSH server
//! bootstrap.
//!
//! The TOML model and the `CLI arg > config file > built-in default` resolution
//! live in [`crate::config`], shared with every other subcommand — while they
//! were private to this module, `migrate` / `backup-db` / `import` and friends
//! had no way to read `[database].url` or `[server].repo_root` at all.

use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::Context;

use crate::admin::validate_jwt_secret;
use crate::config::{
    default_db_connect_timeout, default_db_idle_timeout, default_git_idle_timeout,
    default_git_stream_timeout, default_git_timeout, ensure_regular_file, load_config_file,
    resolve_settings, CliSettings, ResolvedSettings, DEFAULT_LOG_MAX_SIZE_MB,
};
use crate::dbconn;
use crate::telemetry;

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

/// Default `[audit].archive_dir` — a sibling of `repo_root` rather than a
/// cwd-relative path.
///
/// The container image has `WORKDIR /app` and its persistent volume mounted at
/// `/data`, so the historical `./data/audit-archive` default resolved to
/// `/app/data/audit-archive`: inside the container layer, wiped on every
/// `docker compose up --force-recreate`, taking the archived audit log — the
/// one artifact whose whole point is to outlive the rows it replaced — with it.
/// Anchoring on an absolute `repo_root` (`/data/repos` → `/data/audit-archive`)
/// keeps the archive on the same volume as the data it belongs to.
///
/// A relative `repo_root` is left alone: it is already cwd-relative, so there is
/// no ephemeral/persistent mismatch to fix, and moving the default would only
/// strand the archives of an existing install.
fn default_audit_archive_dir(repo_root: &std::path::Path) -> PathBuf {
    match repo_root.parent() {
        // `parent.parent().is_some()` rejects a filesystem root (`/repos` →
        // `/`): writing the archive there needs privileges the server should
        // not have, and demanding them at startup would turn a working install
        // into a boot failure.
        Some(parent) if repo_root.is_absolute() && parent.parent().is_some() => {
            parent.join("audit-archive")
        }
        _ => PathBuf::from("./data/audit-archive"),
    }
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

/// Read a secret-carrying environment variable, treating a blank value as unset.
///
/// `std::env::var` reports `FOO=` as `Ok("")`, and `deploy/.env.example` ships
/// exactly that line for `FORGEKEEP_JWT_SECRET`. Taken literally, an operator
/// who copied the file and forgot to fill it in got a server that started
/// cleanly and signed every token with the empty string — and the same file
/// promised them "startup validation will fail loudly". A blank line in an
/// `.env` means "I have not set this", never "the secret is the empty string",
/// so it falls through to the next source and, if there is none, to that
/// source's own honest error.
fn env_secret(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Resolve the pair of secrets every server-side path needs: the one that
/// *signs* and the one that *encrypts*. Shared with the one-shot subcommands
/// that also have to open at-rest data, so "which key opens this database" has
/// one answer and not one per entry point.
pub(crate) fn resolve_auth_secrets(
    cfg: Option<&crate::config::ConfigFile>,
    jwt_secret: Option<String>,
    encryption_key: Option<String>,
) -> anyhow::Result<(String, String)> {
    // Resolve JWT secret: env var > CLI args > config file > error
    let resolved_jwt_secret = if let Some(env_secret) = env_secret("FORGEKEEP_JWT_SECRET") {
        validate_jwt_secret(&env_secret, "environment variable FORGEKEEP_JWT_SECRET")?;
        tracing::info!("Using JWT secret from environment variable FORGEKEEP_JWT_SECRET");
        env_secret
    } else if let Some(cli_secret) = jwt_secret {
        validate_jwt_secret(&cli_secret, "--jwt-secret CLI argument")?;
        cli_secret
    } else if let Some(cfg_secret) = cfg.and_then(|c| c.auth.jwt_secret.clone()) {
        validate_jwt_secret(&cfg_secret, "config file [auth].jwt_secret")?;
        cfg_secret
    } else {
        anyhow::bail!(
            "No JWT secret provided. Set FORGEKEEP_JWT_SECRET, use --jwt-secret, or configure [auth].jwt_secret in config file"
        );
    };

    // Resolve the at-rest encryption secret: env var > CLI arg > config file >
    // the JWT secret.
    //
    // The fallback is not laziness, it is compatibility: until card_d740512de0a8
    // the two were the same value by construction, so every existing database
    // has its TOTP secrets, CI secrets, mirror/LDAP passwords, SSO client
    // secrets and OAuth tokens encrypted under `jwt_secret`. Splitting them
    // without the fallback would have made this release the exact silent
    // data-loss event the card is about. Setting the key explicitly is what
    // makes rotating the *signing* secret safe from then on — and the startup
    // preflight is what catches an operator who rotated without doing that.
    let resolved_encryption_key = if let Some(env_key) = env_secret("FORGEKEEP_ENCRYPTION_KEY") {
        validate_jwt_secret(&env_key, "environment variable FORGEKEEP_ENCRYPTION_KEY")?;
        tracing::info!("Using at-rest encryption key from FORGEKEEP_ENCRYPTION_KEY");
        env_key
    } else if let Some(cli_key) = encryption_key {
        validate_jwt_secret(&cli_key, "--encryption-key CLI argument")?;
        tracing::info!("Using at-rest encryption key from --encryption-key");
        cli_key
    } else if let Some(cfg_key) = cfg.and_then(|c| c.auth.encryption_key.clone()) {
        validate_jwt_secret(&cfg_key, "config file [auth].encryption_key")?;
        tracing::info!("Using at-rest encryption key from config file [auth].encryption_key");
        cfg_key
    } else {
        tracing::info!(
            "No [auth].encryption_key set — encrypting data at rest with the JWT secret. \
             Set it (to the current JWT secret) before you ever rotate jwt_secret, or the \
             stored secrets become unreadable"
        );
        resolved_jwt_secret.clone()
    };

    Ok((resolved_jwt_secret, resolved_encryption_key))
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
    encryption_key: Option<String>,
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

    let (resolved_jwt_secret, resolved_encryption_key) =
        resolve_auth_secrets(cfg.as_ref(), jwt_secret, encryption_key)?;

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
    let resolved_external_webhook_secret = env_secret("FORGEKEEP_EXTERNAL_WEBHOOK_SECRET")
        .or_else(|| {
            cfg.as_ref()
                .and_then(|c| c.webhooks.external_secret.clone())
        })
        .filter(|secret| !secret.trim().is_empty());
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
    let (log_writer, appender_guard): (
        BoxMakeWriter,
        Option<tracing_appender::non_blocking::WorkerGuard>,
    ) = if let Some(ref log_path) = resolved_log_file {
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
        tracing::warn!(
            error = %format!("{e:#}"),
            "git gateway init failed — git-dependent features may be unavailable"
        );
    } else {
        tracing::info!(git_cmd_secs = resolved_git_timeout, "Git CLI gateway ready");
    }

    // ── Database ──────────────────────────────────────────────────
    tracing::info!(
        "Connecting to database: {}",
        rg_db::redact_database_url(&resolved_db_url)
    );
    let db = dbconn::connect_with_timeouts(
        &resolved_db_url,
        resolved_db_connect_timeout,
        resolved_db_idle_timeout,
    )
    .await?;
    rg_db::run_migrations(&db).await?;
    tracing::info!("Database ready");

    // ── At-rest encryption key preflight ──────────────────────────
    // Refuse to serve with a key that cannot open what is already stored. The
    // alternative — the behaviour before card_d740512de0a8 — is a server that
    // starts fine and then fails MFA login, CI secret injection, mirror sync and
    // LDAP bind one at a time, each with its own unrelated-looking 500, with
    // nothing anywhere naming the changed secret as the cause. Runs after the
    // migrations so the columns it samples are guaranteed to exist.
    rg_core::auth::key_check::verify_encryption_key(&db, &resolved_encryption_key).await?;

    // ── Instance provenance identity ──────────────────────────────
    // The Ed25519 key that signs release attestations and backs the CI OIDC
    // JWKS. Loaded from the database — on the first start it adopts exactly the
    // key the old `jwt_secret` derivation produced, so nothing that was already
    // signed stops verifying, and from then on the identity outlives every
    // rotation of either secret (card_3aecf3708ebe).
    let instance_key = std::sync::Arc::new(
        rg_core::auth::instance_key::load_or_adopt(
            &db,
            &resolved_jwt_secret,
            &resolved_encryption_key,
        )
        .await?,
    );
    tracing::info!(kid = %instance_key.kid(), "instance provenance signing key ready");

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
        if shutdown_tx.send(true).is_err() {
            // Every receiver has already shut down.
        }
    });

    let audit_config = cfg.as_ref().map(|config| &config.audit);
    let _audit_archiver_handle = if audit_config
        .and_then(|config| config.enabled)
        .unwrap_or(true)
    {
        // Note: `spawn_archiver_with_shutdown` below range-checks the numeric
        // knobs (archive_after_days / interval_minutes / batch_size) and then
        // creates + write-probes `archive_dir`, so a 0 or an unusable directory
        // fails the start here rather than an hour later inside the loop.
        let archive_dir = audit_config
            .and_then(|config| config.archive_dir.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| default_audit_archive_dir(&repo_root));
        // Log the *absolute* destination: the historical default resolved
        // against the process cwd, so an operator reading `./data/audit-archive`
        // had no way to tell the archive was landing inside an ephemeral
        // container layer instead of on the data volume.
        tracing::info!(
            archive_dir = %std::path::absolute(&archive_dir)
                .unwrap_or_else(|_| archive_dir.clone())
                .display(),
            "Audit log archival enabled"
        );
        let archive_config = rg_core::audit::archiver::AuditArchiveConfig {
            archive_dir,
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
    let smtp_config = match (
        resolved_smtp_host,
        resolved_smtp_user,
        resolved_smtp_pass,
        resolved_smtp_from,
    ) {
        (Some(host), Some(user), Some(pass), Some(from)) => {
            if resolved_smtp_port == 0 {
                anyhow::bail!("config `smtp.port` must be 1-65535 (got 0) when SMTP is configured");
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

    // One CI engine and one WebSocket hub for the whole process: the SSH
    // transport's post-push hooks trigger pipelines and push `ci_triggered` /
    // `push` events to the very clients the HTTP server's sockets belong to, so
    // a second hub of its own would fan out to nobody (card_b4fefeee8abf).
    let ci_engine: std::sync::Arc<dyn rg_core::ci::CiTrigger + Send + Sync> =
        std::sync::Arc::new(rg_ci::CiEngine);
    let notification_hub = rg_http::ws::NotificationHub::new();
    let instance_settings = rg_core::instance::InstanceSettingsCache::default();

    let http_config = rg_http::HttpServerConfig {
        listen_addr: resolved_http_addr,
        repo_root: repo_root.clone(),
        db: db.clone(),
        jwt_secret: resolved_jwt_secret.clone(),
        encryption_key: resolved_encryption_key.clone(),
        instance_key,
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
        smtp_config: smtp_config.clone(),
        tls_config,
        oci_storage_path: None,
        external_url: resolved_external_url.clone(),
        job_timeout_secs: resolved_job_timeout,
        git_stream_timeout_secs: resolved_git_stream_timeout,
        git_idle_timeout_secs: resolved_git_idle_timeout,
        // M-14: Inject CiEngine via trait object, decoupling rg-http from rg-ci.
        ci_engine: ci_engine.clone(),
        shutdown_rx: shutdown_rx.clone(),
        shutdown_grace_secs: resolved_shutdown_grace,
        attestation_enabled: resolved_attestation_enabled,
        notification_hub: Some(notification_hub.clone()),
        instance_settings: instance_settings.clone(),
    };

    // ── SSH server ────────────────────────────────────────────────
    let host_key_path = resolved_host_key.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        format!("{}/.ssh/id_ed25519", home)
    });

    // A push over SSH must run the same automation as a push over HTTPS — CI,
    // webhooks, open-PR head-SHA refresh, auto-merge / merge-queue. These hooks
    // used to be private to `rg-http`, so SSH (the default once a key is
    // registered) ran none of them (card_b4fefeee8abf).
    let post_push_context = rg_core::push_hooks::PostPushContext {
        repo_root: repo_root.clone(),
        docker_enabled: resolved_docker,
        external_runners: resolved_external_runners,
        allow_host_runner: resolved_allow_host_runner,
        jwt_secret: Some(resolved_jwt_secret.clone()),
        encryption_key: Some(resolved_encryption_key),
        smtp_config,
        ci_engine,
        external_url: resolved_external_url,
        notifier: Some(std::sync::Arc::new(notification_hub)),
        delivery_tracker: rg_core::task_tracker::delivery_tracker().clone(),
    };

    let ssh_config = rg_ssh::SshServerConfig {
        host_key_path: PathBuf::from(&host_key_path),
        listen_addr: resolved_ssh_addr,
        repo_root: repo_root.clone(),
        db: Some(db.clone()),
        git_stream_timeout_secs: resolved_git_stream_timeout,
        git_idle_timeout_secs: resolved_git_idle_timeout,
        post_push: Some(std::sync::Arc::new(post_push_context)),
        instance_settings,
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
mod serve_tests {
    use crate::config::{CliSettings, ConfigFile};

    /// `[auth].encryption_key` must actually parse — the struct carries
    /// `deny_unknown_fields`, so a key documented in `forgekeep.example.toml`
    /// but missing from the model turns every config file that uses it into a
    /// hard startup failure.
    #[test]
    fn the_encryption_key_is_a_real_config_key() {
        let config: ConfigFile =
            toml::from_str("[auth]\njwt_secret = \"signing\"\nencryption_key = \"at-rest\"\n")
                .expect("[auth].encryption_key must be part of the config model");
        assert_eq!(config.auth.jwt_secret.as_deref(), Some("signing"));
        assert_eq!(config.auth.encryption_key.as_deref(), Some("at-rest"));
    }

    /// Omitting it is the supported (and most common) state: existing
    /// deployments encrypted everything under `jwt_secret` and must keep
    /// working untouched.
    #[test]
    fn omitting_the_encryption_key_is_allowed() {
        let config: ConfigFile = toml::from_str("[auth]\njwt_secret = \"signing\"\n").unwrap();
        assert!(config.auth.encryption_key.is_none());
    }

    /// `FOO=` in a `.env` is "not set", not "the empty secret" — see
    /// [`super::env_secret`]. `deploy/.env.example` ships exactly that line.
    #[test]
    fn a_blank_environment_variable_counts_as_unset() {
        let name = "FORGEKEEP_TEST_BLANK_SECRET";
        // SAFETY: single-threaded test, variable is private to this test.
        std::env::set_var(name, "");
        assert_eq!(super::env_secret(name), None);
        std::env::set_var(name, "   \n");
        assert_eq!(super::env_secret(name), None);
        std::env::set_var(name, "an-actual-secret");
        assert_eq!(super::env_secret(name).as_deref(), Some("an-actual-secret"));
        std::env::remove_var(name);
        assert_eq!(super::env_secret(name), None);
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

    /// The container runs from `WORKDIR /app` with its volume on `/data`, so
    /// the old cwd-relative `./data/audit-archive` default put the audit
    /// archive inside the image layer — deleted on the next recreate, while the
    /// rows it had replaced were already purged from the database.
    #[test]
    fn audit_archive_default_follows_an_absolute_repo_root() {
        use std::path::{Path, PathBuf};

        assert_eq!(
            super::default_audit_archive_dir(Path::new("/data/repos")),
            PathBuf::from("/data/audit-archive")
        );
        // A relative repo_root is already cwd-relative — nothing to fix, and
        // moving the default would strand an existing install's archives.
        assert_eq!(
            super::default_audit_archive_dir(Path::new("./repos")),
            PathBuf::from("./data/audit-archive")
        );
        // A repo_root directly under the filesystem root keeps the historical
        // default rather than demanding write access to `/`.
        assert_eq!(
            super::default_audit_archive_dir(Path::new("/repos")),
            PathBuf::from("./data/audit-archive")
        );
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

    /// `serve` shares one resolution path with the one-shot subcommands, so the
    /// server and a later `forgekeep migrate --config <same file>` cannot end up
    /// pointed at two different databases.
    #[test]
    fn serve_and_the_one_shot_subcommands_resolve_the_same_database() {
        let config: ConfigFile =
            toml::from_str("[database]\nurl = \"postgres://forge@db/forgekeep\"\n[server]\nrepo_root = \"/data/repos\"\n")
                .unwrap();

        let resolved = crate::config::resolve_settings(CliSettings::default(), Some(&config));
        assert_eq!(
            resolved.db_url,
            crate::config::resolve_db_url(None, Some(&config))
        );
        assert_eq!(
            resolved.repo_root,
            crate::config::resolve_repo_root(None, Some(&config))
        );
    }
}
