//! Subcommand handlers dispatched from `main`, one `cmd_*` per CLI subcommand
//! (excluding `serve` and `runner`, which live in their own modules).

use std::path::PathBuf;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

use crate::admin;
use crate::cli::PackageCmd;
use crate::config;
use crate::dbconn;

/// Basic stderr logging used by the one-shot subcommands (and by the deprecated
/// `runner` alias, whose delegate reports through `tracing`).
pub(crate) fn init_cli_logging() {
    if let Err(error) = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .try_init()
    {
        tracing::debug!(%error, "CLI logging subscriber is already initialized");
    }
}

/// Resolve `--db-url` against `--config`, applying `CLI arg > config file >
/// built-in default`.
///
/// Every DB-touching subcommand goes through here so it addresses the same
/// database the server does. Before this existed, `--db-url` carried a clap
/// default and the config file was unreachable, so `forgekeep migrate` on a
/// Postgres deployment migrated a fresh, empty `./forgekeep.db` — with no error.
fn resolve_db_url(db_url: Option<String>, config: Option<String>) -> anyhow::Result<String> {
    let cfg = config::load_optional_config_file(config.as_deref())?;
    Ok(config::resolve_db_url(db_url, cfg.as_ref()))
}

/// [`resolve_db_url`] for the subcommands that need `--repo-root` as well, so a
/// repository is created/imported/indexed where the server looks for it.
fn resolve_db_url_and_repo_root(
    db_url: Option<String>,
    repo_root: Option<String>,
    config: Option<String>,
) -> anyhow::Result<(String, String)> {
    let cfg = config::load_optional_config_file(config.as_deref())?;
    Ok((
        config::resolve_db_url(db_url, cfg.as_ref()),
        config::resolve_repo_root(repo_root, cfg.as_ref()),
    ))
}

/// `forgekeep migrate` — run pending database migrations and exit.
pub(crate) async fn cmd_migrate(
    db_url: Option<String>,
    config: Option<String>,
) -> anyhow::Result<()> {
    init_cli_logging();

    let db_url = resolve_db_url(db_url, config)?;
    tracing::info!(
        "Connecting to database: {}",
        rg_db::redact_database_url(&db_url)
    );
    let db = dbconn::connect_offline_migration(&db_url).await?;
    tracing::info!("Running database migrations...");
    rg_db::run_migrations(db.connection()).await?;
    tracing::info!("Migrations complete ✅");
    Ok(())
}

/// `forgekeep gen-secret` — print a fresh JWT secret to stdout.
pub(crate) fn cmd_gen_secret() {
    // Print only the secret to stdout so it can be captured directly,
    // e.g. FORGEKEEP_JWT_SECRET="$(forgekeep gen-secret)".
    println!("{}", admin::generate_jwt_secret());
}

/// `forgekeep rotate-instance-key` — replace this instance's provenance
/// signing key.
///
/// The deliberate, destructive counterpart to the key's whole point. Since
/// card_3aecf3708ebe the Ed25519 identity that signs release attestations and
/// backs the CI OIDC JWKS is stored rather than derived from `jwt_secret`, so
/// rotating the signing secret no longer touches it — which also means a
/// *leaked* instance key has no other way out. This is that way out, and it is
/// explicit because everything the old key signed stops verifying.
pub(crate) async fn cmd_rotate_instance_key(
    db_url: Option<String>,
    config: Option<String>,
    jwt_secret: Option<String>,
    encryption_key: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    init_cli_logging();

    let cfg = config::load_optional_config_file(config.as_deref())?;
    let db_url = config::resolve_db_url(db_url, cfg.as_ref());
    let key_file = config::resolve_encryption_key_file(
        cfg.as_ref(),
        cfg.as_ref()
            .and_then(|config| config.server.host_key.as_deref()),
    );
    let resolved_encryption_key =
        crate::serve::resolve_auth_secrets(cfg.as_ref(), jwt_secret, encryption_key, &key_file)?
            .encryption_key;

    let db = dbconn::connect(&db_url).await?;
    rg_core::auth::key_check::verify_encryption_key(&db, &resolved_encryption_key).await?;
    let current = rg_db::ops::instance_signing_key_ops::find(&db)
        .await
        .context("read the current instance signing key")?;

    if !yes {
        let established = match current.as_ref() {
            Some(row) => format!("established {}", row.created_at.to_rfc3339()),
            None => "not established yet".to_string(),
        };
        anyhow::bail!(
            "refusing to rotate the instance signing key without --yes ({established}).\n\
             \n\
             Rotating mints a new Ed25519 identity for this instance. Every release \
             attestation signed with the current key stops verifying — permanently, for \
             everyone — and every external verifier that fetched /api/v1/ci/oidc/jwks has to \
             refetch it. Do this when the key itself is compromised, not to recover from a \
             rotated jwt_secret: the signing secret and this key have been independent since \
             card_3aecf3708ebe."
        );
    }

    let rotated = rg_core::auth::instance_key::rotate(&db, &resolved_encryption_key).await?;
    println!("{}", rotated.kid());
    tracing::warn!(
        kid = %rotated.kid(),
        "instance signing key rotated; previously signed attestations no longer verify"
    );
    Ok(())
}

/// `forgekeep rotate-encryption-key` — move every at-rest secret onto a new
/// at-rest encryption key.
///
/// The missing half of card_d740512de0a8. Splitting `encryption_key` out of
/// `jwt_secret` made rotating the *signing* secret safe, and the startup
/// preflight made a wrong encryption key loud — but the encryption key itself
/// still had no way out: the README told operators to treat it as permanent for
/// the life of the database, which for a leaked key means wiping every
/// encrypted value and re-enrolling MFA for everyone by hand.
pub(crate) async fn cmd_rotate_encryption_key(
    db_url: Option<String>,
    config: Option<String>,
    jwt_secret: Option<String>,
    old: Option<String>,
    new: String,
    dry_run: bool,
    yes: bool,
) -> anyhow::Result<()> {
    init_cli_logging();

    let cfg = config::load_optional_config_file(config.as_deref())?;
    let db_url = config::resolve_db_url(db_url, cfg.as_ref());

    // Without --old, "the key this database is encrypted with" is whatever this
    // deployment resolves today — the same chain `serve` uses, so the default is
    // right on every instance that has not already changed its config.
    let old_key = match old {
        Some(explicit) => explicit,
        None => {
            let key_file = config::resolve_encryption_key_file(
                cfg.as_ref(),
                cfg.as_ref()
                    .and_then(|config| config.server.host_key.as_deref()),
            );
            crate::serve::resolve_auth_secrets(cfg.as_ref(), jwt_secret, None, &key_file)?
                .encryption_key
        }
    };
    admin::validate_jwt_secret(&new, "--new")?;

    if !dry_run && !yes {
        anyhow::bail!(
            "refusing to re-encrypt the database without --yes.\n\
             \n\
             Run it with --dry-run first: that reports, per column, how many stored values \
             the old key opens and how many it does not, and writes nothing. Then re-run \
             with --yes — and keep the old key until the server has started under the new \
             one, because it is what opens any value this pass could not."
        );
    }

    let db = dbconn::connect(&db_url).await?;
    let report = rg_core::auth::rekey::rekey(&db, &old_key, &new, dry_run).await?;

    println!(
        "{:<38} {:>12} {:>12} {:>10} {:>11}",
        "column", "re-encrypted", "already new", "plaintext", "unreadable"
    );
    for column in &report.columns {
        println!(
            "{:<38} {:>12} {:>12} {:>10} {:>11}",
            column.column,
            column.rewritten,
            column.already_new,
            column.plaintext,
            column.unreadable
        );
    }

    if report.plaintext() > 0 {
        println!(
            "\n{} value(s) are not encrypted at all (columns that predate encryption) and were \
             left as they are.",
            report.plaintext()
        );
    }
    if report.unreadable() > 0 {
        println!(
            "\n{} value(s) in {} open with neither key. They were left untouched — re-enter \
             them by hand once the rotation is done.",
            report.unreadable(),
            report.unreadable_columns().join(", ")
        );
    }

    if dry_run {
        // The apply path refuses this outright; on a dry run it is the whole
        // point of the exercise, so report it and exit non-zero.
        if report.old_key_is_wrong() {
            anyhow::bail!(
                "the old key opens none of the {} encrypted value(s) in this database. \
                 Re-encrypting would seal them away for good, so this is what --dry-run is \
                 for: check --old before running for real. On an instance that rotated \
                 jwt_secret without setting [auth].encryption_key, the key that opens the \
                 data is the *previous* signing secret.",
                report.unreadable()
            );
        }
        println!(
            "\nDry run: nothing was changed. {} value(s) would be re-encrypted.",
            report.rewritten()
        );
        return Ok(());
    }

    println!(
        "\n{} value(s) re-encrypted. Now set the new key — [auth].encryption_key, \
         FORGEKEEP_ENCRYPTION_KEY or --encryption-key — before starting the server; \
         starting it under the old one is refused by the startup key check.",
        report.rewritten()
    );
    Ok(())
}

/// `forgekeep rebuild-fts` — rebuild full-text search indexes.
pub(crate) async fn cmd_rebuild_fts(
    db_url: Option<String>,
    config: Option<String>,
) -> anyhow::Result<()> {
    init_cli_logging();

    let db_url = resolve_db_url(db_url, config)?;
    tracing::info!(
        "Connecting to database: {}",
        rg_db::redact_database_url(&db_url)
    );
    let db = dbconn::connect(&db_url).await?;

    rg_db::rebuild_fts_indexes(&db).await?;

    tracing::info!("Full-text search indexes refreshed successfully ✅");
    Ok(())
}

/// `forgekeep backup-db` — create a consistent SQLite backup.
pub(crate) async fn cmd_backup_db(
    db_url: Option<String>,
    config: Option<String>,
    output: String,
    force: bool,
) -> anyhow::Result<()> {
    init_cli_logging();

    // The worst case of the ignored-config class: with the old clap default, a
    // `backup-db` that forgot `--db-url` inside the container `VACUUM INTO`'d a
    // freshly-created empty database and reported success. Discovered only on
    // restore.
    let db_url = resolve_db_url(db_url, config)?;
    admin::backup_sqlite_db(&db_url, &PathBuf::from(output), force).await?;
    Ok(())
}

/// `forgekeep restore-db` — restore a SQLite database from a backup.
pub(crate) fn cmd_restore_db(
    db_url: Option<String>,
    config: Option<String>,
    input: String,
    force: bool,
) -> anyhow::Result<()> {
    init_cli_logging();

    let db_url = resolve_db_url(db_url, config)?;
    admin::restore_sqlite_db(&db_url, &PathBuf::from(input), force)?;
    Ok(())
}

/// `forgekeep create-repo` — create a bare repository with no DB record.
pub(crate) fn cmd_create_repo(
    owner: String,
    name: String,
    repo_root: Option<String>,
    config: Option<String>,
) -> anyhow::Result<()> {
    // Simple logging for create-repo command
    init_cli_logging();

    let cfg = config::load_optional_config_file(config.as_deref())?;
    let repo_root = PathBuf::from(config::resolve_repo_root(repo_root, cfg.as_ref()));
    let repo_dir = repo_root.join(format!("{}/{}.git", owner, name));
    // `--repo-root` is optional here: without it the root comes from the config
    // file or the built-in default, so the directory that failed is not
    // necessarily one the operator just typed.
    std::fs::create_dir_all(&repo_dir).map_err(|error| {
        rg_core::platform::fs::path_error(
            "repository directory",
            &repo_dir,
            &error,
            rg_core::platform::fs::REPO_ROOT_HINT,
        )
    })?;

    // Replace git init --bare with gix API
    gix::create::into(
        &repo_dir,
        gix::create::Kind::Bare,
        gix::create::Options::default(),
    )
    .with_context(|| "failed to create bare repository")?;

    println!("Created repository: {}/{}.git", owner, name);
    Ok(())
}

/// `forgekeep import` — import a repository from GitHub or GitLab.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_import(
    platform: String,
    source_url: String,
    target_owner: String,
    target_name: Option<String>,
    token: Option<String>,
    repo_root: Option<String>,
    db_url: Option<String>,
    config: Option<String>,
    skip_repo: bool,
    skip_issues: bool,
    skip_prs: bool,
    skip_labels: bool,
    skip_milestones: bool,
    skip_releases: bool,
    import_wiki: bool,
) -> anyhow::Result<()> {
    init_cli_logging();

    let cfg = config::load_optional_config_file(config.as_deref())?;
    let db_url = config::resolve_db_url(db_url, cfg.as_ref());
    let repo_root = config::resolve_repo_root(repo_root, cfg.as_ref());
    let trusted_import_origins = config::resolve_trusted_import_origins(cfg.as_ref())?;

    // SSRF guard (fast, DNS-free): reject an internal/loopback/metadata host or a
    // non-git transport (file://, ext::) before doing any work. The background
    // clone path re-checks with a full DNS-resolving guard.
    trusted_import_origins
        .check_url_static(&source_url)
        .context("invalid source URL")?;

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
        "║  ForgeKeep Import — {} → ForgeKeep",
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
    let db = dbconn::connect_offline_migration(&db_url).await?;
    rg_db::run_migrations(db.connection()).await?;

    // Verify platform is valid
    if platform != "github" && platform != "gitlab" {
        anyhow::bail!(
            "unsupported platform: {}. Use 'github' or 'gitlab'.",
            platform
        );
    }

    let repo_root = PathBuf::from(&repo_root);
    std::fs::create_dir_all(&repo_root).map_err(|error| {
        rg_core::platform::fs::path_error(
            "repository storage root",
            &repo_root,
            &error,
            rg_core::platform::fs::REPO_ROOT_HINT,
        )
    })?;

    // Start import
    println!("\n⏳ Starting import...");
    let task = rg_core::import::service::start_import(
        db.connection(),
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
        &trusted_import_origins,
        &repo_root,
    )
    .await?;

    println!("Import task created: id={}", task.id);
    println!("Polling for completion...");

    // Poll until complete
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let current = rg_db::ops::import_task_ops::find_by_id(db.connection(), task.id)
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
    Ok(())
}

fn build_package_publish_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .redirect(rg_core::net::same_origin_redirect_policy())
        .build()
        .context("failed to build package-publish HTTP client")
}

/// `forgekeep package` — package registry management (publish / list).
pub(crate) async fn cmd_package(cmd: PackageCmd) -> anyhow::Result<()> {
    init_cli_logging();

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
                // Bound only the TCP + TLS handshake: a package upload streams a
                // potentially large file body, so a global request `.timeout(...)`
                // could abort a legitimate slow upload. `connect_timeout` alone
                // still stops a dead/hung `--server-url` from hanging the CLI on
                // connect forever.
                let client = build_package_publish_client()?;
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
            config,
        } => {
            if !rg_core::package_registry::package_types::is_valid(&pkg_type) {
                anyhow::bail!(
                    "Unsupported package type: {}. Supported: {}",
                    pkg_type,
                    rg_core::package_registry::package_types::ALL.join(", ")
                );
            }

            let db_url = resolve_db_url(db_url, config)?;
            tracing::info!(
                "Connecting to database: {}",
                rg_db::redact_database_url(&db_url)
            );
            let db = dbconn::connect_offline_migration(&db_url).await?;
            rg_db::run_migrations(db.connection()).await?;

            match rg_core::package_registry::service::list_packages(
                db.connection(),
                &owner,
                &repo,
                &pkg_type,
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
    Ok(())
}

/// `forgekeep index-repo` — index a repository for code search.
pub(crate) async fn cmd_index_repo(
    repo_slug: String,
    repo_root: Option<String>,
    db_url: Option<String>,
    config: Option<String>,
    ref_name: Option<String>,
) -> anyhow::Result<()> {
    // Simple logging for index-repo command
    init_cli_logging();

    let (db_url, repo_root) = resolve_db_url_and_repo_root(db_url, repo_root, config)?;

    // Parse owner/name from repo_slug
    let parts: Vec<&str> = repo_slug.splitn(2, '/').collect();
    if parts.len() != 2 {
        anyhow::bail!("Invalid repo slug format. Expected: owner/name");
    }
    let owner_name = parts[0];
    let repo_name = parts[1];

    tracing::info!(
        "Connecting to database: {}",
        rg_db::redact_database_url(&db_url)
    );
    let db = dbconn::connect(&db_url).await?;

    // The slug's owner half is a namespace, and the server's canonical resolver
    // is the one that knows both kinds: a username reaches that account's own
    // repositories, an organization name reaches the organization's. Resolving
    // it here as a username and then filtering on `owner_id` alone meant
    // `org/name` never worked while `org-owner/name` reached the
    // organization's repository under the wrong path (card_92019cc97dcd).
    let repo = rg_core::repo::service::find_repo_by_owner_name(&db, owner_name, repo_name)
        .await
        .context("Failed to find repository")?
        .ok_or_else(|| anyhow::anyhow!("Repository not found: {}/{}", owner_name, repo_name))?;

    tracing::info!(
        repo_id = repo.id,
        repo_name = %repo.name,
        default_branch = %repo.default_branch,
        "Found repository"
    );

    // Determine ref to index
    let ref_to_index = ref_name.as_deref().unwrap_or(&repo.default_branch);

    // Construct repo path
    let repo_path =
        std::path::Path::new(&repo_root).join(format!("{}/{}.git", owner_name, repo_name));

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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_package_publish_client, cmd_migrate, cmd_rotate_instance_key};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    async fn read_headers(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    async fn write_response(stream: &mut TcpStream, status: &str, headers: &str) {
        let response =
            format!("HTTP/1.1 {status}\r\n{headers}Content-Length: 0\r\nConnection: close\r\n\r\n");
        stream.write_all(response.as_bytes()).await.unwrap();
    }

    #[derive(Clone, Copy, Debug)]
    enum OriginChange {
        Scheme,
        Host,
        Port,
    }

    async fn assert_origin_change_is_stopped(
        client: &reqwest::Client,
        expected_authorization: &str,
        change: OriginChange,
    ) {
        let source = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_address = source.local_addr().unwrap();
        let sink = if matches!(change, OriginChange::Port) {
            Some(TcpListener::bind("127.0.0.1:0").await.unwrap())
        } else {
            None
        };
        let sink_address = sink.as_ref().map(|listener| listener.local_addr().unwrap());
        let location = match change {
            OriginChange::Scheme => {
                format!("https://127.0.0.1:{}/changed-scheme", source_address.port())
            }
            OriginChange::Host => {
                format!("http://127.0.0.1:{}/changed-host", source_address.port())
            }
            OriginChange::Port => format!("http://{}/changed-port", sink_address.unwrap()),
        };
        let initial_host = if matches!(change, OriginChange::Host) {
            "localhost"
        } else {
            "127.0.0.1"
        };
        let initial_url = format!("http://{initial_host}:{}/start", source_address.port());

        let sink_task = sink.map(|sink| {
            tokio::spawn(async move {
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), sink.accept()).await,
                    Ok(Ok(_))
                )
            })
        });
        let source_task = tokio::spawn(async move {
            let (mut first, _) = source.accept().await.unwrap();
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: {location}\r\n"),
            )
            .await;
            let same_listener_followed = if matches!(change, OriginChange::Port) {
                false
            } else {
                matches!(
                    tokio::time::timeout(std::time::Duration::from_secs(1), source.accept()).await,
                    Ok(Ok(_))
                )
            };
            (first_request, same_listener_followed)
        });

        let response = client
            .get(initial_url)
            .header("Authorization", "Bearer package-token")
            .send()
            .await
            .expect("the cross-origin redirect must be returned, not followed");
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);

        let (first_request, same_listener_followed) = source_task.await.unwrap();
        assert!(
            first_request
                .to_ascii_lowercase()
                .contains(expected_authorization),
            "baseline request did not carry its credential: {first_request}"
        );
        let separate_sink_followed = match sink_task {
            Some(task) => task.await.unwrap(),
            None => false,
        };
        assert!(
            !same_listener_followed && !separate_sink_followed,
            "{change:?}-changing destination was contacted"
        );
    }

    #[tokio::test]
    async fn package_publish_client_stops_every_origin_change_before_sending_the_token() {
        let client = build_package_publish_client().unwrap();
        for change in [OriginChange::Scheme, OriginChange::Host, OriginChange::Port] {
            assert_origin_change_is_stopped(&client, "authorization: bearer package-token", change)
                .await;
        }
    }

    #[tokio::test]
    async fn package_publish_client_keeps_same_origin_redirects_and_the_token() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let first_request = read_headers(&mut first).await;
            write_response(
                &mut first,
                "302 Found",
                &format!("Location: http://{address}/renamed\r\n"),
            )
            .await;
            let (mut second, _) = listener.accept().await.unwrap();
            let second_request = read_headers(&mut second).await;
            write_response(&mut second, "204 No Content", "").await;
            (first_request, second_request)
        });

        let response = build_package_publish_client()
            .unwrap()
            .get(format!("http://{address}/start"))
            .header("Authorization", "Bearer package-token")
            .send()
            .await
            .expect("same-origin redirect");
        assert_eq!(response.status(), reqwest::StatusCode::NO_CONTENT);

        let (first, second) = server.await.unwrap();
        for request in [first, second] {
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer package-token"),
                "same-origin request lost the package token: {request}"
            );
        }
    }

    #[tokio::test]
    async fn migrate_refuses_a_file_backed_sqlite_database_held_by_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}/test.db?mode=rwc", dir.path().display());
        let _server = rg_db::sqlite_process_guard::acquire_server(&db_url)
            .unwrap()
            .unwrap();

        let error = cmd_migrate(Some(db_url), None)
            .await
            .expect_err("migrate must prove the SQLite server is stopped");
        let message = format!("{error:#}");
        assert!(message.contains("server to be stopped"), "{message}");
    }

    #[tokio::test]
    async fn rotate_instance_key_refuses_a_key_that_does_not_open_the_database() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("test.db");
        let db_url = format!("sqlite://{}?mode=rwc", db_path.display());
        let db = rg_db::connect(&db_url).await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        rg_core::auth::key_check::ensure_encryption_key_check(&db, "the-real-at-rest-key")
            .await
            .unwrap();

        let config_path = dir.path().join("forgekeep.toml");
        std::fs::write(
            &config_path,
            format!(
                "[database]\nurl = \"{db_url}\"\n\n[auth]\njwt_secret = \"a-sufficiently-long-jwt-secret\"\nencryption_key = \"a-wrong-at-rest-key\"\n"
            ),
        )
        .unwrap();

        let error = cmd_rotate_instance_key(
            None,
            Some(config_path.to_string_lossy().into_owned()),
            None,
            None,
            true,
        )
        .await
        .expect_err("the command must preflight the configured encryption key");
        let message = format!("{error:#}");
        assert!(message.contains("check marker"), "{message}");
        assert!(
            rg_db::ops::instance_signing_key_ops::find(&db)
                .await
                .unwrap()
                .is_none(),
            "the command must not write a new signing key after a failed preflight"
        );
    }
}
