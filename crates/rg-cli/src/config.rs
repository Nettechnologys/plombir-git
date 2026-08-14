//! TOML configuration model shared by **every** `forgekeep` subcommand, plus
//! the `CLI arg > config file > built-in default` resolution.
//!
//! This lives outside `serve.rs` on purpose: the config file is not a
//! `serve`-only concern. `migrate`, `rebuild-fts`, `backup-db`, `restore-db`,
//! `create-repo`, `import`, `index-repo` and `package list` all address the same
//! `[database].url` / `[server].repo_root`, and while `load_config_file` was
//! private to the `serve` module they *could not* read it — every one of them
//! silently fell back to `sqlite://./forgekeep.db?mode=rwc`, so an operator
//! running `forgekeep migrate` on a Postgres deployment migrated a brand-new
//! empty SQLite file and `forgekeep backup-db` happily "backed up" nothing.

use std::path::PathBuf;

use anyhow::Context;

/// TOML configuration file structure.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct ConfigFile {
    #[serde(default)]
    pub(crate) server: ServerConfig,
    #[serde(default)]
    pub(crate) database: DatabaseConfig,
    #[serde(default)]
    pub(crate) auth: AuthConfig,
    #[serde(default)]
    pub(crate) ci: CiConfig,
    #[serde(default)]
    pub(crate) releases: ReleasesConfig,
    #[serde(default)]
    pub(crate) rate_limit: RateLimitConfig,
    #[serde(default)]
    pub(crate) smtp: SmtpConfig,
    #[serde(default)]
    pub(crate) tls: TlsConfig,
    #[serde(default)]
    pub(crate) logging: LoggingConfig,
    #[serde(default)]
    pub(crate) audit: AuditConfig,
    #[serde(default)]
    pub(crate) backup: BackupConfig,
    #[serde(default)]
    pub(crate) mirror: MirrorConfig,
    #[serde(default)]
    pub(crate) imports: ImportConfig,
    #[serde(default)]
    pub(crate) timeouts: TimeoutConfig,
    #[serde(default)]
    pub(crate) webhooks: WebhooksConfig,
    #[serde(default)]
    pub(crate) observability: ObservabilityConfig,
    /// Server external URL (e.g., "https://git.example.com"). Used for SSO callbacks.
    #[serde(default)]
    pub(crate) external_url: Option<String>,
}

// No `#[allow(dead_code)]` here on purpose: every field below must actually be
// consumed by `resolve_settings` / `run_serve`. If a key is added to the struct
// (and to `forgekeep.example.toml`) but never wired up, the dead-code lint says
// so at build time instead of the operator finding out that their setting is
// silently ignored.
#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerConfig {
    pub(crate) repo_root: Option<String>,
    pub(crate) http_addr: Option<String>,
    pub(crate) ssh_addr: Option<String>,
    pub(crate) host_key: Option<String>,
    /// Maximum decoded package artifact size in MiB. Protocol envelopes such
    /// as npm's base64 JSON receive bounded headroom above this value, but the
    /// artifact stored in the registry may never exceed it.
    pub(crate) package_upload_max_mb: Option<u64>,
    /// External-facing URL for SSO callbacks and links (e.g., "https://git.example.com")
    pub(crate) external_url: Option<String>,
    /// Grace window (seconds) for draining in-flight requests and the CI-log
    /// queue on SIGTERM/ctrl_c before the process is forced down (default: 30).
    pub(crate) shutdown_grace_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct DatabaseConfig {
    pub(crate) url: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct AuthConfig {
    /// Secret that signs session JWTs, PAT-derived tokens and CI job tokens.
    pub(crate) jwt_secret: Option<String>,
    /// Secret that encrypts data at rest — TOTP secrets, CI secrets, mirror and
    /// LDAP passwords, SSO client secrets, OAuth tokens.
    ///
    /// Unset falls through to the durable [`key_file`](Self::key_file), which
    /// the server creates on first start. This keeps at-rest data independent
    /// from a JWT secret that operators are expected to rotate.
    pub(crate) encryption_key: Option<String>,
    /// File that carries the generated at-rest key when no explicit secret is
    /// supplied. Kept next to the SSH host key by default so the whole instance
    /// state remains in one backupable directory.
    pub(crate) key_file: Option<String>,
    /// Whether `POST /users/register` accepts new accounts: `"open"` (the
    /// default, and the historical behaviour) or `"closed"`.
    ///
    /// A string rather than a bool so a third mode (`"invite"`) can be added
    /// without breaking every config file that already spells this out —
    /// self-service sign-up is a product decision with more than two states.
    /// Also settable as `FORGEKEEP_REGISTRATION`, which wins; an unrecognised
    /// value fails the start rather than falling back to `"open"`.
    pub(crate) registration: Option<String>,
}

/// The SSH host-key location used by `serve` without a configured
/// `[server].host_key`. Keeping it here lets the one-shot commands resolve the
/// same default key file as the server.
pub(crate) fn default_host_key_path() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ssh")
        .join("id_ed25519")
}

/// Resolve the durable at-rest key file. The file location is a deployment
/// setting, not a secret source: env/CLI/config values for the key itself still
/// win over its contents.
pub(crate) fn resolve_encryption_key_file(
    cfg: Option<&ConfigFile>,
    resolved_host_key: Option<&str>,
) -> PathBuf {
    if let Some(path) = cfg.and_then(|config| config.auth.key_file.as_ref()) {
        return PathBuf::from(path);
    }

    let host_key = resolved_host_key
        .map(PathBuf::from)
        .unwrap_or_else(default_host_key_path);
    if let Some(parent) = host_key
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        return parent.join("encryption_key");
    }

    PathBuf::from("./data/encryption_key")
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct CiConfig {
    #[serde(default)]
    pub(crate) docker: Option<bool>,
    #[serde(default)]
    pub(crate) external_runners: Option<bool>,
    /// Allow imageless CI jobs to run as a shell on the host (default false).
    #[serde(default)]
    pub(crate) allow_host_runner: Option<bool>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct ReleasesConfig {
    /// Enable opt-in Ed25519 provenance attestation of release assets (default
    /// false). Also settable via `FORGEKEEP_ATTESTATION_ENABLED=1`, which wins.
    #[serde(default)]
    pub(crate) attestation_enabled: Option<bool>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct RateLimitConfig {
    pub(crate) max: Option<u32>,
    pub(crate) window_secs: Option<u64>,
    #[serde(default)]
    pub(crate) trusted_proxies: Vec<String>,
    /// Hard cap on distinct client keys the limiter tracks (memory guard).
    pub(crate) max_keys: Option<usize>,
    /// Stricter per-IP cap for credential endpoints (register/login).
    pub(crate) auth_max: Option<u32>,
    /// Window (seconds) for the credential-endpoint limiter.
    pub(crate) auth_window_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct SmtpConfig {
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
    pub(crate) pass: Option<String>,
    pub(crate) from: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct TlsConfig {
    pub(crate) cert: Option<String>,
    pub(crate) key: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct LoggingConfig {
    pub(crate) file: Option<String>,
    pub(crate) max_size_mb: Option<u64>,
    pub(crate) max_files: Option<usize>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct AuditConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) archive_dir: Option<String>,
    pub(crate) archive_after_days: Option<i64>,
    pub(crate) interval_minutes: Option<u64>,
    pub(crate) batch_size: Option<u64>,
}

/// `[backup]` — the in-process database backup schedule.
///
/// Opt-in (`enabled` defaults to **false**) rather than on-by-default: a
/// snapshot every `interval_hours` with `keep_last` copies retained multiplies
/// the database's disk footprint, and that is not a cost to impose on an
/// existing install during an upgrade. The shipped deployment configs
/// (`forgekeep.example.toml`, `deploy/forgekeep.docker.toml`) turn it on, so a
/// new instance is backed up from the first start; and a server with it off says
/// so at startup, which is the point — "are there backups?" should be answerable
/// from the config file and the log, not from an admin's memory of a cron entry.
#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct BackupConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) dir: Option<String>,
    pub(crate) interval_hours: Option<u64>,
    pub(crate) keep_last: Option<usize>,
}

/// `[mirror]` — the in-process schedule that refreshes repository mirrors.
///
/// On by default (`enabled` defaults to **true**), unlike `[backup]`: a mirror
/// is only ever created by an operator who asked for one, and the create form
/// takes a sync interval and reports a next-sync time. A server that silently
/// never acts on either is the defect this section was added to close
/// (card_d2fd29942436), so the honest off switch is an explicit
/// `enabled = false` — which also stops the server making outbound `git` calls
/// to operator-supplied remotes on a timer, for the deployments that want that
/// decided in the config file rather than per repository.
///
/// `poll_interval_secs` is *polling* granularity, not a mirror's schedule: each
/// mirror carries its own `sync_interval_seconds` and a pass only touches rows
/// whose `next_sync_at` has passed.
#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct MirrorConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) poll_interval_secs: Option<u64>,
    pub(crate) batch_size: Option<u64>,
}

/// `[imports]` — operator-owned trust exceptions for private self-hosted
/// GitHub Enterprise and GitLab origins.
#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportConfig {
    /// Exact HTTP(S) origins (`scheme://host[:port]`) allowed to bypass the
    /// private-address SSRF rejection. Paths and wildcards are rejected.
    #[serde(default)]
    pub(crate) trusted_origins: Vec<String>,
}

/// `[observability]` — OpenTelemetry distributed-tracing (OTLP) export. All
/// fields optional; with no endpoint set (here or via the `OTEL_EXPORTER_OTLP_*`
/// env vars) OTel tracing stays off and only Prometheus `/metrics` + logs run.
#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct ObservabilityConfig {
    /// OTLP/HTTP endpoint, e.g. "http://localhost:4318" (the `/v1/traces` path is
    /// appended automatically). Overridden by `OTEL_EXPORTER_OTLP_ENDPOINT` /
    /// `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT`. Unset ⇒ tracing disabled.
    pub(crate) otlp_endpoint: Option<String>,
    /// `service.name` resource attribute (default "forgekeep"). Overridden by
    /// `OTEL_SERVICE_NAME`.
    pub(crate) service_name: Option<String>,
    /// Head sampling ratio in 0.0..=1.0 (default 1.0 = sample every trace).
    pub(crate) sample_ratio: Option<f64>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
pub(crate) struct WebhooksConfig {
    /// Shared secret for verifying HMAC-SHA256 signatures on *inbound* external
    /// webhooks (`/webhooks/external/*`). Unset = signature checking disabled
    /// (endpoints rely on JWT/PAT auth alone). Also settable via the
    /// `FORGEKEEP_EXTERNAL_WEBHOOK_SECRET` environment variable, which wins.
    pub(crate) external_secret: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TimeoutConfig {
    /// CI job timeout in seconds (default: 3600 = 1 hour).
    #[serde(default = "default_job_timeout")]
    pub(crate) job_secs: u64,
    /// Git CLI command timeout in seconds (default: 120).
    #[serde(default = "default_git_timeout")]
    pub(crate) git_cmd_secs: u64,
    /// Wall-clock timeout in seconds for the streaming git transport —
    /// upload-pack (clone/fetch) and receive-pack (push). Bounds a hung or
    /// pathologically slow `git` subprocess so it can't hold a connection +
    /// process indefinitely. More generous than `git_cmd_secs` because pack
    /// generation over a large repo is legitimately slower than a metadata
    /// command. 0 disables the bound (default: 300).
    #[serde(default = "default_git_stream_timeout")]
    pub(crate) git_stream_secs: u64,
    /// Idle timeout in seconds for the streaming git transport, layered on top
    /// of `git_stream_secs`. The git stream is killed if it makes no read/write
    /// progress for this long — catching a slow-drip push/fetch that dribbles
    /// bytes to stay under the wall-clock budget. Applies to SSH (stream
    /// wrapper) and HTTP (request-body buffering). 0 disables the idle watchdog
    /// (default: 30).
    #[serde(default = "default_git_idle_timeout")]
    pub(crate) git_idle_secs: u64,
    /// Database connect timeout in seconds (default: 10).
    #[serde(default = "default_db_connect_timeout")]
    pub(crate) db_connect_secs: u64,
    /// Database idle timeout in seconds (default: 600).
    #[serde(default = "default_db_idle_timeout")]
    pub(crate) db_idle_secs: u64,
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

pub(crate) fn default_job_timeout() -> u64 {
    3600
}
pub(crate) fn default_git_timeout() -> u64 {
    120
}
pub(crate) fn default_git_stream_timeout() -> u64 {
    300
}
pub(crate) fn default_git_idle_timeout() -> u64 {
    30
}
pub(crate) fn default_db_connect_timeout() -> u64 {
    10
}
pub(crate) fn default_db_idle_timeout() -> u64 {
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
pub(crate) fn ensure_regular_file(
    path: &std::path::Path,
    what: &str,
    hint: &str,
) -> anyhow::Result<()> {
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
        Err(e) => {
            Err(anyhow::Error::new(e)).with_context(|| format!("failed to stat {what} `{shown}`"))
        }
    }
}

/// Remediation appended to every `--config` failure: the file the deployer was
/// supposed to create in the first place.
const CONFIG_FILE_HINT: &str =
    "create it first: `cp forgekeep.example.toml forgekeep.toml` (and bind-mount that file, \
     not a directory)";

/// Return the TOML table active at `byte_offset`, for actionable parse errors.
///
/// `toml::de::Error` names an unknown field and its line, but not the table the
/// field belongs to. For a file with several operator-facing sections that
/// leaves `unknown field htp_addr` unnecessarily ambiguous. ForgeKeep's config
/// model uses ordinary top-level tables, so the closest preceding `[table]`
/// header is the section the operator has to fix.
fn config_section_at(content: &str, byte_offset: usize) -> Option<&str> {
    let prefix = content.get(..byte_offset.min(content.len()))?;
    prefix.lines().rev().find_map(|line| {
        let line = line.trim();
        let table = line.strip_prefix('[')?.split_once(']')?.0.trim();
        (!table.is_empty()
            && table
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')))
        .then_some(table)
    })
}

pub(crate) fn load_config_file(path: &str) -> anyhow::Result<ConfigFile> {
    ensure_regular_file(std::path::Path::new(path), "config file", CONFIG_FILE_HINT)?;
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file `{path}`"))?;
    let config: ConfigFile = toml::from_str(&content).map_err(|error| {
        let section = error
            .span()
            .and_then(|span| config_section_at(&content, span.start));
        let context = section.map_or_else(
            || format!("failed to parse config file `{path}` as TOML"),
            |section| {
                format!("failed to parse config file `{path}` as TOML in section [{section}]")
            },
        );
        anyhow::Error::new(error).context(context)
    })?;
    tracing::info!(path = %path, "Loaded configuration file");
    Ok(config)
}

/// Load the file named by `--config`, or `None` when the flag was not passed.
///
/// A `--config` that *was* passed but cannot be read is always an error, never a
/// fallback to the built-in defaults: that is precisely how `forgekeep migrate
/// --config /data/forgekeep.toml` would end up migrating a fresh, empty
/// `./forgekeep.db` while the real database stayed untouched.
pub(crate) fn load_optional_config_file(path: Option<&str>) -> anyhow::Result<Option<ConfigFile>> {
    match path {
        Some(path) => Ok(Some(load_config_file(path)?)),
        None => Ok(None),
    }
}

/// Built-in defaults for the settings that exist both as a CLI flag and as a
/// config-file key. They live here rather than in clap's `default_value` on
/// purpose: a clap default is indistinguishable from a value the operator
/// typed, so with one the config file could never win over "the flag was not
/// passed" — which is exactly why `[server].repo_root` and `[database].url`
/// were silently ignored, first by `forgekeep serve --config …` and then by
/// every other subcommand.
pub(crate) const DEFAULT_REPO_ROOT: &str = "./repos";
pub(crate) const DEFAULT_HTTP_ADDR: &str = "0.0.0.0:8080";
pub(crate) const DEFAULT_SSH_ADDR: &str = "0.0.0.0:2222";
pub(crate) const DEFAULT_DB_URL: &str = "sqlite://./forgekeep.db?mode=rwc";
pub(crate) const DEFAULT_SMTP_PORT: u16 = 587;
pub(crate) const DEFAULT_RATE_LIMIT_MAX: u32 = 0;
pub(crate) const DEFAULT_RATE_LIMIT_WINDOW: u64 = 60;
pub(crate) const DEFAULT_LOG_MAX_SIZE_MB: u64 = 10;
pub(crate) const DEFAULT_LOG_MAX_FILES: usize = 5;
pub(crate) const DEFAULT_PACKAGE_UPLOAD_MAX_MB: u64 =
    (rg_http::DEFAULT_PACKAGE_UPLOAD_MAX_BYTES / (1024 * 1024)) as u64;

/// Resolve `[server].package_upload_max_mb` to the byte ceiling consumed by
/// rg-http. Zero and values that cannot fit the current platform fail startup
/// instead of silently disabling or wrapping the resource boundary.
pub(crate) fn resolve_package_upload_max_bytes(cfg: Option<&ConfigFile>) -> anyhow::Result<usize> {
    let max_mb = cfg
        .and_then(|config| config.server.package_upload_max_mb)
        .unwrap_or(DEFAULT_PACKAGE_UPLOAD_MAX_MB);
    if max_mb == 0 {
        anyhow::bail!("config `server.package_upload_max_mb` must be greater than zero");
    }
    let bytes = max_mb.checked_mul(1024 * 1024).ok_or_else(|| {
        anyhow::anyhow!(
            "config `server.package_upload_max_mb` is too large to convert to bytes: {max_mb}"
        )
    })?;
    usize::try_from(bytes).map_err(|_| {
        anyhow::anyhow!(
            "config `server.package_upload_max_mb` does not fit this platform: {max_mb} MiB"
        )
    })
}

/// Parse the exact admin-managed origins consumed by both `serve` and the
/// one-shot `forgekeep import --config ...` command.
pub(crate) fn resolve_trusted_import_origins(
    cfg: Option<&ConfigFile>,
) -> anyhow::Result<rg_core::import::trust::TrustedImportOrigins> {
    let values = cfg
        .map(|config| config.imports.trusted_origins.as_slice())
        .unwrap_or_default();
    rg_core::import::trust::TrustedImportOrigins::parse(values)
        .context("invalid config `[imports].trusted_origins`")
}

/// `--db-url` > `[database].url` > [`DEFAULT_DB_URL`].
///
/// The single resolution point for the database URL, used both by `serve` (via
/// [`resolve_settings`]) and by every one-shot subcommand, so `forgekeep serve`
/// and `forgekeep migrate` given the same config file can never disagree about
/// which database they are talking to.
pub(crate) fn resolve_db_url(cli: Option<String>, cfg: Option<&ConfigFile>) -> String {
    cli.or_else(|| cfg.and_then(|c| c.database.url.clone()))
        .unwrap_or_else(|| DEFAULT_DB_URL.to_string())
}

/// `--repo-root` > `[server].repo_root` > [`DEFAULT_REPO_ROOT`].
///
/// Same contract as [`resolve_db_url`]: `import` / `index-repo` / `create-repo`
/// must place (and look for) bare repositories exactly where the server does.
pub(crate) fn resolve_repo_root(cli: Option<String>, cfg: Option<&ConfigFile>) -> String {
    cli.or_else(|| cfg.and_then(|c| c.server.repo_root.clone()))
        .unwrap_or_else(|| DEFAULT_REPO_ROOT.to_string())
}

/// The CLI half of every dual-source knob, resolved against the config file by
/// [`resolve_settings`]. `None` means "flag not passed" — never a default.
#[derive(Debug, Default)]
pub(crate) struct CliSettings {
    pub(crate) repo_root: Option<String>,
    pub(crate) http_addr: Option<String>,
    pub(crate) ssh_addr: Option<String>,
    pub(crate) host_key: Option<String>,
    pub(crate) db_url: Option<String>,
    pub(crate) rate_limit_max: Option<u32>,
    pub(crate) rate_limit_window: Option<u64>,
    pub(crate) smtp_port: Option<u16>,
    pub(crate) log_max_size_mb: Option<u64>,
    pub(crate) log_max_files: Option<usize>,
}

/// The same knobs after `CLI arg > config file > built-in default` has been
/// applied.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ResolvedSettings {
    pub(crate) repo_root: String,
    pub(crate) http_addr: String,
    pub(crate) ssh_addr: String,
    pub(crate) host_key: Option<String>,
    pub(crate) db_url: String,
    pub(crate) rate_limit_max: u32,
    pub(crate) rate_limit_window: u64,
    pub(crate) smtp_port: u16,
    pub(crate) log_max_size_mb: u64,
    pub(crate) log_max_files: usize,
}

/// Apply the documented `CLI arg > config file > built-in default` precedence
/// to every setting that has both a flag and a config key.
///
/// Extracted as a pure function so the wiring (which config key feeds which
/// flag) is unit-testable without booting a server — the original bug was a
/// missing wire, not a bad value.
pub(crate) fn resolve_settings(cli: CliSettings, cfg: Option<&ConfigFile>) -> ResolvedSettings {
    let server = cfg.map(|c| &c.server);
    ResolvedSettings {
        // `repo_root` / `db_url` deliberately route through the same two
        // helpers the one-shot subcommands call.
        repo_root: resolve_repo_root(cli.repo_root, cfg),
        db_url: resolve_db_url(cli.db_url, cfg),
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{CliSettings, ConfigFile};

    fn production_config_source() -> &'static str {
        include_str!("config.rs")
            .split_once("\n#[cfg(test)]\n")
            .map(|(production, _)| production)
            .expect("config.rs must keep its test module behind #[cfg(test)]")
    }

    /// The `pub(crate) name: Type` fields of a struct declared in `source`, in
    /// declaration order. Reading the declaration rather than keeping a list
    /// beside it is the whole point: a knob added to the model joins every
    /// contract below by existing, not by someone remembering to register it.
    fn struct_field_declarations<'a>(source: &'a str, type_name: &str) -> Vec<(&'a str, &'a str)> {
        let declaration = format!("pub(crate) struct {type_name} {{");
        let body = source
            .split_once(declaration.as_str())
            .map(|(_, rest)| rest)
            .and_then(|rest| rest.split_once("\n}").map(|(body, _)| body))
            .unwrap_or_else(|| panic!("{type_name} declaration must be present in config.rs"));

        body.lines()
            .filter_map(|line| line.trim().strip_prefix("pub(crate) "))
            .filter_map(|field| field.split_once(": "))
            .map(|(name, field_type)| (name, field_type.trim_end_matches(',')))
            .collect()
    }

    /// The type a `ConfigFile` field carries, unwrapped from `Option<…>`.
    fn field_config_type(field_type: &str) -> &str {
        field_type
            .strip_prefix("Option<")
            .and_then(|inner| inner.strip_suffix('>'))
            .unwrap_or(field_type)
    }

    /// Derive the nested section/type pairs from the production `ConfigFile`
    /// declaration. This deliberately is not a hand-maintained registry: a new
    /// `FooConfig` field must join the unknown-key contract automatically.
    fn nested_config_sections(source: &str) -> Vec<(&str, &str)> {
        struct_field_declarations(source, "ConfigFile")
            .into_iter()
            .filter_map(|(section, field_type)| {
                let config_type = field_config_type(field_type);
                config_type
                    .ends_with("Config")
                    .then_some((section, config_type))
            })
            .collect()
    }

    /// Every key the model accepts, as `(section, key)` — with `""` for the
    /// handful that live at the root of the document rather than in a
    /// `[section]`.
    fn config_key_inventory(source: &str) -> Vec<(&str, &str)> {
        let mut keys: Vec<(&str, &str)> = struct_field_declarations(source, "ConfigFile")
            .into_iter()
            .filter(|(_, field_type)| !field_config_type(field_type).ends_with("Config"))
            .map(|(key, _)| ("", key))
            .collect();

        for (section, config_type) in nested_config_sections(source) {
            keys.extend(
                struct_field_declarations(source, config_type)
                    .into_iter()
                    .map(|(key, _)| (section, key)),
            );
        }
        keys
    }

    /// The configuration files ForgeKeep actually ships, by the path an
    /// operator is told to copy.
    ///
    /// `include_str!` rather than a runtime `read_to_string`: the paths are
    /// resolved at compile time (so a moved or renamed file breaks the build
    /// instead of silently skipping the check), and editing either file
    /// rebuilds — and therefore re-runs — the tests below.
    const SHIPPED_CONFIGS: [(&str, &str); 2] = [
        (
            "forgekeep.example.toml",
            include_str!("../../../forgekeep.example.toml"),
        ),
        (
            "deploy/forgekeep.docker.toml",
            include_str!("../../../deploy/forgekeep.docker.toml"),
        ),
    ];

    /// The documented first step of every install is `cp forgekeep.example.toml
    /// forgekeep.toml`. With `deny_unknown_fields` on `ConfigFile` and on every
    /// section, one stale key in a file we ship is not a cosmetic drift — it is
    /// a hard startup failure for whoever followed the instructions.
    ///
    /// Deliberately routed through `load_config_file`, not a bare
    /// `toml::from_str`: that is the function `serve` and every one-shot
    /// subcommand call, so this exercises the loader an operator actually hits.
    #[test]
    fn the_shipped_configs_load_through_the_real_loader() {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in SHIPPED_CONFIGS {
            let path = dir.path().join(name.replace('/', "-"));
            std::fs::write(&path, content).unwrap();

            super::load_config_file(path.to_str().unwrap()).unwrap_or_else(|error| {
                panic!(
                    "`cp {name} forgekeep.toml` is the documented first step of an install, \
                     and the result does not load: {error:#}"
                )
            });
        }
    }

    /// A `# key = value` line in a shipped config is documentation an operator
    /// is invited to uncomment — and the parse above cannot see it, because a
    /// comment parses as nothing. Renaming or removing such a key leaves the
    /// file loading perfectly while the very line it advertises turns into
    /// `unknown field` on the next start.
    ///
    /// Every candidate is checked on its own minimal document rather than by
    /// uncommenting the whole file: `trusted_proxies` and `trusted_origins` are
    /// each shipped live *and* commented as an example, and one document
    /// holding both is a duplicate-key error about the test, not about the key.
    #[test]
    fn every_commented_setting_in_the_shipped_configs_is_a_real_key() {
        let mut checked = 0;

        for (name, content) in SHIPPED_CONFIGS {
            let mut section: Option<&str> = None;

            for (index, line) in content.lines().enumerate() {
                let line = line.trim();
                if let Some((header, _)) = line.strip_prefix('[').and_then(|l| l.split_once(']')) {
                    section = Some(header);
                    continue;
                }
                let Some(assignment) = commented_assignment(line) else {
                    continue;
                };

                let document = match section {
                    Some(section) => format!("[{section}]\n{assignment}\n"),
                    None => format!("{assignment}\n"),
                };
                toml::from_str::<ConfigFile>(&document).unwrap_or_else(|error| {
                    panic!(
                        "{name}:{}: `{assignment}` is offered to be uncommented but is not a \
                         real config key — doing what the file says would stop the server \
                         starting: {error}",
                        index + 1
                    )
                });
                checked += 1;
            }
        }

        // A floor, not a count: it fails loudly if `commented_assignment` ever
        // stops recognising the shape and the test quietly checks nothing.
        assert!(
            checked >= 15,
            "only {checked} commented settings found across the shipped configs — \
             the scanner has stopped matching them"
        );
    }

    /// A commented-out assignment (`# key = value`) — the shape an operator
    /// uncomments — as opposed to prose that merely contains an `=`
    /// (`0 = built-in default`, `e.g. 0.1 = sample 10%`, ``` `enabled = true`
    /// fails the start ```). The text left of the `=` has to be a bare TOML key
    /// on its own.
    fn commented_assignment(line: &str) -> Option<&str> {
        let body = line.strip_prefix('#')?.trim();
        assignment_key(body).map(|_| body)
    }

    /// The key of a `key = value` assignment, when the text left of the `=` is
    /// a bare TOML key rather than prose that happens to contain one.
    fn assignment_key(text: &str) -> Option<&str> {
        let (key, _) = text.split_once('=')?;
        let key = key.trim();
        (!key.is_empty()
            && key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .then_some(key)
    }

    /// The documentation an operator reads *before* copying anything. The
    /// shipped configs above are checked because they are copied; these files
    /// are checked because they are copied *from* — the `[section]` blocks in
    /// a README are pasted into `forgekeep.toml` exactly as often, and
    /// `deny_unknown_fields` does not care which of the two the operator used.
    const DOCUMENTED_CONFIGS: [(&str, &str); 2] = [
        ("README.md", include_str!("../../../README.md")),
        (
            "deploy/README.md",
            include_str!("../../../deploy/README.md"),
        ),
    ];

    /// Root sections that belong to some *other* TOML document — a
    /// `Cargo.toml` excerpt in a contributor note, say. Everything else in a
    /// ```toml block is read as ForgeKeep configuration on purpose: a section
    /// that quietly stopped being one of ours is the drift being hunted here,
    /// so the list is an allow-list of foreigners, never of our own sections.
    const FOREIGN_TOML_SECTIONS: [&str; 8] = [
        "package",
        "dependencies",
        "dev-dependencies",
        "build-dependencies",
        "workspace",
        "profile",
        "features",
        "patch",
    ];

    /// The ```toml fenced blocks of a markdown file, as `(line number of the
    /// block's first content line, block body)`.
    fn toml_code_blocks(name: &str, content: &str) -> Vec<(usize, String)> {
        let mut blocks = Vec::new();
        let mut body: Vec<&str> = Vec::new();
        let mut start = 0usize;
        let mut inside = false;

        for (index, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if inside {
                if trimmed == "```" {
                    blocks.push((start, body.join("\n")));
                    body.clear();
                    inside = false;
                } else {
                    body.push(line);
                }
            } else if trimmed == "```toml" {
                inside = true;
                start = index + 2;
            }
        }

        assert!(
            !inside,
            "{name}:{start}: a ```toml block is never closed — the extractor \
             below reads the rest of the document as configuration"
        );
        blocks
    }

    /// The first `[section]` header of a block, when it opens with one.
    fn first_toml_section(body: &str) -> Option<&str> {
        body.lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .and_then(|line| line.strip_prefix('['))
            .and_then(|line| line.split_once(']'))
            .map(|(header, _)| header)
    }

    /// Every ```toml block in the docs has to load as a real `ConfigFile`.
    ///
    /// A block is required to open with its `[section]` header rather than
    /// being parsed as a bare fragment: `external_url` exists both at the root
    /// of `ConfigFile` and under `[server]`, so a headerless fragment would
    /// parse green while advertising a key that does not exist where the
    /// reader will paste it.
    #[test]
    fn every_toml_block_in_the_docs_loads_as_config() {
        let mut checked = 0;

        for (name, content) in DOCUMENTED_CONFIGS {
            for (line, body) in toml_code_blocks(name, content) {
                let section = first_toml_section(&body).unwrap_or_else(|| {
                    panic!(
                        "{name}:{line}: this ```toml block does not open with a `[section]` \
                         header, so what it advertises cannot be checked against the section \
                         a reader would paste it into"
                    )
                });
                if FOREIGN_TOML_SECTIONS.contains(&section) {
                    continue;
                }

                toml::from_str::<ConfigFile>(&body).unwrap_or_else(|error| {
                    panic!(
                        "{name}:{line}: the `[{section}]` block on the page an operator reads \
                         first is not valid ForgeKeep configuration — pasting it into \
                         forgekeep.toml would stop the server starting: {error}"
                    )
                });
                checked += 1;
            }
        }

        // A floor, not a count: it fails loudly if the fence scanner ever stops
        // matching and the test quietly checks nothing.
        assert!(
            checked >= 3,
            "only {checked} configuration blocks found across the documentation — \
             the ```toml scanner has stopped matching them"
        );
    }

    /// The same blind spot the shipped configs had: a `# key = value` line
    /// inside a documented block is an invitation to uncomment, and the parse
    /// above cannot see it.
    #[test]
    fn every_commented_setting_in_the_documented_blocks_is_a_real_key() {
        let mut checked = 0;

        for (name, content) in DOCUMENTED_CONFIGS {
            for (line, body) in toml_code_blocks(name, content) {
                let mut section: Option<&str> = None;

                for (offset, entry) in body.lines().enumerate() {
                    let entry = entry.trim();
                    if let Some((header, _)) =
                        entry.strip_prefix('[').and_then(|e| e.split_once(']'))
                    {
                        section = Some(header);
                        continue;
                    }
                    if section.is_some_and(|s| FOREIGN_TOML_SECTIONS.contains(&s)) {
                        continue;
                    }
                    let Some(assignment) = commented_assignment(entry) else {
                        continue;
                    };

                    let document = match section {
                        Some(section) => format!("[{section}]\n{assignment}\n"),
                        None => format!("{assignment}\n"),
                    };
                    toml::from_str::<ConfigFile>(&document).unwrap_or_else(|error| {
                        panic!(
                            "{name}:{}: `{assignment}` is offered to be uncommented but is not a \
                             real config key — doing what the documentation says would stop the \
                             server starting: {error}",
                            line + offset
                        )
                    });
                    checked += 1;
                }
            }
        }

        assert!(
            checked >= 3,
            "only {checked} commented settings found across the documented blocks — \
             the scanner has stopped matching them"
        );
    }

    /// Every `(section, key)` a reader of a config file can actually see. A
    /// live assignment and a `# key = value` line the operator is invited to
    /// uncomment count the same here: either one tells them the knob exists,
    /// which is the whole question below.
    fn keys_offered_by(content: &str) -> BTreeSet<(&str, &str)> {
        let mut offered = BTreeSet::new();
        let mut section = "";

        for line in content.lines() {
            let line = line.trim();
            if let Some((header, _)) = line.strip_prefix('[').and_then(|l| l.split_once(']')) {
                section = header;
                continue;
            }
            let assignment = line.strip_prefix('#').map_or(line, str::trim);
            if let Some(key) = assignment_key(assignment) {
                offered.insert((section, key));
            }
        }

        offered
    }

    /// Keys the model accepts that `forgekeep.example.toml` deliberately does
    /// not show, each with the reason it is held back. The list exists so that
    /// adding a knob without a line in the operator's template is a decision
    /// someone made, rather than something nobody noticed.
    const UNDOCUMENTED_ON_PURPOSE: [(&str, &str, &str); 1] = [(
        "",
        "external_url",
        "the root-level spelling, kept so config files written before it moved \
         under [server] keep loading (it still wins over the section key); \
         [server].external_url is the one shown to operators",
    )];

    /// The mirror of the checks above: those ask that everything an operator
    /// can read is a real key, this asks that every real key can be read.
    ///
    /// A knob nobody can discover is a quiet loss rather than a loud one — the
    /// built-in default may be wrong for a deployment, and the operator, who
    /// reads `forgekeep.example.toml` and not `config.rs`, never learns there
    /// was anything to set. Only the example file is held to this:
    /// `deploy/forgekeep.docker.toml` is one deployment's answers, not the
    /// catalogue of questions.
    #[test]
    fn every_key_the_model_accepts_is_shown_in_the_example_config() {
        let (name, content) = SHIPPED_CONFIGS[0];
        assert_eq!(
            name, "forgekeep.example.toml",
            "SHIPPED_CONFIGS has been reordered — this test is about the operator template"
        );

        let offered = keys_offered_by(content);
        let inventory = config_key_inventory(production_config_source());
        let mut checked = 0;

        for &(section, key) in &inventory {
            if UNDOCUMENTED_ON_PURPOSE
                .iter()
                .any(|&(excused_section, excused_key, _)| {
                    excused_section == section && excused_key == key
                })
            {
                continue;
            }

            let place = if section.is_empty() {
                format!("`{key}` at the document root")
            } else {
                format!("`{key}` in [{section}]")
            };
            assert!(
                offered.contains(&(section, key)),
                "{name} never mentions {place}, so the only way to find out the setting \
                 exists is to read config.rs — add it to the template (a commented \
                 `# {key} = <default>` line counts) or name it in \
                 UNDOCUMENTED_ON_PURPOSE with the reason it is held back"
            );
            checked += 1;
        }

        // A floor, not a count: it fails loudly if the declaration parser ever
        // stops matching fields and the test quietly checks nothing.
        assert!(
            checked >= 40,
            "only {checked} config keys derived from the model — \
             the ConfigFile declaration parser has stopped matching fields"
        );

        for (section, key, _) in UNDOCUMENTED_ON_PURPOSE {
            assert!(
                inventory.contains(&(section, key)),
                "UNDOCUMENTED_ON_PURPOSE still excuses `{key}` in [{section}], a key the \
                 model no longer accepts — drop the entry so the list keeps meaning \
                 something"
            );
        }
    }

    /// The sentence in `ARCHITECTURE.md` that introduces the model's sections.
    const MODEL_SECTIONS_LEAD: &str = "Model sections include";

    /// `ARCHITECTURE.md` names the sections in hand-written prose, which is the
    /// same drift by a third route: a section added to `ConfigFile` does not
    /// add itself to a sentence, and a section deleted from it does not leave.
    #[test]
    fn the_architecture_doc_lists_exactly_the_model_sections() {
        const ARCHITECTURE_MD: &str = include_str!("../../../ARCHITECTURE.md");

        let sentence = ARCHITECTURE_MD
            .split_once(MODEL_SECTIONS_LEAD)
            .and_then(|(_, rest)| rest.split_once('.'))
            .map(|(sentence, _)| sentence)
            .unwrap_or_else(|| {
                panic!(
                    "ARCHITECTURE.md must keep a sentence starting `{MODEL_SECTIONS_LEAD}` — \
                     it is the enumeration this test checks against the model"
                )
            });

        let listed: BTreeSet<&str> = sentence.split('`').skip(1).step_by(2).collect();
        let declared: BTreeSet<&str> = nested_config_sections(production_config_source())
            .into_iter()
            .map(|(section, _)| section)
            .collect();

        let invented: Vec<&&str> = listed.difference(&declared).collect();
        let missing: Vec<&&str> = declared.difference(&listed).collect();
        assert!(
            invented.is_empty() && missing.is_empty(),
            "ARCHITECTURE.md's list of config sections has drifted from ConfigFile: \
             it names {invented:?}, which the model does not have, and never mentions \
             {missing:?}"
        );
    }

    #[test]
    fn every_nested_config_section_rejects_unknown_keys() {
        let source = production_config_source();
        let sections = nested_config_sections(source);
        assert!(!sections.is_empty(), "nested config inventory is empty");

        for (section, config_type) in sections {
            let declaration = format!("pub(crate) struct {config_type} {{");
            let declaration_offset = source.find(&declaration).unwrap_or_else(|| {
                panic!("ConfigFile section [{section}] uses missing type {config_type}")
            });
            let attributes = &source[source[..declaration_offset]
                .rfind("#[derive(")
                .unwrap_or_else(|| panic!("{config_type} has no derive block"))
                ..declaration_offset];
            assert!(
                attributes.contains("#[serde(deny_unknown_fields)]"),
                "ConfigFile section [{section}] ({config_type}) must deny unknown fields"
            );

            let unknown_key = "definitely_not_a_forgekeep_setting";
            let toml = format!("[{section}]\n{unknown_key} = true\n");
            let error = toml::from_str::<ConfigFile>(&toml).unwrap_err().to_string();
            assert!(
                error.contains(unknown_key),
                "[{section}] rejected the key without naming it: {error}"
            );
        }
    }

    #[test]
    fn nested_config_typos_name_the_path_section_and_key() {
        let cases = [
            ("server", "htp_addr", "\"127.0.0.1:9000\""),
            ("database", "urll", "\"sqlite://wrong.db\""),
            ("observability", "otlp_endpont", "\"http://localhost:4318\""),
        ];

        let dir = tempfile::tempdir().unwrap();
        for (section, key, value) in cases {
            let path = dir.path().join(format!("{section}-typo.toml"));
            std::fs::write(&path, format!("[{section}]\n{key} = {value}\n")).unwrap();

            let error = format!(
                "{:#}",
                super::load_config_file(path.to_str().unwrap()).unwrap_err()
            );
            assert!(
                error.contains(path.to_str().unwrap()),
                "[{section}] error does not name the config path: {error}"
            );
            assert!(
                error.contains(&format!("[{section}]")),
                "error does not name section [{section}]: {error}"
            );
            assert!(
                error.contains(key),
                "[{section}] error does not name misspelled key {key}: {error}"
            );
        }
    }

    #[test]
    fn trusted_import_origins_are_parsed_from_the_operator_config() {
        let config: ConfigFile = toml::from_str(
            r#"
[imports]
trusted_origins = ["http://127.0.0.1:8443"]
"#,
        )
        .expect("parse config");

        let trusted =
            super::resolve_trusted_import_origins(Some(&config)).expect("parse trusted origins");
        trusted
            .check_url_static("http://127.0.0.1:8443/group/project.git")
            .expect("configured private origin");
        assert!(trusted
            .check_url_static("http://127.0.0.1:9443/group/project.git")
            .is_err());
    }

    #[test]
    fn malformed_trusted_import_origin_fails_config_resolution() {
        let config: ConfigFile = toml::from_str(
            r#"
[imports]
trusted_origins = ["https://*.internal.example"]
"#,
        )
        .expect("the TOML shape itself is valid");

        let error = super::resolve_trusted_import_origins(Some(&config))
            .expect_err("wildcard trust must fail");
        assert!(format!("{error:#}").contains("[imports].trusted_origins"));
    }

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
        assert_eq!(
            resolved.db_url,
            "sqlite:////srv/forgekeep/forgekeep.db?mode=rwc"
        );
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

    #[test]
    fn package_upload_ceiling_is_explicit_bounded_and_nonzero() {
        assert_eq!(
            super::resolve_package_upload_max_bytes(None).unwrap(),
            rg_http::DEFAULT_PACKAGE_UPLOAD_MAX_BYTES
        );

        let configured: ConfigFile = toml::from_str(
            r#"
[server]
package_upload_max_mb = 3
"#,
        )
        .unwrap();
        assert_eq!(
            super::resolve_package_upload_max_bytes(Some(&configured)).unwrap(),
            3 * 1024 * 1024
        );

        let zero: ConfigFile = toml::from_str(
            r#"
[server]
package_upload_max_mb = 0
"#,
        )
        .unwrap();
        let error = super::resolve_package_upload_max_bytes(Some(&zero)).unwrap_err();
        assert!(
            error.to_string().contains("must be greater than zero"),
            "{error:#}"
        );
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

    /// No `--config` means "no config file", not "some config file": the
    /// one-shot subcommands must be able to run flag-only exactly as before.
    #[test]
    fn no_config_flag_means_no_config_file() {
        assert!(super::load_optional_config_file(None).unwrap().is_none());
    }

    /// A `--config` that cannot be read must abort the subcommand. Falling back
    /// to the built-in defaults here is the dangerous half of this bug class:
    /// `migrate` would report success after migrating an empty SQLite file that
    /// nothing else ever opens.
    #[test]
    fn an_unreadable_config_flag_is_an_error_not_a_silent_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.toml");

        let err = super::load_optional_config_file(path.to_str())
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not exist"), "unexpected: {err}");
    }

    /// The one-shot subcommands (`migrate`, `backup-db`, `rebuild-fts`, …) had
    /// no way to reach the config file at all: `--db-url` carried a clap default
    /// so an operator on Postgres silently migrated / "backed up" a brand-new
    /// empty `./forgekeep.db`.
    #[test]
    fn one_shot_db_url_resolves_config_then_default() {
        let config: ConfigFile =
            toml::from_str("[database]\nurl = \"postgres://forge:pw@db.internal/forgekeep\"\n")
                .unwrap();

        // No flag → the config file wins over the built-in SQLite default.
        assert_eq!(
            super::resolve_db_url(None, Some(&config)),
            "postgres://forge:pw@db.internal/forgekeep"
        );
        // Flag → wins over the config file.
        assert_eq!(
            super::resolve_db_url(Some("mysql://cli/db".to_string()), Some(&config)),
            "mysql://cli/db"
        );
        // Neither → unchanged historical behaviour.
        assert_eq!(super::resolve_db_url(None, None), super::DEFAULT_DB_URL);
        // A config file without a `[database]` table does not shadow the default.
        let empty: ConfigFile = toml::from_str("[ci]\ndocker = true\n").unwrap();
        assert_eq!(
            super::resolve_db_url(None, Some(&empty)),
            super::DEFAULT_DB_URL
        );
    }

    /// Same chain for `--repo-root`: `import` / `index-repo` / `create-repo`
    /// must not place repositories somewhere the server never looks.
    #[test]
    fn one_shot_repo_root_resolves_config_then_default() {
        let config: ConfigFile = toml::from_str("[server]\nrepo_root = \"/data/repos\"\n").unwrap();

        assert_eq!(super::resolve_repo_root(None, Some(&config)), "/data/repos");
        assert_eq!(
            super::resolve_repo_root(Some("/from/cli".to_string()), Some(&config)),
            "/from/cli"
        );
        assert_eq!(
            super::resolve_repo_root(None, None),
            super::DEFAULT_REPO_ROOT
        );
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
        assert_eq!(
            config.audit.archive_dir.as_deref(),
            Some("/data/audit-archive")
        );
        // The JWT secret belongs in deploy/.env, never in a file that may be
        // committed.
        assert!(config.auth.jwt_secret.is_none());
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

    /// The shipped configs are the answer to "are there backups?" on a fresh
    /// install, so the section has to parse (`deny_unknown_fields` makes a
    /// documented-but-unmodelled key a hard startup failure) *and* actually be
    /// switched on. The docker one must also point at the mounted volume: a
    /// backup under the image's `WORKDIR /app` is erased by the next
    /// `--force-recreate`.
    #[test]
    fn the_shipped_configs_schedule_backups_onto_durable_storage() {
        let example: ConfigFile =
            toml::from_str(include_str!("../../../forgekeep.example.toml")).unwrap();
        assert_eq!(example.backup.enabled, Some(true));
        assert_eq!(example.backup.interval_hours, Some(24));
        assert_eq!(example.backup.keep_last, Some(7));

        let docker: ConfigFile =
            toml::from_str(include_str!("../../../deploy/forgekeep.docker.toml")).unwrap();
        assert_eq!(docker.backup.enabled, Some(true));
        assert_eq!(docker.backup.dir.as_deref(), Some("/data/backups"));
    }
}
