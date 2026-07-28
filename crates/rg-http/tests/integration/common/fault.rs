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

    /// Take the whole blob store away at once — what the sweep needs, where a
    /// per-endpoint test wants exactly one method to fail.
    pub fn fail_everything(&self) {
        for flag in [&self.put, &self.put_file, &self.get, &self.delete] {
            flag.store(true, Ordering::SeqCst);
        }
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

    /// Answers "no local file" while reads are faulted.
    ///
    /// This is not decoration. `local_path` is the read *shortcut* — the
    /// attachment, LFS, package and OCI download paths all take it when the
    /// backend is local and only call [`BlobStorage::get`] when it comes back
    /// `None` — so a decorator that faults `get` but forwards `local_path`
    /// leaves every download running against the real disk. The whole-store
    /// sweep measured exactly that: one route out of ninety-nine noticed the
    /// store was gone. Withdrawing the shortcut sends those callers down their
    /// own `get` branch, which is faulted.
    fn local_path(&self, key: &BlobKey) -> Option<std::path::PathBuf> {
        if self.faults.get.load(Ordering::SeqCst) {
            return None;
        }
        self.inner.local_path(key)
    }
}

/// A [`BlobStorage`] that refuses the writes of exactly one key.
///
/// [`BlobFaults`] switches the whole store at once, which is right for "the
/// backend is gone" but useless for a request that writes more than one object:
/// with `put` faulted, the *first* write already fails and the compensation
/// that only exists for the objects written before the failure never runs. The
/// key this rejects is named by a substring of it, so a test can break the
/// second file of a publish and leave the first one stored.
pub struct RejectOneKey {
    inner: Arc<dyn BlobStorage>,
    needle: String,
}

#[allow(dead_code)]
impl RejectOneKey {
    /// Wrap `inner`, refusing `put` / `put_file` for every key containing
    /// `needle`.
    pub fn wrap(inner: Arc<dyn BlobStorage>, needle: &str) -> Arc<dyn BlobStorage> {
        Arc::new(Self {
            inner,
            needle: needle.to_string(),
        })
    }

    fn rejects(&self, key: &BlobKey) -> bool {
        key.as_str().contains(&self.needle)
    }
}

impl BlobStorage for RejectOneKey {
    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn put<'a>(
        &'a self,
        key: &'a BlobKey,
        data: &'a [u8],
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        Box::pin(async move {
            if self.rejects(key) {
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
            if self.rejects(key) {
                return Err(injected("blob storage put_file", key));
            }
            self.inner.put_file(key, source).await
        })
    }

    fn get<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<Vec<u8>>> {
        self.inner.get(key)
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
        self.inner.delete(key)
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

/// A test server with every seam a whole-server fault sweep needs.
///
/// The per-endpoint tests above take one seam at a time. A sweep that walks the
/// route table needs all of them at once, plus the table itself — and it needs
/// the `repo_root` too, because the third piece of infrastructure a handler can
/// lose is neither the database nor the blob store but the git tree on disk.
pub struct FaultSweepApp {
    pub base: String,
    pub db: rg_db::DatabaseConnection,
    /// The switches of the [`FaultyBlobStorage`] this server was built with.
    pub blob_faults: BlobFaults,
    /// Where bare repositories live: `repo_root/{owner}/{name}.git`.
    pub repo_root: std::path::PathBuf,
    /// The `(method, path, access)` rows recorded by the build that produced
    /// this very router.
    pub facts: Vec<rg_http::route_table::RouteFact>,
}

/// Spawn a server whose database, blob store and git tree can each be taken
/// away independently.
#[allow(dead_code)]
pub async fn spawn_test_app_for_fault_sweep() -> FaultSweepApp {
    let (db, dir) = super::setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let (blob_storage, blob_faults) = FaultyBlobStorage::wrap(Arc::new(
        rg_core::blob_storage::LocalBlobStorage::new(&repo_root),
    ));
    let state = super::build_test_app_state_with(
        db.clone(),
        repo_root.clone(),
        super::StateOverrides {
            blob_storage: Some(blob_storage),
            ..Default::default()
        },
    );
    let (app, facts) = rg_http::create_router_for_test_with_routes(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{}", addr);
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    super::wait_for_listener(&addr.to_string()).await;
    FaultSweepApp {
        base,
        db,
        blob_faults,
        repo_root,
        facts,
    }
}

/// The tables a request needs to be authenticated at all.
///
/// `session_standing_middleware` reads `users` on every authenticated request,
/// so an outage that takes it away answers `503` from the middleware and no
/// handler is ever reached. Keeping it alive is what makes the rest of the
/// server the thing under test.
pub const AUTH_TABLES: &[&str] = &["users"];

/// The tables an *authorization gate* needs on top of [`AUTH_TABLES`].
///
/// Keeping these alive is the difference between measuring the gate and
/// measuring the handler. Under a total outage a repository-scoped route never
/// reaches its handler — `RepoRead` and friends resolve the repository first
/// and fail there — so every handler behind a gate is shielded from the fault
/// and a handler that collapses a database error into `400` sails through.
/// With repository resolution and the permission tables intact, the gate admits
/// the caller and the handler's own queries are the ones that fail.
pub const GATE_TABLES: &[&str] = &[
    "users",
    "repositories",
    "repo_collaborators",
    "organizations",
    "organization_members",
    "teams",
    "team_members",
];

/// Take the database away from a running server, without closing its pool.
///
/// Closing the pool is the blunter outage and `db_outage_status_tests` uses it;
/// it is useless to a sweep, because it fails the session lookup and every
/// request dies in the middleware. Dropping tables selectively lets a caller
/// choose *which layer* the fault lands on — see [`AUTH_TABLES`] and
/// [`GATE_TABLES`].
///
/// Returns how many tables were dropped, so a caller can refuse to trust a sweep
/// that broke nothing.
#[allow(dead_code)]
pub async fn drop_every_table_except(db: &rg_db::DatabaseConnection, keep: &[&str]) -> usize {
    use sea_orm::{DatabaseBackend, Statement};

    async fn names(db: &rg_db::DatabaseConnection, kind: &str, keep: &[&str]) -> Vec<String> {
        db.query_all(Statement::from_string(
            DatabaseBackend::Sqlite,
            format!(
                "SELECT name FROM sqlite_master WHERE type = '{kind}' \
                 AND name NOT LIKE 'sqlite_%'"
            ),
        ))
        .await
        .unwrap_or_else(|error| panic!("failed to list {kind}s: {error}"))
        .iter()
        .map(|row| row.try_get::<String>("", "name").expect("object name"))
        .filter(|name| !keep.contains(&name.as_str()))
        .collect()
    }

    // Triggers and views are dropped wholesale: one attached to a kept table
    // but referencing a dropped one is exactly the "no such table" landmine
    // below, and nothing in the sweep depends on them.
    let tables = names(db, "table", keep).await;

    // One batch on one connection, and both halves of that matter.
    //
    // `PRAGMA foreign_keys` is per-connection while a `DatabaseConnection` is a
    // *pool*, so setting it in its own call disables enforcement on whichever
    // connection happened to serve that call and no other. With enforcement
    // still on elsewhere, dropping a child table runs an implicit delete whose
    // foreign-key action names a parent that is already gone, and the drop dies
    // with "no such table: main.repositories" — the sweep would then fail while
    // *arming* the fault rather than while measuring anything.
    //
    // Triggers and views go first for the same family of reason: SQLite
    // re-parses a table's triggers as it drops the table.
    let mut batch = String::from("PRAGMA foreign_keys = OFF;\n");
    for kind in ["trigger", "view"] {
        for name in names(db, kind, &[]).await {
            batch.push_str(&format!("DROP {kind} IF EXISTS \"{name}\";\n"));
        }
    }
    for name in &tables {
        batch.push_str(&format!("DROP TABLE IF EXISTS \"{name}\";\n"));
    }

    db.execute_unprepared(&batch)
        .await
        .unwrap_or_else(|error| panic!("failed to take the database away: {error}"));
    tables.len()
}

/// Spawn the test app with both seams armed and idle.
///
/// Returns `(base_url, db, blob_faults)`: the database handle is what
/// [`fail_db_writes`] needs, the switches are what the storage half needs, and
/// a test of a compensation path needs both at once.
pub async fn spawn_test_app_with_faults() -> (String, rg_db::DatabaseConnection, BlobFaults) {
    let (db, dir) = super::setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");
    let (blob_storage, faults) = FaultyBlobStorage::wrap(Arc::new(
        rg_core::blob_storage::LocalBlobStorage::new(&repo_root),
    ));
    let state = super::build_test_app_state_with(
        db.clone(),
        repo_root,
        super::StateOverrides {
            blob_storage: Some(blob_storage),
            ..Default::default()
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
