//! `plombir-git serve` subcommand: startup validation and the HTTP + SSH server
//! bootstrap.
//!
//! The TOML model and the `CLI arg > config file > built-in default` resolution
//! live in [`crate::config`], shared with every other subcommand — while they
//! were private to this module, `migrate` / `backup-db` / `import` and friends
//! had no way to read `[database].url` or `[server].repo_root` at all.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Context;

use crate::admin::validate_jwt_secret;
use crate::config::{
    default_db_connect_timeout, default_db_idle_timeout, default_git_idle_timeout,
    default_git_stream_timeout, default_git_timeout, default_job_timeout, default_shutdown_grace,
    ensure_regular_file, load_config_file, resolve_encryption_key_file,
    resolve_import_transport_policy, resolve_ldap_transport_policy,
    resolve_mirror_transport_policy, resolve_oidc_transport_policy,
    resolve_package_upload_max_bytes, resolve_settings, resolve_trusted_import_origins,
    resolve_webhook_transport_policy, CliSettings, ResolvedSettings, DEFAULT_AGENT_RATE_LIMIT_MAX,
    DEFAULT_AGENT_RATE_LIMIT_WINDOW, DEFAULT_ATTESTATION_ENABLED, DEFAULT_AUDIT_ARCHIVE_DIR,
    DEFAULT_AUDIT_ENABLED, DEFAULT_AUTH_RATE_LIMIT_MAX, DEFAULT_AUTH_RATE_LIMIT_WINDOW,
    DEFAULT_BACKUP_ENABLED, DEFAULT_CI_ALLOW_HOST_RUNNER, DEFAULT_CI_DOCKER,
    DEFAULT_CI_EXTERNAL_RUNNERS, DEFAULT_DB_BACKUP_DIR, DEFAULT_METRICS_ENABLED,
    DEFAULT_MIRROR_ENABLED, DEFAULT_RATE_LIMIT_MAX_KEYS, DEFAULT_RETENTION_ENABLED,
};
use crate::dbconn;
use crate::telemetry;

/// The engine every trigger producer in this process spawns embedded runners
/// through.
///
/// `shutdown` is the same `watch` channel the HTTP server, the SSH transport and
/// every background worker get. It reaches the embedded runner through here
/// because the runner is created deep inside `rg-ci`, one call away from
/// whichever push, merge or manual dispatch triggered it, and none of those
/// producers should have to carry the signal (card_34368880dc20).
fn configured_ci_engine(
    notifications: rg_ci::CiNotifications,
    job_timeout_secs: u64,
    runner_labels: Vec<String>,
    shutdown: tokio::sync::watch::Receiver<bool>,
) -> rg_ci::CiEngine {
    rg_ci::CiEngine::with_notifications_and_job_timeout(notifications, job_timeout_secs)
        .with_runner_labels(runner_labels)
        .with_shutdown(shutdown)
}

fn publish_listen_addresses(
    path: &Path,
    http_addr: std::net::SocketAddr,
    ssh_addr: std::net::SocketAddr,
) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create listen-address directory {}",
                parent.display()
            )
        })?;
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("listen-addresses");
    let temporary = path.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
    let contents = format!("http={http_addr}\nssh={ssh_addr}\n");

    let publish = (|| -> anyhow::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| {
                format!(
                    "failed to create temporary listen-address file {}",
                    temporary.display()
                )
            })?;
        file.write_all(contents.as_bytes()).with_context(|| {
            format!(
                "failed to write temporary listen-address file {}",
                temporary.display()
            )
        })?;
        file.sync_all().with_context(|| {
            format!(
                "failed to sync temporary listen-address file {}",
                temporary.display()
            )
        })?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("failed to publish listen addresses at {}", path.display()))?;
        Ok(())
    })();

    if publish.is_err() {
        drop(std::fs::remove_file(&temporary));
    }
    publish
}

/// Say at startup which source link every page will offer, and warn when the
/// build did not record the commit it came from.
///
/// The warning is the only place an operator learns that their image was built
/// without `PLOMBIR_GIT_SOURCE_COMMIT`: the UI still links the repository, so
/// nothing looks broken — it just stops naming the code that is running.
fn announce_source_link(source_url: &str) {
    use rg_http::build_info::{self, SourceCommit};

    let commit = build_info::source_commit();
    let link = build_info::source_link(source_url, commit.known());
    match commit {
        SourceCommit::Known(commit) => {
            tracing::info!(%commit, %link, "source code of this build is offered at the link")
        }
        SourceCommit::Unrecorded => tracing::warn!(
            %link,
            "this binary was built without PLOMBIR_GIT_SOURCE_COMMIT, so the UI links the \
             repository instead of the commit that is running; pass it at build time \
             (docker build --build-arg PLOMBIR_GIT_SOURCE_COMMIT=$(git rev-parse HEAD))"
        ),
        SourceCommit::Malformed(raw) => tracing::warn!(
            %link,
            recorded = raw,
            "PLOMBIR_GIT_SOURCE_COMMIT was set at build time to something that is not a full \
             commit id, so it is ignored and the UI links the repository instead; pass the \
             full `git rev-parse HEAD`"
        ),
    }
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

fn parse_rate_limit_trusted_proxies(
    values: &[String],
) -> anyhow::Result<Vec<rg_http::client_ip::TrustedProxy>> {
    values
        .iter()
        .map(|value| {
            value.parse().map_err(|reason: String| {
                anyhow::anyhow!(
                    "invalid [rate_limit].trusted_proxies entry: {reason} — expected an IP address \
                     or a CIDR network such as 172.16.0.0/12"
                )
            })
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
        _ => PathBuf::from(DEFAULT_AUDIT_ARCHIVE_DIR),
    }
}

/// Default `[backup].dir` — a sibling of `repo_root`, for exactly the reasons
/// spelled out on [`default_audit_archive_dir`]: a cwd-relative default resolves
/// inside the container layer (`WORKDIR /app`), and a backup that disappears on
/// `docker compose up --force-recreate` is worse than no backup, because it
/// looks like one.
fn default_db_backup_dir(repo_root: &std::path::Path) -> PathBuf {
    match repo_root.parent() {
        Some(parent) if repo_root.is_absolute() && parent.parent().is_some() => {
            parent.join("backups")
        }
        _ => PathBuf::from(DEFAULT_DB_BACKUP_DIR),
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
    //
    //    The key half gets the same owner-only treatment as the config file and
    //    the at-rest key: the certificate is public by design, but whoever can
    //    read the private key can terminate this instance's TLS anywhere. A
    //    `0644` key on a shared host is a working server and a decrypted
    //    session for every other local account.
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
        rg_core::platform::fs::ensure_owner_only(key, "TLS private key")?;
    }

    tracing::info!("Configuration validation passed");
    Ok(())
}

/// `[tls]` names both halves or neither. Half of it used to downgrade the
/// listener to plain HTTP behind one WARN line, so a typo in `[tls].key` gave
/// an instance that takes passwords and tokens in the clear while its
/// operator believes it serves HTTPS. Running without TLS is a choice made by
/// leaving both unset, never the fallback for a broken pair. Each half is
/// resolved CLI > config on its own, so `--tls-key` may complete a file's cert.
fn resolve_tls_config(
    cfg: Option<&crate::config::ConfigFile>,
    cli_cert: Option<String>,
    cli_key: Option<String>,
) -> anyhow::Result<Option<(PathBuf, PathBuf)>> {
    let cert = cli_cert.or_else(|| cfg.and_then(|c| c.tls.cert.clone()));
    let key = cli_key.or_else(|| cfg.and_then(|c| c.tls.key.clone()));
    match (cert, key) {
        (Some(cert), Some(key)) => {
            tracing::info!("TLS enabled: cert={}, key={}", cert, key);
            Ok(Some((PathBuf::from(cert), PathBuf::from(key))))
        }
        (Some(_), None) => anyhow::bail!(
            "TLS is half-configured: a certificate is set (`--tls-cert` / `[tls].cert`) but no \
             private key (`--tls-key` / `[tls].key`). Refusing to start rather than serve plain \
             HTTP; set the key that pairs with the certificate, or remove the certificate to run \
             without TLS"
        ),
        (None, Some(_)) => anyhow::bail!(
            "TLS is half-configured: a private key is set (`--tls-key` / `[tls].key`) but no \
             certificate (`--tls-cert` / `[tls].cert`). Refusing to start rather than serve plain \
             HTTP; set the certificate that pairs with the key, or remove the key to run without \
             TLS"
        ),
        (None, None) => Ok(None),
    }
}

/// `[smtp]` (with its `--smtp-*` flags) enables mail only when `host`, `user`,
/// `pass` and `from` are all set; none of them set means mail is off. Anything
/// in between used to switch mail off without a word, so notifications and
/// password reset died on a forgotten `pass`. It now refuses the start and
/// names what is missing. A blank value counts as unset: the transport always
/// authenticates, and an empty password is a placeholder, not a credential.
/// Each field is resolved CLI > config on its own; `port` arrives resolved.
fn resolve_smtp_config(
    cfg: Option<&crate::config::ConfigFile>,
    cli_host: Option<String>,
    port: u16,
    cli_user: Option<String>,
    cli_pass: Option<String>,
    cli_from: Option<String>,
) -> anyhow::Result<Option<rg_core::email::SmtpConfig>> {
    let smtp = cfg.map(|c| &c.smtp);
    let given = |cli: Option<String>, file: Option<&Option<String>>| {
        cli.or_else(|| file.cloned().flatten())
            .filter(|v| !v.trim().is_empty())
    };
    let fields = [
        ("host", given(cli_host, smtp.map(|s| &s.host))),
        ("user", given(cli_user, smtp.map(|s| &s.user))),
        ("pass", given(cli_pass, smtp.map(|s| &s.pass))),
        ("from", given(cli_from, smtp.map(|s| &s.from))),
    ];
    let missing: Vec<&str> = fields
        .iter()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| *name)
        .collect();
    if missing.len() == fields.len() {
        return Ok(None);
    }
    if !missing.is_empty() {
        let flags: Vec<String> = missing
            .iter()
            .map(|name| format!("`--smtp-{name}` / `[smtp].{name}`"))
            .collect();
        anyhow::bail!(
            "SMTP is half-configured: missing {}. Mail (notifications, password reset) needs \
             host, user, pass and from together; set the missing ones, or remove the others to \
             run without mail",
            flags.join(", ")
        );
    }
    if port == 0 {
        anyhow::bail!("config `smtp.port` must be 1-65535 (got 0) when SMTP is configured");
    }
    let [(_, Some(host)), (_, Some(user)), (_, Some(pass)), (_, Some(from))] = fields else {
        unreachable!("every field was checked present above");
    };
    let smtp = rg_core::email::SmtpConfig::new(&host, port, &user, &pass, &from);
    smtp.validate_from()
        .context("`--smtp-from` / `[smtp].from` must be a mailbox such as `Name <addr@host>`")?;
    Ok(Some(smtp))
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
/// exactly that line for `PLOMBIR_GIT_JWT_SECRET`. Taken literally, an operator
/// who copied the file and forgot to fill it in got a server that started
/// cleanly and signed every token with the empty string — and the same file
/// promised them "startup validation will fail loudly". A blank line in an
/// `.env` means "I have not set this", never "the secret is the empty string",
/// so it falls through to the next source and, if there is none, to that
/// source's own honest error.
fn env_secret(name: &str) -> Option<String> {
    non_blank_env_value(std::env::var(name).ok())
}

fn non_blank_env_value(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

/// Resolve whether this instance accepts self-service registrations:
/// `PLOMBIR_GIT_REGISTRATION` > `[auth].registration` > `"open"`.
///
/// A value neither source recognises is a **hard startup error**. Every other
/// unparseable knob in this file behaves the same way, and this one has the
/// sharpest edge: an operator who writes `registration = "close"` is asking for
/// a closed instance, and quietly booting an open one would leave registration
/// wide open on exactly the deployment that tried to shut it.
///
/// An empty / whitespace-only env value counts as unset (the `.env` convention
/// [`env_secret`] documents), so a commented-out line does not fail the start.
/// An empty value *in the config file* is a typo and is rejected — a TOML key
/// you wrote deliberately means something.
fn resolve_registration_mode(
    cfg: Option<&crate::config::ConfigFile>,
    env_value: Option<&str>,
) -> anyhow::Result<rg_core::user::registration::RegistrationMode> {
    use rg_core::user::registration::RegistrationMode;

    if let Some(raw) = env_value.map(str::trim).filter(|value| !value.is_empty()) {
        return RegistrationMode::parse(raw)
            .map_err(|reason| anyhow::anyhow!("env PLOMBIR_GIT_REGISTRATION: {reason}"));
    }

    match cfg.and_then(|config| config.auth.registration.as_deref()) {
        Some(raw) => RegistrationMode::parse(raw)
            .map_err(|reason| anyhow::anyhow!("config `[auth].registration`: {reason}")),
        None => Ok(RegistrationMode::default()),
    }
}

/// `verify-email` registration creates nothing until a mailed link is followed,
/// so an instance that cannot send mail — or can only build links from the
/// requester's own `Host` — would accept every registration and complete none.
/// That is refused at startup rather than discovered by the first person who
/// waits for a mail that never comes.
fn require_mail_for_registration(
    mode: rg_core::user::registration::RegistrationMode,
    smtp_configured: bool,
    external_url_configured: bool,
) -> anyhow::Result<()> {
    if mode != rg_core::user::registration::RegistrationMode::VerifyEmail {
        return Ok(());
    }
    if !smtp_configured {
        anyhow::bail!(
            "[auth].registration = \"verify-email\" mails a confirmation link to every new \
             address, and no [smtp] is configured; configure outbound mail, or choose \
             \"open\" or \"closed\""
        );
    }
    if !external_url_configured {
        anyhow::bail!(
            "[auth].registration = \"verify-email\" mails links into strangers' inboxes, and \
             [server].external_url is not set; a link built from the request Host would let \
             the requester choose where the token goes"
        );
    }
    Ok(())
}

/// Resolve the pair of secrets every server-side path needs: the one that
/// *signs* and the one that *encrypts*. Shared with the one-shot subcommands
/// that also have to open at-rest data, so "which key opens this database" has
/// one answer and not one per entry point.
pub(crate) struct AuthSecrets {
    pub(crate) jwt_secret: String,
    pub(crate) encryption_key: String,
    missing_key_file: Option<PathBuf>,
}

pub(crate) fn resolve_auth_secrets(
    cfg: Option<&crate::config::ConfigFile>,
    jwt_secret: Option<String>,
    encryption_key: Option<String>,
    key_file: &Path,
) -> anyhow::Result<AuthSecrets> {
    // Resolve JWT secret: env var > CLI args > config file > error
    let resolved_jwt_secret = if let Some(env_secret) = env_secret("PLOMBIR_GIT_JWT_SECRET") {
        validate_jwt_secret(&env_secret, "environment variable PLOMBIR_GIT_JWT_SECRET")?;
        tracing::info!("Using JWT secret from environment variable PLOMBIR_GIT_JWT_SECRET");
        env_secret
    } else if let Some(cli_secret) = jwt_secret {
        validate_jwt_secret(&cli_secret, "--jwt-secret CLI argument")?;
        cli_secret
    } else if let Some(cfg_secret) = cfg.and_then(|c| c.auth.jwt_secret.clone()) {
        validate_jwt_secret(&cfg_secret, "config file [auth].jwt_secret")?;
        cfg_secret
    } else {
        anyhow::bail!(
            "No JWT secret provided. Set PLOMBIR_GIT_JWT_SECRET, use --jwt-secret, or configure [auth].jwt_secret in config file"
        );
    };

    // Resolve the at-rest encryption secret: env var > CLI arg > config file >
    // durable key file. A missing file is handled only after migrations and
    // preflight: legacy ciphertext first proves the effective JWT-era key,
    // while an empty database receives a new random key.
    let (resolved_encryption_key, missing_key_file) = if let Some(env_key) =
        env_secret("PLOMBIR_GIT_ENCRYPTION_KEY")
    {
        validate_jwt_secret(&env_key, "environment variable PLOMBIR_GIT_ENCRYPTION_KEY")?;
        tracing::info!("Using at-rest encryption key from PLOMBIR_GIT_ENCRYPTION_KEY");
        (env_key, None)
    } else if let Some(cli_key) = encryption_key {
        validate_jwt_secret(&cli_key, "--encryption-key CLI argument")?;
        tracing::info!("Using at-rest encryption key from --encryption-key");
        (cli_key, None)
    } else if let Some(cfg_key) = cfg.and_then(|c| c.auth.encryption_key.clone()) {
        validate_jwt_secret(&cfg_key, "config file [auth].encryption_key")?;
        tracing::info!("Using at-rest encryption key from config file [auth].encryption_key");
        (cfg_key, None)
    } else {
        match read_key_file(key_file)? {
            Some(key) => {
                validate_jwt_secret(&key, "[auth].key_file")?;
                tracing::info!(path = %key_file.display(), "Using at-rest encryption key from key file");
                (key, None)
            }
            None => {
                tracing::info!(
                    path = %key_file.display(),
                    "No explicit at-rest encryption key or key file; startup will establish one"
                );
                (resolved_jwt_secret.clone(), Some(key_file.to_owned()))
            }
        }
    };

    Ok(AuthSecrets {
        jwt_secret: resolved_jwt_secret,
        encryption_key: resolved_encryption_key,
        missing_key_file,
    })
}

fn read_key_file(path: &Path) -> anyhow::Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    ensure_regular_file(
        path,
        "at-rest encryption key file",
        "point [auth].key_file at a regular file, or remove it and let the server create it",
    )?;
    rg_core::platform::fs::ensure_owner_only(path, "at-rest encryption key file")?;
    let key = std::fs::read_to_string(path).map_err(|error| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "at-rest encryption key file",
            path,
            &error,
            "the server must be able to read [auth].key_file",
        ))
    })?;
    let key = key.trim().to_owned();
    if key.is_empty() {
        anyhow::bail!(
            "at-rest encryption key file {} is empty; restore its original contents or remove it only before the first start",
            path.display()
        );
    }
    Ok(Some(key))
}

/// Atomically persist `key` if the file is missing. A concurrent first start
/// reuses the value it finds rather than replacing the other process's key.
fn ensure_key_file(path: &Path, key: &str) -> anyhow::Result<String> {
    if let Some(existing) = read_key_file(path)? {
        return Ok(existing);
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        // The key file below is `0600` from its first byte, and the directory
        // that holds it is created the same way: it is the server making a
        // state directory that did not exist, not an operator's directory to be
        // left alone.
        rg_core::platform::fs::create_dir_all_owner_only(parent).map_err(|error| {
            anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
                "at-rest encryption key directory",
                parent,
                &error,
                "point [auth].key_file at a directory the server can write to",
            ))
        })?;
    }

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            file.write_all(key.as_bytes())
                .and_then(|()| file.write_all(b"\n"))
                .and_then(|()| file.sync_all())
                .map_err(|error| {
                    anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
                        "at-rest encryption key file",
                        path,
                        &error,
                        "the server must be able to write and fsync [auth].key_file",
                    ))
                })?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| {
                        format!(
                            "failed to set at-rest encryption key permissions: {}",
                            path.display()
                        )
                    })?;
            }
            Ok(key.to_owned())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => read_key_file(path)?
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "at-rest encryption key file {} disappeared during creation",
                    path.display()
                )
            }),
        Err(error) => Err(anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "at-rest encryption key file",
            path,
            &error,
            "the server generates this key on first start and needs write access to its directory",
        ))),
    }
}

fn generate_encryption_key() -> String {
    use base64::Engine;
    use rand::RngCore;

    let mut bytes = [0_u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Complete the part of key resolution that requires the migrated database.
/// It runs before any durable value is written by this server start.
async fn establish_encryption_key(
    db: &rg_db::DatabaseConnection,
    secrets: &mut AuthSecrets,
) -> anyhow::Result<()> {
    let had_marker = rg_core::auth::key_check::has_encryption_key_check(db).await?;
    let legacy_probe =
        rg_core::auth::key_check::verify_encryption_key(db, &secrets.encryption_key).await?;
    let holds_encrypted_data = had_marker || !legacy_probe.is_empty();

    // An at-rest key set to the signing secret makes the two one secret again:
    // whoever reads the JWT secret — it signs every session, and lives in every
    // `.env` — also decrypts TOTP seeds, CI secrets and SSO/LDAP passwords. The
    // deployment guide's own quick start used to write one value into both
    // (card_60b16673b390). On a database that holds nothing encrypted yet there
    // is nothing to keep readable, so the start is refused; on one that does, the
    // data was encrypted under that value and refusing would only lock it away,
    // so it is said loudly instead, with the way out.
    if secrets.missing_key_file.is_none() && secrets.encryption_key == secrets.jwt_secret {
        if !holds_encrypted_data {
            anyhow::bail!(
                "the at-rest encryption key is the same value as the JWT signing secret. Give \
                 PLOMBIR_GIT_ENCRYPTION_KEY (or [auth].encryption_key) a value of its own, or \
                 leave it unset so the server creates its own key file. Nothing has been \
                 written."
            );
        }
        tracing::warn!(
            "the at-rest encryption key is the same value as the JWT signing secret: a leak \
             of one is a leak of both. Move the data to a key of its own with \
             `plombir-git rotate-encryption-key` (see deploy/README.md, \"Secrets and rotation\")"
        );
    }

    if let Some(path) = secrets.missing_key_file.take() {
        let key_to_persist = if had_marker || !legacy_probe.is_empty() {
            tracing::warn!(
                path = %path.display(),
                "materializing the legacy at-rest key into [auth].key_file; future JWT rotations will not change it"
            );
            secrets.encryption_key.clone()
        } else {
            generate_encryption_key()
        };
        secrets.encryption_key = ensure_key_file(&path, &key_to_persist)?;
        // A concurrent first start may have supplied this file. Check the value
        // that will actually be used rather than assuming it matches ours.
        rg_core::auth::key_check::verify_encryption_key(db, &secrets.encryption_key).await?;
        tracing::info!(path = %path.display(), "at-rest encryption key file ready");
    }

    rg_core::auth::key_check::ensure_encryption_key_check(db, &secrets.encryption_key).await
}

/// Initialise and run the Plombir Git server (HTTP + SSH).
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
    log_format: Option<crate::config::LogFormat>,
    log_max_size_mb: Option<u64>,
    log_max_files: Option<usize>,
    listen_address_file: Option<String>,
) -> anyhow::Result<()> {
    // ── Load config file (if specified) ────────────────────────
    let cfg = if let Some(config_path) = &config {
        Some(load_config_file(config_path.as_str())?)
    } else {
        None
    };

    // `max_size_mb` is resolved like every other knob below, but nothing
    // enforces it; whether anyone *asked* for it is what decides the warning.
    let log_max_size_mb_was_set = log_max_size_mb.is_some()
        || cfg
            .as_ref()
            .is_some_and(|c| c.logging.max_size_mb.is_some());
    let resolved_log_format = log_format
        .or_else(|| cfg.as_ref().and_then(|c| c.logging.format))
        .unwrap_or_default();

    // Resolve every dual-source knob in one place: CLI args > config file >
    // built-in default.
    let ResolvedSettings {
        repo_root: resolved_repo_root,
        state_permissions: resolved_state_permissions,
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

    // Git, gix, SQLite, the registry stores and embedded CI all create state.
    // Several of those writers do not expose an open mode, so install the one
    // policy they all inherit before the first creating filesystem call below
    // (the optional log appender is the first one).
    resolved_state_permissions.install();

    let encryption_key_file =
        resolve_encryption_key_file(cfg.as_ref(), resolved_host_key.as_deref());
    let mut resolved_auth_secrets = resolve_auth_secrets(
        cfg.as_ref(),
        jwt_secret,
        encryption_key,
        &encryption_key_file,
    )?;

    let resolved_docker = docker
        || cfg
            .as_ref()
            .and_then(|c| c.ci.docker)
            .unwrap_or(DEFAULT_CI_DOCKER);
    let resolved_external_runners = external_runners
        || cfg
            .as_ref()
            .and_then(|c| c.ci.external_runners)
            .unwrap_or(DEFAULT_CI_EXTERNAL_RUNNERS);
    let resolved_allow_host_runner = allow_host_runner
        || cfg
            .as_ref()
            .and_then(|c| c.ci.allow_host_runner)
            .unwrap_or(DEFAULT_CI_ALLOW_HOST_RUNNER);
    // An operator who writes `runner_labels = []` means it: the embedded runner
    // then answers to nothing and every job carrying a label is refused. So the
    // fallback is keyed on the setting being *absent*, not on the list being
    // empty — `unwrap_or_default()` here would silently turn "answer to
    // nothing" into "answer to everything the default claims".
    let resolved_runner_labels = cfg
        .as_ref()
        .and_then(|c| c.ci.runner_labels.clone())
        .unwrap_or_else(rg_core::ci::default_runner_labels);
    let resolved_registration = resolve_registration_mode(
        cfg.as_ref(),
        std::env::var("PLOMBIR_GIT_REGISTRATION").ok().as_deref(),
    )?;
    // Env var wins over config file; both default off (opt-in).
    let resolved_attestation_enabled = match std::env::var("PLOMBIR_GIT_ATTESTATION_ENABLED") {
        Ok(v) => matches!(v.trim(), "1" | "true" | "yes" | "on"),
        Err(_) => cfg
            .as_ref()
            .and_then(|c| c.releases.attestation_enabled)
            .unwrap_or(DEFAULT_ATTESTATION_ENABLED),
    };
    let resolved_source_url = crate::config::resolve_source_url(cfg.as_ref())?;
    announce_source_link(&resolved_source_url);
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
        .unwrap_or(DEFAULT_RATE_LIMIT_MAX_KEYS);
    let resolved_rate_limit_auth_max = cfg
        .as_ref()
        .and_then(|c| c.rate_limit.auth_max)
        .unwrap_or(DEFAULT_AUTH_RATE_LIMIT_MAX);
    let resolved_rate_limit_auth_window = cfg
        .as_ref()
        .and_then(|c| c.rate_limit.auth_window_secs)
        .unwrap_or(DEFAULT_AUTH_RATE_LIMIT_WINDOW);
    let resolved_rate_limit_agent_max = cfg
        .as_ref()
        .and_then(|c| c.rate_limit.agent_max)
        .unwrap_or(DEFAULT_AGENT_RATE_LIMIT_MAX);
    let resolved_rate_limit_agent_window = cfg
        .as_ref()
        .and_then(|c| c.rate_limit.agent_window_secs)
        .unwrap_or(DEFAULT_AGENT_RATE_LIMIT_WINDOW);
    let resolved_package_upload_max_bytes = resolve_package_upload_max_bytes(cfg.as_ref())?;
    let resolved_trusted_import_origins = resolve_trusted_import_origins(cfg.as_ref())?;
    let resolved_import_transport_policy = resolve_import_transport_policy(cfg.as_ref())?;
    if resolved_import_transport_policy.allows_insecure_http() {
        tracing::warn!(
            "Import credentials may traverse explicitly allowlisted plaintext HTTP origins because \
             [imports].allow_insecure_http_origins is non-empty"
        );
    }
    let resolved_oidc_transport_policy = resolve_oidc_transport_policy(cfg.as_ref())?;
    if resolved_oidc_transport_policy.allows_insecure_http() {
        tracing::warn!(
            "Custom OIDC discovery, client secrets, and access tokens may traverse explicitly \
             allowlisted plaintext HTTP origins because \
             [auth].allow_insecure_oidc_origins is non-empty"
        );
    }
    let resolved_ldap_transport_policy = resolve_ldap_transport_policy(cfg.as_ref())?;
    if resolved_ldap_transport_policy.allows_insecure_ldap() {
        tracing::warn!(
            "LDAP service bind and user passwords may traverse explicitly allowlisted plaintext \
             endpoints because [auth].allow_insecure_ldap_endpoints is non-empty"
        );
    }
    let resolved_mirror_transport_policy = resolve_mirror_transport_policy(cfg.as_ref());
    if resolved_mirror_transport_policy.allows_insecure_http() {
        tracing::warn!(
            "Mirror credentials and repository content may traverse plaintext HTTP because \
             [mirror].allow_insecure_http is enabled"
        );
    }
    let resolved_webhook_transport_policy = resolve_webhook_transport_policy(cfg.as_ref());
    if resolved_webhook_transport_policy.allows_insecure_http() {
        tracing::warn!(
            "Outbound webhook delivery over plaintext HTTP is enabled by \
             [webhooks].allow_insecure_http"
        );
    }

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
    let resolved_external_webhook_secret = env_secret("PLOMBIR_GIT_EXTERNAL_WEBHOOK_SECRET")
        .or_else(|| {
            cfg.as_ref()
                .and_then(|c| c.webhooks.external_secret.clone())
        })
        .filter(|secret| !secret.trim().is_empty());
    if resolved_external_webhook_secret.is_some() {
        tracing::info!("Inbound external-webhook HMAC-SHA256 verification enabled");
    }

    // `GET /metrics` sits on the main HTTP port: behind the recommended proxy
    // it is on the internet unless the proxy refuses it or a token guards it
    // (card_ab8a1ca92a56). Env var wins, then the config file.
    let resolved_metrics_enabled = cfg
        .as_ref()
        .and_then(|c| c.observability.metrics_enabled)
        .unwrap_or(DEFAULT_METRICS_ENABLED);
    let resolved_metrics_token = env_secret("PLOMBIR_GIT_METRICS_TOKEN")
        .or_else(|| {
            cfg.as_ref()
                .and_then(|c| c.observability.metrics_token.clone())
        })
        .filter(|token| !token.trim().is_empty());
    if !resolved_metrics_enabled {
        tracing::info!("GET /metrics is switched off ([observability].metrics_enabled = false)");
    } else if resolved_metrics_token.is_some() {
        tracing::info!("GET /metrics requires the configured bearer token");
    } else {
        tracing::warn!(
            "GET /metrics is open to anyone who reaches the HTTP port: set \
             [observability].metrics_token (or PLOMBIR_GIT_METRICS_TOKEN), or have the reverse \
             proxy refuse /metrics"
        );
    }
    let resolved_metrics_access = rg_http::metrics::MetricsAccess::new(
        resolved_metrics_enabled,
        resolved_metrics_token.as_deref().map(str::trim),
    );

    // Timeouts from config (with defaults)
    let resolved_job_timeout = cfg
        .as_ref()
        .map(|c| c.timeouts.job_secs)
        .unwrap_or_else(default_job_timeout);
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
    let resolved_db_max_connections = match cfg.as_ref().and_then(|c| c.database.max_connections) {
        Some(configured) => configured,
        None => rg_db::default_max_connections(&resolved_db_url)?,
    };

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
    require_positive(
        "rate_limit.agent_window_secs",
        resolved_rate_limit_agent_window,
    )?;
    require_positive(
        "database.max_connections",
        u64::from(resolved_db_max_connections),
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
            .unwrap_or("plombir-git");
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

    let telemetry_guard =
        telemetry::init(log_writer, appender_guard, otel_config, resolved_log_format)?;

    tracing::info!(
        state_permissions = %resolved_state_permissions,
        umask = %format!("{:04o}", resolved_state_permissions.umask()),
        regular_file_mode = %format!("{:04o}", resolved_state_permissions.regular_file_mode()),
        directory_mode = %format!("{:04o}", resolved_state_permissions.directory_mode()),
        "Installed the process-wide creation policy for server-owned state"
    );

    match resolved_log_file {
        Some(ref log_path) => tracing::info!(
            file = %log_path,
            format = resolved_log_format.as_str(),
            "Logging to file with daily rotation"
        ),
        None => tracing::info!(format = resolved_log_format.as_str(), "Logging to stdout"),
    }
    // Said whenever it is set, not only when it differs from the default: the
    // example config used to carry `max_size_mb = 10`, which matched the
    // default and so never warned — a size cap every reader believed in and
    // nothing applied (card_0d7755e0dfe0).
    if log_max_size_mb_was_set {
        tracing::warn!(
            max_size_mb = resolved_log_max_size_mb,
            "[logging].max_size_mb / --log-max-size-mb is not enforced: log files rotate daily, \
             not by size. Use max_files to cap how many are kept, and remove this setting."
        );
    }

    // A half-written `[smtp]` or `[tls]` stops the start here, before the
    // first directory or database is created, rather than quietly disabling
    // mail or downgrading the listener to plain HTTP.
    let smtp_config = resolve_smtp_config(
        cfg.as_ref(),
        smtp_host,
        resolved_smtp_port,
        smtp_user,
        smtp_pass,
        smtp_from,
    )?;
    let tls_config = resolve_tls_config(cfg.as_ref(), tls_cert, tls_key)?;

    let repo_root = PathBuf::from(&resolved_repo_root);
    // What lives below this root is the *content* of every private repository
    // on the instance, so a root the server creates is created `0700` — a bare
    // `create_dir_all` takes its mode from the ambient umask (`0755` on a stock
    // host) and no later call narrows it.
    rg_core::platform::fs::create_dir_all_owner_only(&repo_root).map_err(|e| {
        anyhow::anyhow!(rg_core::platform::fs::describe_path_error(
            "repo_root",
            &repo_root,
            &e,
            "point `--repo-root` / `[server].repo_root` at a directory the server can create",
        ))
    })?;
    // The warning is what remains for the root the server did *not* create: a
    // directory an operator or a quick-start `mkdir` made stays as wide as they
    // made it, because narrowing somebody else's directory can cut off a backup
    // job that reaches it by group.
    rg_core::platform::fs::warn_if_others_can_reach("repo_root", &repo_root);

    // ── Staging sweep ─────────────────────────────────────────────
    // Upload spools under `repo_root/.tmp/` are retired by a destructor, which
    // a `SIGKILL`, the OOM killer and a container restart all skip. Nothing
    // else ever reads those directories, so without this pass an interrupted
    // 512 MiB publish is a permanent 512 MiB. The same pass walks the storage
    // root for the spools that are written *beside* their destination instead —
    // an LFS object is up to 10 GiB of them — because those have no directory
    // of their own to list. Best-effort and never fatal: an unswept spool costs
    // disk, refusing to serve costs the instance. See `rg_core::staging` for the
    // age bound and why it is not zero.
    let sweep =
        rg_core::staging::sweep_stale_spools(&repo_root, rg_core::staging::STALE_SPOOL_AGE).await;
    if sweep != rg_core::staging::SweepReport::default() {
        tracing::info!(
            removed = sweep.removed,
            retained = sweep.retained,
            failed = sweep.failed,
            "swept upload spools left behind by a previous run"
        );
    }

    // ── Git CLI gateway (seed configured command timeout) ─────────
    // Fatal, not a warning. The constructor is also the git version floor
    // (`rg_git::cli_gateway::MIN_GIT_VERSION`): a git that does not know
    // `http.curloptResolve` would clone imports and mirrors through its own
    // resolver — after the SSRF guard looked, with the stored credential
    // attached — so a server on such a host, or on one with no git at all,
    // must not come up and wait for the first import to find out.
    rg_git::cli_gateway::init_global_gateway(std::time::Duration::from_secs(resolved_git_timeout))
        .context("git is required to serve: install git 2.38 or newer and put it on PATH")?;
    tracing::info!(git_cmd_secs = resolved_git_timeout, "Git CLI gateway ready");

    // ── Database ──────────────────────────────────────────────────
    tracing::info!(
        "Connecting to database: {}",
        rg_db::redact_database_url(&resolved_db_url)
    );
    let server_db = dbconn::connect_server_with_timeouts(
        &resolved_db_url,
        &repo_root,
        resolved_db_connect_timeout,
        resolved_db_idle_timeout,
        resolved_db_max_connections,
    )
    .await?;
    let db = server_db.connection().clone();
    rg_db::run_migrations(&db).await?;
    tracing::info!("Database ready");

    // ── Interrupted storage mutations ─────────────────────────────
    // The spool sweep above retires drafts; this pass finishes journalled
    // deletions, moves, repository creations and attachment publications.
    // Deletes and moves carry a
    // marker that decides whether bytes return or stay retired. A create is the
    // inverse: it claims the final Git path before inserting its row, and a
    // process can die after that insert but before its marker write. The pass
    // therefore runs only after migrations and asks the database before it
    // removes a create path. Failure is never guessed through: the entry and
    // path remain for the next start or an operator.
    let recovered = rg_core::deletion_recovery::recover_interrupted_storage_at(
        &db,
        &repo_root,
        rg_core::deletion_recovery::INTERRUPTED_DELETION_AGE,
    )
    .await;
    if recovered != rg_core::deletion_recovery::RecoveryReport::default() {
        tracing::info!(
            restored = recovered.restored,
            destroyed = recovered.destroyed,
            kept = recovered.kept,
            discarded_creations = recovered.discarded_creations,
            discarded_publications = recovered.discarded_publications,
            retained = recovered.retained,
            failed = recovered.failed,
            "finished storage mutations a previous run did not survive"
        );
    }

    // ── At-rest encryption key preflight ──────────────────────────
    // Refuse to serve with a key that cannot open what is already stored. On a
    // first boot this also creates the durable key file and its database marker
    // before any other startup path writes encrypted state.
    establish_encryption_key(&db, &mut resolved_auth_secrets).await?;

    // Webhook delivery is detached from the request that caused it and signs
    // with an encrypted column, so it is the one reader that cannot be handed
    // the key as a parameter. Publish before anything can dispatch.
    rg_core::auth::at_rest_key::publish(&resolved_auth_secrets.encryption_key);

    // The migration renamed `webhooks.secret` to `secret_encrypted`; sealing the
    // values it already holds needs the key, which only exists here. Fatal on
    // failure: a half-sealed column is exactly what this pass exists to prevent,
    // and the write uses the key the preflight above just verified.
    rg_core::webhook::service::seal_legacy_secrets(&db, &resolved_auth_secrets.encryption_key)
        .await
        .context("seal webhook signing secrets at rest")?;

    // The same boot slot, for the credential that was never in a column at all:
    // one typed into the URL of a mirror, an import or a webhook. Create and
    // update now split it out, so these three passes only ever have rows that
    // predate that to convert — and they run before anything can sync, import
    // or deliver, which is where such a URL would be quoted into a persisted
    // error. Fatal for the same reason as the sealing above: a half-converted
    // column is exactly what the pass exists to prevent.
    rg_core::mirror::service::lift_legacy_url_credentials(
        &db,
        &resolved_auth_secrets.encryption_key,
    )
    .await
    .context("move credentials out of mirror remote URLs")?;
    rg_core::import::service::strip_legacy_source_url_credentials(&db)
        .await
        .context("remove tokens from import source URLs")?;
    rg_core::webhook::service::strip_legacy_url_credentials(&db)
        .await
        .context("remove credentials from webhook URLs")?;

    // An owner registered before the reservation existed keeps a name the
    // application answers for itself, and its `/{owner}` page is unreachable
    // for good. Read-only and never fatal: renaming somebody's account is the
    // operator's decision, and a boot pass has no business making it.
    rg_core::namespace::report_owners_holding_reserved_names(&db).await;
    rg_core::namespace::report_names_held_by_an_account_and_an_organization(&db).await;
    rg_core::namespace::report_repositories_the_transport_cannot_address(&db).await;
    rg_core::namespace::report_repositories_with_names_that_are_not_ascii(&db).await;

    // ── Instance provenance identity ──────────────────────────────
    // The Ed25519 key that signs release attestations and backs the CI OIDC
    // JWKS. Loaded from the database — on the first start it adopts exactly the
    // key the old `jwt_secret` derivation produced, so nothing that was already
    // signed stops verifying, and from then on the identity outlives every
    // rotation of either secret (card_3aecf3708ebe).
    let instance_key = std::sync::Arc::new(
        rg_core::auth::instance_key::load_or_adopt(
            &db,
            &resolved_auth_secrets.jwt_secret,
            &resolved_auth_secrets.encryption_key,
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
        .unwrap_or_else(default_shutdown_grace);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let signal_shutdown_tx = shutdown_tx.clone();
    tokio::spawn(async move {
        wait_for_shutdown_signal().await;
        if signal_shutdown_tx.send(true).is_err() {
            // Every receiver has already shut down.
        }
    });

    let audit_config = cfg.as_ref().map(|config| &config.audit);
    let _audit_archiver_handle = if audit_config
        .and_then(|config| config.enabled)
        .unwrap_or(DEFAULT_AUDIT_ENABLED)
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
                .unwrap_or(rg_core::audit::archiver::DEFAULT_ARCHIVE_AFTER_DAYS),
            interval_minutes: audit_config
                .and_then(|config| config.interval_minutes)
                .unwrap_or(rg_core::audit::archiver::DEFAULT_INTERVAL_MINUTES),
            batch_size: audit_config
                .and_then(|config| config.batch_size)
                .unwrap_or(rg_core::audit::archiver::DEFAULT_BATCH_SIZE),
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

    // ── Scheduled database backups ────────────────────────────────
    // The point of doing this in-process rather than in a cron entry on one
    // host: the schedule travels with the config file, the snapshot is taken
    // from the pool the server itself is using (so it cannot address a
    // different database, the way a `backup-db` without `--config` does), and
    // an unusable backup directory fails the start instead of surfacing a day
    // later as a warning.
    let backup_config = cfg.as_ref().map(|config| &config.backup);
    let _db_backup_handle = if backup_config
        .and_then(|config| config.enabled)
        .unwrap_or(DEFAULT_BACKUP_ENABLED)
    {
        let backup_dir = backup_config
            .and_then(|config| config.dir.as_deref())
            .map(PathBuf::from)
            .unwrap_or_else(|| default_db_backup_dir(&repo_root));
        tracing::info!(
            backup_dir = %std::path::absolute(&backup_dir)
                .unwrap_or_else(|_| backup_dir.clone())
                .display(),
            "Scheduled database backups enabled"
        );
        let db_backup_config = rg_core::backup::DbBackupConfig {
            dir: backup_dir,
            interval_hours: backup_config
                .and_then(|config| config.interval_hours)
                .unwrap_or(rg_core::backup::DEFAULT_INTERVAL_HOURS),
            keep_last: backup_config
                .and_then(|config| config.keep_last)
                .unwrap_or(rg_core::backup::DEFAULT_KEEP_LAST),
        };
        Some(rg_core::backup::spawn_db_backup_with_shutdown(
            db.clone(),
            db_backup_config,
            Some(shutdown_rx.clone()),
        )?)
    } else {
        // Said out loud on purpose: an operator who greps the log for "backup"
        // must not have to infer the answer from the absence of a line.
        tracing::info!(
            "Scheduled database backups are OFF ([backup].enabled): this database is only backed \
             up when someone runs `plombir-git backup-db`. Note that repositories under repo_root \
             are never covered by a database backup — snapshot the data volume for those."
        );
        None
    };

    // ── Retention of append-only tables ───────────────────────────
    // Webhook deliveries, notifications and login attempts are written on
    // every event and nothing else ever deletes them. The windows are
    // range-checked here, so a 0 fails the start rather than the first pass.
    let retention_config = cfg.as_ref().map(|config| &config.retention);
    let _retention_handle = if retention_config
        .and_then(|config| config.enabled)
        .unwrap_or(DEFAULT_RETENTION_ENABLED)
    {
        let defaults = rg_core::retention::RetentionConfig::default();
        let config = rg_core::retention::RetentionConfig {
            webhook_delivery_days: retention_config
                .and_then(|config| config.webhook_delivery_days)
                .unwrap_or(defaults.webhook_delivery_days),
            notification_read_days: retention_config
                .and_then(|config| config.notification_read_days)
                .unwrap_or(defaults.notification_read_days),
            notification_unread_days: retention_config
                .and_then(|config| config.notification_unread_days)
                .unwrap_or(defaults.notification_unread_days),
            login_log_days: retention_config
                .and_then(|config| config.login_log_days)
                .unwrap_or(defaults.login_log_days),
            interval_minutes: retention_config
                .and_then(|config| config.interval_minutes)
                .unwrap_or(defaults.interval_minutes),
            batch_size: retention_config
                .and_then(|config| config.batch_size)
                .unwrap_or(defaults.batch_size),
        };
        tracing::info!(
            webhook_delivery_days = config.webhook_delivery_days,
            notification_read_days = config.notification_read_days,
            notification_unread_days = config.notification_unread_days,
            login_log_days = config.login_log_days,
            "Retention sweep enabled"
        );
        Some(rg_core::retention::spawn_retention_with_shutdown(
            db.clone(),
            config,
            Some(shutdown_rx.clone()),
        )?)
    } else {
        // Said out loud, as for backups: "does this instance ever trim these
        // tables?" should be answerable from the log.
        tracing::info!(
            "Retention sweep is OFF ([retention].enabled): webhook deliveries, notifications \
             and login attempts are kept forever"
        );
        None
    };

    // ── HTTP server ───────────────────────────────────────────────
    // A reset link may only name the configured public address — one taken
    // from the request `Host` would let an anonymous requester choose where a
    // victim's token is sent — so `forgot-password` refuses mail without it
    // (card_e67aeb8c09ca). Say so now rather than at the first locked-out user.
    if smtp_config.is_some() && resolved_external_url.is_none() {
        tracing::warn!(
            "SMTP is configured but [server].external_url is not: password reset emails will be refused until external_url names this instance's public URL"
        );
    }

    validate_config(&resolved_auth_secrets.jwt_secret, &repo_root, &tls_config)?;
    require_mail_for_registration(
        resolved_registration,
        smtp_config.is_some(),
        resolved_external_url.is_some(),
    )?;

    // Logged after the subscriber is up, so the one line an operator greps for
    // when a colleague "cannot sign up" actually reaches the log.
    if resolved_registration.is_closed() {
        tracing::info!(
            "self-service registration is CLOSED ([auth].registration): POST /users/register \
             refuses with 403. The first account on an empty instance is still admitted, and \
             LDAP/SSO auto-provision is unaffected."
        );
    }
    tracing::info!(
        max_bytes = resolved_package_upload_max_bytes,
        "Package upload artifact ceiling configured"
    );

    // One CI engine and one WebSocket hub for the whole process: the SSH
    // transport's post-push hooks trigger pipelines and push `ci_triggered` /
    // `push` events to the very clients the HTTP server's sockets belong to, so
    // a second hub of its own would fan out to nobody (card_b4fefeee8abf).
    //
    // The engine gets the hub and the SMTP configuration too: a pipeline that
    // goes green can land a merge commit, and until card_85b8d59246b5 the hooks
    // rg-ci ran for it had neither, so that merge produced no real-time event
    // and no mail while the identical merge over REST produced both.
    let notification_hub = rg_http::ws::NotificationHub::new();
    let ci_engine = configured_ci_engine(
        rg_ci::CiNotifications {
            notifier: Some(std::sync::Arc::new(notification_hub.clone())),
            smtp_config: smtp_config.clone(),
        },
        resolved_job_timeout,
        resolved_runner_labels.clone(),
        shutdown_rx.clone(),
    );
    tracing::info!(
        job_timeout_secs = ci_engine.job_timeout_secs(),
        runner_labels = ci_engine.runner_labels().join(","),
        "Embedded CI runner timeout configured"
    );
    let ci_engine: std::sync::Arc<dyn rg_core::ci::CiTrigger + Send + Sync> =
        std::sync::Arc::new(ci_engine);
    let instance_settings = rg_core::instance::InstanceSettingsCache::default();

    // Steady background writers queue on their own pool (card_bb685235de6f).
    let db_write = rg_db::open_write_pool(
        &resolved_db_url,
        resolved_db_connect_timeout,
        resolved_db_idle_timeout,
        &db,
    )
    .await
    .map_err(|e| dbconn::annotate_db_open_error(e, &resolved_db_url))?;
    let http_config = rg_http::HttpServerConfig {
        listen_addr: resolved_http_addr,
        repo_root: repo_root.clone(),
        db: db.clone(),
        db_write: Some(db_write.clone()),
        jwt_secret: resolved_auth_secrets.jwt_secret.clone(),
        encryption_key: resolved_auth_secrets.encryption_key.clone(),
        instance_key,
        external_webhook_secret: resolved_external_webhook_secret,
        docker_enabled: resolved_docker,
        external_runners: resolved_external_runners,
        allow_host_runner: resolved_allow_host_runner,
        registration: resolved_registration,
        trusted_import_origins: resolved_trusted_import_origins,
        import_transport_policy: resolved_import_transport_policy,
        oidc_transport_policy: resolved_oidc_transport_policy,
        ldap_transport_policy: resolved_ldap_transport_policy,
        mirror_transport_policy: resolved_mirror_transport_policy,
        webhook_transport_policy: resolved_webhook_transport_policy,
        package_upload_max_bytes: resolved_package_upload_max_bytes,
        rate_limit_max: resolved_rate_limit_max,
        rate_limit_window_secs: resolved_rate_limit_window,
        rate_limit_trusted_proxies: resolved_rate_limit_trusted_proxies,
        rate_limit_max_keys: resolved_rate_limit_max_keys,
        rate_limit_auth_max: resolved_rate_limit_auth_max,
        rate_limit_auth_window_secs: resolved_rate_limit_auth_window,
        rate_limit_agent_max: resolved_rate_limit_agent_max,
        rate_limit_agent_window_secs: resolved_rate_limit_agent_window,
        smtp_config: smtp_config.clone(),
        tls_config,
        external_url: resolved_external_url.clone(),
        job_timeout_secs: resolved_job_timeout,
        git_stream_timeout_secs: resolved_git_stream_timeout,
        git_idle_timeout_secs: resolved_git_idle_timeout,
        // M-14: Inject CiEngine via trait object, decoupling rg-http from rg-ci.
        ci_engine: ci_engine.clone(),
        shutdown_rx: shutdown_rx.clone(),
        shutdown_grace_secs: resolved_shutdown_grace,
        attestation_enabled: resolved_attestation_enabled,
        source_url: resolved_source_url,
        notification_hub: Some(notification_hub.clone()),
        instance_settings: instance_settings.clone(),
        metrics_access: resolved_metrics_access,
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
    // LFS for clones made from the SSH address: `git-lfs-authenticate` sends the
    // client to the HTTP side, and the only address it can honestly name is the
    // configured public one (card_d8d274ed134d).
    let ssh_lfs = resolved_external_url
        .clone()
        .map(|external_url| rg_ssh::SshLfsConfig {
            external_url,
            signing_secret: resolved_auth_secrets.jwt_secret.clone(),
        });

    // Notification mail, batched per person (card_349c2b6a0d7c). Without SMTP
    // the rows are still written — the inbox is the notification — and nothing
    // waits to mail them.
    let _notification_mail_handle = smtp_config.clone().map(|smtp| {
        rg_core::notification::mail::spawn_dispatcher(
            db.clone(),
            smtp,
            resolved_external_url.clone(),
            Some(shutdown_rx.clone()),
        )
    });

    let mirror_encryption_key = resolved_auth_secrets.encryption_key.clone();
    let post_push_context = rg_core::push_hooks::PostPushContext {
        repo_root: repo_root.clone(),
        docker_enabled: resolved_docker,
        external_runners: resolved_external_runners,
        allow_host_runner: resolved_allow_host_runner,
        jwt_secret: Some(resolved_auth_secrets.jwt_secret.clone()),
        encryption_key: Some(resolved_auth_secrets.encryption_key),
        smtp_config,
        ci_engine,
        external_url: resolved_external_url,
        notifier: Some(std::sync::Arc::new(notification_hub)),
        delivery_tracker: rg_core::task_tracker::delivery_tracker().clone(),
    };

    // ── Scheduled mirror sync ─────────────────────────────────────
    // The periodic half of the mirror feature. `POST .../mirror` has always
    // taken a `sync_interval_seconds` and written a `next_sync_at` that the
    // settings UI renders as "next sync at …", but nothing in the process ever
    // called `sync_due_mirrors`: only the manual "Sync now" button refreshed a
    // mirror, and it was also the only thing that ever moved `next_sync_at`
    // (card_d2fd29942436).
    let mirror_config = cfg.as_ref().map(|config| &config.mirror);
    let _mirror_sync_handle = if mirror_config
        .and_then(|config| config.enabled)
        .unwrap_or(DEFAULT_MIRROR_ENABLED)
    {
        let sync_config = rg_core::mirror::scheduler::MirrorSyncConfig {
            poll_interval_secs: mirror_config
                .and_then(|config| config.poll_interval_secs)
                .unwrap_or(rg_core::mirror::scheduler::DEFAULT_POLL_INTERVAL_SECS),
            batch_size: mirror_config
                .and_then(|config| config.batch_size)
                .unwrap_or(rg_core::mirror::scheduler::DEFAULT_BATCH_SIZE),
            transport_policy: resolved_mirror_transport_policy,
        };
        tracing::info!(
            poll_interval_secs = sync_config.poll_interval_secs,
            batch_size = sync_config.batch_size,
            "Scheduled mirror sync enabled"
        );
        // The secret is `encryption_key`, never `jwt_secret`: the mirror's
        // stored remote credential is encrypted at rest with the former, and
        // the two stopped being the same value in card_d740512de0a8.
        // With the post-push wiring: a pass that moves a branch owes the open
        // pull requests on it their new head (card_dbc5a1debb0a).
        Some(rg_core::mirror::scheduler::spawn_mirror_sync_with_shutdown(
            db.clone(),
            repo_root.clone(),
            mirror_encryption_key,
            sync_config,
            Some(post_push_context.clone()),
            Some(shutdown_rx.clone()),
        )?)
    } else {
        // Said out loud for the same reason as the backup line below it: an
        // operator whose mirror shows a next-sync time must be able to find out
        // from the log that nothing is going to act on it.
        tracing::info!(
            "Scheduled mirror sync is OFF ([mirror].enabled): configured mirrors are only \
             refreshed when someone triggers a sync from the repository's mirror settings."
        );
        None
    };

    let ssh_config = rg_ssh::SshServerConfig {
        host_key_path: PathBuf::from(&host_key_path),
        listen_addr: resolved_ssh_addr,
        repo_root: repo_root.clone(),
        db: db.clone(),
        db_write,
        git_stream_timeout_secs: resolved_git_stream_timeout,
        git_idle_timeout_secs: resolved_git_idle_timeout,
        post_push: Some(std::sync::Arc::new(post_push_context)),
        lfs: ssh_lfs,
        instance_settings,
        // The second consumer of the fan-out above. It used to be the one the
        // channel never reached, so a `SIGTERM` cut an SSH push mid-objects
        // while the same push over HTTP was drained (card_5317e172fd25).
        shutdown: Some(shutdown_rx.clone()),
        shutdown_grace_secs: resolved_shutdown_grace,
    };

    let (http_handle, ssh_handle) = if let Some(address_file) = listen_address_file {
        if http_config.tls_config.is_some() {
            anyhow::bail!("--listen-address-file is only supported for plain HTTP");
        }

        let http_listener = tokio::net::TcpListener::bind(&http_config.listen_addr)
            .await
            .with_context(|| {
                format!(
                    "failed to bind HTTP listener to {}",
                    http_config.listen_addr
                )
            })?;
        let ssh_listener = tokio::net::TcpListener::bind(&ssh_config.listen_addr)
            .await
            .with_context(|| {
                format!("failed to bind SSH listener to {}", ssh_config.listen_addr)
            })?;
        let http_addr = http_listener
            .local_addr()
            .context("failed to read bound HTTP listener address")?;
        let ssh_addr = ssh_listener
            .local_addr()
            .context("failed to read bound SSH listener address")?;

        publish_listen_addresses(Path::new(&address_file), http_addr, ssh_addr)?;

        let http_handle =
            tokio::spawn(async move { rg_http::run_on_listener(http_config, http_listener).await });
        let ssh_handle = tokio::spawn(async move {
            if let Err(e) = rg_ssh::start_ssh_server_on_listener(ssh_config, ssh_listener).await {
                tracing::error!("SSH server error (HTTP unaffected): {:#}", e);
            }
        });
        (http_handle, ssh_handle)
    } else {
        let http_handle = tokio::spawn(async move { rg_http::run(http_config).await });
        let ssh_handle = tokio::spawn(async move {
            if let Err(e) = rg_ssh::start_ssh_server(ssh_config).await {
                tracing::error!("SSH server error (HTTP unaffected): {:#}", e);
            }
        });
        (http_handle, ssh_handle)
    };

    let http_result = match http_handle.await {
        Ok(result) => result,
        Err(error) => Err(anyhow::Error::new(error).context("HTTP server task terminated")),
    };
    if http_result.is_err() && shutdown_tx.send(true).is_err() {
        // Every receiver stopped before the failed primary transport returned.
    }

    // Give the second transport a bounded chance to observe the same signal and
    // drain its in-flight git sessions. The pre-bound entry point does so; the
    // ordinary entry point still bypasses that shutdown-aware accept path
    // (reopened card_5317e172fd25), so the timeout remains a necessary backstop.
    match tokio::time::timeout(
        std::time::Duration::from_secs(resolved_shutdown_grace) * 2,
        ssh_handle,
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::error!("SSH server task terminated: {:#}", e),
        Err(_) => tracing::warn!(
            grace_secs = resolved_shutdown_grace,
            "SSH server did not stop within twice its grace window — releasing the database lease \
             anyway"
        ),
    }

    // Now the executor. HTTP has stopped and SSH has either stopped or exhausted
    // its bounded wait, so nothing new should be triggered; the open wiring gap
    // above remains operator-visible rather than being described as success.
    // Every embedded runner still on the tracker has been observing the same
    // signal since it was raised — so this is a wait on an unwind already in
    // progress (the interrupted job's container removed, its row handed back to
    // `pending`), not a wait on a whole build. Bounded all the same: a runner
    // that will not stop must not hold the database lease open for ever, and the
    // sweep it falls back to is the ten-minute one that used to be the only
    // outcome (card_34368880dc20).
    //
    // Before the delivery drain below, because a pipeline that finishes as it
    // unwinds spawns its post-success hooks onto *that* tracker.
    let ci_tracker = rg_core::task_tracker::ci_tracker();
    ci_tracker.close();
    if !ci_tracker.is_empty() {
        match tokio::time::timeout(
            std::time::Duration::from_secs(resolved_shutdown_grace),
            ci_tracker.wait(),
        )
        .await
        {
            Ok(()) => tracing::info!("embedded CI runners stopped and handed their jobs back"),
            Err(_) => tracing::warn!(
                grace_secs = resolved_shutdown_grace,
                "embedded CI runners did not stop within the grace window; the jobs they held \
                 stay `running` until the stuck-job sweep reclaims them"
            ),
        }
    }

    // Once more, now that *both* transports have stopped. `rg_http::run` drains
    // this tracker when the HTTP server stops, but a push accepted over SSH
    // after that point spawns its hooks — CI trigger, webhook fan-out — onto a
    // tracker whose `wait()` has already returned, and they would be severed
    // with the runtime. `close()` is idempotent and a second `wait()` on an
    // empty tracker returns at once, so this costs nothing when there was
    // nothing left.
    let delivery_tracker = rg_core::task_tracker::delivery_tracker();
    delivery_tracker.close();
    if !delivery_tracker.is_empty() {
        match tokio::time::timeout(
            std::time::Duration::from_secs(resolved_shutdown_grace),
            delivery_tracker.wait(),
        )
        .await
        {
            Ok(()) => tracing::info!("post-shutdown delivery tasks drained"),
            Err(_) => tracing::warn!(
                grace_secs = resolved_shutdown_grace,
                "delivery tasks spawned after the HTTP drain did not finish within the grace window"
            ),
        }
    }

    // Flush the OTLP exporter (and the non-blocking log appender) before exit so
    // the final batch of spans reaches the collector.
    telemetry_guard.shutdown();

    // Release the SQLite process lease after the bounded transport/worker
    // shutdown sequence above. The remaining ordinary-SSH timeout gap is
    // tracked by card_5317e172fd25 rather than hidden behind a stronger claim.
    drop(server_db);

    http_result
}

#[cfg(test)]
mod serve_tests {
    use std::path::PathBuf;

    use crate::config::{
        resolve_mirror_transport_policy, resolve_webhook_transport_policy, CliSettings, ConfigFile,
    };

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    /// The SSH transport is waited for, and the database lease is released
    /// after it (card_5317e172fd25).
    ///
    /// The sequence in `run_serve` is the whole subject and it exists nowhere
    /// else, so it is read out of the source rather than driven: booting the
    /// real thing to observe `drop` ordering would take a full server, two
    /// listeners and a signal, and would still be asserting on the order of two
    /// statements.
    ///
    /// What went wrong is what the shape below now forbids. The handle was
    /// bound to `_ssh_handle` — the underscore that says "deliberately unused"
    /// — and only HTTP was awaited, while the comment over `drop(server_db)`
    /// claimed the lease was released after *both* transports had stopped using
    /// the database. One of the two was still serving pushes.
    #[test]
    fn the_database_lease_outlives_both_transports() {
        let source = include_str!("serve.rs");
        let code = rust_source::production_rust_code_only(source);

        assert!(
            !code.contains("_ssh_handle"),
            "the SSH task is bound to `_ssh_handle`, which is the spelling that says nobody \
             intends to wait for it — and nobody did"
        );

        let awaited = code
            .find("ssh_handle,")
            .or_else(|| code.find("ssh_handle)"))
            .or_else(|| code.find("ssh_handle."))
            .expect("`run_serve` must do something with the SSH task handle");
        let released = code
            .find("drop(server_db)")
            .expect("`run_serve` must release the database lease");
        assert!(
            awaited < released,
            "the SSH task is used at byte {awaited} and the database lease is dropped at \
             {released}: the lease has to outlive the transport that is still reading through it"
        );

        // The wait itself, not merely a mention of the handle: a bare
        // `drop(ssh_handle)` would satisfy the ordering above and wait for
        // nothing.
        let waits = rust_source::production_function_call_sites(source, "run_serve", &["timeout"]);
        assert!(
            waits.len() >= 3,
            "`run_serve` must bound each of its shutdown waits — the SSH transport, the embedded \
             CI runners and the delivery tasks; found {} `timeout` call(s)",
            waits.len()
        );
    }

    /// The embedded CI runners are drained before the lease goes, too
    /// (card_34368880dc20).
    ///
    /// The same reasoning as the transport above, one layer in: a runner that
    /// observed the stop is in the middle of removing its job's container and
    /// writing the row back to `pending`, and that write goes through the very
    /// database whose lease is released here. Dropping the lease first — or not
    /// waiting at all — turns the fix into the bug it replaced, with the row
    /// left `running` for the ten-minute sweep.
    ///
    /// Read out of the source for the same reason: the subject is the order of
    /// three statements in a function that needs a full server to run.
    #[test]
    fn the_embedded_ci_runners_are_drained_before_the_lease_is_released() {
        let source = include_str!("serve.rs");
        let code = rust_source::production_rust_code_only(source);

        let drained = code
            .find("ci_tracker.wait()")
            .expect("`run_serve` must wait for the embedded CI runners to unwind");
        let released = code
            .find("drop(server_db)")
            .expect("`run_serve` must release the database lease");
        assert!(
            drained < released,
            "the CI drain is at byte {drained} and the lease is dropped at {released}: a runner \
             handing its job back writes through that lease"
        );

        // And the signal has to reach them, or the drain waits for a whole
        // build instead of an unwind.
        let wired = rust_source::production_function_call_sites(
            source,
            "configured_ci_engine",
            &["with_shutdown"],
        );
        assert!(
            !wired.is_empty(),
            "the CI engine is built without the process shutdown signal, so the runners it \
             spawns never learn the process is going down"
        );

        // Same shape, same reason: `ci.runner_labels` decides both whether a
        // `tags:` is refused at trigger and whether the embedded runner will
        // execute a job that carries labels. An engine built without it falls
        // back to the default list, which is a different instance's answer.
        let labelled = rust_source::production_function_call_sites(
            source,
            "configured_ci_engine",
            &["with_runner_labels"],
        );
        assert!(
            !labelled.is_empty(),
            "the CI engine is built without ci.runner_labels, so the embedded runner answers to \
             labels the operator never declared"
        );
    }

    #[test]
    fn listen_addresses_are_published_as_one_complete_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/listen-addresses");
        let http = "127.0.0.1:41001".parse().unwrap();
        let ssh = "127.0.0.1:41002".parse().unwrap();

        super::publish_listen_addresses(&path, http, ssh).unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "http=127.0.0.1:41001\nssh=127.0.0.1:41002\n"
        );
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1,
            "the atomic publisher must not leave its temporary file behind"
        );
    }

    /// `[auth].encryption_key` must actually parse — the struct carries
    /// `deny_unknown_fields`, so a key documented in `plombir-git.example.toml`
    /// but missing from the model turns every config file that uses it into a
    /// hard startup failure.
    #[test]
    fn the_encryption_key_is_a_real_config_key() {
        let config: ConfigFile =
            toml::from_str("[auth]\njwt_secret = \"signing\"\nencryption_key = \"at-rest\"\nkey_file = \"/srv/plombir-git/encryption_key\"\n")
                .expect("[auth].encryption_key must be part of the config model");
        assert_eq!(config.auth.jwt_secret.as_deref(), Some("signing"));
        assert_eq!(config.auth.encryption_key.as_deref(), Some("at-rest"));
        assert_eq!(
            config.auth.key_file.as_deref(),
            Some("/srv/plombir-git/encryption_key")
        );
    }

    #[test]
    fn plaintext_webhook_transport_is_a_real_opt_in_config_key() {
        let bare: ConfigFile = toml::from_str("").expect("an empty config parses");
        let secure = resolve_webhook_transport_policy(Some(&bare));
        assert!(
            !secure.allows_insecure_http(),
            "an omitted [webhooks] section must retain the secure default"
        );

        let configured: ConfigFile = toml::from_str("[webhooks]\nallow_insecure_http = true\n")
            .expect("[webhooks].allow_insecure_http must be part of the config model");
        assert!(
            resolve_webhook_transport_policy(Some(&configured)).allows_insecure_http(),
            "only the explicit true value enables plaintext delivery"
        );
    }

    /// `[mirror]` is documented in `plombir-git.example.toml`, and the config
    /// model carries `deny_unknown_fields` — a section that exists in the
    /// documentation but not in the struct turns every config file that uses it
    /// into a hard startup failure.
    #[test]
    fn the_mirror_schedule_is_a_real_config_section() {
        let config: ConfigFile =
            toml::from_str(
                "[mirror]\nenabled = false\npoll_interval_secs = 120\nbatch_size = 4\nallow_insecure_http = true\n",
            )
                .expect("[mirror] must be part of the config model");
        assert_eq!(config.mirror.enabled, Some(false));
        assert_eq!(config.mirror.poll_interval_secs, Some(120));
        assert_eq!(config.mirror.batch_size, Some(4));
        assert!(
            resolve_mirror_transport_policy(Some(&config)).allows_insecure_http(),
            "only an explicit true enables plaintext mirror HTTP"
        );

        // Absent means "use the defaults", not "off": a mirror created through
        // the UI has to be refreshed on an instance whose config predates this
        // section.
        let bare: ConfigFile = toml::from_str("").expect("an empty config still parses");
        assert_eq!(bare.mirror.enabled, None);
        assert!(
            !resolve_mirror_transport_policy(Some(&bare)).allows_insecure_http(),
            "an omitted [mirror] section must retain the secure transport default"
        );
    }

    /// `[auth].registration` has to reach the model for the same
    /// `deny_unknown_fields` reason, and has to resolve in the documented
    /// order: env > config file > `"open"`.
    #[test]
    fn verify_email_registration_needs_mail_and_a_public_url() {
        use rg_core::user::registration::RegistrationMode;

        assert!(
            super::require_mail_for_registration(RegistrationMode::VerifyEmail, true, true).is_ok()
        );
        let no_mail =
            super::require_mail_for_registration(RegistrationMode::VerifyEmail, false, true)
                .unwrap_err();
        assert!(format!("{no_mail:#}").contains("[smtp]"), "{no_mail:#}");
        let no_url =
            super::require_mail_for_registration(RegistrationMode::VerifyEmail, true, false)
                .unwrap_err();
        assert!(format!("{no_url:#}").contains("external_url"), "{no_url:#}");
        for mode in [RegistrationMode::Open, RegistrationMode::Closed] {
            assert!(super::require_mail_for_registration(mode, false, false).is_ok());
        }
    }

    #[test]
    fn the_registration_mode_resolves_env_then_config_then_open() {
        use rg_core::user::registration::RegistrationMode;

        let closed: ConfigFile = toml::from_str("[auth]\nregistration = \"closed\"\n")
            .expect("[auth].registration must be part of the config model");
        assert_eq!(closed.auth.registration.as_deref(), Some("closed"));

        // No config file, no env → the historical behaviour survives an upgrade.
        assert_eq!(
            super::resolve_registration_mode(None, None).unwrap(),
            RegistrationMode::Open
        );
        // Config file alone.
        assert_eq!(
            super::resolve_registration_mode(Some(&closed), None).unwrap(),
            RegistrationMode::Closed
        );
        // Env wins over the file, in both directions.
        assert_eq!(
            super::resolve_registration_mode(Some(&closed), Some("open")).unwrap(),
            RegistrationMode::Open
        );
        let open: ConfigFile = toml::from_str("[auth]\nregistration = \"open\"\n").unwrap();
        assert_eq!(
            super::resolve_registration_mode(Some(&open), Some("CLOSED")).unwrap(),
            RegistrationMode::Closed
        );
        // A blank env value is "not set", not "the empty mode" — same `.env`
        // convention the secrets follow.
        assert_eq!(
            super::resolve_registration_mode(Some(&closed), Some("  ")).unwrap(),
            RegistrationMode::Closed
        );
    }

    /// The dangerous direction, and the reason this is a hard error: a typo must
    /// never boot an instance that the operator believes is closed.
    #[test]
    fn an_unrecognised_registration_mode_fails_the_start() {
        let typo: ConfigFile = toml::from_str("[auth]\nregistration = \"close\"\n").unwrap();

        let err = super::resolve_registration_mode(Some(&typo), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("[auth].registration"), "no source: {err}");
        assert!(err.contains("\"closed\""), "no vocabulary: {err}");

        let err = super::resolve_registration_mode(None, Some("disabled"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("PLOMBIR_GIT_REGISTRATION"), "no source: {err}");
    }

    /// Omitting it is the supported (and most common) state: existing
    /// deployments encrypted everything under `jwt_secret` and must keep
    /// working untouched.
    #[test]
    fn omitting_the_encryption_key_is_allowed() {
        let config: ConfigFile = toml::from_str("[auth]\njwt_secret = \"signing\"\n").unwrap();
        assert!(config.auth.encryption_key.is_none());
    }

    #[test]
    fn default_encryption_key_file_is_a_sibling_of_the_host_key() {
        let config: ConfigFile =
            toml::from_str("[auth]\nkey_file = \"/secrets/custom-key\"\n").unwrap();
        assert_eq!(
            crate::config::resolve_encryption_key_file(Some(&config), Some("/data/ssh_host_key")),
            PathBuf::from("/secrets/custom-key")
        );

        let config: ConfigFile =
            toml::from_str("[server]\nhost_key = \"/data/ssh_host_key\"\n").unwrap();
        assert_eq!(
            crate::config::resolve_encryption_key_file(
                Some(&config),
                config.server.host_key.as_deref()
            ),
            PathBuf::from("/data/encryption_key")
        );
    }

    async fn fresh_db(path: &std::path::Path) -> rg_db::DatabaseConnection {
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", path.join("test.db").display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            rg_db::DEFAULT_IDLE_TIMEOUT_SECS,
            rg_db::DEFAULT_MAX_CONNECTIONS,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    fn unresolved_secrets(jwt_secret: &str, key_file: PathBuf) -> super::AuthSecrets {
        super::AuthSecrets {
            jwt_secret: jwt_secret.to_owned(),
            encryption_key: jwt_secret.to_owned(),
            missing_key_file: Some(key_file),
        }
    }

    #[tokio::test]
    async fn first_start_generates_a_stable_owner_only_key_file_and_marker() {
        let dir = tempfile::tempdir().unwrap();
        let db = fresh_db(dir.path()).await;
        let key_file = dir.path().join("nested").join("encryption_key");
        let jwt_secret = "the-jwt-secret-used-only-for-signing";
        let mut first = unresolved_secrets(jwt_secret, key_file.clone());

        super::establish_encryption_key(&db, &mut first)
            .await
            .expect("first start establishes the durable key");
        assert_ne!(first.encryption_key, jwt_secret);
        assert_eq!(
            super::read_key_file(&key_file).unwrap().as_deref(),
            Some(first.encryption_key.as_str())
        );
        assert!(rg_core::auth::key_check::has_encryption_key_check(&db)
            .await
            .unwrap());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }

        let mut second = super::AuthSecrets {
            jwt_secret: "a-rotated-jwt-secret".to_owned(),
            encryption_key: super::read_key_file(&key_file).unwrap().unwrap(),
            missing_key_file: None,
        };
        super::establish_encryption_key(&db, &mut second)
            .await
            .expect("second start reuses the generated key");
        assert_eq!(second.encryption_key, first.encryption_key);
    }

    #[tokio::test]
    async fn legacy_jwt_encryption_is_materialized_before_the_jwt_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let db = fresh_db(dir.path()).await;
        let legacy_jwt = "the-jwt-secret-that-encrypted-this-database";
        let user =
            rg_db::ops::user_ops::create_user(&db, "alice", "alice@example.invalid", "", "Alice")
                .await
                .unwrap();
        let cipher = rg_core::auth::encryption::encrypt(
            "JBSWY3DPEHPK3PXP",
            &rg_core::auth::encryption::derive_key(legacy_jwt),
        )
        .unwrap();
        // Enrolment is two writes since card_08400088bb40: setup stages the
        // sealed secret, enable promotes it into `users.totp_secret` — the
        // column the preflight below samples.
        rg_db::ops::user_ops::stage_pending_totp_secret(&db, user.id, &cipher)
            .await
            .unwrap();
        rg_db::ops::user_ops::enable_mfa_with_backup_codes(&db, user.id, &[])
            .await
            .unwrap();

        let key_file = dir.path().join("encryption_key");
        let mut first = unresolved_secrets(legacy_jwt, key_file.clone());
        super::establish_encryption_key(&db, &mut first)
            .await
            .expect("legacy key is proven before materializing it");
        assert_eq!(first.encryption_key, legacy_jwt);

        let mut after_jwt_rotation = super::AuthSecrets {
            jwt_secret: "a-new-jwt-signing-secret".to_owned(),
            encryption_key: super::read_key_file(&key_file).unwrap().unwrap(),
            missing_key_file: None,
        };
        super::establish_encryption_key(&db, &mut after_jwt_rotation)
            .await
            .expect("the old at-rest data remains readable after JWT rotation");
        assert_eq!(after_jwt_rotation.encryption_key, legacy_jwt);
    }

    #[tokio::test]
    async fn a_substituted_key_file_refuses_startup_before_any_feature_uses_it() {
        let dir = tempfile::tempdir().unwrap();
        let db = fresh_db(dir.path()).await;
        let key_file = dir.path().join("encryption_key");
        let mut first = unresolved_secrets("the-original-jwt-secret", key_file.clone());
        super::establish_encryption_key(&db, &mut first)
            .await
            .unwrap();

        std::fs::write(&key_file, "a-substituted-key-file-value\n").unwrap();
        let mut substituted = super::AuthSecrets {
            jwt_secret: "a-new-jwt-secret".to_owned(),
            encryption_key: super::read_key_file(&key_file).unwrap().unwrap(),
            missing_key_file: None,
        };
        let error = super::establish_encryption_key(&db, &mut substituted)
            .await
            .expect_err("a marker must reject a substituted key file");
        let message = format!("{error:#}");
        assert!(
            message.contains("key file") || message.contains("key_file"),
            "{message}"
        );
        assert!(message.contains("Nothing has been changed"), "{message}");
    }

    /// card_60b16673b390: an at-rest key equal to the signing secret is refused
    /// on a database that holds nothing encrypted yet — the quick-start mistake —
    /// and only warned about on one whose data was encrypted under it.
    #[tokio::test]
    async fn an_encryption_key_equal_to_the_jwt_secret_is_refused_on_a_fresh_database() {
        let dir = tempfile::tempdir().unwrap();
        let db = fresh_db(dir.path()).await;
        let shared = "one-value-pasted-into-both-variables";
        let mut equal = super::AuthSecrets {
            jwt_secret: shared.to_owned(),
            encryption_key: shared.to_owned(),
            missing_key_file: None,
        };
        let error = super::establish_encryption_key(&db, &mut equal)
            .await
            .expect_err("equal secrets on a fresh database must refuse the start");
        let message = format!("{error:#}");
        assert!(message.contains("same value"), "{message}");
        assert!(
            !rg_core::auth::key_check::has_encryption_key_check(&db)
                .await
                .unwrap(),
            "a refused start must not stamp the marker"
        );

        let mut distinct = super::AuthSecrets {
            jwt_secret: shared.to_owned(),
            encryption_key: "a-key-of-its-own".to_owned(),
            missing_key_file: None,
        };
        super::establish_encryption_key(&db, &mut distinct)
            .await
            .expect("distinct secrets start");
    }

    #[tokio::test]
    async fn equal_secrets_over_data_encrypted_with_them_still_start() {
        let dir = tempfile::tempdir().unwrap();
        let db = fresh_db(dir.path()).await;
        let shared = "the-value-an-older-guide-put-in-both";
        let mut first = super::AuthSecrets {
            jwt_secret: "a-different-signing-secret".to_owned(),
            encryption_key: shared.to_owned(),
            missing_key_file: None,
        };
        super::establish_encryption_key(&db, &mut first)
            .await
            .expect("stamp the marker under the shared value");

        let mut equal = super::AuthSecrets {
            jwt_secret: shared.to_owned(),
            encryption_key: shared.to_owned(),
            missing_key_file: None,
        };
        super::establish_encryption_key(&db, &mut equal)
            .await
            .expect("refusing would lock away data encrypted under this key");
    }

    /// `FOO=` in a `.env` is "not set", not "the empty secret" — see
    /// [`super::env_secret`]. `deploy/.env.example` ships exactly that line.
    /// Exercise the pure value boundary so the test does not mutate the
    /// process-wide environment observed by its parallel neighbours.
    #[test]
    fn blank_environment_values_count_as_unset() {
        assert_eq!(super::non_blank_env_value(Some(String::new())), None);
        assert_eq!(super::non_blank_env_value(Some("   \n".to_owned())), None);
        assert_eq!(
            super::non_blank_env_value(Some("an-actual-secret".to_owned())).as_deref(),
            Some("an-actual-secret")
        );
        assert_eq!(super::non_blank_env_value(None), None);
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

    /// Half a `[tls]` used to boot a plain-HTTP listener behind one WARN line:
    /// a typo in the key path gave an instance taking passwords in the clear.
    #[test]
    fn a_half_configured_tls_section_refuses_the_start() {
        let cert_only: ConfigFile =
            toml::from_str("[tls]\ncert = \"/etc/tls/fullchain.pem\"\n").unwrap();
        let err = super::resolve_tls_config(Some(&cert_only), None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("[tls].key"), "unexpected: {err}");
        assert!(err.contains("--tls-key"), "unexpected: {err}");
        assert!(err.contains("plain HTTP"), "unexpected: {err}");

        let key_only: ConfigFile =
            toml::from_str("[tls]\nkey = \"/etc/tls/privkey.pem\"\n").unwrap();
        let err = super::resolve_tls_config(Some(&key_only), None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("[tls].cert"), "unexpected: {err}");
        assert!(err.contains("--tls-cert"), "unexpected: {err}");

        // The flag pair alone is just as half-configured as the file.
        let err = super::resolve_tls_config(None, Some("/c.pem".into()), None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("[tls].key"), "unexpected: {err}");
    }

    #[test]
    fn an_absent_or_complete_tls_section_starts() {
        let empty: ConfigFile = toml::from_str("[tls]\n").unwrap();
        assert_eq!(
            super::resolve_tls_config(Some(&empty), None, None).unwrap(),
            None
        );
        assert_eq!(super::resolve_tls_config(None, None, None).unwrap(), None);

        let both: ConfigFile =
            toml::from_str("[tls]\ncert = \"/c.pem\"\nkey = \"/k.pem\"\n").unwrap();
        assert_eq!(
            super::resolve_tls_config(Some(&both), None, None).unwrap(),
            Some((PathBuf::from("/c.pem"), PathBuf::from("/k.pem")))
        );

        // Each half resolves on its own: a flag completes the file's half and
        // overrides the file's value for its own half.
        let cert_only: ConfigFile = toml::from_str("[tls]\ncert = \"/c.pem\"\n").unwrap();
        assert_eq!(
            super::resolve_tls_config(Some(&cert_only), None, Some("/flag-k.pem".into())).unwrap(),
            Some((PathBuf::from("/c.pem"), PathBuf::from("/flag-k.pem")))
        );
        assert_eq!(
            super::resolve_tls_config(Some(&both), Some("/flag-c.pem".into()), None).unwrap(),
            Some((PathBuf::from("/flag-c.pem"), PathBuf::from("/k.pem")))
        );
    }

    fn smtp_from_file(toml_text: &str) -> anyhow::Result<Option<rg_core::email::SmtpConfig>> {
        let cfg: ConfigFile = toml::from_str(toml_text).unwrap();
        super::resolve_smtp_config(Some(&cfg), None, 587, None, None, None)
    }

    /// A forgotten `pass` used to switch mail off without a single log line:
    /// notifications and password reset silently stopped.
    #[test]
    fn a_half_configured_smtp_section_names_what_is_missing() {
        let err = smtp_from_file(
            "[smtp]\nhost = \"smtp.example.com\"\nuser = \"bot\"\nfrom = \"bot@example.com\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`[smtp].pass`"), "unexpected: {err}");
        assert!(err.contains("`--smtp-pass`"), "unexpected: {err}");
        for present in ["[smtp].host", "[smtp].user", "[smtp].from"] {
            assert!(!err.contains(present), "{present} is set: {err}");
        }

        // Every missing field is named, not just the first.
        let err = smtp_from_file("[smtp]\nhost = \"smtp.example.com\"\n")
            .unwrap_err()
            .to_string();
        for missing in ["[smtp].user", "[smtp].pass", "[smtp].from"] {
            assert!(err.contains(missing), "{missing} not named: {err}");
        }

        // A blank value is a placeholder, not a credential.
        let err = smtp_from_file(
            "[smtp]\nhost = \"smtp.example.com\"\nuser = \"bot\"\npass = \"  \"\n\
             from = \"bot@example.com\"\n",
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`[smtp].pass`"), "unexpected: {err}");

        // A flag counts like the file: `--smtp-host` alone is half a setup.
        let err = super::resolve_smtp_config(
            None,
            Some("smtp.example.com".into()),
            587,
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("`--smtp-pass`"), "unexpected: {err}");
    }

    #[test]
    fn an_empty_or_complete_smtp_section_starts() {
        assert!(smtp_from_file("[smtp]\n").unwrap().is_none());
        assert!(smtp_from_file("[smtp]\nport = 2525\n").unwrap().is_none());
        assert!(
            super::resolve_smtp_config(None, None, 587, None, None, None)
                .unwrap()
                .is_none()
        );

        let full = "[smtp]\nhost = \"smtp.example.com\"\nuser = \"bot\"\npass = \"s3cret\"\n\
                    from = \"Plombir Git <bot@example.com>\"\n";
        let smtp = smtp_from_file(full)
            .unwrap()
            .expect("complete [smtp] enables mail");
        assert_eq!(smtp.host, "smtp.example.com");
        assert_eq!(smtp.port, 587);
        assert_eq!(smtp.pass, "s3cret");

        // The flag overrides the file field by field.
        let cfg: ConfigFile = toml::from_str(full).unwrap();
        let smtp =
            super::resolve_smtp_config(Some(&cfg), None, 587, None, Some("flag".into()), None)
                .unwrap()
                .unwrap();
        assert_eq!(smtp.pass, "flag");
        assert_eq!(smtp.user, "bot");
    }

    #[test]
    fn a_complete_smtp_section_with_an_unusable_port_or_sender_refuses_the_start() {
        let cfg: ConfigFile = toml::from_str(
            "[smtp]\nhost = \"smtp.example.com\"\nuser = \"bot\"\npass = \"s3cret\"\n\
             from = \"bot@example.com\"\n",
        )
        .unwrap();
        let err = super::resolve_smtp_config(Some(&cfg), None, 0, None, None, None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("smtp.port"), "unexpected: {err}");

        // Parsed only at send time before, so a typo failed every mail.
        let err = smtp_from_file(
            "[smtp]\nhost = \"smtp.example.com\"\nuser = \"bot\"\npass = \"s3cret\"\n\
             from = \"bot at example.com\"\n",
        )
        .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("[smtp].from"), "unexpected: {err}");
        assert!(err.contains("bot at example.com"), "unexpected: {err}");
    }

    /// The certificate is published to every client that connects; the key is
    /// the whole of the server's TLS identity. A `0644` key on a shared host
    /// lets any other local account terminate this instance's TLS, so it is
    /// refused with the same rule and the same remediation as the config file
    /// and the at-rest encryption key.
    #[cfg(unix)]
    #[test]
    fn a_group_or_world_readable_tls_key_is_refused_before_boot() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("fullchain.pem");
        let key = dir.path().join("privkey.pem");
        std::fs::write(&cert, "cert").unwrap();
        std::fs::write(&key, "key").unwrap();
        std::fs::set_permissions(&cert, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();

        let secret = "a-sufficiently-long-test-jwt-secret-value";
        let err = super::validate_config(secret, dir.path(), &Some((cert.clone(), key.clone())))
            .unwrap_err()
            .to_string();
        assert!(err.contains("TLS private key"), "unexpected: {err}");
        assert!(
            err.contains(&key.display().to_string()),
            "unexpected: {err}"
        );
        assert!(err.contains("mode 0644"), "unexpected: {err}");
        assert!(err.contains("chmod 600"), "unexpected: {err}");

        // The public half stays public: only the key is narrowed.
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        super::validate_config(secret, dir.path(), &Some((cert, key)))
            .expect("an owner-only key with a world-readable certificate must boot");
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
            PathBuf::from(crate::config::DEFAULT_AUDIT_ARCHIVE_DIR)
        );
        // A repo_root directly under the filesystem root keeps the historical
        // default rather than demanding write access to `/`.
        assert_eq!(
            super::default_audit_archive_dir(Path::new("/repos")),
            PathBuf::from(crate::config::DEFAULT_AUDIT_ARCHIVE_DIR)
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
        assert_eq!(
            config.timeouts.job_secs,
            rg_core::ci::DEFAULT_JOB_TIMEOUT_SECS
        );
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

    #[test]
    fn configured_job_timeout_reaches_the_ci_engine() {
        let config: ConfigFile =
            toml::from_str("[timeouts]\njob_secs = 731\n").expect("custom timeout config");

        let (_shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let engine = super::configured_ci_engine(
            rg_ci::CiNotifications::default(),
            config.timeouts.job_secs,
            rg_core::ci::default_runner_labels(),
            shutdown_rx,
        );

        assert_eq!(engine.job_timeout_secs(), 731);
    }

    /// `serve` shares one resolution path with the one-shot subcommands, so the
    /// server and a later `plombir-git migrate --config <same file>` cannot end up
    /// pointed at two different databases.
    #[test]
    fn serve_and_the_one_shot_subcommands_resolve_the_same_database() {
        let config: ConfigFile =
            toml::from_str("[database]\nurl = \"postgres://forge@db/plombir_git\"\n[server]\nrepo_root = \"/data/repos\"\n")
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

    /// The production reason for a process policy rather than another
    /// `OpenOptionsExt`: gix owns the opens that initialise a repository. Run
    /// the mutation in a child test process so changing its process-wide umask
    /// cannot race unrelated tests in this binary.
    #[cfg(unix)]
    #[test]
    fn owner_only_default_protects_a_real_gix_repository_in_an_operator_directory() {
        use std::os::unix::fs::PermissionsExt;

        const CHILD_ROOT: &str = "PLOMBIR_GIT_STATE_UMASK_TEST_CHILD_ROOT";
        if let Some(root) = std::env::var_os(CHILD_ROOT) {
            // SAFETY: this is an isolated child process which exits at the end
            // of this branch; no other test runs in it (`--exact`).
            unsafe { libc::umask(0o002) };

            let permissions =
                crate::config::resolve_settings(CliSettings::default(), None).state_permissions;
            assert_eq!(
                permissions,
                rg_process::StateCreationPermissions::OwnerOnly,
                "the server default must be owner-only"
            );
            permissions.install();

            gix::create::into(
                std::path::PathBuf::from(root).join("private.git"),
                gix::create::Kind::Bare,
                gix::create::Options::default(),
            )
            .expect("gix must create the representative private repository");
            return;
        }

        let operator_root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(operator_root.path(), std::fs::Permissions::from_mode(0o755))
            .unwrap();

        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "serve::serve_tests::owner_only_default_protects_a_real_gix_repository_in_an_operator_directory",
                "--nocapture",
            ])
            .env(CHILD_ROOT, operator_root.path())
            .status()
            .expect("the isolated umask test process must start");
        assert!(status.success(), "isolated umask test failed: {status}");

        let root_mode = std::fs::metadata(operator_root.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            root_mode, 0o755,
            "a directory supplied by the operator must not be narrowed"
        );

        let head_mode = std::fs::metadata(operator_root.path().join("private.git/HEAD"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            head_mode, 0o600,
            "gix-created repository files must inherit the server's owner-only policy, not the child's 0002 umask"
        );
    }
}
