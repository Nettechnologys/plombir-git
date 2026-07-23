//! Command-line interface definitions (clap).

use clap::{Parser, Subcommand};

#[derive(Subcommand)]
pub(crate) enum PackageCmd {
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

        /// ForgeKeep server URL (for token-based auth)
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
        #[arg(long, default_value = "sqlite://./forgekeep.db?mode=rwc")]
        db_url: String,
    },
}

#[derive(Parser)]
#[command(name = "forgekeep", about = "A Git hosting platform written in Rust")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Commands {
    /// Start the ForgeKeep server
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
        #[arg(long, default_value = "sqlite://./forgekeep.db?mode=rwc")]
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

        /// Allow imageless CI jobs to run as a shell directly on the host.
        /// Off by default: on shared/public instances every job must use a
        /// Docker sandbox (`image:`) or a dedicated runner. Enable only on a
        /// trusted single-tenant server.
        #[arg(long, default_value_t = false)]
        allow_host_runner: bool,

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
        #[arg(long, default_value = "sqlite://./forgekeep.db?mode=rwc")]
        db_url: String,
    },

    /// Generate a cryptographically strong JWT secret and print it to stdout.
    ///
    /// Use it to seed `[auth].jwt_secret`, `FORGEKEEP_JWT_SECRET`, or
    /// `--jwt-secret` instead of copying a shared default:
    ///     forgekeep gen-secret
    ///     FORGEKEEP_JWT_SECRET="$(forgekeep gen-secret)"
    GenSecret,

    /// Rebuild or refresh full-text search indexes from main tables
    RebuildFts {
        /// Database URL (sqlite://, postgres://, or mysql://)
        #[arg(long, default_value = "sqlite://./forgekeep.db?mode=rwc")]
        db_url: String,
    },

    /// Create a consistent SQLite database backup.
    BackupDb {
        /// SQLite database URL (e.g. sqlite://./forgekeep.db?mode=rwc)
        #[arg(long, default_value = "sqlite://./forgekeep.db?mode=rwc")]
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
        #[arg(long, default_value = "sqlite://./forgekeep.db?mode=rwc")]
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
        /// ForgeKeep server URL (e.g. http://127.0.0.1:8080)
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

        /// Target owner in ForgeKeep
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
        #[arg(long, default_value = "sqlite://./forgekeep.db?mode=rwc")]
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
        #[arg(long, default_value = "sqlite://./forgekeep.db?mode=rwc")]
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
