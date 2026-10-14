//! A `SUM` over an integer column decodes on every backend.
//!
//! PostgreSQL widens `sum(bigint)` to `numeric` and MySQL answers `DECIMAL`
//! for any exact-integer `SUM`; sqlx refuses either one as `i64` — but only
//! when the value is not `NULL`, so an empty table passes and the first stored
//! row turns `GET /lfs/usage` and the LFS sweep into a database error. SQLite
//! answers `INTEGER`, which is why nothing that runs only on SQLite could see
//! it. These tests honour `PLOMBIR_GIT_TEST_DATABASE_URL` and run in the
//! PostgreSQL and MySQL smoke jobs for exactly that reason.

use rg_db::entities::{
    attachment, ci_cache_entry, issue, lfs_object, oci_blob, oci_repository, package, package_file,
    package_registry, package_version, release, release_asset, repository, time_entry,
};
use rg_db::sea_orm::{ActiveModelTrait, DatabaseConnection, NotSet, Set};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-aggregate-sums-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
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

/// A migrated database with an account, a repository and an issue of its own.
/// Names carry a uuid so repeated runs against one server database never meet.
async fn setup() -> (DatabaseConnection, Option<TempDb>, i64, i64) {
    let (db, temp, _user_id, repo_id, issue_id) = setup_with_owner().await;
    (db, temp, repo_id, issue_id)
}

/// [`setup`], also naming the account that owns the repository.
async fn setup_with_owner() -> (DatabaseConnection, Option<TempDb>, i64, i64, i64) {
    let (url, temp) = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => (url, None),
        _ => {
            let temp = TempDb::new();
            (temp.url(), Some(temp))
        }
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let user = rg_db::ops::user_ops::create_user(
        &db,
        &format!("sums{suffix}"),
        &format!("sums{suffix}@example.com"),
        "",
        "Sums",
    )
    .await
    .expect("create the account the rows hang off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        repository::ActiveModel {
            id: NotSet,
            owner_id: Set(user.id),
            name: Set(format!("sums{suffix}")),
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
    let issue = issue::ActiveModel {
        id: NotSet,
        repo_id: Set(repo.id),
        number: Set(1),
        title: Set("time is tracked here".to_string()),
        body: Set(None),
        state: Set("open".to_string()),
        author_id: Set(user.id),
        assignee_id: Set(None),
        milestone_id: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        closed_at: Set(None),
        deleted_at: Set(None),
    }
    .insert(&db)
    .await
    .expect("create the issue the time entries hang off");
    (db, temp, user.id, repo.id, issue.id)
}

#[tokio::test]
async fn lfs_usage_sums_stored_objects_on_every_backend() {
    let (db, _temp, repo_id, _issue_id) = setup().await;

    // The empty table is the case that always worked: `SUM` of nothing is NULL.
    assert_eq!(
        rg_db::ops::lfs_object_ops::usage(&db, repo_id)
            .await
            .expect("usage of an empty repository"),
        (0, 0)
    );

    // Past `i32::MAX`, so a backend that narrowed the total would show it.
    let sizes = [3_000_000_000_i64, 7];
    for (index, size) in sizes.into_iter().enumerate() {
        rg_db::ops::lfs_object_ops::create(
            &db,
            lfs_object::ActiveModel {
                id: NotSet,
                repo_id: Set(repo_id),
                oid: Set(format!("{index:064x}")),
                size: Set(size),
                uploaded: Set(true),
                created_at: Set(chrono::Utc::now()),
                publisher_token: Set(None),
                publisher_since: Set(None),
                last_claimed_at: Set(None),
            },
        )
        .await
        .expect("store an LFS object");
    }

    assert_eq!(
        rg_db::ops::lfs_object_ops::usage(&db, repo_id)
            .await
            .expect("a SUM over stored objects must decode, not fail on its numeric type"),
        (2, 3_000_000_007)
    );
}

#[tokio::test]
async fn tracked_minutes_sum_on_every_backend() {
    let (db, _temp, _repo_id, issue_id) = setup().await;

    for minutes in [45_i64, 90] {
        rg_db::ops::time_entry_ops::create(
            &db,
            time_entry::ActiveModel {
                id: NotSet,
                issue_id: Set(issue_id),
                user_id: Set(None),
                duration_minutes: Set(minutes),
                description: Set(None),
                created_at: Set(chrono::Utc::now()),
            },
        )
        .await
        .expect("log time");
    }

    assert_eq!(
        rg_db::ops::time_entry_ops::total_minutes_by_issue(&db, issue_id)
            .await
            .expect("a SUM over logged minutes must decode, not fail on its numeric type"),
        135
    );
}

/// Past `i32::MAX` and non-NULL in every store: the sums the storage budget
/// reads must decode on PostgreSQL (`numeric`) and MySQL (`DECIMAL`), or the
/// second upload to any repository with a quota configured answers 5xx.
const BIG: i64 = 3_000_000_000;

#[tokio::test]
async fn storage_budget_sums_decode_in_every_store_on_every_backend() {
    let (db, _temp, user_id, repo_id, _issue_id) = setup_with_owner().await;
    let now = chrono::Utc::now();
    let tag = uuid::Uuid::new_v4().simple().to_string();

    // The empty answers first: the case that always worked.
    assert_eq!(
        rg_db::ops::release_ops::repo_asset_usage(&db, repo_id)
            .await
            .unwrap(),
        (0, 0)
    );
    assert_eq!(
        rg_db::ops::ci_retention_ops::repo_cache_usage(&db, repo_id)
            .await
            .unwrap(),
        (0, 0)
    );
    assert_eq!(
        rg_db::ops::package_file_ops::repo_usage(&db, repo_id)
            .await
            .unwrap(),
        (0, 0)
    );
    assert_eq!(
        rg_db::ops::oci_ops::repo_blob_usage(&db, repo_id)
            .await
            .unwrap(),
        (0, 0)
    );
    assert_eq!(
        rg_db::ops::attachment_ops::repo_size(&db, repo_id)
            .await
            .unwrap(),
        0
    );

    let release = release::ActiveModel {
        repo_id: Set(repo_id),
        tag_name: Set(format!("v{tag}")),
        target_commitish: Set("main".to_string()),
        title: Set("sums".to_string()),
        body: Set(None),
        is_draft: Set(false),
        is_prerelease: Set(false),
        author_id: Set(Some(user_id)),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create a release");
    for (index, size) in [BIG, 7].into_iter().enumerate() {
        release_asset::ActiveModel {
            release_id: Set(release.id),
            filename: Set(format!("asset-{index}.bin")),
            size: Set(size),
            content_type: Set("application/octet-stream".to_string()),
            download_count: Set(0),
            uploader_id: Set(Some(user_id)),
            created_at: Set(now),
            sha256: Set(None),
            attestation: Set(None),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("store a release asset");
    }
    assert_eq!(
        rg_db::ops::release_ops::repo_asset_usage(&db, repo_id)
            .await
            .expect("release asset SUM must decode"),
        (2, BIG + 7)
    );
    assert_eq!(
        rg_db::ops::release_ops::count_assets(&db, release.id)
            .await
            .unwrap(),
        2
    );

    for (index, size) in [BIG, 5].into_iter().enumerate() {
        ci_cache_entry::ActiveModel {
            repo_id: Set(repo_id),
            key_hash: Set(format!("{tag}-{index}")),
            file_path: Set(format!("cache/{tag}-{index}")),
            size: Set(size),
            sha256: Set(None),
            created_at: Set(now),
            expires_at: Set(now + chrono::Duration::days(1)),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("store a CI cache entry");
    }
    assert_eq!(
        rg_db::ops::ci_retention_ops::repo_cache_usage(&db, repo_id)
            .await
            .expect("CI cache SUM must decode"),
        (2, BIG + 5)
    );

    let registry = package_registry::ActiveModel {
        repo_id: Set(repo_id),
        package_type: Set("generic".to_string()),
        enabled: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create a package registry");
    let pkg = package::ActiveModel {
        package_registry_id: Set(registry.id),
        owner_id: Set(user_id),
        name: Set(format!("pkg{tag}")),
        description: Set(None),
        homepage: Set(None),
        repository_url: Set(None),
        is_public: Set(true),
        download_count: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create a package");
    let version = package_version::ActiveModel {
        package_id: Set(pkg.id),
        version: Set("1.0.0".to_string()),
        protocol_version_key: Set(None),
        protocol_variant_key: Set(String::new()),
        semver: Set(None),
        metadata: Set(None),
        size: Set(BIG + 3),
        sha256: Set(None),
        is_yanked: Set(false),
        download_count: Set(0),
        author_id: Set(Some(user_id)),
        created_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create a package version");
    for (index, size) in [BIG, 3].into_iter().enumerate() {
        package_file::ActiveModel {
            version_id: Set(version.id),
            filename: Set(format!("file-{index}.bin")),
            size: Set(size),
            sha256: Set(None),
            sha1: Set(None),
            sha512: Set(None),
            storage_path: Set(format!("packages/{tag}/{index}")),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("store a package file");
    }
    assert_eq!(
        rg_db::ops::package_file_ops::repo_usage(&db, repo_id)
            .await
            .expect("package file SUM must decode"),
        (2, BIG + 3)
    );

    let oci_repo = oci_repository::ActiveModel {
        repo_id: Set(repo_id),
        namespace: Set(format!("sums/{tag}")),
        owner_id: Set(user_id),
        is_public: Set(false),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    }
    .insert(&db)
    .await
    .expect("create an OCI repository");
    for (index, size) in [BIG, 11].into_iter().enumerate() {
        oci_blob::ActiveModel {
            oci_repository_id: Set(oci_repo.id),
            digest: Set(format!("sha256:{index:064x}")),
            media_type: Set("application/octet-stream".to_string()),
            size: Set(size),
            storage_path: Set(format!("oci/{tag}/{index}")),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("store an OCI blob");
    }
    assert_eq!(
        rg_db::ops::oci_ops::repo_blob_usage(&db, repo_id)
            .await
            .expect("OCI blob SUM must decode"),
        (2, BIG + 11)
    );
    assert_eq!(
        rg_db::ops::oci_ops::find_repo_row_by_id(&db, oci_repo.id)
            .await
            .unwrap()
            .map(|row| row.repo_id),
        Some(repo_id)
    );

    for (index, size) in [BIG, 13].into_iter().enumerate() {
        attachment::ActiveModel {
            uuid: Set(format!("{tag}-{index}")),
            repo_id: Set(repo_id),
            uploader_id: Set(Some(user_id)),
            filename: Set(format!("note-{index}.txt")),
            blob_key: Set(format!("attachments/{tag}/{index}")),
            content_type: Set("text/plain".to_string()),
            size: Set(size),
            download_count: Set(0),
            created_at: Set(now),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("store an attachment");
    }
    assert_eq!(
        rg_db::ops::attachment_ops::repo_size(&db, repo_id)
            .await
            .expect("attachment SUM must decode"),
        BIG + 13
    );

    // Artifacts reach their repository through job → stage → pipeline: the
    // nested subqueries have to resolve on every backend, not only the SUM.
    assert_eq!(
        rg_db::ops::artifact_ops::repo_usage(&db, repo_id)
            .await
            .unwrap(),
        (0, 0)
    );
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &db,
        repo_id,
        "1111111111111111111111111111111111111111",
        "refs/heads/main",
        "push",
        None,
    )
    .await
    .expect("create a pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "build", 0)
        .await
        .expect("create a stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        &db, stage.id, "build", "true", None, None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create a job");
    for (index, size) in [BIG, 17].into_iter().enumerate() {
        rg_db::ops::artifact_ops::create_artifact(
            &db,
            job.id,
            &format!("artifact-{index}"),
            &format!("artifacts/{tag}/{index}"),
            size,
            None,
            None,
        )
        .await
        .expect("store a CI artifact");
    }
    assert_eq!(
        rg_db::ops::artifact_ops::repo_usage(&db, repo_id)
            .await
            .expect("CI artifact SUM must decode"),
        (2, BIG + 17)
    );
}
