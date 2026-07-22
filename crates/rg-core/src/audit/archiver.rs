//! Audit log archival — periodically exports old audit logs to compressed NDJSON
//! files and purges them from the database only after durable file creation.

use chrono::{Duration, Utc};
use sea_orm::DatabaseConnection;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use tokio::time;

#[derive(Clone, Debug)]
pub struct AuditArchiveConfig {
    pub archive_dir: PathBuf,
    pub archive_after_days: i64,
    pub interval_minutes: u64,
    pub batch_size: u64,
}

impl AuditArchiveConfig {
    pub fn with_archive_dir(archive_dir: PathBuf) -> Self {
        Self {
            archive_dir,
            archive_after_days: 90,
            interval_minutes: 60,
            batch_size: 1_000,
        }
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.archive_after_days > 0,
            "archive_after_days must be positive"
        );
        anyhow::ensure!(
            self.interval_minutes > 0,
            "interval_minutes must be positive"
        );
        anyhow::ensure!(self.batch_size > 0, "batch_size must be positive");
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct ArchiveResult {
    pub path: PathBuf,
    pub count: usize,
}

/// Start the background audit archive task with default retention settings.
pub fn spawn_archiver(db: DatabaseConnection, archive_dir: PathBuf) {
    if let Err(error) =
        spawn_archiver_with_config(db, AuditArchiveConfig::with_archive_dir(archive_dir))
    {
        tracing::warn!(%error, "audit log archiver configuration is invalid");
    }
}

pub fn spawn_archiver_with_config(
    db: DatabaseConnection,
    config: AuditArchiveConfig,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    config.validate()?;
    Ok(tokio::spawn(async move {
        let mut interval = time::interval(time::Duration::from_secs(
            config.interval_minutes.saturating_mul(60),
        ));
        loop {
            interval.tick().await;
            loop {
                match run_archive_once(&db, &config).await {
                    Ok(Some(result)) => {
                        tracing::info!(
                            count = result.count,
                            path = %result.path.display(),
                            "archived audit logs"
                        );
                        if result.count < config.batch_size as usize {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) => {
                        tracing::warn!(%error, "audit log archive run failed");
                        break;
                    }
                }
            }
        }
    }))
}

/// Archive one bounded batch. Returns `None` when no eligible entries exist.
pub async fn run_archive_once(
    db: &DatabaseConnection,
    config: &AuditArchiveConfig,
) -> anyhow::Result<Option<ArchiveResult>> {
    config.validate()?;
    let cutoff = Utc::now() - Duration::days(config.archive_after_days);
    let old_entries =
        rg_db::ops::audit_log_ops::list_before_limit(db, cutoff, config.batch_size).await?;
    if old_entries.is_empty() {
        return Ok(None);
    }

    tokio::fs::create_dir_all(&config.archive_dir).await?;
    let archive_id = uuid::Uuid::new_v4();
    let filename = format!(
        "audit-{}-{}.ndjson.zst",
        Utc::now().format("%Y%m%dT%H%M%S"),
        archive_id
    );
    let path = config.archive_dir.join(filename);
    let temp_path = temporary_path(&config.archive_dir, archive_id);

    let mut ndjson = Vec::new();
    for entry in &old_entries {
        serde_json::to_writer(&mut ndjson, entry)?;
        ndjson.push(b'\n');
    }
    let compressed =
        tokio::task::spawn_blocking(move || zstd::stream::encode_all(Cursor::new(ndjson), 3))
            .await??;

    if let Err(error) = write_archive_atomically(&temp_path, &path, &compressed).await {
        let _ = tokio::fs::remove_file(&temp_path).await;
        return Err(error);
    }

    let ids = old_entries.iter().map(|entry| entry.id).collect::<Vec<_>>();
    rg_db::ops::audit_log_ops::delete_by_ids(db, &ids).await?;

    Ok(Some(ArchiveResult {
        path,
        count: old_entries.len(),
    }))
}

fn temporary_path(archive_dir: &Path, archive_id: uuid::Uuid) -> PathBuf {
    archive_dir.join(format!(".audit-{archive_id}.tmp"))
}

async fn write_archive_atomically(
    temp_path: &Path,
    path: &Path,
    data: &[u8],
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::File::create(temp_path).await?;
    file.write_all(data).await?;
    file.flush().await?;
    file.sync_all().await?;
    drop(file);
    tokio::fs::rename(temp_path, path).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{run_archive_once, AuditArchiveConfig};
    use chrono::{Duration, Utc};
    use sea_orm::{ConnectOptions, Database, NotSet, Set};
    use std::io::Cursor;

    #[tokio::test]
    async fn archives_only_expired_rows_as_compressed_ndjson() {
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = Database::connect(ConnectOptions::new(db_url))
            .await
            .unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        for (action, created_at) in [
            ("old.action.1", Utc::now() - Duration::days(92)),
            ("old.action.2", Utc::now() - Duration::days(91)),
            ("new.action", Utc::now() - Duration::days(1)),
        ] {
            rg_db::ops::audit_log_ops::insert(
                &db,
                rg_db::entities::audit_log::ActiveModel {
                    id: NotSet,
                    user_id: Set(None),
                    username: Set(None),
                    action: Set(action.to_string()),
                    resource_type: Set(None),
                    resource_id: Set(None),
                    resource_name: Set(None),
                    ip_address: Set(None),
                    user_agent: Set(None),
                    details: Set(None),
                    created_at: Set(created_at),
                },
            )
            .await
            .unwrap();
        }

        let config = AuditArchiveConfig {
            archive_dir: dir.path().join("archive"),
            archive_after_days: 90,
            interval_minutes: 60,
            batch_size: 1,
        };
        let first = run_archive_once(&db, &config).await.unwrap().unwrap();
        let second = run_archive_once(&db, &config).await.unwrap().unwrap();
        assert_eq!(first.count, 1);
        assert_eq!(second.count, 1);
        assert_ne!(first.path, second.path);
        assert!(first.path.extension().is_some_and(|ext| ext == "zst"));
        let mut archived_actions = Vec::new();
        for result in [first, second] {
            let compressed = tokio::fs::read(&result.path).await.unwrap();
            let decoded = zstd::stream::decode_all(Cursor::new(compressed)).unwrap();
            let line: serde_json::Value = serde_json::from_slice(decoded.trim_ascii()).unwrap();
            archived_actions.push(line["action"].as_str().unwrap().to_string());
        }
        assert_eq!(archived_actions, ["old.action.1", "old.action.2"]);

        let remaining = rg_db::ops::audit_log_ops::list_before(&db, Utc::now() + Duration::days(1))
            .await
            .unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].action, "new.action");
        assert!(run_archive_once(&db, &config).await.unwrap().is_none());
    }
}
