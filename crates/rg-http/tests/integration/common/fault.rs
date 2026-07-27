//! Fault injection for the write paths.
//!
//! Every test in this suite drives the server over real HTTP and every one of
//! them takes the happy path: the database accepts the row, the blob store
//! accepts the bytes. The behaviour that the silent-failure sweep changed lives
//! on the other branch — what the server answers when one of those two writes
//! fails after the other has already succeeded — and no amount of happy-path
//! coverage touches it.
//!
//! Both seams already exist in the product; nothing here adds an abstraction to
//! production code:
//!
//! * The database is a real SQLite file, so a `BEFORE INSERT` trigger that
//!   `RAISE(ABORT)`s fails exactly one table's writes and nothing else. This is
//!   deliberately narrower than `sea_orm::MockDatabase`, which replaces the
//!   whole backend and would have to script every query of a full request in
//!   order.
//! * `AppState.blob_storage` is an `Arc<dyn BlobStorage>`, so a decorator that
//!   forwards to `LocalBlobStorage` until a flag is flipped covers the storage
//!   half — including mid-scenario, which is what the compensation paths need
//!   (the write succeeds, the row fails, the rollback delete then fails too).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use rg_core::blob_storage::{BlobKey, BlobMetadata, BlobStorage, BlobStorageError};
use sea_orm::ConnectionTrait;

// ── Database ─────────────────────────────────────────────────

/// The statement kind a [`DbFault`] rejects.
///
/// `DELETE` is missing because nothing needs it yet: SQLite takes the same
/// `BEFORE DELETE` trigger, so add the variant when a test wants it rather than
/// carrying an `#[allow(dead_code)]` for it.
#[derive(Clone, Copy, Debug)]
pub enum DbWrite {
    Insert,
    Update,
}

impl DbWrite {
    fn keyword(self) -> &'static str {
        match self {
            Self::Insert => "INSERT",
            Self::Update => "UPDATE",
        }
    }
}

/// An armed SQLite trigger that fails one kind of write on one table.
///
/// Held by the test so the fault can be lifted again mid-scenario — a test that
/// wants to observe the state the failed request left behind has to be able to
/// read it back through the same API.
pub struct DbFault {
    db: rg_db::DatabaseConnection,
    name: String,
}

impl DbFault {
    /// Disarm the fault. Consumes the guard: a cleared trigger cannot be
    /// cleared twice.
    #[allow(dead_code)]
    pub async fn clear(self) {
        self.db
            .execute_unprepared(&format!("DROP TRIGGER IF EXISTS {}", self.name))
            .await
            .expect("failed to drop the injected failure trigger");
    }
}

/// Make every `write` on `table` fail with a SQLite `ABORT`.
///
/// The failure surfaces to the handler as an ordinary `DbErr::Exec`, which is
/// what a constraint violation, a full disk or a corrupted page look like from
/// the caller's side — it is not a connection outage, so a handler that maps it
/// to 503 instead of 500 is telling the client the wrong thing.
pub async fn fail_db_writes(
    db: &rg_db::DatabaseConnection,
    table: &str,
    write: DbWrite,
) -> DbFault {
    let name = format!("fk_fault_{table}_{}", write.keyword().to_lowercase());
    let keyword = write.keyword();
    db.execute_unprepared(&format!(
        "CREATE TRIGGER {name} BEFORE {keyword} ON {table} \
         BEGIN SELECT RAISE(ABORT, 'injected failure: {keyword} on {table}'); END;"
    ))
    .await
    .unwrap_or_else(|error| panic!("failed to arm the {keyword} fault on {table}: {error}"));
    DbFault {
        db: db.clone(),
        name,
    }
}

// ── Blob storage ─────────────────────────────────────────────

/// The switches of a [`FaultyBlobStorage`], cloneable so a test keeps a handle
/// after the storage itself has been moved into the `AppState`.
#[derive(Clone, Default)]
pub struct BlobFaults {
    put: Arc<AtomicBool>,
    put_file: Arc<AtomicBool>,
    get: Arc<AtomicBool>,
    delete: Arc<AtomicBool>,
}

/// Not every switch has a test yet; the set is complete because a harness that
/// covers three of a trait's four write methods invites a test that quietly
/// exercises the uncovered one.
#[allow(dead_code)]
impl BlobFaults {
    pub fn fail_put(&self) {
        self.put.store(true, Ordering::SeqCst);
    }

    pub fn fail_put_file(&self) {
        self.put_file.store(true, Ordering::SeqCst);
    }

    pub fn fail_get(&self) {
        self.get.store(true, Ordering::SeqCst);
    }

    pub fn fail_delete(&self) {
        self.delete.store(true, Ordering::SeqCst);
    }

    /// Lift every fault, so the test can read back what the failed request left
    /// behind.
    pub fn heal(&self) {
        for flag in [&self.put, &self.put_file, &self.get, &self.delete] {
            flag.store(false, Ordering::SeqCst);
        }
    }
}

/// A [`BlobStorage`] that forwards to a real backend until a switch is flipped.
pub struct FaultyBlobStorage {
    inner: Arc<dyn BlobStorage>,
    faults: BlobFaults,
}

impl FaultyBlobStorage {
    /// Wrap `inner`, handing back the storage to install and the switches to
    /// drive it with.
    pub fn wrap(inner: Arc<dyn BlobStorage>) -> (Arc<dyn BlobStorage>, BlobFaults) {
        let faults = BlobFaults::default();
        let storage = Arc::new(Self {
            inner,
            faults: faults.clone(),
        });
        (storage, faults)
    }
}

/// The error the switches raise.
///
/// `BlobStorageError::Io` rather than `InvalidKey`: the callers classify on the
/// variant, and an injected fault has to look like the backend failing (the
/// server's fault, 500) and not like the caller naming an impossible key (400).
fn injected(what: &str, key: &BlobKey) -> BlobStorageError {
    BlobStorageError::io(
        what,
        std::path::PathBuf::from(format!("<injected>/{key}")),
        std::io::Error::other("injected blob storage failure"),
    )
}

impl BlobStorage for FaultyBlobStorage {
    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn put<'a>(
        &'a self,
        key: &'a BlobKey,
        data: &'a [u8],
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        Box::pin(async move {
            if self.faults.put.load(Ordering::SeqCst) {
                return Err(injected("blob storage put", key));
            }
            self.inner.put(key, data).await
        })
    }

    fn put_file<'a>(
        &'a self,
        key: &'a BlobKey,
        source: &'a std::path::Path,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        Box::pin(async move {
            if self.faults.put_file.load(Ordering::SeqCst) {
                return Err(injected("blob storage put_file", key));
            }
            self.inner.put_file(key, source).await
        })
    }

    fn get<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<Vec<u8>>> {
        Box::pin(async move {
            if self.faults.get.load(Ordering::SeqCst) {
                return Err(injected("blob storage get", key));
            }
            self.inner.get(key).await
        })
    }

    fn metadata<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        self.inner.metadata(key)
    }

    fn exists<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<bool>> {
        self.inner.exists(key)
    }

    fn delete<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<bool>> {
        Box::pin(async move {
            if self.faults.delete.load(Ordering::SeqCst) {
                return Err(injected("blob storage delete", key));
            }
            self.inner.delete(key).await
        })
    }

    fn list<'a>(
        &'a self,
        prefix: Option<&'a BlobKey>,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<Vec<BlobMetadata>>> {
        self.inner.list(prefix)
    }

    fn local_path(&self, key: &BlobKey) -> Option<std::path::PathBuf> {
        self.inner.local_path(key)
    }
}

// ── Harness ──────────────────────────────────────────────────

/// Spawn the test app with both seams armed and idle.
///
/// Returns `(base_url, db, blob_faults)`: the database handle is what
/// [`fail_db_writes`] needs, the switches are what the storage half needs, and
/// a test of a compensation path needs both at once.
pub async fn spawn_test_app_with_faults() -> (String, rg_db::DatabaseConnection, BlobFaults) {
    let (db, dir) = super::setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let (blob_storage, faults) = FaultyBlobStorage::wrap(Arc::new(
        rg_core::blob_storage::LocalBlobStorage::new(&repo_root),
    ));
    let state = super::build_test_app_state_with(
        db.clone(),
        repo_root,
        super::StateOverrides {
            blob_storage: Some(blob_storage),
        },
    );
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base_url = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    super::wait_for_listener(&addr.to_string()).await;
    (base_url, db, faults)
}
