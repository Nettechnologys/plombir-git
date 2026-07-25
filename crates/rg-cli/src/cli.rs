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
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,
    },
}

#[derive(Parser)]
#[command(name = "forgekeep", about = "A Git hosting platform written in Rust")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Commands,
}

/// Every subcommand that reads `[database].url` or `[server].repo_root` takes
/// its own `--config`, and none of the flags those keys feed carries a clap
/// `default_value` — see the note on [`Commands::Serve`]. Without that, a
/// deployment whose config file points at Postgres (or at `/data`) had
/// `forgekeep migrate` quietly create and migrate a *second*, empty
/// `./forgekeep.db`, and `forgekeep backup-db` "successfully" back up nothing.
#[derive(Subcommand)]
#[allow(clippy::large_enum_variant)]
pub(crate) enum Commands {
    /// Start the ForgeKeep server
    ///
    /// Settings that also exist as a `--config` key resolve in the order
    /// CLI arg > config file > built-in default.
    // Hence every such flag is an `Option` with no clap `default_value`: a clap
    // default is indistinguishable from a value the operator typed, so with one
    // the config file could never win over "the flag was not passed". The
    // defaults live in `config::DEFAULT_*` and are named in each flag's help.
    Serve {
        /// Root directory for git repositories [config: [server].repo_root]
        /// [default: ./repos]
        #[arg(long)]
        repo_root: Option<String>,

        /// HTTP listen address [config: [server].http_addr]
        /// [default: 0.0.0.0:8080]
        #[arg(long)]
        http_addr: Option<String>,

        /// SSH listen address [config: [server].ssh_addr]
        /// [default: 0.0.0.0:2222]
        #[arg(long)]
        ssh_addr: Option<String>,

        /// Path to SSH host key [config: [server].host_key]
        #[arg(long)]
        host_key: Option<String>,

        /// Database URL (sqlite://, postgres://, or mysql://)
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

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
        /// [config: [rate_limit].max] [default: 0]
        #[arg(long)]
        rate_limit_max: Option<u32>,

        /// Rate limit: window duration in seconds
        /// [config: [rate_limit].window_secs] [default: 60]
        #[arg(long)]
        rate_limit_window: Option<u64>,

        /// Comma-separated proxy IPs whose X-Forwarded-For / X-Real-IP headers are trusted
        #[arg(long, value_delimiter = ',')]
        rate_limit_trusted_proxies: Vec<String>,

        /// SMTP server host (enables email notifications)
        #[arg(long)]
        smtp_host: Option<String>,

        /// SMTP server port [config: [smtp].port] [default: 587]
        #[arg(long)]
        smtp_port: Option<u16>,

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

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,

        /// Log file path (enables file logging with rotation). If not set, logs to stderr only.
        #[arg(long)]
        log_file: Option<String>,

        /// Log rotation: nominal max log file size in MB. NOTE: the file
        /// appender rotates daily, not by size — this value is advisory only.
        /// [config: [logging].max_size_mb] [default: 10]
        #[arg(long)]
        log_max_size_mb: Option<u64>,

        /// Log rotation: max number of old log files to keep
        /// [config: [logging].max_files] [default: 5]
        #[arg(long)]
        log_max_files: Option<usize>,
    },

    /// Run database migrations and exit
    Migrate {
        /// Database URL (sqlite://, postgres://, or mysql://)
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,
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
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,
    },

    /// Create a consistent SQLite database backup.
    BackupDb {
        /// SQLite database URL (e.g. sqlite://./forgekeep.db?mode=rwc)
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,

        /// Output backup file path.
        output: String,

        /// Overwrite output if it already exists.
        #[arg(long, default_value_t = false)]
        force: bool,
    },

    /// Restore a SQLite database file from a backup.
    RestoreDb {
        /// SQLite database URL to restore into.
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,

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
        /// [config: [server].repo_root] [default: ./repos]
        #[arg(long)]
        repo_root: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,
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
        /// [config: [server].repo_root] [default: ./repos]
        #[arg(long)]
        repo_root: Option<String>,

        /// Database URL (sqlite://, postgres://, or mysql://)
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,

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
        /// [config: [server].repo_root] [default: ./repos]
        #[arg(long)]
        repo_root: Option<String>,

        /// Database URL (sqlite://, postgres://, or mysql://)
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,

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

#[cfg(test)]
mod tests {
    use super::{Cli, Commands, PackageCmd};
    use clap::Parser;

    /// `(db_url, repo_root, config)` as parsed, for the subcommands that carry
    /// any of the three.
    fn knobs(cmd: &Commands) -> (Option<&str>, Option<&str>, Option<&str>) {
        match cmd {
            Commands::Serve {
                db_url,
                repo_root,
                config,
                ..
            } => (db_url.as_deref(), repo_root.as_deref(), config.as_deref()),
            Commands::Migrate { db_url, config }
            | Commands::RebuildFts { db_url, config }
            | Commands::BackupDb { db_url, config, .. }
            | Commands::RestoreDb { db_url, config, .. } => {
                (db_url.as_deref(), None, config.as_deref())
            }
            Commands::CreateRepo {
                repo_root, config, ..
            } => (None, repo_root.as_deref(), config.as_deref()),
            Commands::Import {
                db_url,
                repo_root,
                config,
                ..
            }
            | Commands::IndexRepo {
                db_url,
                repo_root,
                config,
                ..
            } => (db_url.as_deref(), repo_root.as_deref(), config.as_deref()),
            Commands::Package {
                cmd: PackageCmd::List { db_url, config, .. },
            } => (db_url.as_deref(), None, config.as_deref()),
            _ => panic!("subcommand under test carries no db_url/repo_root/config"),
        }
    }

    /// Every one-shot invocation that has to be able to read the config file,
    /// with no flags beyond its required positionals.
    const FLAGLESS_INVOCATIONS: &[&[&str]] = &[
        &["forgekeep", "serve"],
        &["forgekeep", "migrate"],
        &["forgekeep", "rebuild-fts"],
        &["forgekeep", "backup-db", "out.db"],
        &["forgekeep", "restore-db", "in.db"],
        &["forgekeep", "create-repo", "alice", "site"],
        &[
            "forgekeep",
            "import",
            "github",
            "https://github.com/alice/site",
            "--target-owner",
            "alice",
        ],
        &["forgekeep", "index-repo", "alice/site"],
        &["forgekeep", "package", "list", "alice", "site", "cargo"],
    ];

    /// The root cause of the ignored-config bug: a clap `default_value` on
    /// `--db-url` / `--repo-root` makes "flag not passed" indistinguishable from
    /// a value the operator typed, so the config file can never win. Every knob
    /// with a config-file equivalent must therefore parse to `None`, and the
    /// built-in default must live in `crate::config::DEFAULT_*` instead.
    #[test]
    fn config_backed_flags_have_no_clap_default() {
        for argv in FLAGLESS_INVOCATIONS {
            let cli = Cli::try_parse_from(*argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
            let (db_url, repo_root, config) = knobs(&cli.command);
            assert_eq!(
                db_url, None,
                "{argv:?} db_url must not carry a clap default"
            );
            assert_eq!(
                repo_root, None,
                "{argv:?} repo_root must not carry a clap default"
            );
            assert_eq!(config, None, "{argv:?} must not invent a config path");
        }
    }

    /// The other half of the contract: every one of those subcommands accepts
    /// `--config`, so a config-only deployment can run them without repeating
    /// the database URL (and getting it wrong).
    #[test]
    fn every_config_backed_subcommand_accepts_a_config_flag() {
        for argv in FLAGLESS_INVOCATIONS {
            let mut argv: Vec<&str> = argv.to_vec();
            argv.extend(["--config", "/etc/forgekeep/forgekeep.toml"]);
            let cli = Cli::try_parse_from(&argv).unwrap_or_else(|e| panic!("{argv:?}: {e}"));
            let (_, _, config) = knobs(&cli.command);
            assert_eq!(config, Some("/etc/forgekeep/forgekeep.toml"), "{argv:?}");
        }
    }

    /// The card's acceptance check, end to end through clap, the real config
    /// loader and the real resolver: `forgekeep migrate --config <file>` with no
    /// `--db-url` must reach the Postgres URL from the file, not the built-in
    /// SQLite default — and an explicit `--db-url` must still win.
    #[test]
    fn migrate_resolves_the_database_url_from_the_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("forgekeep.toml");
        std::fs::write(
            &path,
            "[database]\nurl = \"postgres://forge:pw@db.internal/forgekeep\"\n",
        )
        .unwrap();
        let path = path.to_str().unwrap();

        let cli = Cli::try_parse_from(["forgekeep", "migrate", "--config", path]).unwrap();
        let Commands::Migrate { db_url, config } = cli.command else {
            panic!("expected migrate");
        };
        let cfg = crate::config::load_optional_config_file(config.as_deref()).unwrap();
        assert_eq!(
            crate::config::resolve_db_url(db_url, cfg.as_ref()),
            "postgres://forge:pw@db.internal/forgekeep"
        );

        let cli = Cli::try_parse_from([
            "forgekeep",
            "migrate",
            "--config",
            path,
            "--db-url",
            "sqlite://./explicit.db?mode=rwc",
        ])
        .unwrap();
        let Commands::Migrate { db_url, config } = cli.command else {
            panic!("expected migrate");
        };
        let cfg = crate::config::load_optional_config_file(config.as_deref()).unwrap();
        assert_eq!(
            crate::config::resolve_db_url(db_url, cfg.as_ref()),
            "sqlite://./explicit.db?mode=rwc"
        );
    }
}
