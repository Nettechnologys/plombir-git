//! One storage namespace per OCI repository: the spelling its row carries.
//!
//! A registry path names a repository by `owner/repo`, and the database decides
//! which spellings reach it. On SQLite and PostgreSQL that is the exact name;
//! on MySQL's default collation `alice/myrepo` finds `Alice/MyRepo` — and
//! `docker` only ever sends lowercase. Storage keys used to be built from the
//! request's spelling, so one repository could hold its layers and manifests
//! under several prefixes, while deletion and transfer walk only the row's: the
//! other copies outlived the repository, and a pull under the row's spelling
//! answered `404` for layers that were present (card_7e82c1b1f642).
//!
//! Keys are now built from the row ([`crate::repo::service::find_repo_and_namespace_by_owner_name`]).
//! [`fold_variant_namespaces`] moves what was written before that, at startup,
//! and rewrites the blob rows to the keys their bytes now live under. Reads
//! still fall back to the key a row recorded (`*_or_recorded` on
//! [`super::storage::OciStorage`]), so an object a failed fold left behind is
//! not lost to a pull.

use std::collections::BTreeSet;

use sea_orm::DatabaseConnection;

use super::storage::OciStorage;

/// What one [`fold_variant_namespaces`] pass did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FoldReport {
    /// Variant prefixes folded into their repository's own.
    pub namespaces: usize,
    /// Objects moved between prefixes.
    pub objects: usize,
    /// Blob rows pointed at their new key.
    pub rows: usize,
    /// Repositories or prefixes left as they were because a step failed; they
    /// are retried on the next start, and reads keep using the recorded key.
    pub failed: usize,
}

/// The namespace prefix of a stored OCI key — its first three segments, as
/// the key spells them (`oci/<owner>/<repo>`, percent-encoded).
fn key_prefix(storage_path: &str) -> Option<&str> {
    let mut boundaries = storage_path.match_indices('/').map(|(index, _)| index);
    let (_, _, end) = (boundaries.next()?, boundaries.next()?, boundaries.next()?);
    let prefix = &storage_path[..end];
    prefix.starts_with("oci/").then_some(prefix)
}

/// Move every OCI object stored under a spelling of its repository's
/// namespace other than the row's own, and point the blob rows at where their
/// bytes went. Run at startup, before the registry serves a request.
///
/// Never fails the start: a repository it cannot settle is logged and counted,
/// and everything about it stays readable through the recorded key.
pub async fn fold_variant_namespaces(db: &DatabaseConnection, storage: &OciStorage) -> FoldReport {
    let mut report = FoldReport::default();
    let repositories = match rg_db::ops::oci_ops::list_repositories(db, None).await {
        Ok(repositories) => repositories,
        Err(error) => {
            tracing::warn!(error = %error, "could not list OCI repositories to settle their storage namespaces");
            report.failed += 1;
            return report;
        }
    };

    for oci_repo in repositories {
        let identity = async {
            let Some(repository) = rg_db::ops::repo_ops::find_by_id(db, oci_repo.repo_id).await?
            else {
                return anyhow::Ok(None);
            };
            let owner = crate::repo::service::repository_namespace_name(
                db,
                repository.owner_id,
                repository.org_id,
            )
            .await?;
            Ok(Some((owner, repository.name)))
        }
        .await;
        let (owner, name) = match identity {
            Ok(Some(identity)) => identity,
            // A repository that is gone is the deletion path's to retire.
            Ok(None) => continue,
            Err(error) => {
                tracing::warn!(repo_id = oci_repo.repo_id, error = %format!("{error:#}"), "could not read an OCI repository's namespace to settle its storage");
                report.failed += 1;
                continue;
            }
        };
        let canonical = match OciStorage::repository_prefix(&owner, &name) {
            Ok(prefix) => prefix,
            Err(error) => {
                tracing::warn!(repo_id = oci_repo.repo_id, error = %format!("{error:#}"), "an OCI repository's namespace does not make a storage key");
                report.failed += 1;
                continue;
            }
        };
        let blobs = match rg_db::ops::oci_ops::list_blobs(db, oci_repo.id).await {
            Ok(blobs) => blobs,
            Err(error) => {
                tracing::warn!(repo_id = oci_repo.repo_id, error = %error, "could not list OCI blobs to settle their storage namespace");
                report.failed += 1;
                continue;
            }
        };
        // Compared byte for byte, here and not in SQL: a `LIKE` on the very
        // collation that caused this would call the variant the same prefix.
        let variants: BTreeSet<String> = blobs
            .iter()
            .filter_map(|blob| key_prefix(&blob.storage_path))
            .filter(|&prefix| prefix != canonical.as_str())
            .map(str::to_string)
            .collect();

        for variant in variants {
            let merged = match crate::blob_storage::BlobKey::new(variant.clone()) {
                Ok(source) => storage.merge_namespace(&source, &owner, &name).await,
                Err(error) => Err(error.into()),
            };
            match merged {
                Ok(moved) => {
                    report.namespaces += 1;
                    report.objects += moved;
                }
                Err(error) => {
                    tracing::warn!(
                        repo_id = oci_repo.repo_id,
                        from = %variant,
                        to = %canonical,
                        error = %format!("{error:#}"),
                        "could not fold an OCI storage namespace into its repository's own; \
                         its blobs stay readable under the keys their rows recorded"
                    );
                    report.failed += 1;
                    continue;
                }
            }

            for blob in blobs
                .iter()
                .filter(|blob| key_prefix(&blob.storage_path) == Some(variant.as_str()))
            {
                // Only a row whose bytes are now where the row's spelling
                // says is rewritten; any other keeps the key that still works.
                let rewritten = async {
                    let key = storage.blob_storage_key(&owner, &name, &blob.digest)?;
                    if !storage.has_object(&key).await? {
                        return anyhow::Ok(false);
                    }
                    Ok(rg_db::ops::oci_ops::set_blob_storage_path(db, blob.id, &key).await?)
                }
                .await;
                match rewritten {
                    Ok(true) => report.rows += 1,
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            repo_id = oci_repo.repo_id,
                            digest = %blob.digest,
                            error = %format!("{error:#}"),
                            "could not point an OCI blob row at its folded key"
                        );
                        report.failed += 1;
                    }
                }
            }
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blob_storage::{BlobKey, BlobStorage, LocalBlobStorage};
    use sea_orm::ConnectionTrait;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;

    /// `Alice/MyRepo`, with an OCI repository row and nothing stored yet.
    async fn registry(
        dir: &std::path::Path,
    ) -> (DatabaseConnection, OciStorage, Arc<LocalBlobStorage>, i64) {
        let url = format!("sqlite://{}?mode=rwc", dir.join("oci.db").display());
        let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
            .await
            .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        for statement in [
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) \
             VALUES(1, 'Alice', 'alice@example.com', 'x', 0, 1, '2024-01-01 00:00:00', '2024-01-01 00:00:00')",
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) \
             VALUES(1, 1, 'MyRepo', 0, 'main', 0, 0, '2024-01-01 00:00:00', '2024-01-01 00:00:00')",
        ] {
            db.execute_unprepared(statement).await.unwrap();
        }
        let oci_repo = rg_db::ops::oci_ops::find_or_create_repo(&db, 1, "Alice/MyRepo", 1)
            .await
            .unwrap();
        let backend = Arc::new(LocalBlobStorage::new(dir.join("blobs")));
        let storage = OciStorage::from_backend(backend.clone(), dir.join("uploads"), None);
        (db, storage, backend, oci_repo.id)
    }

    /// A blob as the pre-fix registry wrote it: bytes under the spelling the
    /// request used, and a row recording that key.
    async fn legacy_blob(
        db: &DatabaseConnection,
        storage: &OciStorage,
        oci_repo_id: i64,
        owner: &str,
        repo: &str,
        payload: &[u8],
    ) -> String {
        let digest = format!("sha256:{}", hex::encode(Sha256::digest(payload)));
        let key = storage
            .store_blob(owner, repo, &digest, payload)
            .await
            .unwrap();
        rg_db::ops::oci_ops::insert_blob(
            db,
            oci_repo_id,
            &digest,
            "application/octet-stream",
            payload.len() as i64,
            &key,
        )
        .await
        .unwrap();
        digest
    }

    async fn recorded_key(db: &DatabaseConnection, oci_repo_id: i64, digest: &str) -> String {
        rg_db::ops::oci_ops::find_blob(db, oci_repo_id, digest)
            .await
            .unwrap()
            .unwrap()
            .storage_path
    }

    /// The upgrade case: everything this repository holds was pushed as
    /// `alice/myrepo`, a manifest object included, and the row's spelling holds
    /// nothing. One fold puts it all where deletion and transfer look.
    #[tokio::test]
    async fn a_variant_namespace_moves_to_the_rows_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let (db, storage, backend, oci_repo_id) = registry(dir.path()).await;
        let digest = legacy_blob(&db, &storage, oci_repo_id, "alice", "myrepo", b"layer").await;
        storage
            .store_manifest(
                "alice",
                "myrepo",
                &format!("sha256:{}", hex::encode(Sha256::digest(b"{}"))),
                b"{}",
            )
            .await
            .unwrap();

        let report = fold_variant_namespaces(&db, &storage).await;

        assert_eq!(report.namespaces, 1, "{report:?}");
        assert_eq!(
            report.objects, 2,
            "the layer and the manifest object: {report:?}"
        );
        assert_eq!(report.rows, 1, "{report:?}");
        assert_eq!(report.failed, 0, "{report:?}");
        assert_eq!(
            storage.read_blob("Alice", "MyRepo", &digest).await.unwrap(),
            b"layer"
        );
        assert_eq!(
            recorded_key(&db, oci_repo_id, &digest).await,
            storage
                .blob_storage_key("Alice", "MyRepo", &digest)
                .unwrap()
        );
        let left = backend
            .list(Some(
                &BlobKey::from_segments(["oci", "alice", "myrepo"]).unwrap(),
            ))
            .await
            .unwrap();
        assert!(left.is_empty(), "the variant prefix kept {left:?}");

        assert_eq!(
            fold_variant_namespaces(&db, &storage).await,
            FoldReport::default(),
            "a settled registry is a no-op on the next start"
        );
    }

    /// Pushes kept arriving under both spellings: the row's spelling already
    /// holds objects, so the fold merges object by object, and a layer present
    /// under both spellings is kept once.
    #[tokio::test]
    async fn a_variant_namespace_merges_into_a_populated_one() {
        let dir = tempfile::tempdir().unwrap();
        let (db, storage, _backend, oci_repo_id) = registry(dir.path()).await;
        let shared = legacy_blob(&db, &storage, oci_repo_id, "alice", "myrepo", b"shared").await;
        let only_variant = legacy_blob(
            &db,
            &storage,
            oci_repo_id,
            "alice",
            "myrepo",
            b"only-variant",
        )
        .await;
        storage
            .store_blob("Alice", "MyRepo", &shared, b"shared")
            .await
            .unwrap();

        let report = fold_variant_namespaces(&db, &storage).await;

        assert_eq!(report.failed, 0, "{report:?}");
        assert_eq!(report.rows, 2, "{report:?}");
        for (digest, payload) in [
            (&shared, &b"shared"[..]),
            (&only_variant, &b"only-variant"[..]),
        ] {
            assert_eq!(
                storage.read_blob("Alice", "MyRepo", digest).await.unwrap(),
                payload
            );
            assert_eq!(
                recorded_key(&db, oci_repo_id, digest).await,
                storage.blob_storage_key("Alice", "MyRepo", digest).unwrap()
            );
        }
    }

    /// Until a fold has run — or where one failed — a pull under the row's
    /// spelling still finds a blob through the key its row recorded.
    #[tokio::test]
    async fn reads_fall_back_to_the_recorded_key() {
        let dir = tempfile::tempdir().unwrap();
        let (db, storage, _backend, oci_repo_id) = registry(dir.path()).await;
        let digest = legacy_blob(&db, &storage, oci_repo_id, "alice", "myrepo", b"layer").await;
        let recorded = recorded_key(&db, oci_repo_id, &digest).await;

        assert!(!storage
            .blob_exists("Alice", "MyRepo", &digest)
            .await
            .unwrap());
        assert!(storage
            .blob_exists_or_recorded("Alice", "MyRepo", &digest, Some(&recorded))
            .await
            .unwrap());
        assert_eq!(
            storage
                .read_blob_or_recorded("Alice", "MyRepo", &digest, Some(&recorded))
                .await
                .unwrap(),
            b"layer"
        );
        assert!(storage
            .blob_local_path_or_recorded("Alice", "MyRepo", &digest, Some(&recorded))
            .unwrap()
            .is_some());

        // The row narrows the spelling, never the object: a recorded key for
        // another digest is not followed.
        let other = format!("sha256:{}", hex::encode(Sha256::digest(b"other")));
        assert!(!storage
            .blob_exists_or_recorded("Alice", "MyRepo", &other, Some(&recorded))
            .await
            .unwrap());
    }
}
