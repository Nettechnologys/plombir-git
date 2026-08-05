//! Ownership of an OCI content-addressed key while it is being published.
//!
//! Publishing a layer is two writes across two stores: the bytes reach their
//! content-addressed key, then the `oci_blob` row makes them reachable. They
//! cannot share a transaction, so the window between them needs compensation —
//! and compensation needs to know whether the bytes at that key are still this
//! request's to take back.
//!
//! [`OciStorage::finalize_upload`] answers that with `published`, derived from
//! an `exists` probe. For a sequential retry that is exactly right: a key that
//! already held the bytes belongs to the push that put them there. For two
//! concurrent *first* pushes of one digest it is not an answer at all.
//! `exists → put` is not atomic, and `oci_ops::insert_blob` treats the
//! `(repository, digest)` conflict as success on purpose, so the request that
//! lost the race for the bytes still records a live row for them. When the
//! winner's own row write then fails, `published` tells it to delete a layer
//! the loser's row already points at, and the image stops pulling.
//!
//! So publication runs under a lease on the key, taken before the `exists`
//! probe and released after the row is committed. Inside that section a
//! request either publishes bytes nobody else can adopt or adopts bytes nobody
//! else can withdraw, and the ambiguity the compensation used to guess at is
//! gone. This is the protocol `lfs::service` uses over `lfs_objects`, over a
//! key that has no row of its own to carry it — see
//! `m20260805_000003_create_oci_publication_lease`.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::DatabaseConnection;
use std::time::Duration;

use super::storage::{FinalizedBlob, OciStorage};

/// How long a publication lease stays valid before another request may take it
/// over.
///
/// The ceiling has to clear the longest legitimate hold — hashing a staged
/// layer, publishing it, and one row insert — because taking the lease off a
/// holder that is merely slow, rather than dead, is precisely the case where
/// two requests both believe they own the same bytes.
///
/// The cost of erring high is that a request killed *while holding* the lease
/// keeps its key locked for this long, and pushes of that one digest answer
/// `503` until it expires. That window is narrow by construction: the body is
/// already staged before the lease is taken, so what it covers is a hash and a
/// publish, not the upload a client is likely to interrupt.
const PUBLICATION_LEASE_TTL_SECONDS: i64 = 15 * 60;

/// How long a request waits for a competing publication of the same key.
///
/// Waiting is the cheap outcome: the holder is writing the very bytes this
/// request wants under the very key it would use, so the waiter usually
/// inherits a finished layer and does nothing.
const PUBLICATION_LEASE_WAIT: Duration = Duration::from_secs(120);

const PUBLICATION_LEASE_POLL_MIN: Duration = Duration::from_millis(25);
const PUBLICATION_LEASE_POLL_MAX: Duration = Duration::from_millis(500);

/// Another request is publishing this key and did not let go in time.
///
/// A distinct type rather than a bare message because the distinction matters
/// to the client: nothing is wrong with the request and nothing is wrong with
/// the server, so a `500` would tell `docker push` to stop when what it should
/// do is come back. The registry answers `503`.
#[derive(Clone, Debug, thiserror::Error)]
#[error("another push is still publishing {key} after {waited_seconds}s")]
pub struct OciPublicationBusy {
    pub key: String,
    pub waited_seconds: u64,
}

/// The right to publish one content-addressed key.
///
/// Held from before the `exists` probe until after the metadata commit, so the
/// whole decision — publish or adopt, record or roll back — happens with no
/// other publisher able to interleave.
pub struct PublicationLease {
    key: String,
    token: String,
    /// Whether this lease was granted rather than taken over from an expired
    /// holder. A taken-over lease cannot prove the previous holder is gone, so
    /// bytes found under the key may still be theirs.
    exclusive: bool,
}

impl PublicationLease {
    /// The key this lease covers.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Whether the holder may treat bytes it published under this lease as
    /// unambiguously its own. See [`PublicationLease::exclusive`] field docs.
    pub fn exclusive(&self) -> bool {
        self.exclusive
    }
}

/// Where the bytes of a blob publication come from.
///
/// The two paths differ only in how the key gets filled; everything after —
/// the row, the compensation, the lease — is the same code, which is the point
/// of naming them rather than duplicating the section around each.
pub enum BlobSource<'a> {
    /// A finalized chunked upload session in this repository.
    Upload { uuid: &'a str },
    /// A cross-repository mount of a blob the instance already holds.
    Mount {
        from_owner: &'a str,
        from_repo: &'a str,
    },
}

/// Publish a blob's bytes and record the row that makes them reachable.
///
/// Returns the published blob, or the first failure — a digest fault from the
/// client, a blob store that refused the write, or a database that refused the
/// row. The caller classifies it; this function's job is that no failure leaves
/// a row without bytes, and that no compensation deletes bytes another request
/// is entitled to.
pub async fn publish_blob(
    db: &DatabaseConnection,
    storage: &OciStorage,
    oci_repo_id: i64,
    owner: &str,
    repo: &str,
    digest: &str,
    source: BlobSource<'_>,
) -> Result<FinalizedBlob> {
    let key = storage.blob_storage_key(owner, repo, digest)?;
    let lease = acquire_publication_lease(db, &key).await?;
    let published = publish_blob_under_lease(
        db,
        storage,
        oci_repo_id,
        owner,
        repo,
        digest,
        source,
        &lease,
    )
    .await;
    release_publication_lease(db, &lease).await;
    published
}

#[allow(clippy::too_many_arguments)]
async fn publish_blob_under_lease(
    db: &DatabaseConnection,
    storage: &OciStorage,
    oci_repo_id: i64,
    owner: &str,
    repo: &str,
    digest: &str,
    source: BlobSource<'_>,
    lease: &PublicationLease,
) -> Result<FinalizedBlob> {
    // Bare `?`, no added context: the caller tells a client-side digest fault
    // apart from a blob store that refused the write by downcasting this error,
    // and it reports the chain verbatim to the operator.
    let blob = match source {
        BlobSource::Upload { uuid } => {
            // The digest the caller named is re-derived from the staged bytes
            // in here, so a mismatch surfaces before anything is published.
            storage.finalize_upload(owner, repo, uuid, digest).await?
        }
        BlobSource::Mount {
            from_owner,
            from_repo,
        } => {
            storage
                .copy_blob_file(from_owner, from_repo, owner, repo, digest)
                .await?
        }
    };

    // The bytes are in blob storage; without this row nothing can find them,
    // and the conflict arm inside `insert_blob` means a concurrent finalizer
    // that adopted the same bytes reaches success here too.
    if let Err(error) = rg_db::ops::oci_ops::insert_blob(
        db,
        oci_repo_id,
        &blob.digest,
        "application/octet-stream",
        blob.size,
        &blob.storage_path,
    )
    .await
    {
        discard_unrecorded_blob(db, storage, oci_repo_id, &blob, lease).await;
        return Err(error).with_context(|| format!("db: record OCI blob {}", blob.digest));
    }

    Ok(blob)
}

/// Roll back bytes no row will ever point at.
///
/// Three things have to hold before the delete is safe, and each of them is a
/// way this code could lose live data:
///
/// * the bytes were published by *this* request, not adopted from an earlier
///   one — a deduplicating finalize writes nothing and so has nothing to take
///   back;
/// * the lease was granted, not taken over — a taken-over lease means another
///   process may still be publishing under the same key;
/// * no `oci_blobs` row currently points at the digest.
///
/// Anything short of all three leaves the bytes in place. An orphaned layer
/// costs disk and stays collectable; deleting a layer a live row points at
/// turns a failed push into somebody else's unpullable image, which is not
/// recoverable at all. The caller still has to report the failure that got us
/// here, so a failed rollback can only be logged.
async fn discard_unrecorded_blob(
    db: &DatabaseConnection,
    storage: &OciStorage,
    oci_repo_id: i64,
    blob: &FinalizedBlob,
    lease: &PublicationLease,
) {
    if !blob.published {
        return;
    }
    if !lease.exclusive {
        tracing::warn!(
            digest = %blob.digest,
            oci_repository_id = oci_repo_id,
            storage_path = %blob.storage_path,
            "orphaned OCI blob: this request published under a taken-over lease and cannot prove the stored bytes are its own — the layer stays in storage rather than risk deleting a live one"
        );
        return;
    }
    match rg_db::ops::oci_ops::find_blob(db, oci_repo_id, &blob.digest).await {
        Ok(None) => {}
        Ok(Some(_)) => {
            tracing::warn!(
                digest = %blob.digest,
                oci_repository_id = oci_repo_id,
                storage_path = %blob.storage_path,
                "kept the OCI blob after a failed row write: the repository already has a row for this digest, so these bytes are serving a live push"
            );
            return;
        }
        Err(error) => {
            tracing::warn!(
                digest = %blob.digest,
                oci_repository_id = oci_repo_id,
                storage_path = %blob.storage_path,
                error = %error,
                "orphaned OCI blob: the rollback could not read the blob row to check for a live push, so the layer stays in storage"
            );
            return;
        }
    }
    if let Err(error) = storage.discard_published_object(&blob.storage_path).await {
        tracing::warn!(
            digest = %blob.digest,
            oci_repository_id = oci_repo_id,
            storage_path = %blob.storage_path,
            error = %format!("{error:#}"),
            "orphaned OCI blob: the oci_blobs row was not created and the rollback delete failed too"
        );
    }
}

/// Wait for, then take, the lease on `key`.
///
/// Polling rather than holding a database transaction is deliberate: the lease
/// covers a blob write that takes as long as the layer is large, and a
/// connection held open for that would starve the pool long before it protected
/// anything.
pub async fn acquire_publication_lease(
    db: &DatabaseConnection,
    key: &str,
) -> Result<PublicationLease> {
    let token = uuid::Uuid::new_v4().to_string();
    let deadline = std::time::Instant::now() + PUBLICATION_LEASE_WAIT;
    let mut backoff = PUBLICATION_LEASE_POLL_MIN;

    loop {
        let stale_before = Utc::now() - chrono::Duration::seconds(PUBLICATION_LEASE_TTL_SECONDS);
        let bid = rg_db::ops::oci_ops::bid_for_publication_lease(db, key, &token, stale_before)
            .await
            .context("db: bid for OCI publication lease")?;
        match bid {
            rg_db::ops::oci_ops::PublicationLeaseBid::Granted => {
                return Ok(PublicationLease {
                    key: key.to_string(),
                    token,
                    exclusive: true,
                })
            }
            rg_db::ops::oci_ops::PublicationLeaseBid::TakenOver => {
                tracing::warn!(
                    storage_key = %key,
                    "took over an expired OCI publication lease — the previous publisher never released it, so this request will keep any bytes it cannot prove are its own"
                );
                return Ok(PublicationLease {
                    key: key.to_string(),
                    token,
                    exclusive: false,
                });
            }
            rg_db::ops::oci_ops::PublicationLeaseBid::Busy => {}
        }

        if std::time::Instant::now() >= deadline {
            return Err(OciPublicationBusy {
                key: key.to_string(),
                waited_seconds: PUBLICATION_LEASE_WAIT.as_secs(),
            }
            .into());
        }
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(PUBLICATION_LEASE_POLL_MAX);
    }
}

/// Give the lease back. A release that finds nothing to release means the lease
/// had already been taken over, which is worth saying out loud — the request
/// that took it over may have published bytes this one still believes are its.
pub async fn release_publication_lease(db: &DatabaseConnection, lease: &PublicationLease) {
    match rg_db::ops::oci_ops::release_publication_lease(db, &lease.key, &lease.token).await {
        Ok(true) => {}
        Ok(false) => tracing::warn!(
            storage_key = %lease.key,
            "OCI publication lease was taken over before this request released it"
        ),
        Err(error) => tracing::warn!(
            storage_key = %lease.key,
            error = %error,
            "failed to release the OCI publication lease — concurrent pushes of this key wait until it expires"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{publish_blob, BlobSource};
    use crate::blob_storage::{
        BlobKey, BlobMetadata, BlobStorage, LocalBlobStorage, Result as BlobResult,
    };
    use crate::package_registry::oci::storage::OciStorage;
    use futures::future::BoxFuture;
    use sea_orm::{ConnectionTrait, DatabaseConnection};
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::Semaphore;

    const OWNER: &str = "alice";
    const REPO: &str = "demo";
    const OCI_REPO_ID: i64 = 1;

    /// A rendezvous point a request can be held at, and the test can observe.
    ///
    /// Two semaphores rather than a `Notify` because both edges have to survive
    /// being signalled before anyone waits: the test must be able to ask "has it
    /// parked yet?" without racing the answer.
    #[derive(Clone)]
    struct Gate {
        reached: Arc<Semaphore>,
        resume: Arc<Semaphore>,
    }

    impl Gate {
        fn new() -> Self {
            Self {
                reached: Arc::new(Semaphore::new(0)),
                resume: Arc::new(Semaphore::new(0)),
            }
        }

        async fn park(&self) {
            self.reached.add_permits(1);
            self.resume
                .acquire()
                .await
                .expect("the gate outlives the parked request")
                .forget();
        }

        fn has_parked(&self) -> bool {
            self.reached.available_permits() > 0
        }

        /// Wait for the request to arrive. Returns false on timeout so a broken
        /// protocol fails an assertion instead of hanging the suite.
        async fn await_arrival(&self) -> bool {
            match tokio::time::timeout(std::time::Duration::from_secs(10), self.reached.acquire())
                .await
            {
                Ok(permit) => {
                    permit
                        .expect("the gate outlives the parked request")
                        .forget();
                    true
                }
                Err(_) => false,
            }
        }

        fn release(&self) {
            self.resume.add_permits(1);
        }
    }

    /// What the backend does once the bytes are on the shared key but the
    /// `oci_blobs` row has not been written — the window every
    /// concurrent-publication hazard lives in.
    enum PutHook {
        None,
        /// Hold the request there so the test can drive a second one into the
        /// same window.
        Park(Gate),
        /// Stand in for a concurrent finalizer that adopted these very bytes
        /// and got its row in first.
        ClaimRow {
            db: DatabaseConnection,
            digest: String,
        },
    }

    impl PutHook {
        async fn run(&self, key: &BlobKey) {
            match self {
                PutHook::None => {}
                PutHook::Park(gate) => gate.park().await,
                PutHook::ClaimRow { db, digest } => {
                    // The stand-in is a different request, so the fault injected
                    // into *this* one must not swallow its row.
                    set_blob_rows(db, true).await;
                    rg_db::ops::oci_ops::insert_blob(
                        db,
                        OCI_REPO_ID,
                        digest,
                        "application/octet-stream",
                        7,
                        key.as_str(),
                    )
                    .await
                    .expect("the stand-in finalizer records its row");
                    set_blob_rows(db, false).await;
                }
            }
        }
    }

    /// Backend-shaped proxy with no `local_path` shortcut. The production tree
    /// currently ships a local backend, but OCI publication ownership must use
    /// only the portable object-store contract, so an S3-like backend gets the
    /// same compensation semantics.
    struct RemoteBlobStorage {
        inner: LocalBlobStorage,
        after_put: PutHook,
    }

    impl RemoteBlobStorage {
        fn with_hook(root: &Path, after_put: PutHook) -> Self {
            Self {
                inner: LocalBlobStorage::new(root),
                after_put,
            }
        }
    }

    impl BlobStorage for RemoteBlobStorage {
        fn backend_name(&self) -> &'static str {
            "remote-test"
        }

        fn put<'a>(
            &'a self,
            key: &'a BlobKey,
            data: &'a [u8],
        ) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            Box::pin(async move {
                let metadata = self.inner.put(key, data).await?;
                self.after_put.run(key).await;
                Ok(metadata)
            })
        }

        fn put_file<'a>(
            &'a self,
            key: &'a BlobKey,
            source: &'a Path,
        ) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            Box::pin(async move {
                let metadata = self.inner.put_file(key, source).await?;
                self.after_put.run(key).await;
                Ok(metadata)
            })
        }

        fn get<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<Vec<u8>>> {
            self.inner.get(key)
        }

        fn metadata<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<BlobMetadata>> {
            self.inner.metadata(key)
        }

        fn exists<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<bool>> {
            self.inner.exists(key)
        }

        fn delete<'a>(&'a self, key: &'a BlobKey) -> BoxFuture<'a, BlobResult<bool>> {
            self.inner.delete(key)
        }

        fn list<'a>(
            &'a self,
            prefix: Option<&'a BlobKey>,
        ) -> BoxFuture<'a, BlobResult<Vec<BlobMetadata>>> {
            self.inner.list(prefix)
        }
    }

    /// A database with a real connection pool: two racing publications have to
    /// contend over separate connections, or the serialisation being tested is
    /// the pool's rather than the protocol's.
    async fn pooled_db(dir: &Path) -> DatabaseConnection {
        let url = format!("sqlite://{}?mode=rwc", dir.join("oci.db").display());
        let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
            .await
            .unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        db
    }

    /// Make every `oci_blob` insert fail, switchably.
    ///
    /// Scoped to that one table on purpose: a blanket trigger would also break
    /// the lease writes, and then the test would be exercising a state
    /// production never reaches.
    async fn fail_blob_rows(db: &DatabaseConnection) {
        db.execute_unprepared(
            "CREATE TABLE oci_blob_switch (fail INTEGER NOT NULL); \
             INSERT INTO oci_blob_switch (fail) VALUES (1); \
             CREATE TRIGGER fail_oci_blob_inserts \
             BEFORE INSERT ON oci_blob \
             WHEN (SELECT fail FROM oci_blob_switch) = 1 \
             BEGIN SELECT RAISE(FAIL, 'injected oci_blob insert failure'); END",
        )
        .await
        .unwrap();
    }

    async fn set_blob_rows(db: &DatabaseConnection, allow: bool) {
        db.execute_unprepared(&format!(
            "UPDATE oci_blob_switch SET fail = {}",
            i32::from(!allow)
        ))
        .await
        .unwrap();
    }

    async fn oci_repo(db: &DatabaseConnection) {
        let repo = rg_db::ops::oci_ops::find_or_create_repo(db, 1, &format!("{OWNER}/{REPO}"), 1)
            .await
            .unwrap();
        assert_eq!(repo.id, OCI_REPO_ID);
    }

    fn digest_of(payload: &[u8]) -> String {
        format!("sha256:{}", hex::encode(Sha256::digest(payload)))
    }

    /// Stage a chunked upload holding `payload` and return its session uuid.
    async fn staged_upload_in(storage: &OciStorage, repo: &str, payload: &[u8]) -> String {
        let (uuid, _) = storage.create_upload(OWNER, repo).await.unwrap();
        storage
            .append_to_upload(OWNER, repo, &uuid, payload)
            .await
            .unwrap();
        uuid
    }

    async fn staged_upload(storage: &OciStorage, payload: &[u8]) -> String {
        staged_upload_in(storage, REPO, payload).await
    }

    fn storage_with(root: &Path, hook: PutHook) -> OciStorage {
        OciStorage::from_backend(
            Arc::new(RemoteBlobStorage::with_hook(root, hook)),
            root.join("_oci_uploads"),
        )
    }

    async fn leases_held(db: &DatabaseConnection) -> u64 {
        use sea_orm::EntityTrait;
        rg_db::entities::oci_publication_lease::Entity::find()
            .all(db)
            .await
            .unwrap()
            .len() as u64
    }

    /// Two first pushes of one digest, one row landing and one failing, must
    /// leave the layer pullable.
    ///
    /// The dangerous interleaving is the one this makes impossible: a second
    /// request observing the first request's bytes before the first has a row,
    /// adopting them, recording its own row — and then watching the first
    /// request's rollback delete the layer that row now points at. The
    /// publication lease is what keeps the second request out of that window,
    /// so it finds either a finished layer or a clean slate, never an
    /// ambiguous one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_first_publications_leave_the_blob_pullable() {
        let dir = tempfile::tempdir().unwrap();
        let db = pooled_db(dir.path()).await;
        oci_repo(&db).await;
        fail_blob_rows(&db).await;

        let payload = b"two clients push the same layer at the same moment";
        let digest = digest_of(payload);
        let root = dir.path().join("remote");

        let first_gate = Gate::new();
        let second_gate = Gate::new();
        let first_storage = Arc::new(storage_with(&root, PutHook::Park(first_gate.clone())));
        let second_storage = Arc::new(storage_with(&root, PutHook::Park(second_gate.clone())));
        let first_upload = staged_upload(&first_storage, payload).await;
        let second_upload = staged_upload(&second_storage, payload).await;

        let first = tokio::spawn({
            let db = db.clone();
            let storage = Arc::clone(&first_storage);
            let digest = digest.clone();
            async move {
                publish_blob(
                    &db,
                    storage.as_ref(),
                    OCI_REPO_ID,
                    OWNER,
                    REPO,
                    &digest,
                    BlobSource::Upload {
                        uuid: &first_upload,
                    },
                )
                .await
            }
        });
        assert!(
            first_gate.await_arrival().await,
            "the first push must reach the window between its blob write and its row"
        );

        // Only now does the second request start, with the first one holding
        // the lease and its bytes already under the shared key.
        let second = tokio::spawn({
            let db = db.clone();
            let storage = Arc::clone(&second_storage);
            let digest = digest.clone();
            async move {
                publish_blob(
                    &db,
                    storage.as_ref(),
                    OCI_REPO_ID,
                    OWNER,
                    REPO,
                    &digest,
                    BlobSource::Upload {
                        uuid: &second_upload,
                    },
                )
                .await
            }
        });

        // It must not get as far as the shared key while the first request
        // still owns it — adopting those bytes is the whole hazard.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert!(
            !second_gate.has_parked(),
            "the second publication reached the shared key while the first still held the lease"
        );

        first_gate.release();
        first
            .await
            .unwrap()
            .expect_err("the injected row failure must land on the first push");

        // The second request now owns the key. Let its row through.
        assert!(
            second_gate.await_arrival().await,
            "the second publication never wrote its own bytes: it either adopted the first \
             request's withdrawn layer or gave up on the lease"
        );
        set_blob_rows(&db, true).await;
        second_gate.release();
        second
            .await
            .unwrap()
            .expect("the second publication owns the key and must succeed");

        let row = rg_db::ops::oci_ops::find_blob(&db, OCI_REPO_ID, &digest)
            .await
            .unwrap()
            .expect("the surviving publication is recorded");
        assert_eq!(row.digest, digest);
        assert_eq!(
            second_storage
                .read_blob(OWNER, REPO, &digest)
                .await
                .unwrap(),
            payload,
            "the recorded row must point at the exact bytes that were pushed"
        );
        assert_eq!(
            leases_held(&db).await,
            0,
            "both publications must have released their lease"
        );
    }

    /// A rollback must keep bytes an `oci_blobs` row already claims, even when
    /// this request is the one that wrote them.
    ///
    /// This is the interleaving the lease alone cannot rule out — a lease taken
    /// over from a holder presumed dead, or a row written by a path that never
    /// bid for the key at all. `published == true` is not a licence to delete;
    /// the row is the last word.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_rollback_keeps_bytes_a_live_row_already_claims() {
        let dir = tempfile::tempdir().unwrap();
        let db = pooled_db(dir.path()).await;
        oci_repo(&db).await;
        fail_blob_rows(&db).await;

        let payload = b"claimed";
        let digest = digest_of(payload);
        let root = dir.path().join("remote");
        let storage = storage_with(
            &root,
            PutHook::ClaimRow {
                db: db.clone(),
                digest: digest.clone(),
            },
        );
        let upload = staged_upload(&storage, payload).await;

        publish_blob(
            &db,
            &storage,
            OCI_REPO_ID,
            OWNER,
            REPO,
            &digest,
            BlobSource::Upload { uuid: &upload },
        )
        .await
        .expect_err("the injected row failure must reach the caller");

        assert!(
            rg_db::ops::oci_ops::find_blob(&db, OCI_REPO_ID, &digest)
                .await
                .unwrap()
                .is_some(),
            "the stand-in finalizer's row is what makes this the dangerous case"
        );
        assert_eq!(
            storage.read_blob(OWNER, REPO, &digest).await.unwrap(),
            payload,
            "the rollback deleted a layer a live row points at"
        );
    }

    /// The lone push whose row fails still cleans up after itself: nothing else
    /// can be pointing at bytes no row was ever written for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_first_publication_removes_its_own_blob() {
        let dir = tempfile::tempdir().unwrap();
        let db = pooled_db(dir.path()).await;
        oci_repo(&db).await;
        fail_blob_rows(&db).await;

        let payload = b"orphan";
        let digest = digest_of(payload);
        let root = dir.path().join("remote");
        let storage = storage_with(&root, PutHook::None);
        let upload = staged_upload(&storage, payload).await;

        publish_blob(
            &db,
            &storage,
            OCI_REPO_ID,
            OWNER,
            REPO,
            &digest,
            BlobSource::Upload { uuid: &upload },
        )
        .await
        .expect_err("the injected row failure must reach the caller");

        assert!(
            !storage.blob_exists(OWNER, REPO, &digest).await.unwrap(),
            "a publication no row will ever claim must take its own bytes back"
        );
        assert_eq!(
            leases_held(&db).await,
            0,
            "the failed publication must still release its lease"
        );
    }

    /// The cross-repository mount publishes into the destination's key the same
    /// way a finalized upload does, so it is bound by the same contract: a
    /// failed row write must not delete a layer a live row claims.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mount_rollback_keeps_bytes_a_live_row_already_claims() {
        let dir = tempfile::tempdir().unwrap();
        let db = pooled_db(dir.path()).await;
        oci_repo(&db).await;

        let payload = b"mounted";
        let digest = digest_of(payload);
        let root = dir.path().join("remote");

        // Seed the source repository the mount copies from.
        let seed = storage_with(&root, PutHook::None);
        let seed_upload = staged_upload_in(&seed, "source", payload).await;
        seed.finalize_upload(OWNER, "source", &seed_upload, &digest)
            .await
            .unwrap();

        fail_blob_rows(&db).await;
        let storage = storage_with(
            &root,
            PutHook::ClaimRow {
                db: db.clone(),
                digest: digest.clone(),
            },
        );

        publish_blob(
            &db,
            &storage,
            OCI_REPO_ID,
            OWNER,
            REPO,
            &digest,
            BlobSource::Mount {
                from_owner: OWNER,
                from_repo: "source",
            },
        )
        .await
        .expect_err("the injected row failure must reach the caller");

        assert_eq!(
            storage.read_blob(OWNER, REPO, &digest).await.unwrap(),
            payload,
            "the mount rollback deleted a layer a live row points at"
        );
        assert_eq!(
            seed.read_blob(OWNER, "source", &digest).await.unwrap(),
            payload,
            "and it must not have touched the repository it copied from"
        );
    }
}
