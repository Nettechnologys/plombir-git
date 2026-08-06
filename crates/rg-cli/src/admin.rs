//! Administrative helpers: SQLite backup/restore and JWT secret generation
//! and validation.

use std::path::{Path, PathBuf};

use anyhow::Context;

pub(crate) async fn backup_sqlite_db(
    db_url: &str,
    output: &PathBuf,
    force: bool,
) -> anyhow::Result<()> {
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

pub(crate) fn restore_sqlite_db(db_url: &str, input: &PathBuf, force: bool) -> anyhow::Result<()> {
    if !input.exists() {
        anyhow::bail!("backup input does not exist: {}", input.display());
    }
    if !input.is_file() {
        anyhow::bail!("backup input is not a file: {}", input.display());
    }

    let target = sqlite_db_path_from_url(db_url)?;
    if target.exists() && !force {
        anyhow::bail!(
            "target database already exists: {} (stop ForgeKeep and use --force to overwrite)",
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

    // The lease has to precede the first destructive step and remain alive
    // through the copy. A live pool otherwise keeps its old database/WAL inode
    // open while this path starts naming an unrelated restored database.
    let _process_guard = rg_db::sqlite_process_guard::acquire_restore(db_url)?;

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

fn remove_sqlite_sidecar_files(db_path: &Path) -> anyhow::Result<()> {
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

/// JWT secrets that must never sign tokens in production.
///
/// - `change-me-in-production` is the placeholder shipped in every
///   `*.example.toml` — starting with it means no secret was ever set.
/// - The base64 value below leaked in the upstream IronForge source repo
///   (committed to VCS), so it is public and forever compromised. Reject it so
///   nobody who copied an old local config can forge tokens (card_a3cd0a5de84a).
const KNOWN_BAD_JWT_SECRETS: &[&str] = &[
    "change-me-in-production",
    "uYT7aF/+zA2Zh6P48xnsuY0IbcHH3WdWA4SAtP/Uv6s=",
];

/// Generate a cryptographically strong JWT secret: 32 random bytes (256 bits)
/// from the OS CSPRNG, standard-base64 encoded — the `openssl rand -base64 32`
/// equivalent. Used by the `gen-secret` subcommand.
pub(crate) fn generate_jwt_secret() -> String {
    use base64::Engine as _;
    use rand::RngCore;

    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Validate JWT secret strength.
/// Common function used for CLI arg, env var, and config file values.
pub(crate) fn validate_jwt_secret(jwt_secret: &str, source: &str) -> anyhow::Result<()> {
    // A blank secret is not a weak secret to warn about — it is no secret at
    // all, and every token this instance ever signs would be forgeable by
    // anyone. It reached this far because `deploy/.env.example` ships
    // `FORGEKEEP_JWT_SECRET=` and `std::env::var` reports that as `Ok("")`, so
    // an operator who followed the file and forgot step 2 got a server that
    // started cleanly and signed everything with "" — while the same file
    // promised "startup validation will fail loudly". Now it does.
    if jwt_secret.trim().is_empty() {
        tracing::error!(
            "FATAL: the secret from {} is empty. Generate one with \
             `forgekeep gen-secret`",
            source
        );
        anyhow::bail!("refusing to start with an empty secret from {source}");
    }
    if KNOWN_BAD_JWT_SECRETS.contains(&jwt_secret) {
        tracing::error!(
            "FATAL: jwt_secret from {} is a known default/compromised value. \
             Generate a fresh one with `forgekeep gen-secret` and set it via \
             FORGEKEEP_JWT_SECRET, --jwt-secret, or config file [auth].jwt_secret",
            source
        );
        anyhow::bail!("refusing to start with default/compromised jwt_secret");
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

#[cfg(test)]
mod scheduled_backup_restore_tests {
    use rg_db::sea_orm::{self, ConnectionTrait};

    async fn connect(db_url: &str) -> sea_orm::DatabaseConnection {
        rg_db::connect_with_pool(db_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
            .await
            .unwrap()
    }

    /// The acceptance criterion for scheduled backups, and the only one that
    /// proves anything: a snapshot the *scheduler* produced must survive a real
    /// `restore-db` and come back with its rows. Testing that the file exists
    /// would pass just as happily on a truncated or empty database — which is
    /// exactly the class of failure `backup-db` without `--db-url` used to
    /// produce, and the reason this path exists at all.
    #[tokio::test]
    async fn a_scheduled_snapshot_restores_into_a_working_database() {
        let dir = tempfile::tempdir().unwrap();
        let live_path = dir.path().join("forgekeep.db");
        let live_url = format!("sqlite://{}?mode=rwc", live_path.display());
        let live = connect(&live_url).await;
        live.execute_unprepared("CREATE TABLE marker (v TEXT)")
            .await
            .unwrap();
        live.execute_unprepared("INSERT INTO marker (v) VALUES ('survives-restore')")
            .await
            .unwrap();

        let config = rg_core::backup::DbBackupConfig::with_dir(dir.path().join("backups"));
        let snapshot = rg_core::backup::run_backup_once(&live, &config)
            .await
            .unwrap();

        // Restore into a *different* database file, so a pass cannot come from
        // reading the original back.
        let restored_path = dir.path().join("restored.db");
        let restored_url = format!("sqlite://{}?mode=rwc", restored_path.display());
        super::restore_sqlite_db(&restored_url, &snapshot.path, false).unwrap();

        let restored = connect(&restored_url).await;
        let rows = restored
            .query_all(sea_orm::Statement::from_string(
                sea_orm::DatabaseBackend::Sqlite,
                "SELECT v FROM marker",
            ))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].try_get::<String>("", "v").unwrap(),
            "survives-restore"
        );
    }
}

#[cfg(test)]
mod jwt_secret_tests {
    use super::{generate_jwt_secret, validate_jwt_secret, KNOWN_BAD_JWT_SECRETS};
    use base64::Engine as _;

    #[test]
    fn rejects_shipped_default() {
        assert!(validate_jwt_secret("change-me-in-production", "test").is_err());
    }

    #[test]
    fn rejects_leaked_upstream_secret() {
        // The value that leaked in the IronForge source repo must stay rejected.
        assert!(
            validate_jwt_secret("uYT7aF/+zA2Zh6P48xnsuY0IbcHH3WdWA4SAtP/Uv6s=", "test").is_err()
        );
    }

    /// `deploy/.env.example` ships `FORGEKEEP_JWT_SECRET=`, and `env::var`
    /// hands that back as `Ok("")` — so this is not a hypothetical value, it is
    /// the one an operator gets by forgetting a step. Signing tokens with it
    /// makes every session forgeable.
    #[test]
    fn rejects_an_empty_or_blank_secret() {
        assert!(validate_jwt_secret("", "test").is_err());
        assert!(validate_jwt_secret("   ", "test").is_err());
        assert!(validate_jwt_secret("\n", "test").is_err());
    }

    #[test]
    fn accepts_strong_secret() {
        assert!(validate_jwt_secret("a-sufficiently-long-random-secret-value", "test").is_ok());
    }

    #[test]
    fn generated_secret_is_strong_and_not_known_bad() {
        let secret = generate_jwt_secret();
        // 32 bytes standard-base64 → 44 chars, well over the 16-char floor.
        assert!(secret.len() >= 16, "generated secret too short: {secret}");
        assert!(!KNOWN_BAD_JWT_SECRETS.contains(&secret.as_str()));
        // Decodes back to exactly 32 bytes of entropy.
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&secret)
            .expect("generated secret must be valid base64");
        assert_eq!(decoded.len(), 32);
        // A fresh call yields a different value (CSPRNG, not a constant).
        assert_ne!(secret, generate_jwt_secret());
        // The generated secret passes validation.
        assert!(validate_jwt_secret(&secret, "test").is_ok());
    }
}
