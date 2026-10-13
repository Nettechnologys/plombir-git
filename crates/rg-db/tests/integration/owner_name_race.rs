//! card_8f3f821705f2: an owner name has one holder, an account or an
//! organization, and the database — not a pre-read — is what makes it so.
//!
//! `rg_core::namespace::owner_name_is_taken` reads `users` and then
//! `organizations`, and no unique key spanned the two: two creates of one name
//! in the two tables could both read "free" and both insert. These tests skip
//! the pre-read entirely and race the inserts themselves, which is the only way
//! to see what the schema enforces. `PLOMBIR_GIT_TEST_DATABASE_URL` points the
//! file at PostgreSQL or MySQL instead of a throwaway SQLite database.

use rg_db::sea_orm::DatabaseConnection;

struct TempDb(Option<std::path::PathBuf>);

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
            }
        }
    }
}

async fn setup() -> (DatabaseConnection, TempDb, String) {
    let suffix = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
    let (url, temp) = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => (url, TempDb(None)),
        _ => {
            let path = std::env::temp_dir().join(format!("plombir-git-owner-names-{suffix}.db"));
            (
                format!("sqlite://{}?mode=rwc", path.display()),
                TempDb(Some(path)),
            )
        }
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 8)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp, suffix)
}

async fn account(db: &DatabaseConnection, name: &str) -> anyhow::Result<i64> {
    rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.invalid"), "", name)
        .await
        .map(|user| user.id)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_and_an_organization_cannot_both_take_one_name() {
    let (db, _temp, suffix) = setup().await;
    let founder = account(&db, &format!("founder{suffix}"))
        .await
        .expect("the organization founder");

    for round in 0..10 {
        let name = format!("acme{suffix}r{round}");
        let user = {
            let db = db.clone();
            let name = name.clone();
            tokio::spawn(async move { account(&db, &name).await })
        };
        let org = {
            let db = db.clone();
            let name = name.clone();
            tokio::spawn(async move {
                rg_db::ops::org_ops::create_org(&db, &name, None, None, founder, "public")
                    .await
                    .map(|org| org.id)
            })
        };
        let user = user.await.expect("user task");
        let org = org.await.expect("org task");

        assert!(
            user.is_ok() != org.is_ok(),
            "round {round}: exactly one of the two creates of '{name}' must land \
             (account: {user:?}, organization: {org:?})"
        );
        let refused = user.err().or(org.err()).expect("one was refused");
        assert!(
            rg_db::is_unique_violation_anyhow(&refused),
            "round {round}: the loser must read as a name already taken, not a failure: \
             {refused:#}"
        );
    }
}

/// The name follows its holder: renaming nothing, deleting the account frees
/// it, and a free name can be taken by the other kind.
#[tokio::test]
async fn a_deleted_holder_releases_its_name() {
    let (db, _temp, suffix) = setup().await;
    let founder = account(&db, &format!("founder{suffix}"))
        .await
        .expect("the organization founder");
    let name = format!("shared{suffix}");
    let user = account(&db, &name).await.expect("the first holder");

    let refused = rg_db::ops::org_ops::create_org(&db, &name, None, None, founder, "public")
        .await
        .expect_err("an account holds the name");
    assert!(rg_db::is_unique_violation_anyhow(&refused), "{refused:#}");

    use rg_db::sea_orm::EntityTrait;
    rg_db::entities::user::Entity::delete_by_id(user)
        .exec(&db)
        .await
        .expect("delete the account");
    rg_db::ops::org_ops::create_org(&db, &name, None, None, founder, "public")
        .await
        .expect("the released name is free again");
    let taken = account(&db, &name)
        .await
        .expect_err("now the organization holds it");
    assert!(rg_db::is_unique_violation_anyhow(&taken), "{taken:#}");
}

/// card_2095e88c98f1: one holder per name in any case, on every backend — not
/// only under MySQL's case-insensitive collation. The holder itself may still
/// change the case of its own name, and an update that is not a rename never
/// touches the key.
#[tokio::test]
async fn a_name_has_one_holder_in_any_case() {
    let (db, _temp, suffix) = setup().await;
    let founder = account(&db, &format!("founder{suffix}"))
        .await
        .expect("the organization founder");
    let name = format!("Twin{suffix}");
    let user = account(&db, &name).await.expect("the first holder");

    let lower = account(&db, &name.to_lowercase())
        .await
        .expect_err("an account in another case");
    assert!(rg_db::is_unique_violation_anyhow(&lower), "{lower:#}");
    let upper =
        rg_db::ops::org_ops::create_org(&db, &name.to_uppercase(), None, None, founder, "public")
            .await
            .expect_err("an organization in another case");
    assert!(rg_db::is_unique_violation_anyhow(&upper), "{upper:#}");
    assert!(
        rg_db::ops::owner_name_ops::is_held_in_any_case(&db, &name.to_uppercase())
            .await
            .expect("look the name up")
    );

    use rg_db::sea_orm::ConnectionTrait;
    db.execute_unprepared(&format!(
        "UPDATE users SET display_name = 'Renamed display' WHERE id = {user}"
    ))
    .await
    .expect("an update that is not a rename");
    let recased = name.to_lowercase();
    db.execute_unprepared(&format!(
        "UPDATE users SET username = '{recased}' WHERE id = {user}"
    ))
    .await
    .expect("the holder changes only the case of its own name");
    assert!(rg_db::ops::owner_name_ops::is_held_in_any_case(&db, &name)
        .await
        .expect("look the name up"));
    assert!(
        rg_db::ops::owner_name_ops::list_names_held_only_in_another_case(&db)
            .await
            .expect("list case twins")
            .is_empty(),
        "a fresh database has no keyless names"
    );
}
