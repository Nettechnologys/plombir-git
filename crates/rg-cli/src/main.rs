//! IronForge CLI — main entry point.
use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Subcommand)]
enum PackageCmd {
    /// Publish a package file
    Publish {
        /// Package type: cargo, npm, generic, etc.
        pkg_type: String,

        /// Package name
        name: String,

        /// Version string
        version: String,

        /// Path to the package file to upload
        file: String,

        /// Owner of the target repository
        #[arg(long)]
        owner: String,

        /// Repository name
        #[arg(long)]
        repo: String,

        /// Access token for authentication (skips JWT auth)
        #[arg(long)]
        token: Option<String>,

        /// IronForge server URL (for token-based auth)
        #[arg(long, default_value = "http://localhost:8080")]
        server_url: String,
    },

    /// List packages in a repository registry
    List {
        /// Owner of the repository
        owner: String,

        /// Repository name
        repo: String,

        /// Package type to list
        pkg_type: String,

        /// Database URL for direct DB access (SQLite, PostgreSQL, or MySQL)
        #[arg(long, default_value = "sqlite://./ironforge.db?mode=rwc")]
        db_url: String,
    },
}

#[derive(Parser)]
#[command(name = "ironforge", about = "A Git hosting platform written in Rust")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
enum Commands {
    /// Start the IronForge server
    Serve {
        /// Root directory for git repositories
        #[arg(long, default_value = "./repos")]
        repo_root: String,

        /// HTTP listen address
        #[arg(long, default_value = "0.0.0.0:8080")]
        http_addr: String,

        /// SSH listen address
        #[arg(long, default_value = "0.0.0.0:2222")]
        ssh_addr: String,

        /// Path to SSH host key
        #[arg(long)]
        host_key: Option<String>,

        /// Database URL (sqlite://, postgres://, or mysql://)
        #[arg(long, default_value = "sqlite://./ironforge.db?mode=rwc")]
        db_url: String,

        /// JWT secret key (use a long random string in production)
        #[arg(long)]
        jwt_secret: Option<String>,

        /// Enable Docker runner for CI jobs with `image` field
        #[arg(long, default_value_t = false)]
        docker: bool,

        /// Use external runners instead of embedded runner for CI jobs
        #[arg(long, default_value_t = false)]
        external_runners: bool,

        /// Rate limit: max requests per window per IP (0 = disabled)
        #[arg(long, default_value_t = 0)]
        rate_limit_max: u32,

        /// Rate limit: window duration in seconds
        #[arg(long, default_value_t = 60)]
        rate_limit_window: u64,

        /// Comma-separated proxy IPs whose X-Forwarded-For / X-Real-IP headers are trusted
        #[arg(long, value_delimiter = ',')]
        rate_limit_trusted_proxies: Vec<String>,

        /// SMTP server host (enables email notifications)
        #[arg(long)]
        smtp_host: Option<String>,

        /// SMTP server port
        #[arg(long, default_value_t = 587)]
        smtp_port: u16,

        /// SMTP username
        #[arg(long)]
        smtp_user: Option<String>,

        /// SMTP password
        #[arg(long)]
        smtp_pass: Option<String>,

        /// SMTP from email address
        #[arg(long)]
        smtp_from: Option<String>,

        /// Path to TLS certificate file (PEM format, enables HTTPS)
        #[arg(long)]
        tls_cert: Option<String>,

        /// Path to TLS private key file (PEM format)
        #[arg(long)]
        tls_key: Option<String>,

        /// Path to TOML configuration file (overrides CLI defaults)
        #[arg(long)]
        config: Option<String>,

        /// Log file path (enables file logging with rotation). If not set, logs to stderr only.
        #[arg(long)]
        log_file: Option<String>,

        /// Log rotation: nominal max log file size in MB. NOTE: the file
        /// appender rotates daily, not by size — this value is advisory only.
        #[arg(long, default_value_t = 10)]
        log_max_size_mb: u64,

        /// Log rotation: max number of old log files to keep (default: 5)
        #[arg(long, default_value_t = 5)]
        log_max_files: usize,
    },

    /// Run database migrations and exit
    Migrate {
        /// Database URL (sqlite://, postgres://, or mysql://)
        #[arg(long, default_value = "sqlite://./ironforge.db?mode=rwc")]
        db_url: String,
    },

    /// Rebuild or refresh full-text search indexes from main tables
    RebuildFts {
        /// Database URL (sqlite://, postgres://, or mysql://)
        #[arg(long, default_value = "sqlite://./ironforge.db?mode=rwc")]
        db_url: String,
    },

    /// Create a consistent SQLite database backup.
    BackupDb {
        /// SQLite database URL (e.g. sqlite://./ironforge.db?mode=rwc)
        #[arg(long, default_value = "sqlite://./ironforge.db?mode=rwc")]
        db_url: String,

        /// Output backup file path.
        output: String,

        /// Overwrite output if it already exists.
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Restore a SQLite database file from a backup.
    RestoreDb {
        /// SQLite database URL to restore into.
        #[arg(long, default_value = "sqlite://./ironforge.db?mode=rwc")]
        db_url: String,

        /// Backup file path to restore from.
        input: String,

        /// Overwrite existing target DB and sidecar WAL/SHM files.
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Create a new bare repository (no DB record — for quick testing)
    CreateRepo {
        /// Owner username
        owner: String,

        /// Repository name (without .git suffix)
        name: String,

        /// Root directory for repositories
        #[arg(long, default_value = "./repos")]
        repo_root: String,
    },

    /// Run as a CI Runner — polls jobs and executes them
    Runner {
        /// IronForge server URL (e.g. http://127.0.0.1:8080)
        #[arg(long, default_value = "http://127.0.0.1:8080")]
        server: String,

        /// Runner name (used for registration if not already registered)
        #[arg(long)]
        name: Option<String>,

        /// Existing runner ID (skip registration)
        #[arg(long)]
        runner_id: Option<i64>,

        /// Existing runner token (skip registration)
        #[arg(long)]
        token: Option<String>,

        /// Admin user JWT used only when this command needs to register a runner
        #[arg(long)]
        auth_token: Option<String>,
    },

    /// Import a repository from GitHub or GitLab
    Import {
        /// Source platform: "github" or "gitlab"
        #[arg(value_parser = ["github", "gitlab"])]
        platform: String,

        /// Source repository URL (e.g., https://github.com/user/repo)
        source_url: String,

        /// Target owner in IronForge
        #[arg(long)]
        target_owner: String,

        /// Target repository name (defaults to source repo name)
        #[arg(long)]
        target_name: Option<String>,

        /// API access token for the source platform
        #[arg(long)]
        token: Option<String>,

        /// Root directory for repositories
        #[arg(long, default_value = "./repos")]
        repo_root: String,

        /// Database URL (sqlite://, postgres://, or mysql://)
        #[arg(long, default_value = "sqlite://./ironforge.db?mode=rwc")]
        db_url: String,

        /// Skip importing the repository itself
        #[arg(long)]
        skip_repo: bool,

        /// Skip importing issues
        #[arg(long)]
        skip_issues: bool,

        /// Skip importing pull/merge requests
        #[arg(long)]
        skip_prs: bool,

        /// Skip importing labels
        #[arg(long)]
        skip_labels: bool,

        /// Skip importing milestones
        #[arg(long)]
        skip_milestones: bool,

        /// Skip importing releases
        #[arg(long)]
        skip_releases: bool,

        /// Also import wiki pages
        #[arg(long)]
        import_wiki: bool,
    },

    /// Index a repository for code search
    IndexRepo {
        /// Repository to index, in the format "owner/name"
        repo_slug: String,

        /// Root directory for repositories
        #[arg(long, default_value = "./repos")]
        repo_root: String,

        /// Database URL (sqlite://, postgres://, or mysql://)
        #[arg(long, default_value = "sqlite://./ironforge.db?mode=rwc")]
        db_url: String,

        /// Git ref to index (default: repository's default branch)
        #[arg(long)]
        ref_name: Option<String>,
    },

    /// Manage package registry
    Package {
        #[command(subcommand)]
        cmd: PackageCmd,
    },
}

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

#[cfg(test)]
mod config_tests {
    use super::ConfigFile;

    #[test]
    fn example_config_includes_valid_audit_archive_settings() {
        let config: ConfigFile =
            toml::from_str(include_str!("../../../ironforge.example.toml")).unwrap();
        assert_eq!(config.audit.enabled, Some(true));
        assert_eq!(config.audit.archive_after_days, Some(90));
        assert_eq!(config.audit.interval_minutes, Some(60));
        assert_eq!(config.audit.batch_size, Some(1_000));
    }
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

async fn backup_sqlite_db(db_url: &str, output: &PathBuf, force: bool) -> anyhow::Result<()> {
    if !db_url.starts_with("sqlite:") {
        anyhow::bail!(
            "database backup is only supported for the SQLite backend in this version; \
             use your PostgreSQL/MySQL server's native dump tool (pg_dump / mysqldump) for other backends"
        );
    }
    if output.exists() && !force {
        anyhow::bail!(
            "backup output already exists: {} (use --force to overwrite)",
            output.display()
        );
    }
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create backup directory: {}", parent.display())
            })?;
        }
    }
    if output.exists() {
        std::fs::remove_file(output)
            .with_context(|| format!("failed to remove existing backup: {}", output.display()))?;
    }

    tracing::info!(db_url = %rg_db::redact_database_url(db_url), output = %output.display(), "Creating SQLite backup");
    let db = rg_db::connect(db_url).await?;
    let output_str = output
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("backup output path is not valid UTF-8"))?;

    use rg_db::sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "VACUUM INTO ?",
        [output_str.into()],
    ))
    .await
    .context("SQLite VACUUM INTO backup failed")?;

    tracing::info!(output = %output.display(), "SQLite backup complete");
    println!("Backup written to {}", output.display());
    Ok(())
}

fn restore_sqlite_db(db_url: &str, input: &PathBuf, force: bool) -> anyhow::Result<()> {
    if !input.exists() {
        anyhow::bail!("backup input does not exist: {}", input.display());
    }
    if !input.is_file() {
        anyhow::bail!("backup input is not a file: {}", input.display());
    }

    let target = sqlite_db_path_from_url(db_url)?;
    if target.exists() && !force {
        anyhow::bail!(
            "target database already exists: {} (stop IronForge and use --force to overwrite)",
            target.display()
        );
    }
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("failed to create database directory: {}", parent.display())
            })?;
        }
    }

    if force {
        remove_sqlite_sidecar_files(&target)?;
    }
    std::fs::copy(input, &target).with_context(|| {
        format!(
            "failed to restore backup {} to {}",
            input.display(),
            target.display()
        )
    })?;

    tracing::info!(input = %input.display(), target = %target.display(), "SQLite restore complete");
    println!("Restored {} to {}", input.display(), target.display());
    Ok(())
}

fn remove_sqlite_sidecar_files(db_path: &PathBuf) -> anyhow::Result<()> {
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{}", db_path.display(), suffix));
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
        }
    }
    Ok(())
}

fn sqlite_db_path_from_url(db_url: &str) -> anyhow::Result<PathBuf> {
    let rest = db_url
        .strip_prefix("sqlite://")
        .ok_or_else(|| anyhow::anyhow!("only sqlite:// database URLs are supported"))?;
    let path_part = rest.split_once('?').map(|(path, _)| path).unwrap_or(rest);
    if path_part.is_empty() || path_part == ":memory:" || path_part == "/:memory:" {
        anyhow::bail!("restore requires a file-backed SQLite database URL");
    }
    Ok(PathBuf::from(path_part))
}

/// Validate JWT secret strength.
/// Common function used for CLI arg, env var, and config file values.
fn validate_jwt_secret(jwt_secret: &str, source: &str) -> anyhow::Result<()> {
    if jwt_secret == "change-me-in-production" {
        tracing::error!(
            "FATAL: jwt_secret is set to the default value from {}. \
             Set a strong secret via IRONFORGE_JWT_SECRET, --jwt-secret, or config file [auth].jwt_secret",
            source
        );
        anyhow::bail!("refusing to start with default jwt_secret");
    }
    if jwt_secret.len() < 16 {
        tracing::warn!(
            jwt_len = jwt_secret.len(),
            "jwt_secret from {} is shorter than 16 characters — consider using a stronger secret",
            source
        );
    }
    Ok(())
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse CLI args first (without initializing logging, to avoid early output)
    let cli = Cli::parse();

    match cli.command {
        Commands::Serve {
            repo_root,
            http_addr,
            ssh_addr,
            host_key,
            db_url,
            jwt_secret,
            docker,
            external_runners,
            rate_limit_max,
            rate_limit_window,
            rate_limit_trusted_proxies,
            smtp_host,
            smtp_port,
            smtp_user,
            smtp_pass,
            smtp_from,
            tls_cert,
            tls_key,
            config,
            log_file,
            log_max_size_mb,
            log_max_files,
        } => {
            run_serve(
                repo_root,
                http_addr,
                ssh_addr,
                host_key,
                db_url,
                jwt_secret,
                docker,
                external_runners,
                rate_limit_max,
                rate_limit_window,
                rate_limit_trusted_proxies,
                smtp_host,
                smtp_port,
                smtp_user,
                smtp_pass,
                smtp_from,
                tls_cert,
                tls_key,
                config,
                log_file,
                log_max_size_mb,
                log_max_files,
            )
            .await?;
        }

        Commands::Migrate { db_url } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_target(false)
                .init();

            tracing::info!(
                "Connecting to database: {}",
                rg_db::redact_database_url(&db_url)
            );
            let db = rg_db::connect(&db_url).await?;
            tracing::info!("Running database migrations...");
            rg_db::run_migrations(&db).await?;
            tracing::info!("Migrations complete ✅");
        }

        Commands::RebuildFts { db_url } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_target(false)
                .init();

            tracing::info!(
                "Connecting to database: {}",
                rg_db::redact_database_url(&db_url)
            );
            let db = rg_db::connect(&db_url).await?;

            rg_db::rebuild_fts_indexes(&db).await?;

            tracing::info!("Full-text search indexes refreshed successfully ✅");
        }

        Commands::BackupDb {
            db_url,
            output,
            force,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_target(false)
                .init();

            backup_sqlite_db(&db_url, &PathBuf::from(output), force).await?;
        }

        Commands::RestoreDb {
            db_url,
            input,
            force,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_target(false)
                .init();

            restore_sqlite_db(&db_url, &PathBuf::from(input), force)?;
        }

        Commands::CreateRepo {
            owner,
            name,
            repo_root,
        } => {
            // Simple logging for create-repo command
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_target(false)
                .init();

            let repo_root = PathBuf::from(&repo_root);
            let repo_dir = repo_root.join(format!("{}/{}.git", owner, name));
            std::fs::create_dir_all(&repo_dir)?;

            // Replace git init --bare with gix API
            gix::create::into(
                &repo_dir,
                gix::create::Kind::Bare,
                gix::create::Options::default(),
            )
            .with_context(|| "failed to create bare repository")?;

            println!("Created repository: {}/{}.git", owner, name);
        }

        Commands::Runner {
            server,
            name,
            runner_id,
            token,
            auth_token,
        } => {
            use reqwest::header;

            let client = reqwest::Client::new();

            // ── Register or use existing credentials ─────────
            let (runner_id, token) = match (runner_id, token) {
                (Some(rid), Some(tok)) => (rid, tok),
                _ => {
                    // Register new runner
                    let name = name.as_deref().unwrap_or("default-runner");
                    let auth_token = auth_token
                        .or_else(|| std::env::var("IRONFORGE_AUTH_TOKEN").ok())
                        .context(
                            "runner auto-registration requires --auth-token or \
                             IRONFORGE_AUTH_TOKEN; alternatively pass --runner-id and --token",
                        )?;
                    let resp: serde_json::Value = client
                        .post(format!("{}/api/v1/runners/register", server))
                        .bearer_auth(auth_token)
                        .json(&serde_json::json!({"name": name}))
                        .send()
                        .await
                        .context("failed to register runner")?
                        .json()
                        .await?;
                    let rid = resp["id"].as_i64().ok_or_else(|| {
                        anyhow::anyhow!("invalid register response: missing 'id'")
                    })?;
                    let tok = resp["token"]
                        .as_str()
                        .ok_or_else(|| {
                            anyhow::anyhow!("invalid register response: missing 'token'")
                        })?
                        .to_string();
                    eprintln!("Runner registered: id={}, token={}", rid, tok);
                    eprintln!("Save these credentials for future runs!");
                    (rid, tok)
                }
            };

            eprintln!("Runner started: server={}, id={}", server, runner_id);

            // ── Main poll loop ─────────────────────────────
            let auth_header = format!("Bearer {}", token);
            loop {
                // 1. Poll for job
                let poll_resp = client
                    .get(format!(
                        "{}/api/v1/runners/{}/jobs/poll?timeout=30",
                        server, runner_id
                    ))
                    .header(header::AUTHORIZATION, &auth_header)
                    .send()
                    .await;

                let job: serde_json::Value = match poll_resp {
                    Ok(r) if r.status() == reqwest::StatusCode::NO_CONTENT => {
                        continue;
                    }
                    Ok(r) => r.json().await?,
                    Err(e) => {
                        eprintln!("Poll error: {}, retrying in 5s", e);
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        continue;
                    }
                };

                let job_id = job["job_id"]
                    .as_i64()
                    .ok_or_else(|| anyhow::anyhow!("invalid poll response"))?;
                let script: Vec<&str> = job["script"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                    .unwrap_or_default();
                let image = job["image"].as_str();

                eprintln!("Got job {}: {}", job_id, job["name"].as_str().unwrap_or(""));

                // 2. Start job
                let _ = client
                    .post(format!(
                        "{}/api/v1/runners/{}/jobs/{}/start",
                        server, runner_id, job_id
                    ))
                    .header(header::AUTHORIZATION, &auth_header)
                    .send()
                    .await;

                // 3. Execute job
                let script_str = script.join("\n");
                let (exit_code, log) = if let Some(img) = image {
                    run_job_docker(img, &script_str).await
                } else {
                    run_job_local(&script_str).await
                };

                // 4. Upload log
                let _ = client
                    .post(format!(
                        "{}/api/v1/runners/{}/jobs/{}/log",
                        server, runner_id, job_id
                    ))
                    .header(header::AUTHORIZATION, &auth_header)
                    .body(log.clone())
                    .send()
                    .await;

                // 5. Finish job
                let status = if exit_code == 0 { "success" } else { "failure" };
                let _ = client
                    .post(format!(
                        "{}/api/v1/runners/{}/jobs/{}/finish",
                        server, runner_id, job_id
                    ))
                    .header(header::AUTHORIZATION, &auth_header)
                    .json(&serde_json::json!({"status": status, "exit_code": exit_code}))
                    .send()
                    .await;

                eprintln!(
                    "Job {} finished: status={}, exit_code={}",
                    job_id, status, exit_code
                );
            }
        }

        Commands::Import {
            platform,
            source_url,
            target_owner,
            target_name,
            token,
            repo_root,
            db_url,
            skip_repo,
            skip_issues,
            skip_prs,
            skip_labels,
            skip_milestones,
            skip_releases,
            import_wiki,
        } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_target(false)
                .init();

            // Resolve target name from source URL if not provided
            let target_repo_name = match target_name {
                Some(n) => n,
                None => {
                    let url = source_url.trim_end_matches('/').trim_end_matches(".git");
                    url.split('/')
                        .next_back()
                        .unwrap_or("imported-repo")
                        .to_string()
                }
            };

            println!("╔══════════════════════════════════════════════════╗");
            println!(
                "║  IronForge Import — {} → IronForge",
                platform.to_uppercase()
            );
            println!("╠══════════════════════════════════════════════════╣");
            println!("║  Source:     {}", source_url);
            println!("║  Target:     {}/{}", target_owner, target_repo_name);
            println!(
                "║  Token:      {}",
                if token.is_some() {
                    "provided"
                } else {
                    "not provided"
                }
            );
            println!(
                "║  Import:     {} {} {} {} {} {}",
                if !skip_repo { "📦repo" } else { "" },
                if !skip_labels { "🏷️labels" } else { "" },
                if !skip_milestones {
                    "🎯milestones"
                } else {
                    ""
                },
                if !skip_issues { "📝issues" } else { "" },
                if !skip_prs { "🔄PRs" } else { "" },
                if !skip_releases { "🚀releases" } else { "" },
            );
            if import_wiki {
                println!("║             📚wiki");
            }
            println!("╚══════════════════════════════════════════════════╝");

            tracing::info!(
                "Connecting to database: {}",
                rg_db::redact_database_url(&db_url)
            );
            let db = rg_db::connect(&db_url).await?;
            rg_db::run_migrations(&db).await?;

            // Verify platform is valid
            if platform != "github" && platform != "gitlab" {
                anyhow::bail!(
                    "unsupported platform: {}. Use 'github' or 'gitlab'.",
                    platform
                );
            }

            let repo_root = PathBuf::from(&repo_root);
            std::fs::create_dir_all(&repo_root)?;

            // Start import
            println!("\n⏳ Starting import...");
            let task = rg_core::import::service::start_import(
                &db,
                1, // user_id — in CLI mode, default to admin (ID 1)
                platform,
                source_url,
                target_owner.clone(),
                target_repo_name,
                token,
                !skip_repo,
                !skip_issues,
                !skip_prs,
                import_wiki,
                !skip_releases,
                !skip_labels,
                !skip_milestones,
                &repo_root,
            )
            .await?;

            println!("Import task created: id={}", task.id);
            println!("Polling for completion...");

            // Poll until complete
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let current = rg_db::ops::import_task_ops::find_by_id(&db, task.id)
                    .await?
                    .ok_or_else(|| anyhow::anyhow!("import task disappeared"))?;

                match current.status.as_str() {
                    "pending" | "cloning" | "importing" => {
                        let stage = current.stage.as_deref().unwrap_or("...");
                        println!("  [{}%] {}", current.progress, stage);
                    }
                    "completed" => {
                        println!("\n✅ Import completed successfully!");
                        if let Some(ref stats) = current.stats {
                            if let Ok(s) = serde_json::from_str::<serde_json::Value>(stats) {
                                println!(
                                    "   Stats: {}",
                                    serde_json::to_string_pretty(&s).unwrap_or_default()
                                );
                            }
                        }
                        break;
                    }
                    "failed" => {
                        let err = current.error.as_deref().unwrap_or("unknown error");
                        anyhow::bail!("Import failed: {err}");
                    }
                    _ => {
                        tracing::warn!("Unknown import status: {}", current.status);
                    }
                }
            }
        }

        Commands::Package { cmd } => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_target(false)
                .init();

            match cmd {
                PackageCmd::Publish {
                    pkg_type,
                    name,
                    version,
                    file,
                    owner,
                    repo,
                    token,
                    server_url,
                } => {
                    if !rg_core::package_registry::package_types::is_valid(&pkg_type) {
                        anyhow::bail!(
                            "Unsupported package type: {}. Supported: {}",
                            pkg_type,
                            rg_core::package_registry::package_types::ALL.join(", ")
                        );
                    }

                    let file_data = tokio::fs::read(&file)
                        .await
                        .context(format!("Failed to read file: {}", file))?;
                    let filename = std::path::Path::new(&file)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("package")
                        .to_string();

                    // Use HTTP API via token if provided, otherwise direct DB
                    if let Some(bearer) = token {
                        let client = reqwest::Client::new();
                        let url = format!(
                            "{}/api/v1/repos/{}/{}/packages/{}/publish?name={}&version={}",
                            server_url.trim_end_matches('/'),
                            owner,
                            repo,
                            pkg_type,
                            name,
                            version
                        );
                        let resp = client
                            .post(&url)
                            .header("Authorization", format!("Bearer {}", bearer))
                            .header(
                                "Content-Disposition",
                                format!("attachment; filename=\"{}\"", filename),
                            )
                            .body(file_data)
                            .send()
                            .await?;

                        let status = resp.status();
                        let body = resp.text().await?;
                        if status.is_success() {
                            println!("✅ Package published: {}/{}@{}", name, name, version);
                            println!("   {}", body);
                        } else {
                            anyhow::bail!("Publish failed ({}): {}", status, body);
                        }
                    } else {
                        // Direct DB access (requires --db-url)
                        // For direct DB: need DB access — but this command doesn't have db_url
                        anyhow::bail!(
                            "Direct DB publish requires a running server. Please use --token to authenticate via HTTP API."
                        );
                    }
                }

                PackageCmd::List {
                    owner,
                    repo,
                    pkg_type,
                    db_url,
                } => {
                    if !rg_core::package_registry::package_types::is_valid(&pkg_type) {
                        anyhow::bail!(
                            "Unsupported package type: {}. Supported: {}",
                            pkg_type,
                            rg_core::package_registry::package_types::ALL.join(", ")
                        );
                    }

                    tracing::info!(
                        "Connecting to database: {}",
                        rg_db::redact_database_url(&db_url)
                    );
                    let db = rg_db::connect(&db_url).await?;
                    rg_db::run_migrations(&db).await?;

                    match rg_core::package_registry::service::list_packages(
                        &db, &owner, &repo, &pkg_type,
                    )
                    .await
                    {
                        Ok(packages) => {
                            println!("Packages ({}) in {}/{}:", pkg_type, owner, repo);
                            for pkg in &packages {
                                println!(
                                    "  {} ({} versions, {} downloads)",
                                    pkg.name, pkg.version_count, pkg.download_count
                                );
                                if let Some(ref desc) = pkg.description {
                                    println!("    {}", desc);
                                }
                                if let Some(ref ver) = pkg.latest_version {
                                    println!("    latest: {}", ver);
                                }
                            }
                            if packages.is_empty() {
                                println!("  (none)");
                            }
                        }
                        Err(e) => anyhow::bail!("Failed to list packages: {e:#}"),
                    }
                }
            }
        }

        Commands::IndexRepo {
            repo_slug,
            repo_root,
            db_url,
            ref_name,
        } => {
            // Simple logging for index-repo command
            tracing_subscriber::fmt()
                .with_env_filter(
                    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
                )
                .with_target(false)
                .init();

            // Parse owner/name from repo_slug
            let parts: Vec<&str> = repo_slug.splitn(2, '/').collect();
            if parts.len() != 2 {
                anyhow::bail!("Invalid repo slug format. Expected: owner/name");
            }
            let owner_username = parts[0];
            let repo_name = parts[1];

            tracing::info!(
                "Connecting to database: {}",
                rg_db::redact_database_url(&db_url)
            );
            let db = rg_db::connect(&db_url).await?;

            // Find owner by username
            let owner = rg_db::ops::user_ops::find_by_username(&db, owner_username)
                .await
                .context("Failed to find owner")?
                .ok_or_else(|| anyhow::anyhow!("User not found: {}", owner_username))?;

            // Find repository by owner_id and name
            let repo = rg_db::ops::repo_ops::find_by_owner_and_name(&db, owner.id, repo_name)
                .await
                .context("Failed to find repository")?
                .ok_or_else(|| {
                    anyhow::anyhow!("Repository not found: {}/{}", owner_username, repo_name)
                })?;

            tracing::info!(
                repo_id = repo.id,
                repo_name = %repo.name,
                default_branch = %repo.default_branch,
                "Found repository"
            );

            // Determine ref to index
            let ref_to_index = ref_name.as_deref().unwrap_or(&repo.default_branch);

            // Construct repo path
            let repo_path = std::path::Path::new(&repo_root)
                .join(format!("{}/{}.git", owner_username, repo_name));

            if !repo_path.exists() {
                anyhow::bail!("Repository path does not exist: {}", repo_path.display());
            }

            tracing::info!(
                repo_path = %repo_path.display(),
                ref_name = %ref_to_index,
                "Indexing repository"
            );

            // Create indexer and index repository
            let indexer = rg_core::search::code_indexer::CodeIndexer::new(db.clone());
            let start_time = std::time::Instant::now();
            let indexed_count = indexer
                .index_repository(repo.id, &repo_path, ref_to_index)
                .await
                .context("Failed to index repository")?;
            let elapsed = start_time.elapsed();

            println!(
                "✅ Indexed {} files in {:.2}s",
                indexed_count,
                elapsed.as_secs_f64()
            );
            tracing::info!(
                indexed_count = indexed_count,
                elapsed_ms = elapsed.as_millis(),
                "Repository indexing complete"
            );
        }
    }

    Ok(())
}

/// Initialise and run the IronForge server (HTTP + SSH).
#[allow(clippy::too_many_arguments)]
async fn run_serve(
    repo_root: String,
    http_addr: String,
    ssh_addr: String,
    host_key: Option<String>,
    db_url: String,
    jwt_secret: Option<String>,
    docker: bool,
    external_runners: bool,
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
    let resolved_jwt_secret = if let Ok(env_secret) = std::env::var("IRONFORGE_JWT_SECRET") {
        validate_jwt_secret(&env_secret, "environment variable IRONFORGE_JWT_SECRET")?;
        tracing::info!("Using JWT secret from environment variable IRONFORGE_JWT_SECRET");
        env_secret
    } else if let Some(cli_secret) = jwt_secret {
        validate_jwt_secret(&cli_secret, "--jwt-secret CLI argument")?;
        cli_secret
    } else if let Some(cfg_secret) = cfg.as_ref().and_then(|c| c.auth.jwt_secret.clone()) {
        validate_jwt_secret(&cfg_secret, "config file [auth].jwt_secret")?;
        cfg_secret
    } else {
        anyhow::bail!(
            "No JWT secret provided. Set IRONFORGE_JWT_SECRET, use --jwt-secret, or configure [auth].jwt_secret in config file"
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
            .unwrap_or("ironforge");
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

    tracing::info!("IronForge server started (Phase 20)");

    if let Err(e) = http_handle.await {
        tracing::error!("HTTP server task terminated: {:#}", e);
    }

    Ok(())
}

/// Execute a job script locally via platform-appropriate shell.
async fn run_job_local(script: &str) -> (i32, String) {
    #[cfg(unix)]
    let output = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .output()
        .await;

    #[cfg(windows)]
    let output = tokio::process::Command::new("powershell.exe")
        .args(&["-NoProfile", "-NonInteractive", "-Command", script])
        .output()
        .await;

    match output {
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let mut log = String::new();
            if !o.stdout.is_empty() {
                log.push_str(&String::from_utf8_lossy(&o.stdout));
            }
            if !o.stderr.is_empty() {
                if !log.is_empty() {
                    log.push('\n');
                }
                log.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            (code, log)
        }
        Err(e) => (-1, format!("Failed to spawn job: {}", e)),
    }
}

/// Execute a job script inside a Docker container.
async fn run_job_docker(image: &str, script: &str) -> (i32, String) {
    // Check if Docker is available
    let docker_check = tokio::process::Command::new("docker")
        .arg("info")
        .output()
        .await;

    match docker_check {
        Ok(check) if !check.status.success() => {
            return run_job_local(script).await;
        }
        Err(_) => {
            return run_job_local(script).await;
        }
        _ => {}
    }

    let output = tokio::process::Command::new("docker")
        .args(["run", "--rm", image, "sh", "-c", script])
        .output()
        .await;

    match output {
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let mut log = String::new();
            if !o.stdout.is_empty() {
                log.push_str(&String::from_utf8_lossy(&o.stdout));
            }
            if !o.stderr.is_empty() {
                if !log.is_empty() {
                    log.push('\n');
                }
                log.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            if code != 0 && log.is_empty() {
                log = format!("Docker exited with code {}", code);
            }
            (code, log)
        }
        Err(e) => (-1, format!("Failed to run docker: {}", e)),
    }
}
