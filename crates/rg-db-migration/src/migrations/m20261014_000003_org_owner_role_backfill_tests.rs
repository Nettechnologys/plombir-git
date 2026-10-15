use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, Statement,
};
use sea_orm_migration::MigratorTrait;

use super::{backfill_owner_roles, NAME};
use rg_db::entities::{audit_log, organization_member};

struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new() -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-org-owner-backfill-{}.db",
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

async fn user(db: &DatabaseConnection, name: &str) -> i64 {
    rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.invalid"), "", name)
        .await
        .expect("create account")
        .id
}

/// An organization owned by `owner` in `owner_id`, with its membership left as
/// `role` — or removed altogether, the state an admin could leave it in on an
/// earlier release.
async fn org(db: &DatabaseConnection, name: &str, owner: i64, role: Option<&str>) -> i64 {
    let org = rg_db::ops::org_ops::create_org(db, name, None, None, owner, "private")
        .await
        .expect("create organization");
    match role {
        Some("owner") => {}
        Some(role) => {
            organization_member::Entity::update_many()
                .col_expr(
                    organization_member::Column::Role,
                    sea_orm::sea_query::Expr::value(role),
                )
                .filter(organization_member::Column::OrgId.eq(org.id))
                .exec(db)
                .await
                .expect("demote the creator");
        }
        None => {
            organization_member::Entity::delete_many()
                .filter(organization_member::Column::OrgId.eq(org.id))
                .exec(db)
                .await
                .expect("remove the creator");
        }
    }
    org.id
}

async fn owners(db: &DatabaseConnection, org_id: i64) -> Vec<organization_member::Model> {
    organization_member::Entity::find()
        .filter(organization_member::Column::OrgId.eq(org_id))
        .filter(organization_member::Column::Role.eq("owner"))
        .all(db)
        .await
        .expect("owner-role members decode — created_at included")
}

async fn backfill_audit(db: &DatabaseConnection, org_id: i64) -> Vec<serde_json::Value> {
    audit_log::Entity::find()
        .filter(audit_log::Column::Action.eq("org.owner_role_backfill"))
        .filter(audit_log::Column::ResourceId.eq(org_id))
        .all(db)
        .await
        .expect("audit rows decode")
        .into_iter()
        .map(|row| serde_json::from_str(row.details.as_deref().unwrap_or("{}")).unwrap())
        .collect()
}

/// The text a timestamp is stored as, with its digits blanked: two writers
/// agree on the format exactly when these match.
async fn created_at_shape(db: &DatabaseConnection, member_id: i64) -> String {
    let row = db
        .query_one(Statement::from_sql_and_values(
            db.get_database_backend(),
            "SELECT CAST(created_at AS TEXT) AS created_at FROM organization_members WHERE id = $1",
            [member_id.into()],
        ))
        .await
        .unwrap()
        .expect("membership row");
    let text: String = row.try_get("", "created_at").unwrap();
    text.chars()
        .map(|c| if c.is_ascii_digit() { '0' } else { c })
        .collect()
}

/// The acceptance of card_03d784041747, through the migrator on the schema
/// right before it: an organization with no owner-role member gets its
/// `owner_id` as one — inserted when the account had left, upgraded when it
/// was still a member — with an audit row each; an organization that has an
/// owner, one that is being deleted and one whose `owner_id` names nobody are
/// left alone.
#[tokio::test]
async fn organizations_without_an_owner_get_their_owner_id_as_one() {
    let temp = TempDb::new();
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to throwaway SQLite database");
    let before = crate::migrations::Migrator::migrations()
        .iter()
        .position(|migration| migration.name() == NAME)
        .unwrap_or_else(|| panic!("{NAME} must be part of the migration list"));
    crate::migrations::Migrator::up(&db, Some(u32::try_from(before).unwrap()))
        .await
        .expect("migrate to the schema right before the backfill");

    let alice = user(&db, "alice").await;
    let bob = user(&db, "bob").await;
    let carol = user(&db, "carol").await;
    let left = org(&db, "left", alice, None).await;
    let demoted = org(&db, "demoted", bob, Some("admin")).await;
    let owned = org(&db, "owned", carol, Some("owner")).await;
    let retiring = org(&db, "retiring", alice, None).await;
    db.execute_unprepared(&format!(
        "UPDATE organizations SET deleted_at = '2026-10-01 00:00:00+00:00' WHERE id = {retiring}"
    ))
    .await
    .unwrap();
    let nobody = org(&db, "nobody", carol, None).await;
    db.execute_unprepared(&format!(
        "UPDATE organizations SET owner_id = 9999 WHERE id = {nobody}"
    ))
    .await
    .unwrap();
    let by_entity = owners(&db, owned).await[0].id;

    crate::migrations::Migrator::up(&db, Some(1))
        .await
        .expect("run the backfill");

    let left_owners = owners(&db, left).await;
    assert_eq!(
        left_owners.iter().map(|m| m.user_id).collect::<Vec<_>>(),
        vec![alice]
    );
    let demoted_owners = owners(&db, demoted).await;
    assert_eq!(
        demoted_owners.iter().map(|m| m.user_id).collect::<Vec<_>>(),
        vec![bob]
    );
    assert_eq!(
        owners(&db, owned).await.len(),
        1,
        "an owned organization is left alone"
    );
    assert!(owners(&db, retiring).await.is_empty());
    assert!(owners(&db, nobody).await.is_empty());

    assert_eq!(
        created_at_shape(&db, left_owners[0].id).await,
        created_at_shape(&db, by_entity).await,
        "an inserted membership must carry the timestamp format every other row has"
    );

    let left_audit = backfill_audit(&db, left).await;
    assert_eq!(left_audit.len(), 1);
    assert_eq!(left_audit[0]["membership"], "inserted");
    assert_eq!(left_audit[0]["owner_user_id"], alice);
    let demoted_audit = backfill_audit(&db, demoted).await;
    assert_eq!(demoted_audit.len(), 1);
    assert_eq!(demoted_audit[0]["membership"], "upgraded");
    for untouched in [owned, retiring, nobody] {
        assert!(backfill_audit(&db, untouched).await.is_empty());
    }
}

/// The same backfill against whatever database `PLOMBIR_GIT_TEST_DATABASE_URL`
/// names — the PostgreSQL and MySQL jobs run it there — on a schema that is
/// already fully migrated, and a second run changes nothing.
#[tokio::test]
async fn the_backfill_converges_on_every_backend() {
    let temp = TempDb::new();
    let url = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => url,
        _ => temp.url(),
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect to the test database");
    crate::run_migrations(&db).await.expect("run migrations");

    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let owner = user(&db, &format!("bf{suffix}")).await;
    let orphan = org(&db, &format!("bforg{suffix}"), owner, None).await;

    backfill_owner_roles(&db).await.expect("backfill");
    let granted = owners(&db, orphan).await;
    assert_eq!(
        granted.iter().map(|m| m.user_id).collect::<Vec<_>>(),
        vec![owner]
    );
    assert_eq!(backfill_audit(&db, orphan).await.len(), 1);

    backfill_owner_roles(&db).await.expect("second backfill");
    assert_eq!(
        owners(&db, orphan).await.len(),
        1,
        "a second run adds no row"
    );
    assert_eq!(backfill_audit(&db, orphan).await.len(), 1);
}
