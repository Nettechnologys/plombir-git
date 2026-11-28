//! card_adbe712869d6: a read-then-write transaction on SQLite must not answer
//! 500 because another connection committed while it was between its read and
//! its first write.
//!
//! SeaORM's `BEGIN` is `BEGIN DEFERRED`. The first `SELECT` takes a read
//! snapshot, the first write asks to upgrade it, and if anyone committed in
//! between SQLite refuses at once with `SQLITE_BUSY_SNAPSHOT` (517) —
//! `busy_timeout` is not consulted, because waiting cannot make the snapshot
//! fresh again. `rg_db::contention` describes the class; these are the
//! transactions the card's sweep found still shaped that way.
//!
//! Each test parks a writer on SQLite's writer slot that commits a change to an
//! unrelated table, and starts the transaction under test while it holds. On
//! the old code the subject read its snapshot under the holder, waited for the
//! lock, and was then refused with 517. A fixed subject either takes the writer
//! lock before it reads (so it simply waits its turn) or re-runs from a fresh
//! snapshot.

use std::time::Duration;

use sea_orm::{ConnectionTrait, TransactionTrait};

/// How long the competing writer keeps SQLite's write lock: long enough that
/// the subject has certainly read before the holder commits, well inside every
/// `busy_timeout` and retry budget.
const HOLD: Duration = Duration::from_millis(750);

async fn fresh_db(directory: &std::path::Path) -> sea_orm::DatabaseConnection {
    crate::common::migrated_sqlite(&directory.join("test.db"), 8).await
}

async fn account(db: &sea_orm::DatabaseConnection, username: &str) -> i64 {
    rg_db::ops::user_ops::create_user(
        db,
        username,
        &format!("{username}@example.invalid"),
        "",
        "Snapshot Owner",
    )
    .await
    .expect("create an account")
    .id
}

/// Hold SQLite's writer slot with an uncommitted change to `users`, commit it
/// after [`HOLD`]. Await the handle: a holder that died is reported as itself.
async fn hold_the_write_lock(db: &sea_orm::DatabaseConnection) -> tokio::task::JoinHandle<()> {
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let holder_db = db.clone();
    let holder = tokio::spawn(async move {
        let transaction = holder_db.begin().await.expect("open the holding writer");
        transaction
            .execute_unprepared("UPDATE users SET updated_at = updated_at")
            .await
            .expect("take SQLite's write lock");
        held_tx.send(()).expect("announce the lock is held");
        tokio::time::sleep(HOLD).await;
        transaction
            .commit()
            .await
            .expect("commit the unrelated write");
    });
    held_rx.await.expect("the writer took the lock");
    holder
}

/// The card's path: `PATCH /admin/settings` while another request commits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn instance_settings_survive_a_writer_committing_mid_transaction() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    account(&db, "settingsholder").await;
    let cache = rg_core::instance::InstanceSettingsCache::default();
    cache
        .update(&db, |settings| {
            settings.banner_message = Some("before".into())
        })
        .await
        .expect("seed the settings row");

    let holder = hold_the_write_lock(&db).await;
    let started = std::time::Instant::now();
    let updated = cache
        .update(&db, |settings| {
            settings.banner_message = Some("after".into())
        })
        .await;
    holder.await.expect("the holding writer finished");

    let change = updated
        .unwrap_or_else(|error| panic!("an unrelated commit broke the settings update: {error:#}"));
    assert_eq!(change.current.banner_message.as_deref(), Some("after"));
    assert!(
        started.elapsed() >= HOLD,
        "the update returned in {:?}, before the holder let go — nothing contended",
        started.elapsed()
    );
    let stored = rg_db::ops::instance_settings_ops::find(&db)
        .await
        .expect("read the row back")
        .expect("the row exists");
    assert_eq!(stored.banner_message.as_deref(), Some("after"));
}

/// The proof from the review, in its own shape: an unrelated request commits
/// *between* the settings read and the settings write. It now has to wait for
/// the settings transaction instead of overtaking it, and both writes land.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unrelated_write_waits_for_the_settings_transaction_instead_of_breaking_it() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let cache = rg_core::instance::InstanceSettingsCache::default();
    cache
        .update(&db, |settings| {
            settings.banner_message = Some("before".into())
        })
        .await
        .expect("seed the settings row");

    let other_db = db.clone();
    let mut other = None;
    let updated = cache
        .update(&db, |settings| {
            let writer = tokio::spawn(async move { account(&other_db, "otherwriter").await });
            // Give the other request every chance to commit in the middle: the
            // old shape let it (and then failed with 517); the fixed one holds
            // the writer lock, so it is still waiting when this gives up.
            let finished_inside = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    writer.is_finished()
                })
            });
            other = Some((writer, finished_inside));
            settings.banner_message = Some("after".into());
        })
        .await;
    let (writer, finished_inside) = other.expect("the update ran its closure");

    updated.unwrap_or_else(|error| panic!("a committed unrelated write broke settings: {error:#}"));
    writer.await.expect("the other request finished");
    assert!(
        !finished_inside,
        "another connection committed between the settings read and write: SQLite did not \
         serialize the writers, so the read can go stale again"
    );
}

/// The other form of the same proof: instances of a fleet, each with its own
/// memo, all saving at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_instance_settings_updates_all_succeed() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    rg_db::ops::instance_settings_ops::ensure_exists(&db)
        .await
        .expect("create the settings row");
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(12));
    let tasks = (0..12)
        .map(|index| {
            let db = db.clone();
            let barrier = barrier.clone();
            tokio::spawn(async move {
                let cache = rg_core::instance::InstanceSettingsCache::default();
                barrier.wait().await;
                cache
                    .update(&db, |settings| {
                        settings.banner_message = Some(format!("banner-{index}"))
                    })
                    .await
            })
        })
        .collect::<Vec<_>>();
    let mut errors = Vec::new();
    for task in tasks {
        if let Err(error) = task.await.expect("the update task finished") {
            errors.push(format!("{error:#}"));
        }
    }
    assert!(
        errors.is_empty(),
        "concurrent settings PATCH failed: {errors:?}"
    );
}

/// Sweep: `PATCH /admin/sso/providers/{id}` reads the provider, then writes it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_sso_provider_update_survives_a_writer_committing_mid_transaction() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    account(&db, "ssoholder").await;
    let input = |name| rg_db::ops::sso_provider_ops::SsoProviderInput {
        name,
        slug: "corp",
        provider_type: "oidc",
        client_id: Some("client"),
        client_secret_enc: None,
        discovery_url: Some("https://sso.example.invalid/.well-known/openid-configuration"),
        scopes: None,
        ldap_host: None,
        ldap_port: None,
        ldap_bind_dn: None,
        ldap_bind_password_enc: None,
        ldap_base_dn: None,
        ldap_user_filter: None,
        enabled: true,
        auto_provision: false,
        allowed_email_domains: None,
        icon_url: None,
    };
    let provider = rg_db::ops::sso_provider_ops::create(&db, input("Corp"))
        .await
        .expect("create the provider");

    let holder = hold_the_write_lock(&db).await;
    let started = std::time::Instant::now();
    let updated =
        rg_db::ops::sso_provider_ops::update_settings(&db, provider.id, input("Renamed")).await;
    holder.await.expect("the holding writer finished");

    let outcome = updated.unwrap_or_else(|error| {
        panic!("an unrelated commit broke the SSO provider update: {error}")
    });
    assert!(
        matches!(
            outcome,
            rg_db::ops::sso_provider_ops::SsoProviderUpdate::Written { .. }
        ),
        "{outcome:?}"
    );
    assert!(started.elapsed() >= HOLD, "nothing contended");
}

/// Sweep: `POST /repos/{owner}/{repo}/runners` reads the live repository, then
/// inserts the runner.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_runner_registration_survives_a_writer_committing_mid_transaction() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner = account(&db, "runnerholder").await;
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            owner_id: sea_orm::Set(owner),
            name: sea_orm::Set("runners".to_string()),
            is_private: sea_orm::Set(false),
            default_branch: sea_orm::Set("main".to_string()),
            created_at: sea_orm::Set(now),
            updated_at: sea_orm::Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("create the repository");

    let holder = hold_the_write_lock(&db).await;
    let started = std::time::Instant::now();
    let registered = rg_db::ops::runner_ops::register_runner(
        &db,
        repo.id,
        "contended",
        r#"["linux"]"#,
        None,
        None,
        None,
    )
    .await;
    holder.await.expect("the holding writer finished");

    let (runner, token) = registered
        .unwrap_or_else(|error| panic!("an unrelated commit broke runner registration: {error:#}"));
    assert!(started.elapsed() >= HOLD, "nothing contended");
    let stored = rg_db::ops::runner_ops::find_by_token(&db, &token)
        .await
        .expect("look the token up")
        .expect("the returned token is the committed one");
    assert_eq!(stored.id, runner.id);
}

/// card_8751c686a2da: `rg_db::contention::retry_transaction` itself, on a path
/// that really reads before it writes. Moving an npm dist-tag first looks the
/// target version up, then writes the tag, so on SQLite it is refused with 517
/// for as long as the holder keeps the lock. Under the old budget of eight
/// attempts that was spent in well under a second and the request answered
/// 503; under the time budget it waits the holder out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_npm_dist_tag_move_survives_a_writer_committing_mid_transaction() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner = account(&db, "disttagholder").await;
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            owner_id: sea_orm::Set(owner),
            name: sea_orm::Set("packages".to_string()),
            is_private: sea_orm::Set(false),
            default_branch: sea_orm::Set("main".to_string()),
            created_at: sea_orm::Set(now),
            updated_at: sea_orm::Set(now),
            ..Default::default()
        },
    )
    .await
    .expect("create the repository");
    let registry = rg_db::ops::package_registry_ops::create(&db, repo.id, "npm")
        .await
        .expect("create the npm registry");
    let package =
        rg_db::ops::package_ops::create(&db, registry.id, owner, "left-pad", None, None, None)
            .await
            .expect("create the package");
    for version in ["1.0.0", "2.0.0"] {
        rg_db::ops::package_version_ops::create(
            &db,
            package.id,
            version,
            None,
            Some(version),
            None,
            1,
            None,
            Some(owner),
        )
        .await
        .expect("create a version");
    }

    let holder = hold_the_write_lock(&db).await;
    let started = std::time::Instant::now();
    let moved = rg_core::package_registry::service::set_npm_dist_tag(
        &db,
        "disttagholder",
        "packages",
        "left-pad",
        "stable",
        "1.0.0",
    )
    .await;
    holder.await.expect("the holding writer finished");

    moved.unwrap_or_else(|error| panic!("an unrelated commit broke the dist-tag move: {error:#}"));
    assert!(started.elapsed() >= HOLD, "nothing contended");
    let tags = rg_core::package_registry::service::list_npm_dist_tags(
        &db,
        "disttagholder",
        "packages",
        "left-pad",
    )
    .await
    .expect("read the tags back");
    assert_eq!(
        tags.get("stable").map(String::as_str),
        Some("1.0.0"),
        "{tags:?}"
    );
}
