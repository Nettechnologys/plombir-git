//! card_1a5dab9714b8: six `rg-db` primitives promise create-or-update but read
//! and write in two statements. Concurrent *first* calls all see no row and all
//! insert; the key is UNIQUE, so one of them used to come back with a
//! constraint error on an operation that was never the caller's fault.
//! card_d9d5f4ff7809 adds the related toggle contract: concurrent requests that
//! all mean "make this star exist" must not leak the losing UNIQUE error.
//!
//! The operations, and the ordinary concurrency that reaches each:
//!
//! * `commit_status_ops::create_or_update` — a build matrix reporting the same
//!   context on one commit.
//! * `ci_secret_ops::upsert` — two admins saving the settings form.
//! * `ci_retention_ops::upsert_policy` — the same, for retention.
//! * `ci_retention_ops::upsert_cache_entry` — parallel jobs uploading one cache key.
//! * `instance_settings_ops::save` — the singleton on a never-configured instance.
//! * `repo_watch_ops::set_watch_state` — a double-clicked watch button.
//! * `repo_star_ops::toggle_star` — a double-clicked star button.
//!
//! What the tests guard:
//!
//! * **The race resolves to one row**, and every caller gets a result rather
//!   than a constraint error.
//! * **The surviving row is coherent.** Fields that describe one submission —
//!   a cache blob's path, size and digest; a banner's text and type — come
//!   from a single call, not merged from two.
//! * **A real write failure is still a failure.** The retry is armed only by a
//!   UNIQUE violation; a foreign-key failure must not be re-read into a
//!   fabricated success.

use rg_db::entities::{commit_status, repository};
use rg_db::sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseBackend, DatabaseConnection, NotSet, Set, Statement,
};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-upsert-race-{label}-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        Self { path }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// A migrated database with more than one pooled connection, so concurrent
/// tasks really do run their statements against separate connections.
async fn setup(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

/// A user and a repository to hang the raced rows off.
async fn fixture(db: &DatabaseConnection) -> (i64, i64) {
    let user = rg_db::ops::user_ops::create_user(db, "dana", "dana@example.com", "", "Dana")
        .await
        .expect("create the account the rows hang off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set("forge".to_string()),
            description: Set(None),
            is_private: Set(false),
            default_branch: Set("main".to_string()),
            fork_id: Set(None),
            stars_count: Set(0),
            forks_count: Set(0),
            org_id: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            origin_repo_id: Set(None),
        },
    )
    .await
    .expect("create the repository the rows hang off");
    (user.id, repo.id)
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .expect("query")
        .expect("one row")
        .try_get::<i64>("", "n")
        .expect("count column")
}

/// How many callers race each primitive. Enough that several of them read
/// "no such row" before any of them has written one.
const ATTEMPTS: usize = 8;

fn assert_all_ok<T, E: std::fmt::Debug>(results: &[Result<T, E>], what: &str) {
    for (i, result) in results.iter().enumerate() {
        assert!(
            result.is_ok(),
            "caller {i} of a concurrent first {what} failed: {:?}",
            result.as_ref().err()
        );
    }
}

// Every race test is multi-threaded on purpose: the window each guards sits
// between a `SELECT` and an `INSERT` on two different pooled connections.

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_commit_status_reports_all_succeed_and_leave_one_row() {
    let (db, _temp) = setup("status").await;
    let (user_id, repo_id) = fixture(&db).await;

    let attempts = (0..ATTEMPTS).map(|i| {
        let db = db.clone();
        async move {
            let now = chrono::Utc::now();
            rg_db::ops::commit_status_ops::create_or_update(
                &db,
                repo_id,
                "deadbeef",
                "ci/build",
                commit_status::ActiveModel {
                    id: NotSet,
                    repo_id: Set(repo_id),
                    sha: Set("deadbeef".to_string()),
                    state: Set(format!("state-{i}")),
                    context: Set("ci/build".to_string()),
                    description: Set(Some(format!("run {i}"))),
                    target_url: Set(None),
                    creator_id: Set(Some(user_id)),
                    created_at: Set(now),
                    updated_at: Set(now),
                },
            )
            .await
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "commit status report");

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM commit_statuses \
             WHERE sha = 'deadbeef' AND context = 'ci/build'",
        )
        .await,
        1,
        "one context on one commit must occupy exactly one row",
    );

    // The row that survived carries one report, not a blend of two.
    let statuses = rg_db::ops::commit_status_ops::list_by_sha(&db, repo_id, "deadbeef")
        .await
        .expect("read the status back");
    let status = statuses.first().expect("the status exists");
    let run = status
        .state
        .strip_prefix("state-")
        .expect("the state came from one of the callers");
    assert_eq!(
        status.description.as_deref(),
        Some(format!("run {run}").as_str()),
        "state and description must come from the same report",
    );
}

#[tokio::test]
async fn repository_cascade_after_commit_status_read_is_absence_not_record_not_updated() {
    let (db, _temp) = setup("status-parent-delete").await;
    let (user_id, repo_id) = fixture(&db).await;
    let now = chrono::Utc::now();
    let created = rg_db::ops::commit_status_ops::create_or_update(
        &db,
        repo_id,
        "deadbeef",
        "ci/build",
        commit_status::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            sha: Set("deadbeef".to_string()),
            state: Set("pending".to_string()),
            context: Set("ci/build".to_string()),
            description: Set(Some("first report".to_string())),
            target_url: Set(None),
            creator_id: Set(Some(user_id)),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await
    .expect("create the status the raced report observes")
    .expect("the repository is live");

    // This fires inside the real UPDATE, after `create_or_update` has already
    // read `created`. Deleting the parent cascades the child row and fixes the
    // interleaving without a timing race or a production-only test seam.
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_status_repository_before_update \
             BEFORE UPDATE ON commit_statuses WHEN OLD.id = {} \
             BEGIN DELETE FROM repositories WHERE id = OLD.repo_id; END",
            created.id
        ),
    ))
    .await
    .expect("install the competing repository delete");

    let raced = rg_db::ops::commit_status_ops::create_or_update(
        &db,
        repo_id,
        "deadbeef",
        "ci/build",
        commit_status::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            sha: Set("deadbeef".to_string()),
            state: Set("success".to_string()),
            context: Set("ci/build".to_string()),
            description: Set(Some("too late".to_string())),
            target_url: Set(None),
            creator_id: Set(Some(user_id)),
            created_at: Set(now),
            updated_at: Set(chrono::Utc::now()),
        },
    )
    .await
    .expect("a winning parent DELETE is an outcome, not a database error");
    assert!(
        raced.is_none(),
        "the losing report claimed a deleted status"
    );
    assert!(rg_db::ops::repo_ops::find_by_id(&db, repo_id)
        .await
        .expect("look for the deleted repository")
        .is_none());
    assert!(
        rg_db::ops::commit_status_ops::list_by_sha(&db, repo_id, "deadbeef")
            .await
            .expect("look for a resurrected status")
            .is_empty(),
        "the losing report recreated a status after repository deletion"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_ci_secret_saves_all_succeed_and_leave_one_row() {
    let (db, _temp) = setup("secret").await;
    let (user_id, repo_id) = fixture(&db).await;

    let attempts = (0..ATTEMPTS).map(|i| {
        let db = db.clone();
        async move {
            rg_db::ops::ci_secret_ops::upsert(
                &db,
                repo_id,
                "DEPLOY_TOKEN",
                &format!("ciphertext-{i}"),
                user_id,
            )
            .await
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "CI secret save");

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM ci_secrets WHERE name = 'DEPLOY_TOKEN'",
        )
        .await,
        1,
        "one secret name in one repository must occupy exactly one row",
    );

    let secret = rg_db::ops::ci_secret_ops::find_by_repo_and_name(&db, repo_id, "DEPLOY_TOKEN")
        .await
        .expect("read the secret back")
        .expect("the secret exists");
    assert!(
        secret.encrypted_value.starts_with("ciphertext-"),
        "the stored value came from one of the callers, got {:?}",
        secret.encrypted_value,
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_retention_policy_saves_all_succeed_and_leave_one_coherent_row() {
    let (db, _temp) = setup("policy").await;
    let (_user_id, repo_id) = fixture(&db).await;

    // Artifact and cache days are offset by a constant, so a row that mixed two
    // submissions would break the relation rather than merely look odd.
    let attempts = (0..ATTEMPTS).map(|i| {
        let db = db.clone();
        async move {
            rg_db::ops::ci_retention_ops::upsert_policy(&db, repo_id, i as i32, i as i32 + 100)
                .await
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "retention policy save");

    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM ci_retention_policies").await,
        1,
        "a repository has exactly one retention policy",
    );

    let policy = rg_db::ops::ci_retention_ops::get_policy(&db, repo_id)
        .await
        .expect("read the policy back");
    assert_eq!(
        policy.cache_retention_days,
        policy.artifact_retention_days + 100,
        "both numbers must come from the same submission",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_cache_registrations_all_succeed_and_leave_one_coherent_row() {
    let (db, _temp) = setup("cache").await;
    let (_user_id, repo_id) = fixture(&db).await;

    // Path, size and digest describe one uploaded blob. A row assembled from
    // two uploads would point at one file and vouch for another.
    let attempts = (0..ATTEMPTS).map(|i| {
        let db = db.clone();
        async move {
            rg_db::ops::ci_retention_ops::upsert_cache_entry(
                &db,
                repo_id,
                "key-hash-1",
                &format!("cache-{i}.tar"),
                i as i64,
                Some(&format!("sha-{i}")),
                7,
            )
            .await
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "cache registration");

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM ci_cache_entries WHERE key_hash = 'key-hash-1'",
        )
        .await,
        1,
        "one cache key in one repository must occupy exactly one row",
    );

    let entry = rg_db::ops::ci_retention_ops::find_cache_entry(&db, repo_id, "key-hash-1")
        .await
        .expect("read the cache entry back")
        .expect("the entry exists");
    assert_eq!(entry.file_path, format!("cache-{}.tar", entry.size));
    assert_eq!(
        entry.sha256.as_deref(),
        Some(format!("sha-{}", entry.size).as_str()),
        "the digest must belong to the file the row points at",
    );
}

/// An eviction can win after the database has selected the conflicting cache
/// row but before the conflict UPDATE writes it. SQLite's trigger makes that
/// exact interleaving deterministic: the first convergence statement loses its
/// target, and the bounded second one must create the fresh publication.
#[tokio::test]
async fn cache_registration_converges_when_eviction_removes_the_conflict_target() {
    let (db, _temp) = setup("cache-eviction").await;
    let (_user_id, repo_id) = fixture(&db).await;
    let old = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        &db,
        repo_id,
        "evicted-key",
        "old-cache.tar",
        3,
        Some("old-sha"),
        7,
    )
    .await
    .expect("publish the old cache entry");

    db.execute_unprepared("CREATE TABLE cache_eviction_probe (n INTEGER NOT NULL)")
        .await
        .expect("create the eviction proof table");
    db.execute_unprepared(&format!(
        "CREATE TRIGGER evict_cache_inside_upsert BEFORE UPDATE ON ci_cache_entries \
         WHEN OLD.id = {} \
         BEGIN \
             INSERT INTO cache_eviction_probe (n) VALUES (1); \
             DELETE FROM ci_cache_entries WHERE id = OLD.id; \
         END;",
        old.id
    ))
    .await
    .expect("install the deterministic retention eviction");

    let fresh = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        &db,
        repo_id,
        "evicted-key",
        "fresh-cache.tar",
        5,
        Some("fresh-sha"),
        7,
    )
    .await
    .expect("eviction is convergence, not RecordNotUpdated");

    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM cache_eviction_probe").await,
        1,
        "the deterministic eviction did not run",
    );
    assert_eq!(fresh.file_path, "fresh-cache.tar");
    assert_eq!(fresh.size, 5);
    assert_eq!(fresh.sha256.as_deref(), Some("fresh-sha"));
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM ci_cache_entries WHERE key_hash = 'evicted-key'",
        )
        .await,
        1,
        "the bounded convergence left anything other than one fresh row",
    );
}

/// Readers may extend the publication they observed, but must never point the
/// row back at it after a newer upload won. The same stale snapshot must also be
/// powerless as an eviction candidate once the row is fresh again.
#[tokio::test]
async fn stale_cache_refresh_and_eviction_cannot_replace_or_delete_a_new_publication() {
    let (db, _temp) = setup("cache-stale-reader").await;
    let (_user_id, repo_id) = fixture(&db).await;
    let old = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        &db,
        repo_id,
        "refresh-key",
        "old-cache.tar",
        3,
        Some("old-sha"),
        7,
    )
    .await
    .expect("publish the old cache entry");
    let fresh = rg_db::ops::ci_retention_ops::upsert_cache_entry(
        &db,
        repo_id,
        "refresh-key",
        "fresh-cache.tar",
        5,
        Some("fresh-sha"),
        7,
    )
    .await
    .expect("publish the replacement cache entry");

    assert!(
        !rg_db::ops::ci_retention_ops::refresh_cache_entry(&db, &old, 7)
            .await
            .expect("classify the stale refresh"),
        "a stale reader refreshed the superseded publication",
    );

    // Make the replacement independently eligible for eviction. The stale
    // snapshot must still be powerless: publication identity, not merely the
    // current expiry, owns the delete.
    let mut expired_fresh: rg_db::entities::ci_cache_entry::ActiveModel = fresh.clone().into();
    expired_fresh.expires_at = Set(chrono::Utc::now() - chrono::Duration::days(1));
    expired_fresh
        .update(&db)
        .await
        .expect("expire the replacement publication");
    assert!(
        !rg_db::ops::ci_retention_ops::delete_cache_entry_if_expired(&db, &old)
            .await
            .expect("classify the stale eviction"),
        "a stale eviction deleted the replacement publication",
    );

    let current = rg_db::ops::ci_retention_ops::find_cache_entry(&db, repo_id, "refresh-key")
        .await
        .expect("read the surviving publication")
        .expect("the replacement publication survives");
    assert_eq!(current.id, fresh.id);
    assert_eq!(current.file_path, "fresh-cache.tar");
    assert_eq!(current.size, 5);
    assert_eq!(current.sha256.as_deref(), Some("fresh-sha"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_instance_settings_saves_all_succeed_and_leave_one_coherent_row() {
    let (db, _temp) = setup("settings").await;

    let attempts = (0..ATTEMPTS).map(|i| {
        let db = db.clone();
        async move {
            rg_db::ops::instance_settings_ops::save(
                &db,
                true,
                Some(&format!("banner-{i}")),
                &format!("type-{i}"),
            )
            .await
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "instance settings save");

    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM instance_settings").await,
        1,
        "the settings singleton must stay a singleton",
    );

    let settings = rg_db::ops::instance_settings_ops::find(&db)
        .await
        .expect("read the settings back")
        .expect("the row exists");
    assert!(settings.maintenance_mode);
    let admin = settings
        .banner_type
        .strip_prefix("type-")
        .expect("the banner type came from one of the callers");
    assert_eq!(
        settings.banner_message.as_deref(),
        Some(format!("banner-{admin}").as_str()),
        "banner text and type must come from the same submission",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_watch_writes_all_succeed_and_leave_one_row() {
    let (db, _temp) = setup("watch").await;
    let (user_id, repo_id) = fixture(&db).await;

    let attempts = (0..ATTEMPTS).map(|_| {
        let db = db.clone();
        async move {
            rg_db::ops::repo_watch_ops::set_watch_state(&db, user_id, repo_id, "watching").await
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "watch write");

    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM repo_watches").await,
        1,
        "one user watching one repository must occupy exactly one row",
    );
    assert_eq!(
        rg_db::ops::repo_watch_ops::get_watch_state(&db, user_id, repo_id)
            .await
            .expect("read the watch state back"),
        Some("watching".to_string()),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_star_toggles_all_succeed_and_leave_a_coherent_counter() {
    let (db, _temp) = setup("star").await;
    let (user_id, repo_id) = fixture(&db).await;

    let attempts = (0..ATTEMPTS).map(|_| {
        let db = db.clone();
        async move {
            let starred = rg_db::ops::repo_star_ops::toggle_star(&db, user_id, repo_id).await?;
            // This is the same write sequence as `rg_core::repo::service::toggle_star`.
            rg_db::ops::repo_ops::update_stars_count(&db, repo_id).await?;
            Ok::<bool, anyhow::Error>(starred)
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "star toggle");

    let actual = scalar(
        &db,
        &format!(
            "SELECT COUNT(*) AS n FROM repo_stars WHERE user_id = {user_id} AND repo_id = {repo_id}"
        ),
    )
    .await;
    assert!(
        (0..=1).contains(&actual),
        "one user may hold at most one star for one repository"
    );
    assert_eq!(
        scalar(
            &db,
            &format!("SELECT stars_count AS n FROM repositories WHERE id = {repo_id}"),
        )
        .await,
        actual,
        "the cached star count must match the rows after concurrent toggles",
    );
}

/// card_9dbabcda3c19: tagged manifest writes have two distinct UNIQUE keys.
/// Two first pushes of `latest` can both observe no tag, but the loser must
/// recover only when the winner occupies that same tag.  A digest collision at
/// another tag is not interchangeable with this race.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_manifest_tag_pushes_all_succeed_and_leave_one_tag_row() {
    let (db, _temp) = setup("manifest-tag").await;
    let (user_id, repo_id) = fixture(&db).await;
    let oci_repo = rg_db::ops::oci_ops::find_or_create_repo(&db, repo_id, "registry", user_id)
        .await
        .expect("create the OCI repository the tag belongs to");

    let attempts = (0..ATTEMPTS).map(|i| {
        let db = db.clone();
        async move {
            let digest = format!("sha256:{i:064x}");
            let manifest_json = format!(r#"{{"race_writer":{i}}}"#);
            rg_db::ops::oci_ops::upsert_tag_manifest(
                &db,
                oci_repo.id,
                "latest",
                &digest,
                "application/vnd.docker.distribution.manifest.v2+json",
                manifest_json.len() as i64,
                &manifest_json,
                2,
                &[],
            )
            .await
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "OCI manifest tag push");

    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) AS n FROM oci_tag WHERE oci_repository_id = {} AND tag = 'latest'",
                oci_repo.id
            ),
        )
        .await,
        1,
        "concurrent pushes must leave exactly one row for one tag",
    );

    let winner = rg_db::ops::oci_ops::find_manifest_by_tag(&db, oci_repo.id, "latest")
        .await
        .expect("read the winning tag")
        .expect("one tag row remains");
    assert!(
        results
            .iter()
            .filter_map(|result| result.as_ref().ok())
            .any(|manifest| {
                manifest.digest == winner.digest && manifest.manifest_json == winner.manifest_json
            }),
        "the surviving row must be one complete push, not a fabricated merge",
    );
}

/// card_56f118bbe845: an image answers to as many names as it was given.
///
/// `docker tag app:$SHA app:latest` followed by two pushes is the most ordinary
/// sequence a CI runs, and both pushes carry the identical manifest body. While
/// the tag lived as a unique column on the image row the second one collided
/// with the digest key and the handler answered `500`; both names must now be
/// recorded, resolve to the same bytes, and appear in `tags/list`.
#[tokio::test]
async fn a_second_tag_on_one_image_is_recorded_rather_than_refused() {
    let (db, _temp) = setup("manifest-second-tag").await;
    let (user_id, repo_id) = fixture(&db).await;
    let oci_repo = rg_db::ops::oci_ops::find_or_create_repo(&db, repo_id, "registry", user_id)
        .await
        .expect("create the OCI repository the tags belong to");
    let digest = "sha256:one-image-two-names";
    let manifest_json = r#"{"race_writer":"first"}"#;

    let push_under = |tag: &'static str| {
        let db = db.clone();
        async move {
            rg_db::ops::oci_ops::upsert_tag_manifest(
                &db,
                oci_repo.id,
                tag,
                digest,
                "application/vnd.docker.distribution.manifest.v2+json",
                manifest_json.len() as i64,
                manifest_json,
                2,
                &[],
            )
            .await
        }
    };

    let first = push_under("v1").await.expect("the first name is recorded");
    let second = push_under("latest")
        .await
        .expect("a second name on one image must not be refused");
    assert_eq!(
        first.id, second.id,
        "the same bytes must remain one content-addressed row under both names",
    );

    for tag in ["v1", "latest"] {
        assert_eq!(
            rg_db::ops::oci_ops::find_manifest_by_tag(&db, oci_repo.id, tag)
                .await
                .unwrap_or_else(|error| panic!("read {tag}: {error}"))
                .unwrap_or_else(|| panic!("{tag} must name the image it was pushed under"))
                .manifest_json,
            manifest_json,
        );
    }
    assert_eq!(
        rg_db::ops::oci_ops::list_tags(&db, oci_repo.id, None, None)
            .await
            .expect("list the tags of the image"),
        vec!["latest".to_string(), "v1".to_string()],
    );
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) AS n FROM oci_manifest WHERE oci_repository_id = {}",
                oci_repo.id
            ),
        )
        .await,
        1,
        "two names for one digest must not duplicate the image row",
    );
}

/// Moving a tag to different bytes still replaces exactly one mapping, and
/// leaves the image the tag used to name reachable by digest.
#[tokio::test]
async fn moving_a_tag_repoints_it_without_disturbing_the_other_names() {
    let (db, _temp) = setup("manifest-tag-move").await;
    let (user_id, repo_id) = fixture(&db).await;
    let oci_repo = rg_db::ops::oci_ops::find_or_create_repo(&db, repo_id, "registry", user_id)
        .await
        .expect("create the OCI repository the tags belong to");

    let push = |tag: &'static str, digest: &'static str, body: &'static str| {
        let db = db.clone();
        async move {
            rg_db::ops::oci_ops::upsert_tag_manifest(
                &db,
                oci_repo.id,
                tag,
                digest,
                "application/vnd.docker.distribution.manifest.v2+json",
                body.len() as i64,
                body,
                2,
                &[],
            )
            .await
        }
    };

    let old_body = r#"{"build":"old"}"#;
    let new_body = r#"{"build":"new"}"#;
    push("v1", "sha256:old", old_body).await.expect("push v1");
    push("latest", "sha256:old", old_body)
        .await
        .expect("point latest at the same image");
    push("latest", "sha256:new", new_body)
        .await
        .expect("move latest onto the new image");

    assert_eq!(
        rg_db::ops::oci_ops::find_manifest_by_tag(&db, oci_repo.id, "latest")
            .await
            .expect("read the moved tag")
            .expect("latest still names an image")
            .manifest_json,
        new_body,
    );
    assert_eq!(
        rg_db::ops::oci_ops::find_manifest_by_tag(&db, oci_repo.id, "v1")
            .await
            .expect("read the untouched tag")
            .expect("v1 still names an image")
            .manifest_json,
        old_body,
        "moving one name must not drag the other names with it",
    );
    assert!(
        rg_db::ops::oci_ops::find_manifest_by_digest(&db, oci_repo.id, "sha256:old")
            .await
            .expect("read the superseded image")
            .is_some(),
        "the image a moved tag left behind stays pullable by digest",
    );
}

/// The re-read is armed by a UNIQUE violation and nothing else. Each primitive
/// is given a write that fails on a foreign key instead — SQLite enforces them
/// (`connect_sqlite` sets `foreign_keys = ON`) — and must still report failure
/// rather than re-read its way to a fabricated success.
#[tokio::test]
async fn writes_that_fail_on_something_other_than_uniqueness_are_still_errors() {
    let (db, _temp) = setup("fk").await;
    let (user_id, _repo_id) = fixture(&db).await;

    const ORPHAN: i64 = 9999;
    let now = chrono::Utc::now();

    let status = rg_db::ops::commit_status_ops::create_or_update(
        &db,
        ORPHAN,
        "deadbeef",
        "ci/build",
        commit_status::ActiveModel {
            id: NotSet,
            repo_id: Set(ORPHAN),
            sha: Set("deadbeef".to_string()),
            state: Set("success".to_string()),
            context: Set("ci/build".to_string()),
            description: Set(None),
            target_url: Set(None),
            creator_id: Set(Some(user_id)),
            created_at: Set(now),
            updated_at: Set(now),
        },
    )
    .await;
    assert!(
        status.is_err(),
        "a status for a repository that does not exist must stay a failure",
    );

    assert!(
        rg_db::ops::ci_secret_ops::upsert(&db, ORPHAN, "DEPLOY_TOKEN", "ciphertext", user_id)
            .await
            .is_err(),
        "a secret for a repository that does not exist must stay a failure",
    );
    assert!(
        rg_db::ops::ci_retention_ops::upsert_policy(&db, ORPHAN, 30, 7)
            .await
            .is_err(),
        "a policy for a repository that does not exist must stay a failure",
    );
    assert!(
        rg_db::ops::ci_retention_ops::upsert_cache_entry(
            &db,
            ORPHAN,
            "key-hash-1",
            "cache.tar",
            1,
            None,
            7,
        )
        .await
        .is_err(),
        "a cache entry for a repository that does not exist must stay a failure",
    );
    assert!(
        rg_db::ops::repo_watch_ops::set_watch_state(&db, ORPHAN, ORPHAN, "watching")
            .await
            .is_err(),
        "a watch for a user and repository that do not exist must stay a failure",
    );
    assert!(
        rg_db::ops::repo_star_ops::toggle_star(&db, ORPHAN, ORPHAN)
            .await
            .is_err(),
        "a star for a user and repository that do not exist must stay a failure",
    );

    for (table, what) in [
        ("commit_statuses", "commit status"),
        ("ci_secrets", "CI secret"),
        ("ci_retention_policies", "retention policy"),
        ("ci_cache_entries", "cache entry"),
        ("repo_watches", "watch"),
        ("repo_stars", "star"),
    ] {
        assert_eq!(
            scalar(&db, &format!("SELECT COUNT(*) AS n FROM {table}")).await,
            0,
            "nothing may be written when the {what} insert failed",
        );
    }
}

/// card_2827bf918a9d: the first two steps of a package publish are the same
/// read-then-insert. A repository that has never served a package type, and a
/// package name that has never been published, are both created by whichever
/// request arrives first — and the request behind it contradicts nothing, so it
/// must adopt that row instead of failing on the UNIQUE index.
///
/// The ordinary traffic that reaches this: two CI jobs publishing *different
/// versions of one new package* at the same time. The loser used to take a 5xx.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_package_publishes_all_succeed_and_leave_one_registry_and_package() {
    let (db, _temp) = setup("package-registry").await;
    let (user_id, repo_id) = fixture(&db).await;

    // Step 1 and step 2 of `publish`, run together the way two jobs run them:
    // every caller creates the registry and then the package, and each carries
    // its own descriptive columns so a blended row would be visible.
    let attempts = (0..ATTEMPTS).map(|i| {
        let db = db.clone();
        async move {
            let registry = rg_db::ops::package_registry_ops::find_or_create(&db, repo_id, "npm")
                .await
                .map_err(|error| format!("registry: {error}"))?;
            let package = rg_db::ops::package_ops::find_or_create(
                &db,
                registry.id,
                user_id,
                "matrix-race",
                Some(&format!("published by {i}")),
                Some(&format!("https://example.invalid/{i}")),
                None,
            )
            .await
            .map_err(|error| format!("package: {error}"))?;
            Ok::<(i64, i64), String>((registry.id, package.id))
        }
    });

    let results = futures_join_all(attempts).await;
    assert_all_ok(&results, "package publish");

    // Everyone ends up on the same two rows — not merely "nobody errored".
    let first = *results[0].as_ref().expect("the first caller succeeded");
    for (i, result) in results.iter().enumerate() {
        assert_eq!(
            *result.as_ref().expect("checked above"),
            first,
            "caller {i} must publish into the same registry and package as the rest",
        );
    }

    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) AS n FROM package_registry \
                 WHERE repo_id = {repo_id} AND package_type = 'npm'"
            ),
        )
        .await,
        1,
        "one repository serves one package type from exactly one registry row",
    );
    assert_eq!(
        scalar(
            &db,
            &format!(
                "SELECT COUNT(*) AS n FROM packages \
                 WHERE package_registry_id = {} AND name = 'matrix-race'",
                first.0
            ),
        )
        .await,
        1,
        "one package name occupies exactly one row in its registry",
    );

    // The surviving package row belongs to one publisher, not to a merge of two.
    let package = rg_db::ops::package_ops::find_by_registry_and_name(&db, first.0, "matrix-race")
        .await
        .expect("read the package back")
        .expect("the package exists");
    let publisher = package
        .description
        .as_deref()
        .and_then(|d| d.strip_prefix("published by "))
        .expect("the description came from one of the callers")
        .to_string();
    assert_eq!(
        package.homepage.as_deref(),
        Some(format!("https://example.invalid/{publisher}").as_str()),
        "description and homepage must come from the same publish",
    );
}

/// The adoption is keyed on the whole schema key, not on the part that reads
/// like an identity. `idx_package_registry_name` is UNIQUE on
/// `(package_registry_id, name)`, so the same package name in two registries is
/// two rows — a re-read by name alone would hand the second caller the first
/// registry's package and publish npm versions into the cargo registry.
///
/// (The usual companion guard — force a non-UNIQUE failure and check it is
/// still an error — has nothing to force here: unlike most of this file's
/// tables, `package_registry` and `packages` declare no foreign keys at all, so
/// an orphan insert simply succeeds. That gap is filed separately; it is not
/// something this test can assert around.)
#[tokio::test]
async fn one_package_name_in_two_registries_stays_two_rows() {
    let (db, _temp) = setup("package-key").await;
    let (user_id, repo_id) = fixture(&db).await;

    let npm = rg_db::ops::package_registry_ops::find_or_create(&db, repo_id, "npm")
        .await
        .expect("create the npm registry");
    let cargo = rg_db::ops::package_registry_ops::find_or_create(&db, repo_id, "cargo")
        .await
        .expect("create the cargo registry");
    assert_ne!(npm.id, cargo.id, "two package types are two registries");

    let in_npm =
        rg_db::ops::package_ops::find_or_create(&db, npm.id, user_id, "shared", None, None, None)
            .await
            .expect("create the npm package");
    let in_cargo =
        rg_db::ops::package_ops::find_or_create(&db, cargo.id, user_id, "shared", None, None, None)
            .await
            .expect("create the cargo package");

    assert_ne!(
        in_npm.id, in_cargo.id,
        "one name in two registries must not collapse onto one package row",
    );
    assert_eq!(in_cargo.package_registry_id, cargo.id);
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM packages WHERE name = 'shared'"
        )
        .await,
        2,
    );
}

/// `futures::future::join_all` without taking a dependency on `futures` for one
/// call: poll the futures together by handing them to the runtime as tasks.
async fn futures_join_all<F>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.expect("task panicked"));
    }
    out
}
