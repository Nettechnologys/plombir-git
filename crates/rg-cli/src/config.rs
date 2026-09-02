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

use std::path::{Path, PathBuf};

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
    /// Exact plaintext HTTP origins allowed to serve custom OIDC discovery and
    /// receive OIDC client secrets or Bearer access tokens. HTTPS remains the
    /// default; paths and wildcards are rejected.
    #[serde(default)]
    pub(crate) allow_insecure_oidc_origins: Vec<String>,
    /// Exact plaintext LDAP endpoints allowed to receive the service bind and
    /// incoming user's password. A custom port alone never enables plaintext.
    #[serde(default)]
    pub(crate) allow_insecure_ldap_endpoints: Vec<String>,
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

/// Seconds `serve` allows for draining in-flight requests and the CI-log queue
/// after SIGTERM before the process is forced down, without a configured
/// `[server].shutdown_grace_secs`.
///
/// A function beside the other defaults rather than an `unwrap_or(30)` at the
/// resolution site: `forgekeep.example.toml` offers `# shutdown_grace_secs = 30`
/// as the value an operator gets by leaving it commented, and a number that
/// exists only inside one `unwrap_or` is a number no contract can reach — which
/// is exactly how `timeouts.job_secs` came to be resolved by a literal `3600`
/// while its five neighbours called their default function.
pub(crate) fn default_shutdown_grace() -> u64 {
    30
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
    /// Labels the in-process runner answers to, for a job's `tags:` /
    /// `runs-on:`. Absent takes [`rg_core::ci::default_runner_labels`]; an
    /// explicit empty list means the runner answers to nothing and every job
    /// carrying a label is refused.
    #[serde(default)]
    pub(crate) runner_labels: Option<Vec<String>>,
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
    /// Permit mirror credentials and fetched content over plaintext `http://`.
    /// Off unless the instance operator explicitly accepts that exposure;
    /// native `git://` is always disabled and does not inherit this exception.
    #[serde(default)]
    pub(crate) allow_insecure_http: Option<bool>,
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
    /// Exact plaintext HTTP origins permitted to receive import credentials.
    /// This is intentionally separate from `trusted_origins`: private-network
    /// reachability and transport confidentiality are independent decisions.
    #[serde(default)]
    pub(crate) allow_insecure_http_origins: Vec<String>,
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
    /// Permit outbound webhook payloads and HMAC signatures over plaintext
    /// `http://`. Off by default; intended only for an operator-controlled
    /// development network where HTTPS termination is deliberately absent.
    #[serde(default)]
    pub(crate) allow_insecure_http: Option<bool>,
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
    /// wrapper) and HTTP (request-body disk staging). 0 disables the idle watchdog
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
    rg_core::ci::DEFAULT_JOB_TIMEOUT_SECS
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
    "create it first: `install -m 600 forgekeep.example.toml forgekeep.toml` (and bind-mount that file, \
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
    let path_ref = std::path::Path::new(path);
    ensure_regular_file(path_ref, "config file", CONFIG_FILE_HINT)?;
    rg_core::platform::fs::ensure_owner_only(path_ref, "config file")?;
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

#[cfg(test)]
pub(crate) fn write_test_config(
    path: &std::path::Path,
    content: impl AsRef<[u8]>,
) -> std::io::Result<()> {
    std::fs::write(path, content)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
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

/// Built-in defaults for the config-file-only knobs `serve` resolves. They have
/// no CLI flag, so nothing about them was ever named anywhere: each used to be
/// a bare literal inside the `.unwrap_or(…)` that resolves it, which is the one
/// shape no doc-versus-code check can reach — a number with no name has nothing
/// to be compared against, and every one of these is *also* written out in
/// `forgekeep.example.toml` and `deploy/forgekeep.docker.toml`.
///
/// The precedent is `[mirror]`, whose numeric knobs already resolve through
/// `rg_core::mirror::scheduler::DEFAULT_*`; the values that live in rg-core
/// (`[audit]`, `[backup]`) are named there for the same reason and used from
/// here, so a single declaration serves the server and the one-shot commands.
///
/// `[ci]` and `[releases]` are opt-in switches: the default is `false` because
/// turning them on hands pushed CI config a shell or the host Docker socket,
/// which is a decision an upgrade must never make on the operator's behalf.
pub(crate) const DEFAULT_CI_DOCKER: bool = false;
pub(crate) const DEFAULT_CI_EXTERNAL_RUNNERS: bool = false;
pub(crate) const DEFAULT_CI_ALLOW_HOST_RUNNER: bool = false;
pub(crate) const DEFAULT_ATTESTATION_ENABLED: bool = false;
/// `[webhooks].allow_insecure_http`: plaintext transport is never enabled by
/// an upgrade or by a missing config section.
pub(crate) const DEFAULT_WEBHOOKS_ALLOW_INSECURE_HTTP: bool = false;

/// `[rate_limit].max_keys`: 0 is a sentinel, not a cap — it means "use the
/// limiter's own bound", `rg_http::rate_limit::DEFAULT_MAX_KEYS`.
pub(crate) const DEFAULT_RATE_LIMIT_MAX_KEYS: usize = 0;

/// The credential-endpoint limiter (`/users/register`, `/users/login`). Always
/// on, independent of `[rate_limit].max`, so registration spam and password
/// guessing stay throttled on an instance that disabled the global limit.
pub(crate) const DEFAULT_AUTH_RATE_LIMIT_MAX: u32 = 10;
pub(crate) const DEFAULT_AUTH_RATE_LIMIT_WINDOW: u64 = 60;

/// `[audit].enabled`: on by default, because an audit log that is never trimmed
/// grows until the disk does.
pub(crate) const DEFAULT_AUDIT_ENABLED: bool = true;

/// `[backup].enabled`: off by default so an upgrade never starts consuming
/// `keep_last` × database-size of disk unannounced. Both shipped templates turn
/// it on deliberately — a fresh install should be backed up from the first
/// start — which is why the code default is stated in their prose instead, and
/// that sentence is what `prose_defaults()` holds to this constant.
pub(crate) const DEFAULT_BACKUP_ENABLED: bool = false;

/// `[mirror].enabled`: on by default, unlike `[backup]` — a mirror only exists
/// because an operator asked for one, and its settings page shows a next-sync
/// time that nothing would act on with the sweep off.
pub(crate) const DEFAULT_MIRROR_ENABLED: bool = true;
/// `[mirror].allow_insecure_http`: an omitted/new section is always secure.
pub(crate) const DEFAULT_MIRROR_ALLOW_INSECURE_HTTP: bool = false;

/// Resolve the instance-owned outbound mirror transport policy.
pub(crate) fn resolve_mirror_transport_policy(
    cfg: Option<&ConfigFile>,
) -> rg_core::mirror::transport::MirrorTransportPolicy {
    rg_core::mirror::transport::MirrorTransportPolicy::new(
        cfg.and_then(|config| config.mirror.allow_insecure_http)
            .unwrap_or(DEFAULT_MIRROR_ALLOW_INSECURE_HTTP),
    )
}

/// Resolve the process-wide outbound webhook transport policy.
pub(crate) fn resolve_webhook_transport_policy(
    cfg: Option<&ConfigFile>,
) -> rg_core::webhook::transport::WebhookTransportPolicy {
    rg_core::webhook::transport::WebhookTransportPolicy::new(
        cfg.and_then(|config| config.webhooks.allow_insecure_http)
            .unwrap_or(DEFAULT_WEBHOOKS_ALLOW_INSECURE_HTTP),
    )
}

/// Fallback `[audit].archive_dir` / `[backup].dir` for a `repo_root` that is
/// not an absolute path. When it is, both default to a *sibling* of it instead
/// (`/data/repos` → `/data/audit-archive`), so the archive lands on the volume
/// holding the rest of the state rather than inside the container layer.
pub(crate) const DEFAULT_AUDIT_ARCHIVE_DIR: &str = "./data/audit-archive";
pub(crate) const DEFAULT_DB_BACKUP_DIR: &str = "./data/backups";

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

/// Parse the exact origins on which import credentials may cross plaintext
/// HTTP. Both `serve` and one-shot `import` consume this same policy.
pub(crate) fn resolve_import_transport_policy(
    cfg: Option<&ConfigFile>,
) -> anyhow::Result<rg_core::import::trust::ImportTransportPolicy> {
    let values = cfg
        .map(|config| config.imports.allow_insecure_http_origins.as_slice())
        .unwrap_or_default();
    rg_core::import::trust::ImportTransportPolicy::parse(values)
        .context("invalid config `[imports].allow_insecure_http_origins`")
}

/// Parse exact origins on which custom OIDC traffic may cross plaintext HTTP.
pub(crate) fn resolve_oidc_transport_policy(
    cfg: Option<&ConfigFile>,
) -> anyhow::Result<rg_core::auth::sso::OidcTransportPolicy> {
    let values = cfg
        .map(|config| config.auth.allow_insecure_oidc_origins.as_slice())
        .unwrap_or_default();
    rg_core::auth::sso::OidcTransportPolicy::parse(values)
        .context("invalid config `[auth].allow_insecure_oidc_origins`")
}

/// Parse exact endpoints on which LDAP credentials may cross plaintext TCP.
pub(crate) fn resolve_ldap_transport_policy(
    cfg: Option<&ConfigFile>,
) -> anyhow::Result<rg_core::auth::ldap::LdapTransportPolicy> {
    let values = cfg
        .map(|config| config.auth.allow_insecure_ldap_endpoints.as_slice())
        .unwrap_or_default();
    rg_core::auth::ldap::LdapTransportPolicy::parse(values)
        .context("invalid config `[auth].allow_insecure_ldap_endpoints`")
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

/// A path as the filesystem sees it, for a diagnostic that has to name *which*
/// file or directory a relative setting actually addressed.
///
/// Both defaults an operator can leave untouched are relative — [`DEFAULT_DB_URL`]
/// and [`DEFAULT_REPO_ROOT`] — which makes the spelling on the command line the
/// one thing a message must not simply echo back: `./repos` is identical on the
/// machine where the command did what was meant and on the one where it
/// addressed nothing.
///
/// `.` components are dropped rather than kept, because both defaults start with
/// one and `/opt/./repos` reads like a typo in the message rather than as the
/// answer to "which directory did it mean".
pub(crate) fn absolute_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(path),
            Err(_) => return path.to_path_buf(),
        }
    };
    absolute
        .components()
        .filter(|component| !matches!(component, std::path::Component::CurDir))
        .collect()
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

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }

    fn production_config_source() -> String {
        rust_source::production_rust_code_only(include_str!("config.rs"))
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

    /// The documented first step of every install is `install -m 600
    /// forgekeep.example.toml forgekeep.toml`. With `deny_unknown_fields` on
    /// `ConfigFile` and on every section, one stale key in a file we ship is not
    /// a cosmetic drift — it is a hard startup failure for whoever followed the
    /// instructions.
    ///
    /// Deliberately routed through `load_config_file`, not a bare
    /// `toml::from_str`: that is the function `serve` and every one-shot
    /// subcommand call, so this exercises the loader an operator actually hits.
    #[test]
    fn the_shipped_configs_load_through_the_real_loader() {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in SHIPPED_CONFIGS {
            let path = dir.path().join(name.replace('/', "-"));
            super::write_test_config(&path, content).unwrap();

            super::load_config_file(path.to_str().unwrap()).unwrap_or_else(|error| {
                panic!(
                    "`install -m 600 {name} forgekeep.toml` is the documented first step of an install, \
                     and the result does not load: {error:#}"
                )
            });
        }
    }

    /// Git cannot preserve `0600`, so the operator must create the live copy
    /// with an explicit mode. Keep every shipped quick-start on that safe
    /// spelling; plain `cp` would immediately create a file the loader refuses.
    #[test]
    fn every_documented_config_install_creates_an_owner_only_file() {
        const SURFACES: [(&str, &str, &str); 4] = [
            (
                "README.md",
                include_str!("../../../README.md"),
                "install -m 600 forgekeep.example.toml forgekeep.toml",
            ),
            (
                "deploy/README.md",
                include_str!("../../../deploy/README.md"),
                "install -m 600 forgekeep.docker.toml forgekeep.toml",
            ),
            (
                "deploy/forgekeep.docker.toml",
                include_str!("../../../deploy/forgekeep.docker.toml"),
                "install -m 600 forgekeep.docker.toml forgekeep.toml",
            ),
            (
                "deploy/docker-compose.hostdir.yml",
                include_str!("../../../deploy/docker-compose.hostdir.yml"),
                "install -m 600 forgekeep.docker.toml forgekeep.toml",
            ),
        ];

        for (name, body, safe_install) in SURFACES {
            assert!(
                body.contains(safe_install),
                "{name} must create forgekeep.toml with owner-only permissions: {safe_install}"
            );
            assert!(
                !body.contains("cp forgekeep.example.toml forgekeep.toml")
                    && !body.contains("cp forgekeep.docker.toml forgekeep.toml"),
                "{name} must not recommend a umask-dependent config copy"
            );
        }
    }

    /// `deploy/.env` ends up holding `FORGEKEEP_JWT_SECRET` and
    /// `FORGEKEEP_ENCRYPTION_KEY` in plain text — the token-signing key and the
    /// at-rest key, either of which is enough on its own to take the instance
    /// over. Unlike `forgekeep.toml` nothing validates its mode at startup
    /// (docker compose reads it, not us), so the copy instruction is the only
    /// place the permission can be got right, and `cp` under a stock umask
    /// gets it wrong every time.
    #[test]
    fn every_documented_env_install_creates_an_owner_only_file() {
        const SURFACES: [(&str, &str, &str); 4] = [
            (
                "deploy/README.md",
                include_str!("../../../deploy/README.md"),
                "install -m 600 .env.example .env",
            ),
            (
                "deploy/docker-compose.hostdir.yml",
                include_str!("../../../deploy/docker-compose.hostdir.yml"),
                "install -m 600 .env.example .env",
            ),
            (
                "deploy/docker-compose.yml",
                include_str!("../../../deploy/docker-compose.yml"),
                "install -m 600 deploy/.env.example deploy/.env",
            ),
            (
                ".github/workflows/regression.yml",
                include_str!("../../../.github/workflows/regression.yml"),
                "install -m 600 deploy/.env.example deploy/.env",
            ),
        ];

        for (name, body, safe_install) in SURFACES {
            assert!(
                body.contains(safe_install),
                "{name} must create the .env with owner-only permissions: {safe_install}"
            );
            assert!(
                !body.contains("cp .env.example .env")
                    && !body.contains("cp deploy/.env.example deploy/.env"),
                "{name} must not recommend a umask-dependent copy of the secret-bearing .env"
            );
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

    /// README sections whose ```toml blocks describe a **different** ForgeKeep
    /// document, each with the test that checks them instead.
    ///
    /// `FOREIGN_TOML_SECTIONS` above tells a foreigner apart by its first line;
    /// `runner.toml` cannot be told apart that way, because it is flat and opens
    /// with no `[section]` header at all. So this list keys on the heading the
    /// block sits under, and — like that one — it is an allow-list of
    /// foreigners: a block of ours that stopped looking like ours must still
    /// fail, and a block excused here has to be checked somewhere.
    const FOREIGN_TOML_DOCUMENTS: [(&str, &str, &str); 1] = [(
        "README.md",
        "## CI runner (`forgekeep-runner`)",
        "`runner.toml`, checked against the `RunnerConfig` declaration by \
         `rg-runner/src/config.rs::every_toml_block_in_the_readme_runner_section_loads_as_a_runner_config`",
    )];

    /// The `##`-level heading the given line sits under.
    fn enclosing_heading(content: &str, line: usize) -> Option<&str> {
        content
            .lines()
            .take(line)
            .filter(|line| line.starts_with("## "))
            .last()
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
                let foreign_document =
                    FOREIGN_TOML_DOCUMENTS
                        .iter()
                        .find(|(document, heading, _)| {
                            *document == name && enclosing_heading(content, line) == Some(*heading)
                        });
                if let Some((_, heading, checked_by)) = foreign_document {
                    assert!(
                        !checked_by.is_empty(),
                        "{name}: the ```toml blocks under `{heading}` are excused from this \
                         check without naming what checks them instead"
                    );
                    continue;
                }

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

        // An excuse for a heading nobody writes any more is a hole waiting for
        // the next block that should have been checked here.
        for (document, heading, _) in FOREIGN_TOML_DOCUMENTS {
            assert!(
                DOCUMENTED_CONFIGS
                    .iter()
                    .any(|(name, content)| *name == document && content.contains(heading)),
                "FOREIGN_TOML_DOCUMENTS excuses the ```toml blocks under `{heading}`, and \
                 {document} no longer has that heading — drop the entry, or point it at the \
                 heading the section was renamed to"
            );
        }
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

    /// The phrase that turns a ```toml block from an *example* into a
    /// **quotation**: a claim about what a file this repository ships actually
    /// contains, rather than a fragment a reader is invited to paste.
    ///
    /// Read from the page rather than kept in a registry beside it: a second
    /// sentence saying it, about any file in [`SHIPPED_CONFIGS`], joins the
    /// check below by being written.
    const QUOTATION_LEAD: &str = "ships with:";

    /// The path a quotation sentence names in backticks — ``**Backs itself
    /// up.** `deploy/forgekeep.docker.toml` ships with:`` → the path.
    fn quoted_file(line: &str) -> Option<&str> {
        line.split_once(QUOTATION_LEAD)
            .map(|(before, _)| before.trim_end())
            .and_then(|before| before.strip_suffix('`'))
            .and_then(|before| before.rsplit_once('`'))
            .map(|(_, path)| path)
    }

    /// The ```toml block a quotation sentence introduces, keyed the way
    /// [`toml_code_blocks`] keys it. Found rather than assumed: a sentence
    /// whose block drifted away from under it is a claim with nothing beneath
    /// it, and silently checking the next block on the page would be worse
    /// than checking none.
    fn block_introduced_at(content: &str, sentence: usize) -> Option<usize> {
        content
            .lines()
            .enumerate()
            .skip(sentence + 1)
            .find(|(_, line)| !line.trim().is_empty())
            .filter(|(_, line)| line.trim() == "```toml")
            .map(|(index, _)| index + 2)
    }

    /// Every live `(section, key, value)` a config file states, spelled the way
    /// that file spells it.
    fn assignments_stated_by(content: &str) -> BTreeSet<(&str, &str, &str)> {
        let mut stated = BTreeSet::new();
        let mut section = "";

        for line in content.lines() {
            let line = line.trim();
            if let Some((header, _)) = line.strip_prefix('[').and_then(|l| l.split_once(']')) {
                section = header;
                continue;
            }
            if let Some((key, value)) = live_assignment_parts(line) {
                stated.insert((section, key, value));
            }
        }
        stated
    }

    /// A block introduced as *what a shipped file contains* is a different
    /// claim from the ones above, and needs a different check.
    ///
    /// Every other test on this page asks whether a block would work: it loads
    /// as a `ConfigFile`, its keys are real, its values match the code. A
    /// quotation also asserts something about a **file** — and that assertion
    /// is the one nothing held. `deploy/forgekeep.docker.toml` can have its
    /// `[backup]` section retuned without the page that quotes it changing a
    /// character, and both sides stay individually valid: the file still loads,
    /// the block still parses, every number in it is still a real default. The
    /// disagreement is only visible to someone holding the two open at once,
    /// and `deploy/README.md` is precisely the page whose reader has no source
    /// tree at all.
    ///
    /// A subset, not an equality: quoting the four lines of `[backup]` says
    /// nothing about the rest of the file, and a page is free to show only the
    /// part it is talking about.
    #[test]
    fn every_block_quoting_a_shipped_config_states_what_that_file_says() {
        assert_eq!(
            quoted_file("**Backs itself up.** `deploy/forgekeep.docker.toml` ships with:"),
            Some("deploy/forgekeep.docker.toml"),
            "the reader does not find the file a quotation sentence names"
        );
        assert_eq!(
            quoted_file("the server backs itself up, and one section decides how"),
            None,
            "the reader takes an ordinary sentence for a quotation"
        );

        let mut checked = 0;

        for (name, content) in DOCUMENTED_CONFIGS {
            let blocks = toml_code_blocks(name, content);

            for (index, line) in content.lines().enumerate() {
                let Some(path) = quoted_file(line) else {
                    continue;
                };

                let (_, quoted) = SHIPPED_CONFIGS
                    .iter()
                    .find(|(shipped, _)| *shipped == path)
                    .unwrap_or_else(|| {
                        panic!(
                            "{name}:{}: this sentence quotes `{path}`, which is not a file \
                             these tests include! — add it to SHIPPED_CONFIGS, or name the \
                             file the page really quotes",
                            index + 1
                        )
                    });

                let start = block_introduced_at(content, index).unwrap_or_else(|| {
                    panic!(
                        "{name}:{}: this sentence promises what `{path}` contains and no \
                         ```toml block follows it — the claim reaches the reader with \
                         nothing under it, and this check with nothing to compare",
                        index + 1
                    )
                });
                let (_, body) = blocks
                    .iter()
                    .find(|(block, _)| *block == start)
                    .unwrap_or_else(|| {
                        panic!("{name}:{start}: the ```toml block below the quotation of `{path}` was not extracted")
                    });

                let stated = assignments_stated_by(quoted);
                let mut section = "";

                for (offset, entry) in body.lines().enumerate() {
                    let entry = entry.trim();
                    if let Some((header, _)) =
                        entry.strip_prefix('[').and_then(|e| e.split_once(']'))
                    {
                        section = header;
                        assert!(
                            stated.iter().any(|(stated, _, _)| *stated == section),
                            "{name}:{}: this block is introduced as what `{path}` contains, \
                             and that file has no `[{section}]` section at all",
                            start + offset
                        );
                        continue;
                    }
                    let Some((key, value)) = live_assignment_parts(entry) else {
                        continue;
                    };

                    assert!(
                        stated.contains(&(section, key, value)),
                        "{name}:{}: this block is introduced as what `{path}` contains, and \
                         that file does not state `{key} = {value}` in [{section}] — the \
                         page quotes a file its reader cannot open",
                        start + offset
                    );
                    checked += 1;
                }
            }
        }

        // A floor, not a count: a quotation whose sentence was reworded away
        // stops being checked without failing anything else, so the reader has
        // to say out loud that it still finds one.
        assert!(
            checked >= 4,
            "only {checked} quoted lines found across the documentation — either no page \
             says `{QUOTATION_LEAD}` about a shipped config any more, or the reader has \
             stopped matching the sentence"
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
        let source = production_config_source();
        let inventory = config_key_inventory(&source);
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

    /// One claim a shipped config makes about a built-in default: a
    /// `key = value` line, either commented — "leave it alone and this is what
    /// you get" — or live, shipping the default as the value.
    ///
    /// The value is never written out in the table below — every row reads
    /// whatever produces it, so a changed default changes what the templates are
    /// held to rather than quietly disagreeing with them.
    struct TemplateDefault {
        section: &'static str,
        key: &'static str,
        /// What produces the value, named for the failure message and for the
        /// census in [`every_default_function_is_either_shown_in_the_template_or_excused`].
        source: &'static str,
        /// Rendered the way TOML spells it, read from that source.
        value: String,
    }

    /// The pairing table. The section/key spellings have to be written out —
    /// no rule derives `default_job_timeout` from `[timeouts].job_secs` — but
    /// the numbers never are.
    ///
    /// Keyed by `(section, key)` and applied to *every* shipped config: wherever
    /// one of them states `[audit].archive_after_days`, that value is the
    /// built-in default unless the file is named in
    /// [`TEMPLATE_VALUES_NOT_DEFAULTS`] with the reason it differs.
    fn template_defaults() -> Vec<TemplateDefault> {
        fn row(
            section: &'static str,
            key: &'static str,
            source: &'static str,
            value: String,
        ) -> TemplateDefault {
            TemplateDefault {
                section,
                key,
                source,
                value,
            }
        }

        vec![
            row(
                "server",
                "repo_root",
                "DEFAULT_REPO_ROOT",
                format!("{:?}", super::DEFAULT_REPO_ROOT),
            ),
            row(
                "server",
                "http_addr",
                "DEFAULT_HTTP_ADDR",
                format!("{:?}", super::DEFAULT_HTTP_ADDR),
            ),
            row(
                "server",
                "ssh_addr",
                "DEFAULT_SSH_ADDR",
                format!("{:?}", super::DEFAULT_SSH_ADDR),
            ),
            row(
                "server",
                "package_upload_max_mb",
                "DEFAULT_PACKAGE_UPLOAD_MAX_MB",
                super::DEFAULT_PACKAGE_UPLOAD_MAX_MB.to_string(),
            ),
            row(
                "server",
                "shutdown_grace_secs",
                "default_shutdown_grace",
                super::default_shutdown_grace().to_string(),
            ),
            row(
                "database",
                "url",
                "DEFAULT_DB_URL",
                format!("{:?}", super::DEFAULT_DB_URL),
            ),
            row(
                "auth",
                "registration",
                "user::registration::RegistrationMode::default",
                format!(
                    "{:?}",
                    rg_core::user::registration::RegistrationMode::default().as_str()
                ),
            ),
            row(
                "auth",
                "allow_insecure_oidc_origins",
                "Vec::<String>::default",
                format!("{:?}", Vec::<String>::default()),
            ),
            row(
                "auth",
                "allow_insecure_ldap_endpoints",
                "Vec::<String>::default",
                format!("{:?}", Vec::<String>::default()),
            ),
            row(
                "ci",
                "docker",
                "DEFAULT_CI_DOCKER",
                super::DEFAULT_CI_DOCKER.to_string(),
            ),
            row(
                "ci",
                "external_runners",
                "DEFAULT_CI_EXTERNAL_RUNNERS",
                super::DEFAULT_CI_EXTERNAL_RUNNERS.to_string(),
            ),
            row(
                "ci",
                "allow_host_runner",
                "DEFAULT_CI_ALLOW_HOST_RUNNER",
                super::DEFAULT_CI_ALLOW_HOST_RUNNER.to_string(),
            ),
            row(
                "ci",
                "runner_labels",
                "rg_core::ci::default_runner_labels",
                format!("{:?}", rg_core::ci::default_runner_labels()),
            ),
            row(
                "releases",
                "attestation_enabled",
                "DEFAULT_ATTESTATION_ENABLED",
                super::DEFAULT_ATTESTATION_ENABLED.to_string(),
            ),
            row(
                "rate_limit",
                "max",
                "DEFAULT_RATE_LIMIT_MAX",
                super::DEFAULT_RATE_LIMIT_MAX.to_string(),
            ),
            row(
                "rate_limit",
                "window_secs",
                "DEFAULT_RATE_LIMIT_WINDOW",
                super::DEFAULT_RATE_LIMIT_WINDOW.to_string(),
            ),
            row(
                "rate_limit",
                "max_keys",
                "DEFAULT_RATE_LIMIT_MAX_KEYS",
                super::DEFAULT_RATE_LIMIT_MAX_KEYS.to_string(),
            ),
            row(
                "rate_limit",
                "auth_max",
                "DEFAULT_AUTH_RATE_LIMIT_MAX",
                super::DEFAULT_AUTH_RATE_LIMIT_MAX.to_string(),
            ),
            row(
                "rate_limit",
                "auth_window_secs",
                "DEFAULT_AUTH_RATE_LIMIT_WINDOW",
                super::DEFAULT_AUTH_RATE_LIMIT_WINDOW.to_string(),
            ),
            row(
                "logging",
                "max_size_mb",
                "DEFAULT_LOG_MAX_SIZE_MB",
                super::DEFAULT_LOG_MAX_SIZE_MB.to_string(),
            ),
            row(
                "logging",
                "max_files",
                "DEFAULT_LOG_MAX_FILES",
                super::DEFAULT_LOG_MAX_FILES.to_string(),
            ),
            row(
                "audit",
                "enabled",
                "DEFAULT_AUDIT_ENABLED",
                super::DEFAULT_AUDIT_ENABLED.to_string(),
            ),
            row(
                "audit",
                "archive_dir",
                "DEFAULT_AUDIT_ARCHIVE_DIR",
                format!("{:?}", super::DEFAULT_AUDIT_ARCHIVE_DIR),
            ),
            row(
                "audit",
                "archive_after_days",
                "audit::archiver::DEFAULT_ARCHIVE_AFTER_DAYS",
                rg_core::audit::archiver::DEFAULT_ARCHIVE_AFTER_DAYS.to_string(),
            ),
            row(
                "audit",
                "interval_minutes",
                "audit::archiver::DEFAULT_INTERVAL_MINUTES",
                rg_core::audit::archiver::DEFAULT_INTERVAL_MINUTES.to_string(),
            ),
            row(
                "audit",
                "batch_size",
                "audit::archiver::DEFAULT_BATCH_SIZE",
                rg_core::audit::archiver::DEFAULT_BATCH_SIZE.to_string(),
            ),
            row(
                "backup",
                "dir",
                "DEFAULT_DB_BACKUP_DIR",
                format!("{:?}", super::DEFAULT_DB_BACKUP_DIR),
            ),
            row(
                "backup",
                "interval_hours",
                "backup::DEFAULT_INTERVAL_HOURS",
                rg_core::backup::DEFAULT_INTERVAL_HOURS.to_string(),
            ),
            row(
                "backup",
                "keep_last",
                "backup::DEFAULT_KEEP_LAST",
                rg_core::backup::DEFAULT_KEEP_LAST.to_string(),
            ),
            row(
                "mirror",
                "enabled",
                "DEFAULT_MIRROR_ENABLED",
                super::DEFAULT_MIRROR_ENABLED.to_string(),
            ),
            row(
                "mirror",
                "allow_insecure_http",
                "DEFAULT_MIRROR_ALLOW_INSECURE_HTTP",
                super::DEFAULT_MIRROR_ALLOW_INSECURE_HTTP.to_string(),
            ),
            row(
                "mirror",
                "poll_interval_secs",
                "mirror::scheduler::DEFAULT_POLL_INTERVAL_SECS",
                rg_core::mirror::scheduler::DEFAULT_POLL_INTERVAL_SECS.to_string(),
            ),
            row(
                "mirror",
                "batch_size",
                "mirror::scheduler::DEFAULT_BATCH_SIZE",
                rg_core::mirror::scheduler::DEFAULT_BATCH_SIZE.to_string(),
            ),
            row(
                "imports",
                "allow_insecure_http_origins",
                "Vec::<String>::default",
                format!("{:?}", Vec::<String>::default()),
            ),
            row(
                "webhooks",
                "allow_insecure_http",
                "DEFAULT_WEBHOOKS_ALLOW_INSECURE_HTTP",
                super::DEFAULT_WEBHOOKS_ALLOW_INSECURE_HTTP.to_string(),
            ),
            row(
                "smtp",
                "port",
                "DEFAULT_SMTP_PORT",
                super::DEFAULT_SMTP_PORT.to_string(),
            ),
            row(
                "timeouts",
                "job_secs",
                "default_job_timeout",
                super::default_job_timeout().to_string(),
            ),
            row(
                "timeouts",
                "git_cmd_secs",
                "default_git_timeout",
                super::default_git_timeout().to_string(),
            ),
            row(
                "timeouts",
                "git_stream_secs",
                "default_git_stream_timeout",
                super::default_git_stream_timeout().to_string(),
            ),
            row(
                "timeouts",
                "git_idle_secs",
                "default_git_idle_timeout",
                super::default_git_idle_timeout().to_string(),
            ),
            row(
                "timeouts",
                "db_connect_secs",
                "default_db_connect_timeout",
                super::default_db_connect_timeout().to_string(),
            ),
            row(
                "timeouts",
                "db_idle_secs",
                "default_db_idle_timeout",
                super::default_db_idle_timeout().to_string(),
            ),
            // `{:?}` rather than `to_string()`: TOML spells a string with its
            // quotes and a float with its point, and `1.0f64.to_string()` is
            // `"1"` — which is not what the file says, nor valid here.
            row(
                "observability",
                "service_name",
                "telemetry::DEFAULT_OTEL_SERVICE_NAME",
                format!("{:?}", crate::telemetry::DEFAULT_OTEL_SERVICE_NAME),
            ),
            row(
                "observability",
                "sample_ratio",
                "telemetry::DEFAULT_OTEL_SAMPLE_RATIO",
                format!("{:?}", crate::telemetry::DEFAULT_OTEL_SAMPLE_RATIO),
            ),
        ]
    }

    /// Lines in a shipped config that state a value which is *not* the built-in
    /// default — a placeholder (`# host_key = "/path/to/ssh_host_key"` shows the
    /// shape of a value, it does not claim the server uses that path), a path
    /// that only makes sense inside the container, or a knob the file turns on
    /// deliberately against the code's default.
    ///
    /// Keyed by file, because the same key is a default in one and a decision in
    /// the other: `[audit].archive_dir` is the built-in fallback in
    /// `forgekeep.example.toml` and `/data/audit-archive` in the Docker one.
    ///
    /// This list is what makes the check below a closed contract rather than
    /// rows that happen to be right today — a new `key = 42` line is either
    /// paired with the code that produces the 42, or declared here with a
    /// reason.
    const TEMPLATE_VALUES_NOT_DEFAULTS: [(&str, &str, &str, &str); 30] = [
        (
            "forgekeep.example.toml",
            "server",
            "host_key",
            "a placeholder path; the real default is derived from $HOME at run time \
             by default_host_key_path()",
        ),
        (
            "forgekeep.example.toml",
            "server",
            "external_url",
            "no default: left unset, links point at the address the server bound to",
        ),
        (
            "forgekeep.example.toml",
            "auth",
            "jwt_secret",
            "the placeholder every install has to replace; that it is a secret the \
             server refuses to start with is pinned separately, by \
             the_shipped_jwt_placeholder_is_a_secret_the_server_refuses",
        ),
        (
            "forgekeep.example.toml",
            "auth",
            "encryption_key",
            "a secret to paste, not a value the server picks",
        ),
        (
            "forgekeep.example.toml",
            "auth",
            "key_file",
            "a placeholder path; left unset it is derived beside [server].host_key",
        ),
        (
            "forgekeep.example.toml",
            "webhooks",
            "external_secret",
            "a secret to paste; unset means inbound signature checking stays off",
        ),
        (
            "forgekeep.example.toml",
            "rate_limit",
            "trusted_proxies",
            "the empty list is what unset means, and the commented line below it is \
             an illustration of the shape — neither is a value the code names",
        ),
        (
            "forgekeep.example.toml",
            "smtp",
            "host",
            "a placeholder host; unset means no email",
        ),
        (
            "forgekeep.example.toml",
            "smtp",
            "user",
            "a placeholder account name",
        ),
        (
            "forgekeep.example.toml",
            "smtp",
            "pass",
            "a secret to paste",
        ),
        (
            "forgekeep.example.toml",
            "smtp",
            "from",
            "a placeholder sender address",
        ),
        (
            "forgekeep.example.toml",
            "tls",
            "cert",
            "a placeholder path; unset means plain HTTP",
        ),
        (
            "forgekeep.example.toml",
            "tls",
            "key",
            "a placeholder path; unset means plain HTTP",
        ),
        (
            "forgekeep.example.toml",
            "logging",
            "file",
            "a placeholder path; unset means logs go to stdout only",
        ),
        (
            "forgekeep.example.toml",
            "backup",
            "enabled",
            "deliberately the opposite of the code default: an upgrade must not \
             start consuming disk unannounced, but a fresh install should be \
             backed up from the first start. The code's `false` is stated in the \
             section's prose instead, and prose_defaults() holds it to \
             DEFAULT_BACKUP_ENABLED",
        ),
        (
            "forgekeep.example.toml",
            "imports",
            "trusted_origins",
            "the empty list is what unset means, and the commented line below it is \
             an illustration of the shape — neither is a value the code names",
        ),
        (
            "forgekeep.example.toml",
            "observability",
            "otlp_endpoint",
            "an example collector address; unset is what keeps tracing off, so there \
             is no default to state",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "server",
            "repo_root",
            "a path inside the container, mounted from the host `./data` volume",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "server",
            "host_key",
            "a path inside the container: the host key has to live on the volume so \
             client known_hosts entries survive a rebuild",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "server",
            "external_url",
            "no default: left unset, links point at the address the server bound to",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "database",
            "url",
            "a path inside the container, on the mounted volume rather than the \
             image's WORKDIR",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "auth",
            "jwt_secret",
            "an elision, not a value: the secret belongs in deploy/.env, which wins \
             over this file",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "auth",
            "encryption_key",
            "an elision, not a value: unset means the server generates and keeps one",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "auth",
            "key_file",
            "a path inside the container, on the mounted volume",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "rate_limit",
            "trusted_proxies",
            "an illustration of the shape, with the address a default Docker bridge \
             happens to use",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "logging",
            "file",
            "a placeholder path; unset is deliberate here so `docker compose logs` \
             keeps working",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "audit",
            "archive_dir",
            "a path inside the container: the archive has to land on the mounted \
             volume, not in the image layer",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "backup",
            "enabled",
            "deliberately the opposite of the code default, for the reason spelled \
             out on the same key in forgekeep.example.toml",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "backup",
            "dir",
            "a path inside the container: snapshots have to land on the mounted \
             volume, not in the image layer",
        ),
        (
            "deploy/forgekeep.docker.toml",
            "imports",
            "trusted_origins",
            "the empty list is what unset means, and the commented line below it is \
             an illustration of the shape — neither is a value the code names",
        ),
    ];

    /// A commented assignment split into its key and the value as the file
    /// spells it — `# job_secs = 3600` → `("job_secs", "3600")`.
    fn commented_assignment_parts(line: &str) -> Option<(&str, &str)> {
        let body = commented_assignment(line.trim())?;
        let (key, value) = body.split_once('=')?;
        Some((key.trim(), value.trim()))
    }

    /// A *live* assignment split the same way, with any trailing comment cut off
    /// — `max = 0          # 0 = disabled` → `("max", "0")`.
    ///
    /// The cut is only safe outside a quoted string, so a quoted value is taken
    /// up to its closing quote instead: `url = "sqlite://…?mode=rwc"` carries
    /// both an `=` and, in other files, a `#`.
    fn live_assignment_parts(line: &str) -> Option<(&str, &str)> {
        let line = line.trim();
        if line.starts_with('#') {
            return None;
        }
        let key = assignment_key(line)?;
        let value = line.split_once('=')?.1.trim();
        let value = match value.strip_prefix('"') {
            Some(rest) => &value[..rest.find('"')? + 2],
            None => value.split('#').next()?.trim(),
        };
        Some((key, value))
    }

    /// The shipped configs state built-in defaults a third way, and the least
    /// visible of the three: not in `--help`, not in the README table, but on a
    /// `key = value` line an operator reads while deciding what to set. The two
    /// checks that already cover these files ask whether the key is real;
    /// neither looks at the value beside it.
    ///
    /// Both placements drift, differently. A **commented** line parses as
    /// nothing, so `deny_unknown_fields` is silent here by construction: an
    /// operator reads `# job_secs = 3600`, decides not to set it, and never
    /// finds out the server uses another number. A **live** line does reach the
    /// parser — and that is the worse half, because a stale copy does not read
    /// as a disagreement, it silently *overrides* the default on every install
    /// that started from this file, while the code's value applies to everyone
    /// who did not.
    ///
    /// The numbers matter concretely: `[timeouts]` is the answer to "how long
    /// does my push live before it is killed", `max_keys` is the memory bound
    /// under a distinct-IP flood, and `[rate_limit].auth_max` is the throttle on
    /// password guessing.
    #[test]
    fn every_default_the_shipped_configs_offer_is_the_value_the_code_produces() {
        // The readers have to be able to answer "no" before their "yes" means
        // anything: these files' prose is full of `=` signs that are not
        // assignments.
        assert_eq!(
            commented_assignment_parts("# job_secs = 3600"),
            Some(("job_secs", "3600")),
            "the reader does not split a commented assignment into key and value"
        );
        assert_eq!(
            commented_assignment_parts("# service_name = \"forgekeep\""),
            Some(("service_name", "\"forgekeep\"")),
            "the reader strips the quotes TOML spells a string with"
        );
        assert!(
            commented_assignment_parts(
                "# instead of growing the map without limit. 0 = built-in default (100000)."
            )
            .is_none(),
            "the reader mistakes prose containing an `=` for an assignment"
        );
        assert_eq!(
            live_assignment_parts("max = 0          # 0 = disabled (global per-IP limit)"),
            Some(("max", "0")),
            "the live reader keeps the trailing comment as part of the value"
        );
        assert_eq!(
            live_assignment_parts("url = \"sqlite://./forgekeep.db?mode=rwc\""),
            Some(("url", "\"sqlite://./forgekeep.db?mode=rwc\"")),
            "the live reader mis-splits a quoted value that contains an `=`"
        );
        assert_eq!(
            live_assignment_parts("# job_secs = 3600"),
            None,
            "the live reader accepts a commented line as live"
        );

        let pinned = template_defaults();
        let mut checked: BTreeSet<(&str, &str)> = BTreeSet::new();
        let mut excused: BTreeSet<(&str, &str, &str)> = BTreeSet::new();

        for (name, content) in SHIPPED_CONFIGS {
            let mut section = "";

            for (index, line) in content.lines().enumerate() {
                let line = line.trim();
                if let Some((header, _)) = line.strip_prefix('[').and_then(|l| l.split_once(']')) {
                    section = header;
                    continue;
                }
                let commented = commented_assignment_parts(line);
                let Some((key, value)) = commented.or_else(|| live_assignment_parts(line)) else {
                    continue;
                };
                let shown = if commented.is_some() {
                    format!("# {key} = {value}")
                } else {
                    format!("{key} = {value}")
                };

                let excuse = TEMPLATE_VALUES_NOT_DEFAULTS
                    .iter()
                    .find(|&&(file, s, k, _)| file == name && s == section && k == key);
                if excuse.is_some() {
                    excused.insert((name, section, key));
                    continue;
                }

                let entry = pinned
                    .iter()
                    .find(|entry| entry.section == section && entry.key == key)
                    .unwrap_or_else(|| {
                        panic!(
                            "{name}:{}: `{shown}` states a value nothing checks — pair it in \
                             template_defaults() with whatever produces it, or name it in \
                             TEMPLATE_VALUES_NOT_DEFAULTS with the reason it is not the \
                             built-in default",
                            index + 1
                        )
                    });
                assert_eq!(
                    value,
                    entry.value,
                    "{name}:{}: the file states `{shown}`, and `{}` produces `{}`. {}",
                    index + 1,
                    entry.source,
                    entry.value,
                    if commented.is_some() {
                        "An operator who reads this line decides not to set the knob, and \
                         never finds out, because a comment reaches no parser"
                    } else {
                        "This line is live, so every install started from this file gets the \
                         stale value while everyone else gets the code's — the disagreement \
                         is invisible from either side"
                    }
                );
                checked.insert((section, key));
            }
        }

        // A floor, not a count: it fails loudly if either reader stops matching
        // and the test quietly checks nothing.
        assert!(
            checked.len() >= 30,
            "only {} settings pinned across the shipped configs — a reader has stopped \
             matching assignments",
            checked.len()
        );

        // The mirror: a pin no file states any more checks nothing, and an
        // excuse for a line that is gone is a claim about nothing.
        for entry in &pinned {
            assert!(
                checked.contains(&(entry.section, entry.key)),
                "no shipped config states `{}` in [{}] any more, so nothing holds `{}` to \
                 what they say — drop the row or restore the line",
                entry.key,
                entry.section,
                entry.source
            );
        }
        for (file, section, key, _) in TEMPLATE_VALUES_NOT_DEFAULTS {
            assert!(
                excused.contains(&(file, section, key)),
                "TEMPLATE_VALUES_NOT_DEFAULTS still excuses `{key}` in [{section}] of \
                 {file}, which no longer states it — drop the entry so the list keeps \
                 meaning something"
            );
        }
    }

    /// The placeholder `jwt_secret` the templates ship is the one value the
    /// server must refuse to sign with — and the two are written in different
    /// files, with nothing between them.
    ///
    /// Change the template's placeholder alone and `cp forgekeep.example.toml
    /// forgekeep.toml` produces an instance that starts cleanly and signs every
    /// token with a secret published in this repository. The rejection is not a
    /// nicety: `validate_jwt_secret` is what turns "no secret was ever set" into
    /// a failed start.
    #[test]
    fn the_shipped_jwt_placeholder_is_a_secret_the_server_refuses() {
        let mut seen = 0;

        for (name, content) in SHIPPED_CONFIGS {
            for (index, line) in content.lines().enumerate() {
                let Some(("jwt_secret", value)) = live_assignment_parts(line) else {
                    continue;
                };
                let secret = value.trim_matches('"');
                assert!(
                    crate::admin::KNOWN_BAD_JWT_SECRETS.contains(&secret),
                    "{name}:{}: this file ships `jwt_secret = {value}` for an operator to \
                     replace, and validate_jwt_secret() does not refuse it — an install that \
                     copied the file and forgot the step would start, and sign every token \
                     with a secret that is public. Add it to KNOWN_BAD_JWT_SECRETS",
                    index + 1
                );
                seen += 1;
            }
        }

        assert!(
            seen > 0,
            "no shipped config states a live `jwt_secret` any more — either the placeholder \
             moved (drop this test) or the reader stopped matching it"
        );
    }

    /// The `pub(crate) fn default_*` names `source` declares. Reading the
    /// declarations rather than keeping a list beside them is the whole point:
    /// a default added to the model joins the census by existing.
    fn declared_default_functions(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("pub(crate) fn "))
            .filter_map(|rest| rest.split_once('('))
            .map(|(name, _)| name)
            .filter(|name| name.starts_with("default_"))
            .collect()
    }

    /// The `pub(crate) const DEFAULT_*` names `source` declares — the other
    /// half of the census, and the half this file writes most of its defaults
    /// as. Derived the same way and for the same reason: a constant added to
    /// the model joins the census by existing.
    fn declared_default_constants(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("pub(crate) const "))
            .filter_map(|rest| rest.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("DEFAULT_"))
            .collect()
    }

    /// Built-in defaults with no fixed value a shipped config could state, each
    /// with the reason. The list exists so that a new default nobody put in
    /// front of the operator is a decision someone made, rather than something
    /// that quietly escaped the templates.
    const DEFAULTS_NOT_IN_TEMPLATE: [(&str, &str); 2] = [
        (
            "default_host_key_path",
            "derived from $HOME at run time, so there is no one number or path a template \
             line could state; `# host_key = \"/path/to/ssh_host_key\"` shows the shape instead",
        ),
        (
            "DEFAULT_BACKUP_ENABLED",
            "both shipped configs turn backups ON deliberately, against this default, so \
             neither states it as a value; the sentence that does is pinned by \
             prose_defaults()",
        ),
    ];

    /// The mirror of the check above: that one asks that every value the shipped
    /// configs state is right, this asks that every default *has* a line — or is
    /// excused on purpose. Without it the pairing table rots the moment a knob
    /// is added, and the drift these tests exist to catch walks past them.
    #[test]
    fn every_default_function_is_either_shown_in_the_template_or_excused() {
        assert_eq!(
            declared_default_functions(
                "pub(crate) fn default_x() -> u64 {\npub(crate) fn other() -> u64 {\n"
            ),
            BTreeSet::from(["default_x"]),
            "the declaration scan does not read `pub(crate) fn default_*` the way \
             config.rs writes it"
        );
        assert_eq!(
            declared_default_constants(
                "pub(crate) const DEFAULT_X: u64 = 1;\npub(crate) const OTHER: u64 = 2;\n"
            ),
            BTreeSet::from(["DEFAULT_X"]),
            "the declaration scan does not read `pub(crate) const DEFAULT_*` the way \
             config.rs writes it"
        );

        let source = production_config_source();
        let mut declared = declared_default_functions(&source);
        let constants = declared_default_constants(&source);
        assert!(
            declared.len() >= 7 && constants.len() >= 20,
            "only {} `default_*()` functions and {} `DEFAULT_*` constants found in \
             config.rs — a declaration scan has stopped matching them",
            declared.len(),
            constants.len()
        );
        declared.extend(&constants);

        let paired: BTreeSet<&str> = template_defaults()
            .iter()
            .map(|entry| entry.source)
            .collect();

        for name in &declared {
            let excused = DEFAULTS_NOT_IN_TEMPLATE
                .iter()
                .any(|&(excused, _)| excused == *name);
            assert!(
                paired.contains(name) || excused,
                "`{name}` is a built-in default that no row of template_defaults() ties to \
                 a line in a shipped config — an operator who reads one to decide what to \
                 set never learns the knob exists. Pair it, or name it in \
                 DEFAULTS_NOT_IN_TEMPLATE with the reason it has no stateable value"
            );
        }

        for (name, _) in DEFAULTS_NOT_IN_TEMPLATE {
            assert!(
                declared.contains(name),
                "DEFAULTS_NOT_IN_TEMPLATE still excuses `{name}`, which config.rs no longer \
                 declares — drop the entry so the list keeps meaning something"
            );
        }

        // Renaming a paired name breaks the build, but *moving* one out of
        // config.rs would not: it would simply leave the census, taking its row
        // with it. Rows naming a source in another crate (`backup::DEFAULT_…`)
        // are outside this file's census by construction and carry the `::`
        // that says so.
        for name in paired
            .iter()
            .filter(|name| !name.contains("::") && name.starts_with("DEFAULT_"))
            .chain(paired.iter().filter(|name| name.starts_with("default_")))
        {
            assert!(
                declared.contains(name),
                "template_defaults() pairs `{name}`, which config.rs no longer declares — \
                 the census reads that one file, so a default that moved elsewhere escapes \
                 it"
            );
        }
    }

    /// Pages that state built-in defaults in running prose and carry no
    /// ```toml block anyone pastes — so they belong to the prose census below
    /// and to none of the block checks above.
    ///
    /// Kept apart from [`DOCUMENTED_CONFIGS`] on purpose: that list means "a
    /// reader copies configuration out of this page", and every check keyed on
    /// it reads the page as a source of blocks. `docs/FEATURE_INVENTORY.md` is
    /// a different document — the inventory that answers "does this knob exist
    /// and what does it do by default" — and its numbers were the last copies
    /// of two defaults that nothing held to the code.
    const NARRATIVE_DOCS: [(&str, &str); 1] = [(
        "docs/FEATURE_INVENTORY.md",
        include_str!("../../../docs/FEATURE_INVENTORY.md"),
    )];

    /// A built-in default stated in prose rather than as an assignment: the
    /// sentence that tells an operator what happens when they set nothing.
    struct ProseDefault {
        file: &'static str,
        /// The `[section]` the sentence has to sit under, when the same wording
        /// occurs in more than one: `[audit]` and `[backup]` explain their
        /// directory fallback in the same words, differing only in the path.
        section: Option<&'static str>,
        /// The distinctive phrase that carries the claim. Every line holding it
        /// has to state the value, and it has to occur at least once — a lead
        /// that disappeared is a pin that checks nothing.
        lead: &'static str,
        /// The exact spelling the sentence must contain, built from the source.
        /// It carries the surrounding punctuation on purpose: `1.0` is a
        /// substring of `1.05`, and the sampling line already contains `1.0`
        /// twice for reasons that have nothing to do with the default.
        expected: String,
        source: &'static str,
    }

    fn prose_defaults() -> Vec<ProseDefault> {
        fn row(
            file: &'static str,
            lead: &'static str,
            source: &'static str,
            expected: String,
        ) -> ProseDefault {
            ProseDefault {
                file,
                section: None,
                lead,
                expected,
                source,
            }
        }

        fn row_in(
            file: &'static str,
            section: &'static str,
            lead: &'static str,
            source: &'static str,
            expected: String,
        ) -> ProseDefault {
            ProseDefault {
                section: Some(section),
                ..row(file, lead, source, expected)
            }
        }

        vec![
            row(
                "forgekeep.example.toml",
                "0 = built-in default",
                "rg_http::rate_limit::DEFAULT_MAX_KEYS",
                format!("({})", rg_http::rate_limit::DEFAULT_MAX_KEYS),
            ),
            // The one default both templates state *only* in prose, because
            // both deliberately ship the opposite value. Without this row the
            // sentence is the last unchecked copy of it.
            row(
                "forgekeep.example.toml",
                "so an upgrade never starts consuming",
                "DEFAULT_BACKUP_ENABLED",
                format!("Defaults to {} in code", super::DEFAULT_BACKUP_ENABLED),
            ),
            row(
                "forgekeep.example.toml",
                "unlike [backup]",
                "DEFAULT_MIRROR_ENABLED",
                format!("Defaults to {} in code", super::DEFAULT_MIRROR_ENABLED),
            ),
            // Each of these paths is written twice in the same file: once in
            // the sentence explaining when the fallback applies, once on the
            // live line below it. The live line is pinned by
            // template_defaults(); this is the other copy.
            row_in(
                "forgekeep.example.toml",
                "audit",
                "the same volume as the rest of the state; otherwise",
                "DEFAULT_AUDIT_ARCHIVE_DIR",
                format!("`{}`", super::DEFAULT_AUDIT_ARCHIVE_DIR),
            ),
            row_in(
                "forgekeep.example.toml",
                "backup",
                "the same volume as the rest of the state; otherwise",
                "DEFAULT_DB_BACKUP_DIR",
                format!("`{}`", super::DEFAULT_DB_BACKUP_DIR),
            ),
            // The credential limiter, on the page whose reader is deciding
            // whether to leave registration open. The value used to be spelled
            // out as an English word ("ten accounts a minute"), which no source
            // can produce — digits are what makes the claim checkable.
            row(
                "deploy/README.md",
                "throttles that to",
                "DEFAULT_AUTH_RATE_LIMIT_MAX",
                format!("to {} accounts", super::DEFAULT_AUTH_RATE_LIMIT_MAX),
            ),
            row(
                "deploy/README.md",
                "but never refuses",
                "DEFAULT_AUTH_RATE_LIMIT_WINDOW",
                format!("{} seconds", super::DEFAULT_AUTH_RATE_LIMIT_WINDOW),
            ),
            row(
                "forgekeep.example.toml",
                "service.name resource attribute",
                "telemetry::DEFAULT_OTEL_SERVICE_NAME",
                format!(
                    "(default {:?})",
                    crate::telemetry::DEFAULT_OTEL_SERVICE_NAME
                ),
            ),
            row(
                "forgekeep.example.toml",
                "Head sampling ratio",
                "telemetry::DEFAULT_OTEL_SAMPLE_RATIO",
                format!(
                    "(default {:?} ",
                    crate::telemetry::DEFAULT_OTEL_SAMPLE_RATIO
                ),
            ),
            row(
                "README.md",
                "they fall back to",
                "DEFAULT_DB_URL",
                format!("`{}`", super::DEFAULT_DB_URL),
            ),
            row(
                "deploy/README.md",
                "Passing **neither** falls back to",
                "DEFAULT_DB_URL",
                format!("`{}`", super::DEFAULT_DB_URL),
            ),
            row(
                "deploy/README.md",
                "Git-over-SSH listens on",
                "DEFAULT_SSH_ADDR",
                format!("`{}`", super::DEFAULT_SSH_ADDR),
            ),
            // The feature inventory states three of these numbers, in the one
            // register where a number reads least like a value someone has to
            // maintain: a "Notes" cell. The page is read to decide whether a
            // knob needs building at all, so a stale cell is answered with
            // work, not with a config edit.
            row(
                "docs/FEATURE_INVENTORY.md",
                "новый ключ отвергается ДО вставки",
                "rg_http::rate_limit::DEFAULT_MAX_KEYS",
                format!("default {}", rg_http::rate_limit::DEFAULT_MAX_KEYS),
            ),
            // Both halves of the credential limiter, on both rows that state
            // them: `(10/60s)` is one spelling of two constants, so each is
            // pinned to the side of the slash it produces.
            row(
                "docs/FEATURE_INVENTORY.md",
                "always-on per-route rate-limit",
                "DEFAULT_AUTH_RATE_LIMIT_MAX",
                format!("({}/", super::DEFAULT_AUTH_RATE_LIMIT_MAX),
            ),
            row(
                "docs/FEATURE_INVENTORY.md",
                "always-on per-route rate-limit",
                "DEFAULT_AUTH_RATE_LIMIT_WINDOW",
                format!("/{}s", super::DEFAULT_AUTH_RATE_LIMIT_WINDOW),
            ),
            row(
                "docs/FEATURE_INVENTORY.md",
                "всегда включён по умолчанию",
                "DEFAULT_AUTH_RATE_LIMIT_MAX",
                format!("({}/", super::DEFAULT_AUTH_RATE_LIMIT_MAX),
            ),
            row(
                "docs/FEATURE_INVENTORY.md",
                "всегда включён по умолчанию",
                "DEFAULT_AUTH_RATE_LIMIT_WINDOW",
                format!("/{}s", super::DEFAULT_AUTH_RATE_LIMIT_WINDOW),
            ),
        ]
    }

    /// The same drift by its last route: a sentence. The README table and the
    /// help text are checked in `cli.rs`, and the template's assignments above
    /// — but a default also gets stated in running prose, where it looks least
    /// like a value and is copied into a deployment decision just as readily.
    ///
    /// `deploy/README.md` is the page whose reader has no source tree open at
    /// all.
    #[test]
    fn every_default_the_documentation_states_in_prose_is_the_value_the_code_produces() {
        for claim in prose_defaults() {
            let content = SHIPPED_CONFIGS
                .iter()
                .chain(DOCUMENTED_CONFIGS.iter())
                .chain(NARRATIVE_DOCS.iter())
                .find(|(name, _)| *name == claim.file)
                .map(|(_, content)| *content)
                .unwrap_or_else(|| {
                    panic!(
                        "{} is not one of the files these tests include! — add it before \
                         pinning a sentence in it",
                        claim.file
                    )
                });

            let mut seen = 0;
            let mut section = "";
            for (index, line) in content.lines().enumerate() {
                if let Some((header, "")) = line
                    .trim()
                    .strip_prefix('[')
                    .and_then(|rest| rest.split_once(']'))
                {
                    section = header;
                }
                if !line.contains(claim.lead)
                    || claim.section.is_some_and(|wanted| wanted != section)
                {
                    continue;
                }
                seen += 1;
                assert!(
                    line.contains(&claim.expected),
                    "{}:{}: this sentence tells an operator what they get by setting \
                     nothing, and `{}` produces `{}`, which it does not say: {}",
                    claim.file,
                    index + 1,
                    claim.source,
                    claim.expected,
                    line.trim()
                );
            }

            assert!(
                seen > 0,
                "{} no longer contains `{}`, so nothing holds `{}` to what that page says \
                 — drop the row or restore the sentence",
                claim.file,
                claim.lead,
                claim.source
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
        let source = production_config_source();
        let declared: BTreeSet<&str> = nested_config_sections(&source)
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
        let sections = nested_config_sections(&source);
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
            super::write_test_config(&path, format!("[{section}]\n{key} = {value}\n")).unwrap();

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

    #[test]
    fn plaintext_import_origins_are_a_separate_exact_http_policy() {
        let config: ConfigFile = toml::from_str(
            r#"
[imports]
trusted_origins = ["http://127.0.0.1:8443"]
allow_insecure_http_origins = ["http://127.0.0.1:8443"]
"#,
        )
        .expect("parse config");

        let transport = super::resolve_import_transport_policy(Some(&config))
            .expect("parse plaintext import origins");
        transport
            .require_confidential_credentials(
                "http://127.0.0.1:8443/group/project.git",
                Some("source-token"),
            )
            .expect("configured plaintext credential origin");
        assert!(transport
            .require_confidential_credentials(
                "http://127.0.0.1:9443/group/project.git",
                Some("source-token"),
            )
            .is_err());

        let only_private_trust = super::resolve_import_transport_policy(Some(
            &toml::from_str::<ConfigFile>(
                "[imports]\ntrusted_origins = [\"http://127.0.0.1:8443\"]\n",
            )
            .expect("private trust config"),
        ))
        .expect("empty transport policy");
        assert!(only_private_trust
            .require_confidential_credentials(
                "http://127.0.0.1:8443/group/project.git",
                Some("source-token"),
            )
            .is_err());
    }

    #[test]
    fn malformed_plaintext_import_origin_names_its_own_config_key() {
        let config: ConfigFile = toml::from_str(
            r#"
[imports]
allow_insecure_http_origins = ["https://gitlab.internal"]
"#,
        )
        .expect("the TOML shape itself is valid");

        let error = super::resolve_import_transport_policy(Some(&config))
            .expect_err("a HTTPS value grants no plaintext exception");
        assert!(format!("{error:#}").contains("[imports].allow_insecure_http_origins"));
    }

    #[test]
    fn plaintext_oidc_origins_are_a_separate_exact_http_policy() {
        let config: ConfigFile = toml::from_str(
            r#"
[auth]
allow_insecure_oidc_origins = ["http://idp.internal:8080"]
"#,
        )
        .expect("parse config");

        let policy = super::resolve_oidc_transport_policy(Some(&config))
            .expect("parse plaintext OIDC origins");
        policy
            .require_confidential_endpoint("http://idp.internal:8080/token", "token")
            .expect("the exact configured origin is allowed");
        assert!(policy
            .require_confidential_endpoint("http://idp.internal:8081/token", "token")
            .is_err());

        let malformed: ConfigFile =
            toml::from_str("[auth]\nallow_insecure_oidc_origins = [\"https://idp.internal\"]\n")
                .expect("the TOML shape itself is valid");
        let error = super::resolve_oidc_transport_policy(Some(&malformed))
            .expect_err("HTTPS grants no plaintext exception");
        assert!(format!("{error:#}").contains("[auth].allow_insecure_oidc_origins"));
    }

    #[test]
    fn plaintext_ldap_endpoints_are_a_separate_exact_policy() {
        let config: ConfigFile = toml::from_str(
            r#"
[auth]
allow_insecure_ldap_endpoints = ["ldap://directory.internal:1389"]
"#,
        )
        .expect("parse config");

        let policy = super::resolve_ldap_transport_policy(Some(&config))
            .expect("parse plaintext LDAP endpoints");
        policy
            .resolve_endpoint("ldap://directory.internal", Some(1389))
            .expect("the exact configured endpoint is allowed");
        assert!(policy
            .resolve_endpoint("ldap://directory.internal", Some(1390))
            .is_err());

        let malformed: ConfigFile = toml::from_str(
            "[auth]\nallow_insecure_ldap_endpoints = [\"ldaps://directory.internal:636\"]\n",
        )
        .expect("the TOML shape itself is valid");
        let error = super::resolve_ldap_transport_policy(Some(&malformed))
            .expect_err("LDAPS grants no plaintext exception");
        assert!(format!("{error:#}").contains("[auth].allow_insecure_ldap_endpoints"));
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
            err.contains("install -m 600 forgekeep.example.toml forgekeep.toml"),
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
            err.contains("install -m 600 forgekeep.example.toml forgekeep.toml"),
            "no remediation: {err}"
        );
    }

    #[test]
    fn malformed_config_file_names_the_path_it_failed_to_parse() {
        // A TOML syntax error otherwise surfaces as a bare parser message with
        // no clue about *which* file the operator has to fix.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.toml");
        super::write_test_config(&path, "[server\nrepo_root = \"/data/repos\"\n").unwrap();

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
        super::write_test_config(&path, "[rate_limit]\nmax = 0\n").unwrap();

        let config = super::load_config_file(path.to_str().unwrap()).unwrap();
        assert_eq!(config.timeouts.db_connect_secs, 10);
    }

    #[cfg(unix)]
    #[test]
    fn a_group_or_world_readable_config_is_refused_until_it_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forgekeep.toml");
        super::write_test_config(&path, "[rate_limit]\nmax = 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let error = super::load_config_file(path.to_str().unwrap())
            .expect_err("a config readable by other local accounts must be refused");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(path.to_str().unwrap()),
            "no path: {rendered}"
        );
        assert!(
            rendered.contains("mode 0644"),
            "no observed mode: {rendered}"
        );
        assert!(rendered.contains("chmod 600"), "no remediation: {rendered}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        super::load_config_file(path.to_str().unwrap())
            .expect("the same owner-only config must load");
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
        // Read from the sources rather than typed out again: a test that keeps
        // its own copy of `90` is one more place the default has to be changed,
        // and one more place it can be forgotten.
        assert_eq!(config.audit.enabled, Some(super::DEFAULT_AUDIT_ENABLED));
        assert_eq!(
            config.audit.archive_after_days,
            Some(rg_core::audit::archiver::DEFAULT_ARCHIVE_AFTER_DAYS)
        );
        assert_eq!(
            config.audit.interval_minutes,
            Some(rg_core::audit::archiver::DEFAULT_INTERVAL_MINUTES)
        );
        assert_eq!(
            config.audit.batch_size,
            Some(rg_core::audit::archiver::DEFAULT_BATCH_SIZE)
        );
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
        // `enabled` stays a literal `true` on purpose — it is the decision this
        // test exists to hold, and it is deliberately the opposite of
        // DEFAULT_BACKUP_ENABLED. The schedule is not a decision, so it is read
        // from the source instead of copied.
        assert_eq!(example.backup.enabled, Some(true));
        assert_eq!(
            example.backup.interval_hours,
            Some(rg_core::backup::DEFAULT_INTERVAL_HOURS)
        );
        assert_eq!(
            example.backup.keep_last,
            Some(rg_core::backup::DEFAULT_KEEP_LAST)
        );

        let docker: ConfigFile =
            toml::from_str(include_str!("../../../deploy/forgekeep.docker.toml")).unwrap();
        assert_eq!(docker.backup.enabled, Some(true));
        assert_eq!(docker.backup.dir.as_deref(), Some("/data/backups"));
    }
}
