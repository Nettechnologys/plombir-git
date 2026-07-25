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
pub(crate) struct ServerConfig {
    pub(crate) repo_root: Option<String>,
    pub(crate) http_addr: Option<String>,
    pub(crate) ssh_addr: Option<String>,
    pub(crate) host_key: Option<String>,
    /// External-facing URL for SSO callbacks and links (e.g., "https://git.example.com")
    pub(crate) external_url: Option<String>,
    /// Grace window (seconds) for draining in-flight requests and the CI-log
    /// queue on SIGTERM/ctrl_c before the process is forced down (default: 30).
    pub(crate) shutdown_grace_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub(crate) struct DatabaseConfig {
    pub(crate) url: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[allow(dead_code)]
pub(crate) struct AuthConfig {
    pub(crate) jwt_secret: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
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
#[allow(dead_code)]
pub(crate) struct ReleasesConfig {
    /// Enable opt-in Ed25519 provenance attestation of release assets (default
    /// false). Also settable via `FORGEKEEP_ATTESTATION_ENABLED=1`, which wins.
    #[serde(default)]
    pub(crate) attestation_enabled: Option<bool>,
}

#[derive(Debug, serde::Deserialize, Default)]
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
pub(crate) struct SmtpConfig {
    pub(crate) host: Option<String>,
    pub(crate) port: Option<u16>,
    pub(crate) user: Option<String>,
    pub(crate) pass: Option<String>,
    pub(crate) from: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub(crate) struct TlsConfig {
    pub(crate) cert: Option<String>,
    pub(crate) key: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub(crate) struct LoggingConfig {
    pub(crate) file: Option<String>,
    pub(crate) max_size_mb: Option<u64>,
    pub(crate) max_files: Option<usize>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub(crate) struct AuditConfig {
    pub(crate) enabled: Option<bool>,
    pub(crate) archive_dir: Option<String>,
    pub(crate) archive_after_days: Option<i64>,
    pub(crate) interval_minutes: Option<u64>,
    pub(crate) batch_size: Option<u64>,
}

/// `[observability]` — OpenTelemetry distributed-tracing (OTLP) export. All
/// fields optional; with no endpoint set (here or via the `OTEL_EXPORTER_OTLP_*`
/// env vars) OTel tracing stays off and only Prometheus `/metrics` + logs run.
#[derive(Debug, serde::Deserialize, Default)]
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
#[allow(dead_code)]
pub(crate) struct WebhooksConfig {
    /// Shared secret for verifying HMAC-SHA256 signatures on *inbound* external
    /// webhooks (`/webhooks/external/*`). Unset = signature checking disabled
    /// (endpoints rely on JWT/PAT auth alone). Also settable via the
    /// `FORGEKEEP_EXTERNAL_WEBHOOK_SECRET` environment variable, which wins.
    pub(crate) external_secret: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
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

fn default_job_timeout() -> u64 {
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

pub(crate) fn load_config_file(path: &str) -> anyhow::Result<ConfigFile> {
    ensure_regular_file(std::path::Path::new(path), "config file", CONFIG_FILE_HINT)?;
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read config file `{path}`"))?;
    let config: ConfigFile = toml::from_str(&content)
        .with_context(|| format!("failed to parse config file `{path}` as TOML"))?;
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
}
