//! Subcommand handlers dispatched from `main`, one `cmd_*` per CLI subcommand
//! (excluding `serve` and `runner`, which live in their own modules).

use std::path::PathBuf;

use anyhow::Context;
use tracing_subscriber::EnvFilter;

use crate::admin;
use crate::cli::PackageCmd;

/// Basic stderr logging used by the one-shot subcommands.
fn init_cli_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_target(false)
        .init();
}

/// `forgekeep migrate` — run pending database migrations and exit.
pub(crate) async fn cmd_migrate(db_url: String) -> anyhow::Result<()> {
    init_cli_logging();

    tracing::info!(
        "Connecting to database: {}",
        rg_db::redact_database_url(&db_url)
    );
    let db = rg_db::connect(&db_url).await?;
    tracing::info!("Running database migrations...");
    rg_db::run_migrations(&db).await?;
    tracing::info!("Migrations complete ✅");
    Ok(())
}

/// `forgekeep gen-secret` — print a fresh JWT secret to stdout.
pub(crate) fn cmd_gen_secret() {
    // Print only the secret to stdout so it can be captured directly,
    // e.g. FORGEKEEP_JWT_SECRET="$(forgekeep gen-secret)".
    println!("{}", admin::generate_jwt_secret());
}

/// `forgekeep rebuild-fts` — rebuild full-text search indexes.
pub(crate) async fn cmd_rebuild_fts(db_url: String) -> anyhow::Result<()> {
    init_cli_logging();

    tracing::info!(
        "Connecting to database: {}",
        rg_db::redact_database_url(&db_url)
    );
    let db = rg_db::connect(&db_url).await?;

    rg_db::rebuild_fts_indexes(&db).await?;

    tracing::info!("Full-text search indexes refreshed successfully ✅");
    Ok(())
}

/// `forgekeep backup-db` — create a consistent SQLite backup.
pub(crate) async fn cmd_backup_db(
    db_url: String,
    output: String,
    force: bool,
) -> anyhow::Result<()> {
    init_cli_logging();

    admin::backup_sqlite_db(&db_url, &PathBuf::from(output), force).await?;
    Ok(())
}

/// `forgekeep restore-db` — restore a SQLite database from a backup.
pub(crate) fn cmd_restore_db(db_url: String, input: String, force: bool) -> anyhow::Result<()> {
    init_cli_logging();

    admin::restore_sqlite_db(&db_url, &PathBuf::from(input), force)?;
    Ok(())
}

/// `forgekeep create-repo` — create a bare repository with no DB record.
pub(crate) fn cmd_create_repo(
    owner: String,
    name: String,
    repo_root: String,
) -> anyhow::Result<()> {
    // Simple logging for create-repo command
    init_cli_logging();

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
    repo_root: String,
    db_url: String,
    skip_repo: bool,
    skip_issues: bool,
    skip_prs: bool,
    skip_labels: bool,
    skip_milestones: bool,
    skip_releases: bool,
    import_wiki: bool,
) -> anyhow::Result<()> {
    init_cli_logging();

    // SSRF guard (fast, DNS-free): reject an internal/loopback/metadata host or a
    // non-git transport (file://, ext::) before doing any work. The background
    // clone path re-checks with a full DNS-resolving guard.
    rg_core::net::check_git_url_static(&source_url).context("invalid source URL")?;

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
    Ok(())
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
                let client = reqwest::Client::builder()
                    .connect_timeout(std::time::Duration::from_secs(10))
                    .build()
                    .context("failed to build package-publish HTTP client")?;
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

            match rg_core::package_registry::service::list_packages(&db, &owner, &repo, &pkg_type)
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
    repo_root: String,
    db_url: String,
    ref_name: Option<String>,
) -> anyhow::Result<()> {
    // Simple logging for index-repo command
    init_cli_logging();

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
        .ok_or_else(|| anyhow::anyhow!("Repository not found: {}/{}", owner_username, repo_name))?;

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
        std::path::Path::new(&repo_root).join(format!("{}/{}.git", owner_username, repo_name));

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
