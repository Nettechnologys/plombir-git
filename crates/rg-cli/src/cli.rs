//! Command-line interface definitions (clap).

use clap::{Parser, Subcommand};

use crate::runner::DEFAULT_RUNNER_CONFIG;

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
    ///
    /// File-backed SQLite applies pending migrations before listing and requires
    /// every ForgeKeep server using that database to be stopped.
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

        /// Secret encrypting data at rest — TOTP secrets, CI secrets, mirror
        /// and LDAP passwords, SSO client secrets, OAuth tokens
        /// [config: [auth].encryption_key] [default: [auth].key_file]
        ///
        /// With no explicit source the server creates and reuses a durable key
        /// file, independent of `--jwt-secret`.
        #[arg(long)]
        encryption_key: Option<String>,

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

    /// Run database migrations and exit.
    ///
    /// File-backed SQLite migrations are offline-only: stop every ForgeKeep
    /// server using this database first. The command checks that contract
    /// before opening its pool. PostgreSQL and MySQL are unaffected.
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

    /// Replace this instance's Ed25519 provenance signing key.
    ///
    /// The key signs release attestations and backs `/api/v1/ci/oidc/jwks`. It
    /// is stored, not derived from `jwt_secret`, so rotating the signing secret
    /// leaves it alone — use this only when the key itself is compromised.
    /// Every attestation signed with the previous key stops verifying.
    RotateInstanceKey {
        /// Database URL (sqlite://, postgres://, or mysql://)
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,

        /// JWT signing secret [config: [auth].jwt_secret]
        /// [env: FORGEKEEP_JWT_SECRET]
        #[arg(long)]
        jwt_secret: Option<String>,

        /// At-rest encryption key, which is what the stored key material is
        /// sealed with [config: [auth].encryption_key]
        /// [env: FORGEKEEP_ENCRYPTION_KEY]
        #[arg(long)]
        encryption_key: Option<String>,

        /// Confirm the rotation. Without it the command refuses and explains
        /// what would be invalidated.
        #[arg(long)]
        yes: bool,
    },

    /// Re-encrypt every at-rest secret under a new encryption key.
    ///
    /// Run it with the server stopped: TOTP secrets, CI secrets, mirror and
    /// LDAP passwords, SSO client secrets, OAuth tokens and the instance
    /// signing key are opened with the old key and sealed with the new one, in
    /// a single transaction. Start `--dry-run` first — it reports what every
    /// column would do and changes nothing. Afterwards, replace the configured
    /// key source or its `[auth].key_file` with the new secret.
    RotateEncryptionKey {
        /// Database URL (sqlite://, postgres://, or mysql://)
        /// [config: [database].url] [default: sqlite://./forgekeep.db?mode=rwc]
        #[arg(long)]
        db_url: Option<String>,

        /// Path to TOML configuration file; a flag passed on the command line
        /// wins over the corresponding config key
        #[arg(long)]
        config: Option<String>,

        /// JWT signing secret, used only to work out the current encryption key
        /// when `--old` is omitted [config: [auth].jwt_secret]
        /// [env: FORGEKEEP_JWT_SECRET]
        #[arg(long)]
        jwt_secret: Option<String>,

        /// The key the database is encrypted with today. Defaults to the key
        /// this deployment resolves normally (FORGEKEEP_ENCRYPTION_KEY ›
        /// [auth].encryption_key › [auth].key_file)
        #[arg(long)]
        old: Option<String>,

        /// The key to re-encrypt onto. Generate one with `forgekeep gen-secret`
        #[arg(long)]
        new: String,

        /// Report what each column would do and change nothing
        #[arg(long)]
        dry_run: bool,

        /// Confirm the rewrite. Not needed with --dry-run
        #[arg(long)]
        yes: bool,
    },

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

    /// [DEPRECATED] Run as a CI runner — use `forgekeep-runner run` instead
    ///
    /// Kept as an alias so existing invocations keep working: it now delegates to
    /// the very same implementation `forgekeep-runner run` uses, flag for flag.
    /// It used to be a second, much older copy of the runner that never read
    /// `runner.toml` (so every start registered a new runner) and had no
    /// heartbeat, workspace snapshot, job timeout or cache.
    ///
    /// Settings that also exist as a `--config` key resolve in the order
    /// CLI arg > config file > built-in default.
    // Hence `--server` is an `Option` with no clap `default_value`: a clap
    // default is indistinguishable from a value the operator typed, so with one
    // the config file's `server` could never win over "the flag was not passed".
    Runner {
        /// ForgeKeep server URL [config: server]
        /// [default: http://127.0.0.1:8080]
        #[arg(long)]
        server: Option<String>,

        /// Runner name [config: name] [default: system hostname]
        #[arg(long)]
        name: Option<String>,

        /// Runner labels (comma-separated) [config: labels]
        #[arg(long)]
        labels: Option<String>,

        /// Existing runner ID (used with --token) [config: runner_id]
        #[arg(long)]
        runner_id: Option<i64>,

        /// Existing runner token (skip registration) [config: token]
        #[arg(long)]
        token: Option<String>,

        /// Admin user JWT used only when this command needs to register a runner
        #[arg(long)]
        auth_token: Option<String>,

        /// Path to the runner config file, written by
        /// `forgekeep-runner register --save`
        #[arg(long, default_value = DEFAULT_RUNNER_CONFIG)]
        config: String,
    },

    /// Import a repository from GitHub or GitLab
    ///
    /// File-backed SQLite applies pending migrations before importing and
    /// requires every ForgeKeep server using that database to be stopped.
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
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    use super::{Cli, Commands, PackageCmd, DEFAULT_RUNNER_CONFIG};
    use clap::{CommandFactory, Parser};

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

    /// The deprecated `runner` alias must be able to reach `runner.toml` at all:
    /// it used to have no `--config` whatsoever, so the file written by
    /// `forgekeep-runner register --save` was unreachable and every start
    /// registered a brand-new runner. Its `--server` must also parse to `None`,
    /// for the same reason `--db-url` does — a clap default would shadow the
    /// file's `server` key forever.
    #[test]
    fn the_runner_alias_reads_the_same_config_file_as_forgekeep_runner() {
        let cli = Cli::try_parse_from(["forgekeep", "runner"]).unwrap();
        let Commands::Runner {
            server,
            runner_id,
            token,
            config,
            ..
        } = cli.command
        else {
            panic!("expected runner");
        };
        assert_eq!(
            config, DEFAULT_RUNNER_CONFIG,
            "the alias must default to the path `forgekeep-runner register --save` writes"
        );
        assert_eq!(server, None, "--server must not carry a clap default");
        assert_eq!(runner_id, None);
        assert_eq!(token, None);

        let cli = Cli::try_parse_from(["forgekeep", "runner", "--config", "/data/runner.toml"])
            .expect("the alias must accept --config");
        let Commands::Runner { config, .. } = cli.command else {
            panic!("expected runner");
        };
        assert_eq!(config, "/data/runner.toml");
    }

    /// The alias exists to keep old invocations working, so every flag the
    /// pre-deprecation command accepted must still parse — plus `--labels`,
    /// which it lacked and `forgekeep-runner run` has.
    #[test]
    fn the_runner_alias_still_accepts_every_flag_it_used_to() {
        let cli = Cli::try_parse_from([
            "forgekeep",
            "runner",
            "--server",
            "https://ci.example.com",
            "--name",
            "builder-1",
            "--labels",
            "docker,linux",
            "--runner-id",
            "7",
            "--token",
            "tok",
            "--auth-token",
            "jwt",
        ])
        .expect("the documented pre-deprecation flags must keep parsing");
        let Commands::Runner {
            server,
            name,
            labels,
            runner_id,
            token,
            auth_token,
            ..
        } = cli.command
        else {
            panic!("expected runner");
        };
        assert_eq!(server.as_deref(), Some("https://ci.example.com"));
        assert_eq!(name.as_deref(), Some("builder-1"));
        assert_eq!(labels.as_deref(), Some("docker,linux"));
        assert_eq!(runner_id, Some(7));
        assert_eq!(token.as_deref(), Some("tok"));
        assert_eq!(auth_token.as_deref(), Some("jwt"));
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

    // ---------------------------------------------------------------------
    // The operator-facing surface: the flags a page invites you to copy, the
    // config keys `--help` names as their equivalent, and the environment
    // variables the deploy guide tells you to set.
    //
    // `config.rs` already holds the config-file half of this contract (the
    // shipped template against the model, `ARCHITECTURE.md`'s section list
    // against the model). What is checked below is the other three surfaces an
    // operator reads before ever opening `forgekeep.toml` — none of which any
    // compiler sees. Renaming a flag turns a documented quick-start into
    // `error: unexpected argument`, and renaming a config key turns the help
    // text that names its equivalent into a quiet lie.
    // ---------------------------------------------------------------------

    /// The production half of `cli.rs`. The `--help` an operator reads is
    /// generated from the doc comments in it, so the marker scan below reads the
    /// declaration itself rather than a list kept beside it.
    fn production_cli_source() -> &'static str {
        include_str!("cli.rs")
            .split_once("\n#[cfg(test)]\n")
            .map(|(production, _)| production)
            .expect("cli.rs must keep its test module behind #[cfg(test)]")
    }

    /// `include_str!` rather than a runtime read: the paths resolve at compile
    /// time (a moved or renamed page breaks the build instead of silently
    /// skipping the check) and editing either page rebuilds — and therefore
    /// re-runs — the tests below.
    const README_MD: &str = include_str!("../../../README.md");
    const DEPLOY_README_MD: &str = include_str!("../../../deploy/README.md");

    /// Every `--long-flag` token in `text` — the spelling both the runnable
    /// quick-start block and the flag table use.
    ///
    /// A match has to start at a word boundary and continue into a letter, so
    /// that neither a hyphenated word in prose nor a markdown table's
    /// `|------|` separator row can be read as a flag.
    fn long_flags(text: &str) -> BTreeSet<&str> {
        let mut flags = BTreeSet::new();

        for (index, _) in text.match_indices("--") {
            let preceded_by_word = text[..index]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-');
            if preceded_by_word {
                continue;
            }

            let tail = &text[index + 2..];
            if !tail.starts_with(|c: char| c.is_ascii_alphanumeric()) {
                continue;
            }
            let len = tail
                .find(|c: char| !c.is_ascii_alphanumeric() && c != '-')
                .unwrap_or(tail.len());
            flags.insert(&text[index..index + 2 + len]);
        }

        flags
    }

    /// The rows of the first markdown table that follows `lead`, as their
    /// leading cell — which is where both documented tables put the name.
    fn first_table_cells<'a>(page: &'a str, lead: &str) -> Vec<&'a str> {
        let table = page
            .split_once(lead)
            .map(|(_, rest)| rest)
            .unwrap_or_else(|| panic!("the page must keep the `{lead}` table this test checks"));

        table
            .lines()
            .skip_while(|line| !line.starts_with('|'))
            .take_while(|line| line.starts_with('|'))
            .filter_map(|line| line.trim_start_matches('|').split('|').next())
            .collect()
    }

    /// The line that introduces the README's table of `serve` flags.
    const SERVE_FLAG_TABLE_LEAD: &str = "Common `serve` flags:";

    /// The start of the runnable `serve` invocation the README offers to copy.
    const SERVE_QUICKSTART_LEAD: &str = "./target/release/forgekeep serve";

    /// Every `serve` flag the README shows an operator, from both places it
    /// shows them: the command it invites you to paste into a shell, and the
    /// table below it.
    fn readme_serve_flags() -> BTreeSet<&'static str> {
        let quickstart = README_MD
            .split_once(SERVE_QUICKSTART_LEAD)
            .and_then(|(_, rest)| rest.split_once("```"))
            .map(|(block, _)| block)
            .unwrap_or_else(|| {
                panic!(
                    "README.md must keep the fenced `{SERVE_QUICKSTART_LEAD}` example — \
                     it is the first command a new operator runs"
                )
            });

        let mut flags = long_flags(quickstart);
        let mut rows_with_a_flag = 0;

        for cell in first_table_cells(README_MD, SERVE_FLAG_TABLE_LEAD) {
            let named = long_flags(cell);
            if !named.is_empty() {
                rows_with_a_flag += 1;
            }
            flags.extend(named);
        }

        // A floor, not a count: it fails loudly if the table parser stops
        // matching rows and the test quietly checks the quick-start alone.
        assert!(
            rows_with_a_flag >= 10,
            "only {rows_with_a_flag} rows of the README's `serve` flag table named a flag — \
             the table scanner has stopped matching them"
        );

        flags
    }

    /// The README hands a new operator a `serve` command to paste and a table of
    /// the flags it considers common. Neither is checked by anything today, so a
    /// renamed flag stays on the page and is discovered by a person, as
    /// `error: unexpected argument`, on their first install.
    ///
    /// Only this direction is checked: the table says *common* flags, so `serve`
    /// having knobs the README leaves out is the intent, not drift.
    #[test]
    fn every_serve_flag_the_readme_advertises_exists() {
        let command = Cli::command();
        let serve = command
            .get_subcommands()
            .find(|sub| sub.get_name() == "serve")
            .expect("`serve` must remain a subcommand of `forgekeep`");
        let accepted: BTreeSet<String> = serve
            .get_arguments()
            .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
            .collect();

        let advertised = readme_serve_flags();
        assert!(
            advertised.len() >= 15,
            "only {} flags found across the README's `serve` example and table — \
             the flag scanner has stopped matching them",
            advertised.len()
        );

        for flag in &advertised {
            assert!(
                accepted.contains(*flag),
                "README.md offers `forgekeep serve {flag}`, which clap does not accept — \
                 an operator following the page gets `error: unexpected argument`. \
                 Rename it on the page too, or restore the flag."
            );
        }
    }

    /// `[config: key]` markers that name a key of `runner.toml` rather than of
    /// `forgekeep.toml`: the deprecated `forgekeep runner` alias reads the
    /// runner's own config file. `RunnerConfig` is private to `rg-runner`, so
    /// these are listed rather than parsed — the point of the list is that a
    /// *new* unbracketed marker fails the test instead of quietly escaping the
    /// check that the bracketed ones get.
    const RUNNER_CONFIG_MARKERS: [&str; 5] = ["server", "name", "labels", "runner_id", "token"];

    /// Every `[config: …]` marker in the help text, split by which file it
    /// points at: `(line, section, key)` for the `[section].key` form that names
    /// a `forgekeep.toml` key, and the bare names that mean `runner.toml`.
    #[allow(clippy::type_complexity)]
    fn help_config_markers(source: &str) -> (Vec<(usize, &str, &str)>, BTreeSet<&str>) {
        const MARKER: &str = "[config: ";

        let mut config_file_keys = Vec::new();
        let mut runner_keys = BTreeSet::new();

        for (index, line) in source.lines().enumerate() {
            let line_no = index + 1;
            let Some((_, rest)) = line.split_once(MARKER) else {
                continue;
            };

            // `[config: [server].http_addr]` — the section is itself bracketed,
            // so the closing bracket of the marker is the *second* one.
            if let Some(bracketed) = rest.strip_prefix('[') {
                let (section, tail) = bracketed.split_once(']').unwrap_or_else(|| {
                    panic!("cli.rs:{line_no}: `{MARKER}[` marker never closes its section")
                });
                let key = tail
                    .strip_prefix('.')
                    .and_then(|tail| tail.split_once(']'))
                    .map(|(key, _)| key)
                    .unwrap_or_else(|| {
                        panic!(
                            "cli.rs:{line_no}: `{MARKER}[{section}]` must be followed by \
                             `.key]` — that is the spelling `--help` shows"
                        )
                    });
                config_file_keys.push((line_no, section, key));
            } else {
                let (key, _) = rest
                    .split_once(']')
                    .unwrap_or_else(|| panic!("cli.rs:{line_no}: `{MARKER}` marker never closes"));
                runner_keys.insert(key);
            }
        }

        (config_file_keys, runner_keys)
    }

    /// `ARCHITECTURE.md` declares `--help` the canonical place where a flag's
    /// config-file equivalent is named, and the README sends operators there
    /// instead of repeating the mapping. Nothing checks it: renaming a key in
    /// `ConfigFile` leaves 26 `[config: …]` markers in the help text pointing at
    /// keys the model no longer has, and the operator who follows one gets
    /// `unknown field` on the next start.
    ///
    /// Only the key's *existence* is asserted, not its type — the probe value is
    /// arbitrary, so a type mismatch is this test's noise while `unknown field`
    /// is exactly its signal.
    #[test]
    fn every_config_key_named_in_help_is_a_real_key() {
        fn unknown_field_error(document: &str) -> Option<String> {
            let error = toml::from_str::<crate::config::ConfigFile>(document).err()?;
            let error = error.to_string();
            error.contains("unknown field").then_some(error)
        }

        // The detector has to bite before its silence means anything.
        assert!(
            unknown_field_error("[server]\nnot_a_real_key = \"probe\"\n").is_some(),
            "ConfigFile no longer rejects unknown keys, so this test cannot tell a real \
             config key from an invented one"
        );
        assert!(
            unknown_field_error("[not_a_real_section]\nkey = \"probe\"\n").is_some(),
            "ConfigFile no longer rejects unknown sections"
        );

        let (documented, runner_markers) = help_config_markers(production_cli_source());

        for (line_no, section, key) in &documented {
            let document = format!("[{section}]\n{key} = \"probe\"\n");
            if let Some(error) = unknown_field_error(&document) {
                panic!(
                    "cli.rs:{line_no}: `--help` tells the operator that this flag's \
                     config-file equivalent is `[{section}].{key}`, and the model has no \
                     such key — following the help text yields `unknown field` on the next \
                     start. ({error})"
                );
            }
        }

        assert!(
            documented.len() >= 20,
            "only {} `[config: [section].key]` markers found in cli.rs — \
             the help-text scanner has stopped matching them",
            documented.len()
        );

        let expected: BTreeSet<&str> = RUNNER_CONFIG_MARKERS.into_iter().collect();
        let unexpected: Vec<&&str> = runner_markers.difference(&expected).collect();
        assert!(
            unexpected.is_empty(),
            "cli.rs names {unexpected:?} as `[config: <key>]` without a `[section]`, so the \
             check above skipped them. A `forgekeep.toml` key must be written \
             `[config: [section].key]`; if these really are `runner.toml` keys, add them to \
             RUNNER_CONFIG_MARKERS."
        );
    }

    /// The heading that introduces `deploy/README.md`'s environment table.
    const ENV_TABLE_LEAD: &str = "### Environment variables";

    /// The `FORGEKEEP_*` variables the deploy guide tells an operator to set.
    fn documented_env_vars() -> BTreeSet<&'static str> {
        let mut names = BTreeSet::new();

        for cell in first_table_cells(DEPLOY_README_MD, ENV_TABLE_LEAD) {
            names.extend(cell.split('`').filter(|token| {
                token.starts_with("FORGEKEEP_")
                    && token.chars().all(|c| c.is_ascii_uppercase() || c == '_')
            }));
        }

        names
    }

    /// Every production `.rs` file of the workspace, with its `#[cfg(test)]`
    /// tail removed.
    ///
    /// A directory walk rather than a list of `include_str!`s: the question is
    /// whether *anything* still reads a variable, and a fixed list would have to
    /// be edited whenever the read moves — which is the maintenance these drift
    /// tests exist to remove. The `#[cfg(test)]` cut is what keeps the census
    /// honest: a variable named only by an assertion about an old error message
    /// is not a variable anything reads.
    fn production_workspace_sources() -> Vec<(PathBuf, String)> {
        let crates = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .canonicalize()
            .expect("the workspace `crates/` directory must be reachable");

        let mut sources = Vec::new();
        let mut pending = vec![crates];

        while let Some(dir) = pending.pop() {
            let entries = std::fs::read_dir(&dir)
                .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));

            for entry in entries {
                let path = entry.expect("a readable directory entry").path();
                let name = path.file_name().unwrap_or_default().to_string_lossy();

                if path.is_dir() {
                    // `tests/` is integration tests, `target/` is build output.
                    if name != "tests" && name != "target" {
                        pending.push(path);
                    }
                    continue;
                }
                if !name.ends_with(".rs") || name.ends_with("_tests.rs") {
                    continue;
                }

                let text = std::fs::read_to_string(&path)
                    .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
                let production = match text.split_once("\n#[cfg(test)]\n") {
                    Some((production, _)) => production.to_string(),
                    None => text,
                };
                sources.push((path, production));
            }
        }

        sources
    }

    /// The file that names `variable` as a string literal on a line of code, if
    /// any. Requiring the quotes is what separates a read from prose: the help
    /// text and the error messages mention these names constantly, and none of
    /// those mentions makes the variable do anything.
    fn source_reading_env_var(sources: &[(PathBuf, String)], variable: &str) -> Option<PathBuf> {
        let literal = format!("\"{variable}\"");

        sources
            .iter()
            .find(|(_, text)| {
                text.lines().any(|line| {
                    let line = line.trim_start();
                    !line.starts_with("//") && line.contains(&literal)
                })
            })
            .map(|(path, _)| path.clone())
    }

    /// `deploy/README.md` lists the environment variables the container is
    /// driven by, and a container is exactly where a mistyped or retired
    /// variable is invisible: setting one that nothing reads looks identical to
    /// setting one that works, right up to the point where an instance meant to
    /// be closed to registration is open.
    #[test]
    fn every_environment_variable_the_deploy_readme_documents_is_read_by_the_code() {
        let documented = documented_env_vars();
        assert!(
            documented.len() >= 5,
            "only {} variables found in deploy/README.md's environment table — \
             the table scanner has stopped matching them",
            documented.len()
        );

        let sources = production_workspace_sources();
        assert!(
            sources.len() >= 50,
            "the workspace walk found only {} production sources — it is looking in the \
             wrong place",
            sources.len()
        );

        for variable in &documented {
            assert!(
                source_reading_env_var(&sources, variable).is_some(),
                "deploy/README.md tells the operator to set `{variable}`, and no production \
                 source under crates/ names it — the variable was renamed or retired, and \
                 setting it now silently does nothing"
            );
        }

        // The census has to be able to answer "no" before its "yes" is worth
        // anything (a substring scan that matches everything is vacuously green).
        assert_eq!(
            source_reading_env_var(&sources, "FORGEKEEP_NOT_A_REAL_VARIABLE"),
            None,
            "the source census matches a variable that does not exist, so it cannot \
             detect one that stopped existing"
        );
    }
}
