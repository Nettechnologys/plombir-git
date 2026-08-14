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
    /// CLI arg > config file > built-in default. The defaults are the runner's
    /// own and are stated by `forgekeep-runner run --help`; this alias does not
    /// restate them, so there is one page to keep true instead of two.
    // Hence `--server` is an `Option` with no clap `default_value`: a clap
    // default is indistinguishable from a value the operator typed, so with one
    // the config file's `server` could never win over "the flag was not passed".
    //
    // The `[default: …]` note this help used to carry was a third copy of
    // `rg_runner`'s `config::DEFAULT_SERVER`, in a crate that constant is not
    // visible from — nothing could have bound it, so it could only drift. What
    // keeps a re-added one honest is a check in the crate that owns the value:
    // `rg-runner/src/config.rs::the_deprecated_alias_promises_no_runner_default_of_its_own`.
    Runner {
        /// ForgeKeep server URL [config: server]
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
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

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
            | Commands::RestoreDb { db_url, config, .. }
            | Commands::RotateInstanceKey { db_url, config, .. }
            | Commands::RotateEncryptionKey { db_url, config, .. } => {
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
    /// carrying nothing beyond what clap requires of it — never `--db-url`,
    /// `--repo-root` or `--config`, which is what the two tests below assert
    /// about.
    ///
    /// Kept in step with the declaration by
    /// [`flagless_invocations_lists_every_config_backed_subcommand`]: the list
    /// has to be written out (only a person knows a runnable positional), but
    /// which subcommands belong on it is clap's to say.
    const FLAGLESS_INVOCATIONS: &[&[&str]] = &[
        &["forgekeep", "serve"],
        &["forgekeep", "migrate"],
        &["forgekeep", "rebuild-fts"],
        &["forgekeep", "backup-db", "out.db"],
        &["forgekeep", "restore-db", "in.db"],
        &["forgekeep", "create-repo", "alice", "site"],
        &["forgekeep", "rotate-instance-key"],
        &["forgekeep", "rotate-encryption-key", "--new", "replacement"],
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

    /// The rows of the first markdown table that follows `lead`, split into
    /// trimmed cells.
    fn first_table_rows<'a>(page: &'a str, lead: &str) -> Vec<Vec<&'a str>> {
        let table = page
            .split_once(lead)
            .map(|(_, rest)| rest)
            .unwrap_or_else(|| panic!("the page must keep the `{lead}` table this test checks"));

        table
            .lines()
            .skip_while(|line| !line.starts_with('|'))
            .take_while(|line| line.starts_with('|'))
            .map(|line| line.trim_matches('|').split('|').map(str::trim).collect())
            .collect()
    }

    /// The rows of that table as their leading cell — which is where every
    /// documented table puts the name.
    fn first_table_cells<'a>(page: &'a str, lead: &str) -> Vec<&'a str> {
        first_table_rows(page, lead)
            .into_iter()
            .filter_map(|row| row.into_iter().next())
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

    /// The command the deployment files spell out for this binary.
    const SERVE_INVOCATION: &str = "forgekeep serve";

    /// The files an operator copies a command out of: the shipped compose files,
    /// the image's own default command, and the two guides that quote them.
    ///
    /// Walked at run time rather than pinned with `include_str!` so that a
    /// compose file added to `deploy/` joins the contract by existing. The
    /// floors below are what keep a walk that stopped matching from passing for
    /// agreement.
    fn repository_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the repository root must be reachable from the crate directory")
    }

    fn deployment_files() -> Vec<(String, String)> {
        fn read(path: &Path) -> String {
            std::fs::read_to_string(path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        }

        let root = repository_root();

        let deploy = root.join("deploy");
        let mut files = Vec::new();

        for entry in std::fs::read_dir(&deploy)
            .unwrap_or_else(|error| panic!("{}: {error}", deploy.display()))
        {
            let path = entry.expect("a readable directory entry").path();
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            if name.starts_with("docker-compose") && name.ends_with(".yml") {
                files.push((format!("deploy/{name}"), read(&path)));
            }
        }

        // Directory order is not stable across machines; which failure is
        // reported first should not depend on the filesystem.
        files.sort_by(|(left, _), (right, _)| left.cmp(right));

        for extra in ["Dockerfile", "deploy/README.md", "README.md"] {
            files.push((extra.to_string(), read(&root.join(extra))));
        }

        files
    }

    /// A deployment file reduced to one command line per line: the `#` of a
    /// commented-out block dropped, the punctuation that only holds a command
    /// together turned into whitespace, and runs of whitespace collapsed.
    ///
    /// Three spellings have to survive it — a folded `command: >` block with one
    /// flag per line, an exec-form `command: ["forgekeep", "serve", …]` on a
    /// single line, and the Dockerfile's backslash-continued `CMD` — plus the
    /// backticks a markdown guide wraps the same command in.
    fn command_lines(text: &str) -> Vec<String> {
        text.lines()
            .map(|line| {
                let line = line.trim();
                let line = line.strip_prefix('#').unwrap_or(line);
                let line = line.replace(['[', ']', '"', ',', '\\', '`'], " ");
                line.split_whitespace().collect::<Vec<_>>().join(" ")
            })
            .collect()
    }

    /// Every invocation of `leader` in `lines`, as the command text following it.
    ///
    /// An invocation is the rest of the line the leader starts on plus every
    /// line after it that starts with a flag — the one rule that collapses all
    /// three spellings above, since only a folded block puts its flags on lines
    /// of their own, and the first line that is not a flag is the next YAML key.
    fn invocations(lines: &[String], leader: &str) -> Vec<String> {
        let mut found = Vec::new();

        for (index, line) in lines.iter().enumerate() {
            let Some((before, rest)) = line.split_once(leader) else {
                continue;
            };

            // A whole word on both sides: neither a longer binary name nor a
            // longer subcommand is this invocation. A path separator ends the
            // word too — `./target/release/forgekeep serve` is the command the
            // README hands a new operator, not a different binary.
            if before
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_whitespace() && c != '/')
                || rest.starts_with(|c: char| !c.is_whitespace())
            {
                continue;
            }

            let mut parts = Vec::new();
            let rest = rest.trim();
            if !rest.is_empty() {
                parts.push(rest);
            }
            for next in &lines[index + 1..] {
                if !next.starts_with("--") {
                    break;
                }
                parts.push(next);
            }

            found.push(parts.join(" "));
        }

        found
    }

    /// Every leaf subcommand of this binary, keyed by the invocation an operator
    /// types, paired with the long flags clap accepts for it.
    ///
    /// Leaves only — `forgekeep package list`, never `forgekeep package`: a
    /// group matched as a leader would read its child's flags as its own and
    /// fail on every one of them.
    fn subcommand_flags() -> BTreeMap<String, BTreeSet<String>> {
        fn walk(
            command: &clap::Command,
            path: &str,
            into: &mut BTreeMap<String, BTreeSet<String>>,
        ) {
            for sub in command.get_subcommands() {
                let path = format!("{path} {}", sub.get_name());
                if sub.get_subcommands().next().is_some() {
                    walk(sub, &path, into);
                    continue;
                }

                let mut accepted: BTreeSet<String> = sub
                    .get_arguments()
                    .filter_map(|arg| arg.get_long().map(|long| format!("--{long}")))
                    .collect();
                // clap generates `--help` in a build step this walk does not
                // run, so the declaration it reads never carries it — and it is
                // the one flag the pages tell an operator to reach for first.
                accepted.insert("--help".to_string());
                into.insert(path, accepted);
            }
        }

        let command = Cli::command();
        let mut flags = BTreeMap::new();
        walk(&command, command.get_name(), &mut flags);
        flags
    }

    /// The subcommands the two guides spell out with at least one flag. Listed
    /// rather than derived: what a page hands an operator is an editorial fact
    /// about the page, and a derived list would agree with whatever the page
    /// happens to say — including saying nothing.
    const SUBCOMMANDS_THE_PAGES_HAND_AN_OPERATOR: [&str; 7] = [
        "serve",
        "migrate",
        "rotate-instance-key",
        "rotate-encryption-key",
        "backup-db",
        "restore-db",
        "create-repo",
    ];

    /// The image's default command, the compose files' `command:` blocks and the
    /// lines of the two guides that quote them are the invocations an operator
    /// runs without ever having typed them. Nothing checks any of them: the
    /// gate that validates those files is `docker compose config`, which parses
    /// the YAML and therefore never looks inside the commented-out block, never
    /// at the Dockerfile, and never at a flag's spelling.
    ///
    /// Every leaf subcommand is a leader here, not just `serve`. `serve` is the
    /// command whose failure is loudest — the container dies on boot — but it is
    /// the least dangerous one to get wrong: the pages also hand an operator
    /// `rotate-encryption-key`, `restore-db` and `migrate`, each of which is run
    /// exactly once, against a stopped server, by someone who is already having
    /// a bad day.
    ///
    /// Only this direction is checked: a flag no page mentions is the intent,
    /// not drift.
    #[test]
    fn every_subcommand_flag_the_operator_pages_offer_exists() {
        // Every shape the shipped files use, each followed by the line that ends
        // it. A scanner that swallowed the next key, or stopped matching a
        // shape, is how this test would go quietly green.
        let fixture = command_lines(concat!(
            "    # command: >\n",
            "    #   forgekeep serve\n",
            "    #   --config /app/forgekeep.toml\n",
            "    networks:\n",
            "      - forgekeep-net\n",
            "    command: [\"forgekeep\", \"serve\", \"--http-addr\", \"0.0.0.0:8080\"]\n",
            "CMD [\"forgekeep\", \"serve\", \\\n",
            "     \"--repo-root\", \"/data/repos\", \\\n",
            "     \"--log-file\", \"/data/logs/forgekeep.log\"]\n",
            "The image runs `forgekeep serve --db-url sqlite:///data/forgekeep.db`.\n",
        ));
        assert_eq!(
            invocations(&fixture, SERVE_INVOCATION),
            vec![
                "--config /app/forgekeep.toml".to_string(),
                "--http-addr 0.0.0.0:8080".to_string(),
                "--repo-root /data/repos --log-file /data/logs/forgekeep.log".to_string(),
                "--db-url sqlite:///data/forgekeep.db .".to_string(),
            ],
            "the invocation scanner no longer reads the deployment files the way they spell \
             the command"
        );
        assert!(
            invocations(
                &command_lines("forgekeep serve-forever --nope\n"),
                SERVE_INVOCATION
            )
            .is_empty(),
            "the invocation scanner reads a longer subcommand as `{SERVE_INVOCATION}`"
        );

        let by_subcommand = subcommand_flags();
        assert!(
            by_subcommand.contains_key(SERVE_INVOCATION)
                && by_subcommand.contains_key("forgekeep package list"),
            "the subcommand walk no longer reaches a top-level command and a nested one — \
             it has stopped describing this binary"
        );

        let mut documented = BTreeSet::new();
        let mut offered = 0;
        let mut checked = 0;

        for (name, text) in deployment_files() {
            let lines = command_lines(&text);
            for (invocation, accepted) in &by_subcommand {
                for command in invocations(&lines, invocation) {
                    offered += 1;
                    for flag in long_flags(&command) {
                        assert!(
                            accepted.contains(flag),
                            "{name} runs `{invocation} {flag}`, which clap does not accept — \
                             an operator pasting that line gets `error: unexpected argument`, \
                             and for every command here but `serve` that happens with the \
                             server already stopped. Rename it on the page too, or restore \
                             the flag."
                        );
                        checked += 1;
                        documented.insert(invocation.clone());
                    }
                }
            }
        }

        // Floors, not counts: 16 invocations carrying 32 flags at the time of
        // writing. `serve` alone would satisfy both, which is why the check that
        // matters is the named one below.
        assert!(
            offered >= 12,
            "only {offered} subcommand invocations found across the operator pages — \
             the scanner has stopped matching them"
        );
        assert!(
            checked >= 24,
            "only {checked} flags found across those invocations — the flag scanner has \
             stopped matching them"
        );

        // A floor would let a renamed subcommand pass: the leader stops matching
        // the page, the page keeps a command that no longer exists, and the
        // count merely drops. Naming them is what makes the rename red — from
        // either side, since renaming it in clap moves the leader and renaming
        // it on the page moves the text.
        let expected: BTreeSet<String> = SUBCOMMANDS_THE_PAGES_HAND_AN_OPERATOR
            .iter()
            .map(|name| format!("forgekeep {name}"))
            .collect();
        let missing: Vec<&String> = expected.difference(&documented).collect();
        assert!(
            missing.is_empty(),
            "{missing:?} no longer appears with a flag on any operator page — either the \
             subcommand was renamed and the pages still spell the old name, or the page \
             stopped handing an operator the command. Fix the page, or drop the name from \
             `SUBCOMMANDS_THE_PAGES_HAND_AN_OPERATOR`."
        );
    }

    /// The sentence in `deploy/README.md` that makes `--config` enough for the
    /// admin commands, up to the parenthesis in which it names the set that
    /// promise covers.
    const DB_TOUCHING_CLAIM_LEAD: &str = "every DB-touching subcommand (";

    /// Every leaf subcommand clap gives a `--db-url`, spelled the way an
    /// operator types it — `package list`, not `forgekeep package list`.
    ///
    /// This is the definition of "DB-touching": a subcommand addresses a
    /// database exactly when it accepts the flag that names one.
    fn db_touching_subcommands() -> BTreeSet<String> {
        subcommand_flags()
            .into_iter()
            .filter(|(_, accepted)| accepted.contains("--db-url"))
            .filter_map(|(path, _)| path.strip_prefix("forgekeep ").map(str::to_string))
            .collect()
    }

    /// The names a page lists between `lead` and `close`.
    ///
    /// The span, not the line: every sentence read this way wraps across source
    /// lines, and which name lands on which line is a detail of the page's fill
    /// width. Inside the span, an inline `` `code` `` is a name.
    fn names_listed_between(page: &str, lead: &str, close: &str) -> BTreeSet<String> {
        let listed = page
            .split_once(lead)
            .and_then(|(_, rest)| rest.split_once(close))
            .map(|(listed, _)| listed)
            .unwrap_or_else(|| {
                panic!(
                    "the page must keep the `{lead}…{close}` sentence — it is where it \
                     promises something about a set and then names the set"
                )
            });

        listed
            .split('`')
            .skip(1)
            .step_by(2)
            .map(|name| name.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect()
    }

    /// `deploy/README.md` promises that `--config` is enough for "every
    /// DB-touching subcommand" and then names them. That set is not editorial:
    /// a subcommand is DB-touching exactly when clap gives it `--db-url`, so the
    /// sentence is a mechanically checkable claim about a declaration in the
    /// same repository — checked, until now, by a person.
    ///
    /// Both directions, because both failures land on an operator. A subcommand
    /// clap has and the sentence omits is still covered by the blanket promise:
    /// whoever passes it `--config /app/forgekeep.toml` on the strength of that
    /// promise gets whatever the omitted command actually does with the flag.
    /// A name the sentence has and clap does not is the reverse — the recipe
    /// that spells it fails with `error: unexpected argument`.
    #[test]
    fn the_deploy_guide_names_exactly_the_db_touching_subcommands() {
        let with_db_url = db_touching_subcommands();

        // The one name the sentence leaves out, and the exclusion has to stay
        // deliberate: `serve` is the other side of the promise — the admin
        // command and the server pointed at one database — not an omission.
        assert!(
            with_db_url.contains("serve"),
            "`serve` no longer takes `--db-url`, so subtracting it below has quietly \
             stopped meaning anything — re-read the sentence in deploy/README.md before \
             changing this"
        );
        let expected: BTreeSet<String> = with_db_url
            .into_iter()
            .filter(|name| name != "serve")
            .collect();

        let documented = names_listed_between(DEPLOY_README_MD, DB_TOUCHING_CLAIM_LEAD, ")");

        // A floor, not a count: a span parser that stopped matching would
        // otherwise read as the page agreeing with clap about nothing.
        assert!(
            documented.len() >= 7,
            "only {} names parsed out of the `{DB_TOUCHING_CLAIM_LEAD}…)` sentence \
             ({documented:?}) — the scanner has stopped reading the list it makes",
            documented.len()
        );

        assert_eq!(
            documented, expected,
            "deploy/README.md promises `--config` reaches `[database].url` for every \
             DB-touching subcommand and then lists them, and the list no longer matches the \
             subcommands clap gives a `--db-url`. Fix the sentence, or the declaration."
        );
    }

    /// The same set, used the other way round: every invocation of one of those
    /// subcommands that an operator page hands over has to name the database it
    /// addresses.
    ///
    /// Passing neither `--config` nor `--db-url` is not a shorter spelling of
    /// the same command — it falls back to `sqlite://./forgekeep.db?mode=rwc`
    /// relative to the image's `WORKDIR /app`, an empty database that nothing
    /// else ever opens. Every command in this set is run once, against a stopped
    /// server, by someone already having a bad day, and each one of them
    /// "succeeds" against that file: `backup-db` writes a snapshot of nothing,
    /// `rotate-encryption-key` re-encrypts nothing.
    ///
    /// `serve` is excluded for the same reason it is excluded above — the image
    /// gives it the URL in its own `CMD`, and its `FORGEKEEP_*` environment is a
    /// third way to configure it that these commands do not have.
    #[test]
    fn every_admin_invocation_on_an_operator_page_names_its_database() {
        let by_subcommand = subcommand_flags();
        let admin: BTreeSet<String> = db_touching_subcommands()
            .into_iter()
            .filter(|name| name != "serve")
            .map(|name| format!("forgekeep {name}"))
            .collect();
        assert!(
            admin
                .iter()
                .all(|leader| by_subcommand.contains_key(leader)),
            "the subcommand walk and the DB-touching set disagree about this binary"
        );

        let mut checked = 0;

        for (name, text) in deployment_files() {
            let lines = command_lines(&text);
            for leader in &admin {
                for command in invocations(&lines, leader) {
                    let named = long_flags(&command);
                    assert!(
                        named.contains("--config") || named.contains("--db-url"),
                        "{name} runs `{leader} {command}`, which names no database — it \
                         falls back to `sqlite://./forgekeep.db?mode=rwc` under the image's \
                         `WORKDIR /app` and reports success against an empty file. Add \
                         `--config /app/forgekeep.toml` or the deployment's `--db-url`."
                    );
                    checked += 1;
                }
            }
        }

        // A floor, not a count: 7 such invocations at the time of writing. With
        // none of them matched, the assertion above never runs and the pages
        // pass for checked.
        assert!(
            checked >= 5,
            "only {checked} admin invocations found across the operator pages — the \
             scanner has stopped matching them"
        );
    }

    /// The line that introduces the deploy guide's table of shipped binaries.
    const RUNTIME_BINARY_TABLE_LEAD: &str = "The Docker image includes all runtime binaries:";

    /// Every binary the workspace declares.
    ///
    /// The image's payload is not a list either: the Dockerfile asks
    /// `cargo metadata` which bin targets exist and copies all of them, so a
    /// binary joins the image by being declared. This walk therefore reads the
    /// manifests — and asserts away the one shape it could not see, a crate that
    /// lets cargo discover an undeclared `src/main.rs` or `src/bin/`.
    fn workspace_binaries() -> BTreeSet<String> {
        let crates = repository_root().join("crates");
        let mut names = BTreeSet::new();

        for entry in std::fs::read_dir(&crates)
            .unwrap_or_else(|error| panic!("{}: {error}", crates.display()))
        {
            let dir = entry.expect("a readable directory entry").path();
            let manifest = dir.join("Cargo.toml");
            if !manifest.is_file() {
                continue;
            }

            let parsed: toml::Value = std::fs::read_to_string(&manifest)
                .unwrap_or_else(|error| panic!("{}: {error}", manifest.display()))
                .parse()
                .unwrap_or_else(|error| panic!("{}: {error}", manifest.display()));
            let declared: Vec<String> = parsed
                .get("bin")
                .and_then(toml::Value::as_array)
                .map(|bins| {
                    bins.iter()
                        .filter_map(|bin| Some(bin.get("name")?.as_str()?.to_string()))
                        .collect()
                })
                .unwrap_or_default();

            assert!(
                !dir.join("src/bin").is_dir(),
                "{}/src/bin holds binaries cargo discovers without a [[bin]] name, so this \
                 walk cannot see them and the image ships them undocumented",
                dir.display()
            );
            assert!(
                !declared.is_empty() || !dir.join("src/main.rs").is_file(),
                "{} has a src/main.rs and declares no [[bin]], so cargo discovers a binary \
                 under the package name that this walk cannot see — declare it",
                dir.display()
            );

            names.extend(declared);
        }

        names
    }

    /// `deploy/README.md` tells an operator which binaries are in the image, and
    /// the image's own answer is `cargo metadata`'s: the Dockerfile copies every
    /// bin target the workspace declares, deliberately, so that a new binary is
    /// opt-out rather than opt-in. The table is the only thing that stayed
    /// opt-in — a binary added to the workspace ships undocumented, and a
    /// renamed one leaves the table naming something the image does not have.
    #[test]
    fn the_deploy_guide_lists_every_binary_the_image_ships() {
        let documented: BTreeSet<String> =
            first_table_cells(DEPLOY_README_MD, RUNTIME_BINARY_TABLE_LEAD)
                .into_iter()
                .filter_map(|cell| Some(cell.strip_prefix('`')?.strip_suffix('`')?.to_string()))
                .collect();

        // A floor, not a count: the header and separator rows carry no
        // backticks, so a table scanner that matched nothing else looks exactly
        // like a table with no binaries in it.
        assert!(
            documented.len() >= 3,
            "only {} binaries parsed out of the `{RUNTIME_BINARY_TABLE_LEAD}` table \
             ({documented:?}) — the table scanner has stopped reading its rows",
            documented.len()
        );

        assert_eq!(
            documented,
            workspace_binaries(),
            "the `{RUNTIME_BINARY_TABLE_LEAD}` table and the workspace's [[bin]] targets \
             disagree, and the Dockerfile ships the [[bin]] targets. Fix the table."
        );
    }

    /// Every leaf subcommand `forgekeep.toml` reaches: one that takes
    /// `--config` together with a knob that file feeds (`--db-url` or
    /// `--repo-root`).
    ///
    /// The second half is what keeps the deprecated `runner` alias out — its
    /// `--config` is `runner.toml`, a different file with a different model.
    fn config_backed_subcommands() -> BTreeSet<String> {
        subcommand_flags()
            .into_iter()
            .filter(|(_, accepted)| {
                accepted.contains("--config")
                    && (accepted.contains("--db-url") || accepted.contains("--repo-root"))
            })
            .filter_map(|(path, _)| path.strip_prefix("forgekeep ").map(str::to_string))
            .collect()
    }

    const ARCHITECTURE_MD: &str = include_str!("../../../ARCHITECTURE.md");

    /// Every page that promises the `CLI arg > config file > built-in default`
    /// order to a set of subcommands and then names the set, as
    /// `(page, text, lead, close)`.
    ///
    /// Two pages, one sentence each, saying the same thing about the same nine
    /// subcommands — the README for the operator, `ARCHITECTURE.md` for whoever
    /// adds the tenth. That is the shape of the drift: the knowledge is
    /// mechanical, the copies are prose, and nothing joined them to the
    /// declaration they describe.
    const CONFIG_BACKED_CLAIMS: [(&str, &str, &str, &str); 2] = [
        (
            "README.md",
            README_MD,
            "Every subcommand that touches the database or the repository directory",
            ")",
        ),
        (
            "ARCHITECTURE.md",
            ARCHITECTURE_MD,
            "shared by **every** subcommand, not just `serve`:",
            " all take",
        ),
    ];

    /// The wider version of the deploy guide's promise — not just
    /// `[database].url` but `--db-url` / `--repo-root` resolving as
    /// `CLI arg > config file > built-in default`. Same class as the deploy
    /// guide's list, same mechanical set: a subcommand belongs exactly when clap
    /// gives it `--config` plus one of the two knobs that file feeds.
    #[test]
    fn every_page_that_lists_the_config_backed_subcommands_lists_all_of_them() {
        let expected: BTreeSet<String> = config_backed_subcommands()
            .into_iter()
            // Named by both sentences in their own right — "the same `--config`
            // as `serve`", "not just `serve`" — and so not one of the
            // subcommands either of them goes on to list.
            .filter(|name| name != "serve")
            .collect();

        for (page, text, lead, close) in CONFIG_BACKED_CLAIMS {
            let documented = names_listed_between(text, lead, close);
            assert!(
                documented.len() >= 8,
                "only {} names parsed out of `{page}`'s `{lead}` sentence ({documented:?}) — \
                 the scanner has stopped reading the list it makes",
                documented.len()
            );

            assert_eq!(
                documented, expected,
                "{page} promises the `CLI arg > config file > built-in default` order to \
                 every subcommand it lists here, and the list no longer matches the \
                 subcommands clap backs with a `forgekeep.toml` knob. Fix the sentence, or \
                 the declaration."
            );
        }
    }

    /// The line that introduces the README's inventory of subcommands.
    const CLI_TABLE_LEAD: &str = "Beyond `serve`, the `forgekeep` binary offers:";

    /// The README's CLI table claims to be an inventory — "the `forgekeep`
    /// binary offers" — so a subcommand missing from it is a feature an operator
    /// has no way to learn about, and a row clap no longer has is a command that
    /// fails on the first try.
    ///
    /// Top-level names only: the table's rows are what an operator scans for,
    /// and `package`'s children are described in its own row's prose.
    #[test]
    fn the_readme_table_lists_every_subcommand_the_binary_offers() {
        // Rows name more than one command (`backup-db` / `restore-db`) and carry
        // their arguments (`index-repo <owner/name>`), so a row contributes the
        // first word of each of its first cell's code spans.
        let documented: BTreeSet<String> = first_table_cells(README_MD, CLI_TABLE_LEAD)
            .into_iter()
            .flat_map(|cell| {
                cell.split('`')
                    .skip(1)
                    .step_by(2)
                    .filter_map(|span| span.split_whitespace().next())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .collect();

        let offered: BTreeSet<String> = Cli::command()
            .get_subcommands()
            .map(|sub| sub.get_name().to_string())
            .collect();

        assert!(
            documented.len() >= 10,
            "only {} commands parsed out of the `{CLI_TABLE_LEAD}` table ({documented:?}) — \
             the table scanner has stopped reading its rows",
            documented.len()
        );

        assert_eq!(
            documented, offered,
            "the README's CLI table says it is what the binary offers, and it no longer is. \
             A subcommand missing from it is one an operator cannot discover; a row clap \
             does not have is `error: unrecognized subcommand` on the first try."
        );
    }

    /// `FLAGLESS_INVOCATIONS` is another hand-written copy of the same
    /// knowledge. The two tests that iterate it are only as complete as it is,
    /// so a config-backed subcommand nobody adds to it gets neither — and what
    /// those tests catch is the clap `default_value` that made the config file
    /// unreachable in the first place.
    #[test]
    fn flagless_invocations_lists_every_config_backed_subcommand() {
        let by_subcommand = subcommand_flags();

        let expected: BTreeSet<String> = config_backed_subcommands()
            .into_iter()
            .map(|name| format!("forgekeep {name}"))
            .collect();

        let listed: BTreeSet<String> = FLAGLESS_INVOCATIONS
            .iter()
            .map(|argv| {
                let typed = argv.join(" ");
                by_subcommand
                    .keys()
                    .filter(|leaf| {
                        typed
                            .strip_prefix(leaf.as_str())
                            .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
                    })
                    // Longest wins: `forgekeep package list …` starts with no
                    // other leaf, but a future `forgekeep package` leaf would.
                    .max_by_key(|leaf| leaf.len())
                    .unwrap_or_else(|| {
                        panic!("`{typed}` invokes no leaf subcommand of this binary")
                    })
                    .clone()
            })
            .collect();
        assert_eq!(
            listed.len(),
            FLAGLESS_INVOCATIONS.len(),
            "two entries of FLAGLESS_INVOCATIONS invoke the same subcommand, so one of them \
             is standing in for a subcommand nothing checks"
        );

        assert_eq!(
            listed, expected,
            "FLAGLESS_INVOCATIONS is what `config_backed_flags_have_no_clap_default` and \
             `every_config_backed_subcommand_accepts_a_config_flag` iterate, and it no \
             longer matches the subcommands clap backs with a `forgekeep.toml` knob. One \
             missing here is a subcommand whose `--config` nothing checks; one listed here \
             that clap no longer backs is a stale row."
        );
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

    // ---------------------------------------------------------------------
    // The next rung of the same contract. The checks above pin the *names* an
    // operator reads — flags, config keys, environment variables. What none of
    // them looks at is the *values* those same lines promise: the 21 `[default:
    // …]` notes in the help text and the README table's `Default` column.
    //
    // A flag with a config-file equivalent deliberately carries no clap
    // `default_value` (with one, the config file could never win over "the flag
    // was not passed"), so clap cannot print the default itself. Every
    // `[default: 5]` is therefore a number a person typed beside
    // `DEFAULT_LOG_MAX_FILES`, joined to it by nothing but memory. Changing the
    // constant leaves the old number on both pages, and the operator sizes a
    // deployment — or a threat model, for `[rate_limit].max` — around a value
    // the server will not use.
    // ---------------------------------------------------------------------

    /// The production half of `config.rs`, where the built-in defaults live.
    fn production_config_source() -> &'static str {
        include_str!("config.rs")
            .split_once("\n#[cfg(test)]\n")
            .map(|(production, _)| production)
            .expect("config.rs must keep its test module behind #[cfg(test)]")
    }

    /// A built-in default that `--help` and the README both state, bound to the
    /// constant that actually produces it.
    struct DocumentedDefault {
        /// The `forgekeep.toml` section named by the flag's `[config: …]` marker,
        section: &'static str,
        /// …and the key inside it. Together they locate the help lines to check.
        key: &'static str,
        /// The flag as `README.md`'s `serve` table spells it.
        flag: &'static str,
        /// The `config::DEFAULT_*` this row pairs, for the census below.
        constant: &'static str,
        /// Its value, read from the constant rather than copied beside it.
        value: String,
    }

    /// The pairing table. The section/key/flag spellings have to be written out
    /// — no rule derives `DEFAULT_DB_URL` from `[database].url` — but the
    /// *values* never are: each row reads its constant, so renaming one breaks
    /// the build and changing one fails the test.
    fn documented_defaults() -> Vec<DocumentedDefault> {
        macro_rules! defaults {
            ($(($section:literal, $key:literal, $flag:literal, $konst:ident)),+ $(,)?) => {
                vec![$(DocumentedDefault {
                    section: $section,
                    key: $key,
                    flag: $flag,
                    constant: stringify!($konst),
                    value: crate::config::$konst.to_string(),
                }),+]
            };
        }

        defaults![
            ("server", "repo_root", "--repo-root", DEFAULT_REPO_ROOT),
            ("server", "http_addr", "--http-addr", DEFAULT_HTTP_ADDR),
            ("server", "ssh_addr", "--ssh-addr", DEFAULT_SSH_ADDR),
            ("database", "url", "--db-url", DEFAULT_DB_URL),
            ("smtp", "port", "--smtp-port", DEFAULT_SMTP_PORT),
            (
                "rate_limit",
                "max",
                "--rate-limit-max",
                DEFAULT_RATE_LIMIT_MAX
            ),
            (
                "rate_limit",
                "window_secs",
                "--rate-limit-window",
                DEFAULT_RATE_LIMIT_WINDOW
            ),
            (
                "logging",
                "max_size_mb",
                "--log-max-size-mb",
                DEFAULT_LOG_MAX_SIZE_MB
            ),
            (
                "logging",
                "max_files",
                "--log-max-files",
                DEFAULT_LOG_MAX_FILES
            ),
        ]
    }

    /// Built-in defaults with no operator-facing spelling, each with the reason.
    /// The list exists so that a new `DEFAULT_*` nobody documented is a decision
    /// someone made, rather than something that quietly escaped both pages.
    const NOT_NAMED_IN_HELP: [(&str, &str); 13] = [
        (
            "DEFAULT_PACKAGE_UPLOAD_MAX_MB",
            "config-file-only: `[server].package_upload_max_mb` has no CLI flag, so no help \
             text names it, and its value is derived from \
             `rg_http::DEFAULT_PACKAGE_UPLOAD_MAX_BYTES` rather than written out",
        ),
        // The `serve` knobs that exist only in the config file. Every one of
        // them IS held to an operator-facing document — the shipped templates
        // and their prose, by `config.rs` — just not to a `--help` paragraph,
        // because there is no flag to hang one on.
        (
            "DEFAULT_CI_DOCKER",
            "config-file-only: `[ci].docker` has a `--docker` flag, but the flag is a bare \
             switch whose absence *is* the default, so its help states no value",
        ),
        (
            "DEFAULT_CI_EXTERNAL_RUNNERS",
            "config-file-only: `--external-runners` is a bare switch, as above",
        ),
        (
            "DEFAULT_CI_ALLOW_HOST_RUNNER",
            "config-file-only: `--allow-host-runner` is a bare switch, as above",
        ),
        (
            "DEFAULT_ATTESTATION_ENABLED",
            "config-file-only: `[releases].attestation_enabled` has no CLI flag (it is \
             settable as FORGEKEEP_ATTESTATION_ENABLED instead)",
        ),
        (
            "DEFAULT_RATE_LIMIT_MAX_KEYS",
            "config-file-only: `[rate_limit].max_keys` has no CLI flag, and 0 is a sentinel \
             meaning `rg_http::rate_limit::DEFAULT_MAX_KEYS` rather than a cap",
        ),
        (
            "DEFAULT_AUTH_RATE_LIMIT_MAX",
            "config-file-only: `[rate_limit].auth_max` has no CLI flag",
        ),
        (
            "DEFAULT_AUTH_RATE_LIMIT_WINDOW",
            "config-file-only: `[rate_limit].auth_window_secs` has no CLI flag",
        ),
        (
            "DEFAULT_AUDIT_ENABLED",
            "config-file-only: `[audit].enabled` has no CLI flag",
        ),
        (
            "DEFAULT_AUDIT_ARCHIVE_DIR",
            "config-file-only: `[audit].archive_dir` has no CLI flag, and this constant is \
             only the fallback for a relative repo_root — an absolute one puts the archive \
             beside it instead",
        ),
        (
            "DEFAULT_BACKUP_ENABLED",
            "config-file-only: `[backup].enabled` has no CLI flag",
        ),
        (
            "DEFAULT_DB_BACKUP_DIR",
            "config-file-only: `[backup].dir` has no CLI flag, and this constant is only \
             the fallback for a relative repo_root",
        ),
        (
            "DEFAULT_MIRROR_ENABLED",
            "config-file-only: `[mirror].enabled` has no CLI flag",
        ),
    ];

    /// The `pub(crate) const DEFAULT_*` names `source` declares. Reading the
    /// declarations rather than keeping a list beside them is the whole point:
    /// a constant added to the model joins the census by existing.
    fn declared_default_constants(source: &str) -> BTreeSet<&str> {
        source
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("pub(crate) const "))
            .filter_map(|rest| rest.split_once(':'))
            .map(|(name, _)| name.trim())
            .filter(|name| name.starts_with("DEFAULT_"))
            .collect()
    }

    /// Does the help paragraph that starts at `line_no` promise `[default:
    /// <value>]`? The note sits either on the marker's own line or wraps onto
    /// the next one — both spellings occur in `cli.rs`, and clap joins them into
    /// one paragraph either way.
    fn help_promises_default(lines: &[&str], line_no: usize, value: &str) -> bool {
        let start = line_no - 1;
        lines[start..lines.len().min(start + 2)]
            .join(" ")
            .contains(&format!("[default: {value}]"))
    }

    /// Every flag whose `--help` names a config-file equivalent also states the
    /// built-in default that applies when neither is set. Nothing checks that
    /// number against the constant that produces it.
    #[test]
    fn every_default_the_help_text_promises_is_the_constant_that_produces_it() {
        // The reader has to be able to answer "no" before its "yes" is worth
        // anything, and it has to see the wrapped spelling as well as the inline
        // one — `cli.rs` uses both.
        let sample = [
            "/// HTTP listen address [config: [server].http_addr]",
            "/// [default: 0.0.0.0:8080]",
            "/// Database URL [config: [database].url] [default: sqlite://probe]",
        ];
        assert!(
            help_promises_default(&sample, 1, "0.0.0.0:8080"),
            "the reader misses a `[default: …]` note wrapped onto the next line"
        );
        assert!(
            help_promises_default(&sample, 3, "sqlite://probe"),
            "the reader misses a `[default: …]` note on the marker's own line"
        );
        assert!(
            !help_promises_default(&sample, 3, "0.0.0.0:8080"),
            "the reader accepts a default the paragraph does not state, so its \
             agreement means nothing"
        );

        let source = production_cli_source();
        let lines: Vec<&str> = source.lines().collect();
        let (markers, _) = help_config_markers(source);
        let documented = documented_defaults();
        let mut checked = 0;

        for entry in &documented {
            let mut seen = 0;
            for &(line_no, section, key) in &markers {
                if section != entry.section || key != entry.key {
                    continue;
                }
                seen += 1;
                assert!(
                    help_promises_default(&lines, line_no, &entry.value),
                    "cli.rs:{line_no}: `--help` points this flag at `[{section}].{key}` \
                     but does not promise `[default: {}]` — `config::{}` is what the \
                     server actually falls back to, so the help text states a value it \
                     will not use",
                    entry.value,
                    entry.constant
                );
            }
            assert!(
                seen > 0,
                "no `[config: [{}].{}]` marker is left in cli.rs, so nothing pins \
                 `config::{}` to the help text — drop the row or restore the marker",
                entry.section,
                entry.key,
                entry.constant
            );
            checked += seen;
        }

        // A floor, not a count: it fails loudly if the marker scan or the
        // pairing table stops matching and the test quietly checks a handful.
        assert!(
            checked >= 20,
            "only {checked} help markers were matched against a `config::DEFAULT_*` — \
             the pairing table has drifted away from the help text"
        );
    }

    /// The mirror of the check above: that one asks that every documented
    /// default is right, this asks that every default is documented — or is
    /// excused on purpose. Without it the pairing table rots the moment a knob
    /// is added, and the drift the tests exist to catch walks straight past
    /// them.
    #[test]
    fn every_default_constant_is_either_named_in_help_or_excused() {
        assert_eq!(
            declared_default_constants(
                "pub(crate) const DEFAULT_X: u8 = 1;\npub(crate) const OTHER: u8 = 2;\n"
            ),
            BTreeSet::from(["DEFAULT_X"]),
            "the declaration scan does not read `pub(crate) const DEFAULT_*` the way \
             config.rs writes it"
        );

        let declared = declared_default_constants(production_config_source());
        assert!(
            declared.len() >= 9,
            "only {} `DEFAULT_*` constants found in config.rs — the declaration scan \
             has stopped matching them",
            declared.len()
        );

        let paired: BTreeSet<&str> = documented_defaults()
            .iter()
            .map(|entry| entry.constant)
            .collect();

        for name in &declared {
            let excused = NOT_NAMED_IN_HELP
                .iter()
                .any(|&(excused, _)| excused == *name);
            assert!(
                paired.contains(name) || excused,
                "`config::{name}` is a built-in default that no row of \
                 documented_defaults() pins to a `[config: …]` marker — pair it with the \
                 flag whose `--help` promises it, or name it in NOT_NAMED_IN_HELP with \
                 the reason it has no operator-facing spelling"
            );
        }

        for (name, _) in NOT_NAMED_IN_HELP {
            assert!(
                declared.contains(name),
                "NOT_NAMED_IN_HELP still excuses `{name}`, which config.rs no longer \
                 declares — drop the entry so the list keeps meaning something"
            );
        }

        // Renaming a paired constant breaks the build, but *moving* one out of
        // config.rs would not: it would simply leave the census, taking its row
        // with it.
        for name in paired {
            assert!(
                declared.contains(name),
                "documented_defaults() pairs `config::{name}`, which config.rs no longer \
                 declares — the census reads that one file, so a constant that moved \
                 elsewhere escapes it"
            );
        }
    }

    /// The backtick-quoted tokens of a markdown cell — how the README's
    /// `Default` column spells every value it states. Tokens rather than a
    /// substring search: `0` is a substring of `10`, and a column reading
    /// `— / \`5\`` states one default and withholds another.
    fn quoted_tokens(cell: &str) -> BTreeSet<&str> {
        cell.split('`').skip(1).step_by(2).collect()
    }

    /// The README repeats the same defaults a third time, in a column an
    /// operator reads *before* running anything — and, unlike the help text,
    /// without the flag beside it to make a stale number look suspicious.
    #[test]
    fn every_default_the_readme_table_states_is_the_constant_that_produces_it() {
        assert_eq!(
            quoted_tokens("`0` / `60`"),
            BTreeSet::from(["0", "60"]),
            "the cell reader does not split the README's multi-value `Default` column"
        );
        assert!(
            !quoted_tokens("`10`").contains("0"),
            "the cell reader matches a substring of a stated default, so `0` would be \
             satisfied by `10`"
        );

        let documented = documented_defaults();
        let mut checked = 0;

        for row in first_table_rows(README_MD, SERVE_FLAG_TABLE_LEAD) {
            let (Some(flags), Some(stated)) = (row.first(), row.get(2)) else {
                continue;
            };
            let named = long_flags(flags);
            let stated = quoted_tokens(stated);

            for entry in &documented {
                if !named.contains(entry.flag) {
                    continue;
                }
                assert!(
                    stated.contains(entry.value.as_str()),
                    "README.md's `serve` table gives `{}` the default {stated:?}, and \
                     `config::{}` is `{}` — an operator plans a deployment around the \
                     number on the page, and for a limit like `[rate_limit].max` that \
                     number is a threat model",
                    entry.flag,
                    entry.constant,
                    entry.value
                );
                checked += 1;
            }
        }

        // A floor, not a count: the table names fewer flags than `serve` has, so
        // a silent drop to zero matches would otherwise read as agreement.
        assert!(
            checked >= 7,
            "only {checked} rows of the README's `serve` table were matched against a \
             `config::DEFAULT_*` — the table scanner or the flag spellings have drifted"
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

    // ---------------------------------------------------------------------
    // The mirror of the check above. That one asks that every documented
    // variable is real; this one asks that every real variable is documented.
    //
    // Only one of the two directions was ever checked, and it is the cheaper
    // one: a documented variable that stopped existing wastes an afternoon,
    // while an *undocumented* one cannot be found at all. Two of the three
    // shipped binaries were configured entirely by variables in this state —
    // `forgekeep-runner` will not register without `FORGEKEEP_AUTH_TOKEN`, and
    // `forgekeep-mcp` has no configuration besides `FORGEKEEP_URL` /
    // `FORGEKEEP_PAT` — and the only place either was written down was the
    // source, or a `//!` comment aimed at whoever edits it.
    // ---------------------------------------------------------------------

    /// The pages an operator reads *before* setting anything. `include_str!`
    /// rather than a runtime read: a renamed or moved document breaks the build
    /// instead of quietly leaving the census with nothing to match against.
    const OPERATOR_DOCUMENTS: [(&str, &str); 4] = [
        ("README.md", README_MD),
        ("deploy/README.md", DEPLOY_README_MD),
        (
            "forgekeep.example.toml",
            include_str!("../../../forgekeep.example.toml"),
        ),
        ("deploy/.env.example", ENV_EXAMPLE),
    ];

    /// The file an operator copies to `deploy/.env` before the first `up`.
    const ENV_EXAMPLE: &str = include_str!("../../../deploy/.env.example");

    /// A character that can appear inside an environment-variable name, used to
    /// keep both scans below off the substrings of longer names.
    fn is_name_char(c: char) -> bool {
        c.is_ascii_alphanumeric() || c == '_'
    }

    /// The variable name that starts at the front of `text`, if there is one.
    fn env_var_at(text: &str) -> Option<&str> {
        let end = text
            .char_indices()
            .find(|(_, c)| !is_name_char(*c))
            .map_or(text.len(), |(offset, _)| offset);
        (end > 0).then(|| &text[..end])
    }

    /// Does `text` name `variable` as a whole word? A document answers "where
    /// do I read about this" in any spelling — a table cell, a `.env` line, a
    /// sentence — but `FORGEKEEP_URL` must not be answered by a page that only
    /// mentions `FORGEKEEP_URLS`.
    fn names_the_variable(text: &str, variable: &str) -> bool {
        text.match_indices(variable).any(|(index, _)| {
            let before = text[..index].chars().next_back();
            let after = text[index + variable.len()..].chars().next();
            !before.is_some_and(is_name_char) && !after.is_some_and(is_name_char)
        })
    }

    /// Every environment variable production code reads, with the file that
    /// reads it.
    ///
    /// Two rules, because a read is written in two ways. Inside the project's
    /// own `FORGEKEEP_` prefix any quoted literal counts: the server's
    /// variables travel through helpers (`env_secret("FORGEKEEP_JWT_SECRET")`,
    /// `credentials::USERNAME_ENV`), so a census pinned to `env::var` would miss
    /// most of them. Outside that prefix the read has to be an actual
    /// `env::var(` / `env::var_os(` call site — a bare `"PATH"` in a source file
    /// is far more likely to be a map key than a variable.
    ///
    /// Either way the quotes are what separate a read from prose: help text and
    /// error messages name these variables constantly, and none of those
    /// mentions makes one do anything. Same rule
    /// [`source_reading_env_var`] applies from the other side, run as a census
    /// rather than as a lookup.
    fn env_vars_read_by_the_code(sources: &[(PathBuf, String)]) -> BTreeMap<String, PathBuf> {
        const CALL_SITES: [&str; 2] = ["env::var(\"", "env::var_os(\""];

        let mut read = BTreeMap::new();

        for (path, text) in sources {
            for line in text.lines() {
                if line.trim_start().starts_with("//") {
                    continue;
                }

                let starts = line
                    .match_indices("\"FORGEKEEP_")
                    .map(|(index, _)| index + 1)
                    .chain(CALL_SITES.iter().flat_map(|opening| {
                        line.match_indices(opening)
                            .map(|(index, _)| index + opening.len())
                    }));

                for start in starts {
                    let rest = &line[start..];
                    let Some(name) = env_var_at(rest) else {
                        continue;
                    };
                    if rest[name.len()..].starts_with('"') {
                        read.entry(name.to_owned()).or_insert_with(|| path.clone());
                    }
                }
            }
        }
        read
    }

    /// Variables the code reads that no operator is meant to set, each with the
    /// reason. The list exists so that the next undocumented variable is a
    /// decision someone made, rather than one that quietly escaped every page.
    const NOT_OPERATOR_FACING: [(&str, &str); 6] = [
        (
            "FORGEKEEP_GIT_USERNAME",
            "internal: the server exports it into the `git` subprocess it spawns and reads it \
             back through the credential helper — an operator never sets it, and setting it \
             would only be overwritten",
        ),
        (
            "FORGEKEEP_GIT_PASSWORD",
            "internal: the other half of the same credential handoff to the `git` subprocess",
        ),
        (
            "FORGEKEEP_NATIVE_INDEX_PACK",
            "developer toggle: an opt-in, default-off PoC of native pack indexing, described \
             in `docs/git-protocol.md` beside the code it switches — not a supported \
             deployment knob",
        ),
        (
            "HOME",
            "inherited: read to expand a leading `~` in a path the operator gave, not a knob \
             ForgeKeep asks anyone to set",
        ),
        (
            "PATH",
            "inherited: forwarded into CI job processes so the tools on the machine stay \
             reachable — the value is the machine's, not a ForgeKeep setting",
        ),
        (
            "LANG",
            "inherited: forwarded into CI job processes alongside `PATH`, for the same reason",
        ),
    ];

    /// A variable that configures a shipped binary and is named on no page an
    /// operator reads can only be found by reading the source — which is not
    /// something the person wiring `forgekeep-mcp` into an agent, or registering
    /// a runner on a build machine, has open.
    #[test]
    fn every_environment_variable_the_code_reads_is_named_in_an_operator_document() {
        // Both scanners have to be able to answer "no" before their "yes" means
        // anything: one that matches everything is vacuously green.
        assert!(
            names_the_variable("set `FORGEKEEP_PAT` in the agent's env", "FORGEKEEP_PAT"),
            "the document scanner does not see a variable the pages name in backticks"
        );
        assert!(
            !names_the_variable("set FORGEKEEP_PAT_FILE instead", "FORGEKEEP_PAT"),
            "the document scanner accepts a longer name as a mention of a shorter one, so a \
             page that documents neither can still pass for one that documents both"
        );

        let probe = env_vars_read_by_the_code(&[(
            PathBuf::from("probe.rs"),
            "let a = env_secret(\"FORGEKEEP_REAL\");\n\
             let b = std::env::var(\"OTEL_REAL\").ok();\n\
             // \"FORGEKEEP_COMMENTED\"\n\
             let prose = \"see $FORGEKEEP_SHELL\";\n\
             let map = json!({ \"PATH\": 1 });\n"
                .to_owned(),
        )]);
        assert_eq!(
            probe.into_keys().collect::<Vec<_>>(),
            vec!["FORGEKEEP_REAL".to_string(), "OTEL_REAL".to_string()],
            "the source census counts prose, comments and plain string keys as reads, so it \
             cannot tell a variable the code uses from one it merely mentions"
        );

        let sources = production_workspace_sources();
        let read = env_vars_read_by_the_code(&sources);
        assert!(
            read.len() >= 15,
            "only {} environment variables found in the workspace sources — the census has \
             stopped matching them",
            read.len()
        );

        let mut documented = 0;
        for (variable, path) in &read {
            if let Some((_, reason)) = NOT_OPERATOR_FACING
                .iter()
                .find(|(excused, _)| excused == variable)
            {
                assert!(!reason.is_empty(), "{variable} is excused without a reason");
                continue;
            }
            assert!(
                OPERATOR_DOCUMENTS
                    .iter()
                    .any(|(_, content)| names_the_variable(content, variable)),
                "{} reads `{variable}`, and none of the {} operator documents names it — the \
                 only way to learn the variable exists is to read that file. Document it, or \
                 name it in NOT_OPERATOR_FACING with the reason nobody outside the code is \
                 meant to set it.",
                path.display(),
                OPERATOR_DOCUMENTS.len()
            );
            documented += 1;
        }

        // A floor, not a count: it fails loudly if the document scan starts
        // matching nothing and the loop above quietly agrees with itself.
        assert!(
            documented >= 8,
            "only {documented} variables were matched against an operator document — the \
             scan has drifted away from how the pages spell them"
        );

        // An excuse that outlives the read it excuses is how this list rots into
        // a place to hide the next undocumented variable.
        for (variable, _) in NOT_OPERATOR_FACING {
            assert!(
                read.contains_key(variable),
                "NOT_OPERATOR_FACING still excuses `{variable}`, which no production source \
                 reads any more — drop the entry so the list keeps meaning something"
            );
        }
    }

    // ---------------------------------------------------------------------
    // The third env surface, and the one neither check above can see.
    //
    // Both censuses above run between a document and the *Rust* sources, and
    // four of the variables `deploy/.env.example` offers — `FORGEKEEP_UID`,
    // `FORGEKEEP_GID`, `FORGEKEEP_HTTP_PORT`, `FORGEKEEP_SSH_PORT` — are never
    // read by any Rust source at all. Their only consumer is a `${NAME}`
    // substitution inside a compose file, and nothing tied the two files
    // together: renaming `${FORGEKEEP_HTTP_PORT}` in the compose file leaves
    // the `.env.example` line looking alive and doing nothing. The symptom is
    // milder than the rest of this phase's — not a refused start but a setting
    // silently ignored — and it shows up only as a port that will not change.
    // ---------------------------------------------------------------------

    /// The shipped compose files, taken out of [`deployment_files`] — the same
    /// run-time walk of `deploy/`, so a compose file added tomorrow joins this
    /// contract by existing too.
    ///
    /// Narrowed to compose because a compose file is the only member of that
    /// set that *expands* a variable: the `Dockerfile` declares build args of
    /// its own, and `deploy/README.md` quotes both in prose. Counting a page
    /// that merely writes a variable down as its consumer is exactly the
    /// mistake this pair of checks exists to catch.
    fn deploy_compose_files() -> Vec<(String, String)> {
        deployment_files()
            .into_iter()
            .filter(|(name, _)| name.starts_with("deploy/docker-compose"))
            .collect()
    }

    /// Is `name` spelled the way an environment variable is? Uppercase is what
    /// separates an offer from a sentence: a `.env` template is half prose, and
    /// the prose contains `=` too.
    fn is_env_var_name(name: &str) -> bool {
        name.starts_with(|c: char| c.is_ascii_uppercase())
            && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    }

    /// The variables `text` *offers*, in either of the two spellings a `.env`
    /// template uses: `NAME=value` for the ones an operator must fill in, and
    /// `# NAME=value` for the optional ones shipped commented out. Both are
    /// offers, so both need a consumer — a commented line is how this file
    /// says "set this if you need it", not how it says "this is dead".
    ///
    /// The name has to open the line. The file also *talks about* variables —
    /// `echo "FORGEKEEP_UID=$(id -u)" >> .env` sits in a comment two lines
    /// above the offer itself — and a snippet showing how to append one is not
    /// a second offer of it.
    fn env_vars_offered_in(text: &str) -> BTreeSet<String> {
        let mut offered = BTreeSet::new();

        for line in text.lines() {
            let line = line.trim_start();
            let line = line.strip_prefix('#').map_or(line, str::trim_start);
            let Some((name, _)) = line.split_once('=') else {
                continue;
            };
            if is_env_var_name(name) {
                offered.insert(name.to_owned());
            }
        }

        offered
    }

    /// Every variable a compose file expands, with the file that expands it.
    ///
    /// Commented lines count, deliberately. The `runner` service in both
    /// compose files ships commented out and `deploy/.env.example` offers its
    /// credentials under "the commented `runner` service" — uncommenting is
    /// the documented way to turn it on, so a `${FORGEKEEP_RUNNER_TOKEN}` that
    /// only exists behind a `#` is still the consumer of that offer.
    fn env_vars_substituted_by_compose(files: &[(String, String)]) -> BTreeMap<String, String> {
        let mut substituted = BTreeMap::new();

        for (name, text) in files {
            for (index, _) in text.match_indices("${") {
                let Some(variable) = env_var_at(&text[index + 2..]) else {
                    continue;
                };
                substituted
                    .entry(variable.to_owned())
                    .or_insert_with(|| name.clone());
            }
        }

        substituted
    }

    /// A variable offered by `deploy/.env.example` that nothing consumes is the
    /// quietest kind of wrong: the operator sets it, the deploy comes up, and
    /// the setting is simply not there. Nothing about the file distinguishes
    /// the four variables that only a compose substitution reads from the six
    /// the server reads itself, so nothing about it survives renaming one.
    #[test]
    fn every_variable_deploy_env_example_offers_has_a_consumer() {
        // The scanner has to be able to answer "no" before its "yes" is worth
        // anything: one that matches every line is vacuously green.
        let probe = env_vars_offered_in(
            "FORGEKEEP_REAL=1\n\
             # FORGEKEEP_OPTIONAL=2\n\
             #   echo \"FORGEKEEP_APPENDED=$(id -u)\" >> .env\n\
             # Empty = the JWT secret is used.\n\
             # FORGEKEEP_PROSE is named here but never offered.\n",
        );
        assert_eq!(
            probe.into_iter().collect::<Vec<_>>(),
            vec![
                "FORGEKEEP_OPTIONAL".to_string(),
                "FORGEKEEP_REAL".to_string()
            ],
            "the `.env` scanner counts prose and shell snippets as offers, so it cannot tell \
             a variable the file offers from one it merely mentions"
        );

        let offered = env_vars_offered_in(ENV_EXAMPLE);
        assert!(
            offered.len() >= 10,
            "only {} variables found in deploy/.env.example — the scanner has stopped \
             matching the way the file spells them",
            offered.len()
        );

        let compose = deploy_compose_files();
        assert!(
            compose.len() >= 3,
            "the deploy/ walk found only {} compose files — it is looking in the wrong place",
            compose.len()
        );

        let sources = production_workspace_sources();
        let substituted = env_vars_substituted_by_compose(&compose);

        for variable in &offered {
            assert!(
                source_reading_env_var(&sources, variable).is_some()
                    || substituted.contains_key(variable),
                "deploy/.env.example offers `{variable}`, and nothing consumes it: no \
                 production source under crates/ names it, and no deploy/docker-compose*.yml \
                 expands `${{{variable}}}`. The variable was renamed on the consuming side, \
                 and setting it now silently does nothing — give it a consumer or drop the \
                 line"
            );
        }
    }

    /// The mirror. A `${NAME}` a compose file expands and `.env.example` never
    /// offers is the same drift read from the other end: compose expands an
    /// unset variable to the empty string without a word, so the operator's
    /// copied `.env` has no line to fill in and no way to learn one was wanted.
    #[test]
    fn every_variable_the_deploy_compose_files_substitute_is_offered_in_env_example() {
        let probe = env_vars_substituted_by_compose(&[(
            "probe.yml".to_string(),
            "      - \"127.0.0.1:${FORGEKEEP_PORT:-8080}:8080\"\n\
             #     --runner-id ${FORGEKEEP_COMMENTED}\n\
             # prose about $FORGEKEEP_BARE and FORGEKEEP_NAKED\n\
             - '--collector.filesystem.mount-points-exclude=^/(sys|proc)($$|/)'\n"
                .to_owned(),
        )]);
        assert_eq!(
            probe.into_keys().collect::<Vec<_>>(),
            vec![
                "FORGEKEEP_COMMENTED".to_string(),
                "FORGEKEEP_PORT".to_string()
            ],
            "the compose scanner does not read `${{NAME}}` the way compose does — it either \
             misses a substitution or counts a bare `$NAME` mention as one"
        );

        let compose = deploy_compose_files();
        assert!(
            compose.len() >= 3,
            "the deploy/ walk found only {} compose files — it is looking in the wrong place",
            compose.len()
        );

        let substituted = env_vars_substituted_by_compose(&compose);
        assert!(
            substituted.len() >= 6,
            "only {} substitutions found across the deploy compose files — the scanner has \
             stopped matching them",
            substituted.len()
        );

        let offered = env_vars_offered_in(ENV_EXAMPLE);
        for (variable, file) in &substituted {
            assert!(
                offered.contains(variable),
                "{file} expands `${{{variable}}}`, and deploy/.env.example never offers it — \
                 the operator copies a `.env` with no line for it, and compose substitutes \
                 the empty string without saying so"
            );
        }
    }
}
