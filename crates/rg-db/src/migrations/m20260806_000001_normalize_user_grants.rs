//! Give the three user-id allow-lists relational ownership without changing
//! their existing JSON response contract.
//!
//! The JSON columns remain as compatibility mirrors, but every usable id is
//! also copied into a join table with `ON DELETE CASCADE` on both parents.
//! Runtime writes update both representations through `user_grants::replace`.

use std::collections::{BTreeSet, HashSet};

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::sea_query::Expr;
use sea_orm_migration::sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QuerySelect, Set,
};

use crate::entities::{
    ci_environment, ci_environment_approver_grant, protected_branch, protected_branch_push_grant,
    protected_tag, protected_tag_push_grant, user,
};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260806_000001_normalize_user_grants"
    }
}

#[derive(Default)]
struct Backfill {
    branches: Vec<(i64, i64)>,
    branch_mirrors: Vec<(i64, Option<String>)>,
    tags: Vec<(i64, i64)>,
    tag_mirrors: Vec<(i64, Option<String>)>,
    environments: Vec<(i64, i64)>,
    environment_mirrors: Vec<(i64, Option<String>)>,
}

fn decode_into(
    destination: &mut Vec<(i64, i64)>,
    target_id: i64,
    json: Option<String>,
    usable_users: &BTreeSet<i64>,
    location: &str,
) -> Result<Option<String>, DbErr> {
    let Some(json) = json else {
        return Ok(None);
    };
    let ids: Vec<i64> = serde_json::from_str(&json).map_err(|error| {
        DbErr::Migration(format!(
            "m20260806_000001_normalize_user_grants: unreadable {location}: {error}"
        ))
    })?;
    let mut seen = HashSet::new();
    let ids: Vec<_> = ids
        .into_iter()
        .filter(|id| usable_users.contains(id) && seen.insert(*id))
        .collect();
    destination.extend(ids.iter().map(|&user_id| (target_id, user_id)));
    Ok(Some(serde_json::to_string(&ids).map_err(|error| {
        DbErr::Migration(format!(
            "m20260806_000001_normalize_user_grants: cannot normalize {location}: {error}"
        ))
    })?))
}

async fn plan_backfill(db: &impl ConnectionTrait) -> Result<Backfill, DbErr> {
    let usable_users: BTreeSet<i64> = user::Entity::find()
        .select_only()
        .column(user::Column::Id)
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .into_tuple()
        .all(db)
        .await?
        .into_iter()
        .collect();
    let branches: Vec<(i64, Option<String>)> = protected_branch::Entity::find()
        .select_only()
        .columns([
            protected_branch::Column::Id,
            protected_branch::Column::AllowedPushUserIds,
        ])
        .into_tuple()
        .all(db)
        .await?;
    let tags: Vec<(i64, Option<String>)> = protected_tag::Entity::find()
        .select_only()
        .columns([
            protected_tag::Column::Id,
            protected_tag::Column::AllowedUserIds,
        ])
        .into_tuple()
        .all(db)
        .await?;
    let environments: Vec<(i64, Option<String>)> = ci_environment::Entity::find()
        .select_only()
        .columns([
            ci_environment::Column::Id,
            ci_environment::Column::AllowedApproverIds,
        ])
        .into_tuple()
        .all(db)
        .await?;

    // Finish decoding every mirror before the first schema or data write. A
    // migration runner is not transactional on every supported backend.
    let mut plan = Backfill::default();
    for (id, json) in branches {
        let mirror = decode_into(
            &mut plan.branches,
            id,
            json,
            &usable_users,
            "protected_branches.allowed_push_user_ids",
        )?;
        plan.branch_mirrors.push((id, mirror));
    }
    for (id, json) in tags {
        let mirror = decode_into(
            &mut plan.tags,
            id,
            json,
            &usable_users,
            "protected_tags.allowed_user_ids",
        )?;
        plan.tag_mirrors.push((id, mirror));
    }
    for (id, json) in environments {
        let mirror = decode_into(
            &mut plan.environments,
            id,
            json,
            &usable_users,
            "ci_environments.allowed_approver_ids",
        )?;
        plan.environment_mirrors.push((id, mirror));
    }
    Ok(plan)
}

async fn create_tables(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    manager
        .create_table(
            Table::create()
                .table(ProtectedBranchPushGrants::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(ProtectedBranchPushGrants::ProtectedBranchId)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(ProtectedBranchPushGrants::UserId)
                        .big_integer()
                        .not_null(),
                )
                .primary_key(
                    Index::create()
                        .col(ProtectedBranchPushGrants::ProtectedBranchId)
                        .col(ProtectedBranchPushGrants::UserId),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_branch_push_grant_rule")
                        .from(
                            ProtectedBranchPushGrants::Table,
                            ProtectedBranchPushGrants::ProtectedBranchId,
                        )
                        .to(ProtectedBranches::Table, ProtectedBranches::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_branch_push_grant_user")
                        .from(
                            ProtectedBranchPushGrants::Table,
                            ProtectedBranchPushGrants::UserId,
                        )
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_branch_push_grants_user")
                .table(ProtectedBranchPushGrants::Table)
                .col(ProtectedBranchPushGrants::UserId)
                .if_not_exists()
                .to_owned(),
        )
        .await?;

    manager
        .create_table(
            Table::create()
                .table(ProtectedTagPushGrants::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(ProtectedTagPushGrants::ProtectedTagId)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(ProtectedTagPushGrants::UserId)
                        .big_integer()
                        .not_null(),
                )
                .primary_key(
                    Index::create()
                        .col(ProtectedTagPushGrants::ProtectedTagId)
                        .col(ProtectedTagPushGrants::UserId),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_tag_push_grant_rule")
                        .from(
                            ProtectedTagPushGrants::Table,
                            ProtectedTagPushGrants::ProtectedTagId,
                        )
                        .to(ProtectedTags::Table, ProtectedTags::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_tag_push_grant_user")
                        .from(
                            ProtectedTagPushGrants::Table,
                            ProtectedTagPushGrants::UserId,
                        )
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_tag_push_grants_user")
                .table(ProtectedTagPushGrants::Table)
                .col(ProtectedTagPushGrants::UserId)
                .if_not_exists()
                .to_owned(),
        )
        .await?;

    manager
        .create_table(
            Table::create()
                .table(CiEnvironmentApproverGrants::Table)
                .if_not_exists()
                .col(
                    ColumnDef::new(CiEnvironmentApproverGrants::EnvironmentId)
                        .big_integer()
                        .not_null(),
                )
                .col(
                    ColumnDef::new(CiEnvironmentApproverGrants::UserId)
                        .big_integer()
                        .not_null(),
                )
                .primary_key(
                    Index::create()
                        .col(CiEnvironmentApproverGrants::EnvironmentId)
                        .col(CiEnvironmentApproverGrants::UserId),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_environment_approver_grant_rule")
                        .from(
                            CiEnvironmentApproverGrants::Table,
                            CiEnvironmentApproverGrants::EnvironmentId,
                        )
                        .to(CiEnvironments::Table, CiEnvironments::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .foreign_key(
                    ForeignKey::create()
                        .name("fk_environment_approver_grant_user")
                        .from(
                            CiEnvironmentApproverGrants::Table,
                            CiEnvironmentApproverGrants::UserId,
                        )
                        .to(Users::Table, Users::Id)
                        .on_delete(ForeignKeyAction::Cascade),
                )
                .to_owned(),
        )
        .await?;
    manager
        .create_index(
            Index::create()
                .name("idx_environment_approver_grants_user")
                .table(CiEnvironmentApproverGrants::Table)
                .col(CiEnvironmentApproverGrants::UserId)
                .if_not_exists()
                .to_owned(),
        )
        .await
}

async fn apply_backfill(db: &impl ConnectionTrait, plan: Backfill) -> Result<(), DbErr> {
    // A failed, unrecorded migration can be retried safely: JSON mirrors stay
    // untouched, and these derived rows are rebuilt from them from scratch.
    ci_environment_approver_grant::Entity::delete_many()
        .exec(db)
        .await?;
    protected_tag_push_grant::Entity::delete_many()
        .exec(db)
        .await?;
    protected_branch_push_grant::Entity::delete_many()
        .exec(db)
        .await?;

    for (protected_branch_id, user_id) in plan.branches {
        protected_branch_push_grant::ActiveModel {
            protected_branch_id: Set(protected_branch_id),
            user_id: Set(user_id),
        }
        .insert(db)
        .await?;
    }
    for (protected_tag_id, user_id) in plan.tags {
        protected_tag_push_grant::ActiveModel {
            protected_tag_id: Set(protected_tag_id),
            user_id: Set(user_id),
        }
        .insert(db)
        .await?;
    }
    for (environment_id, user_id) in plan.environments {
        ci_environment_approver_grant::ActiveModel {
            environment_id: Set(environment_id),
            user_id: Set(user_id),
        }
        .insert(db)
        .await?;
    }
    for (id, mirror) in plan.branch_mirrors {
        protected_branch::Entity::update_many()
            .col_expr(
                protected_branch::Column::AllowedPushUserIds,
                Expr::value(mirror),
            )
            .filter(protected_branch::Column::Id.eq(id))
            .exec(db)
            .await?;
    }
    for (id, mirror) in plan.tag_mirrors {
        protected_tag::Entity::update_many()
            .col_expr(protected_tag::Column::AllowedUserIds, Expr::value(mirror))
            .filter(protected_tag::Column::Id.eq(id))
            .exec(db)
            .await?;
    }
    for (id, mirror) in plan.environment_mirrors {
        ci_environment::Entity::update_many()
            .col_expr(
                ci_environment::Column::AllowedApproverIds,
                Expr::value(mirror),
            )
            .filter(ci_environment::Column::Id.eq(id))
            .exec(db)
            .await?;
    }
    Ok(())
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let plan = plan_backfill(manager.get_connection()).await?;
        create_tables(manager).await?;
        apply_backfill(manager.get_connection(), plan).await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(CiEnvironmentApproverGrants::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ProtectedTagPushGrants::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(ProtectedBranchPushGrants::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum ProtectedBranchPushGrants {
    Table,
    ProtectedBranchId,
    UserId,
}

#[derive(DeriveIden)]
enum ProtectedTagPushGrants {
    Table,
    ProtectedTagId,
    UserId,
}

#[derive(DeriveIden)]
enum CiEnvironmentApproverGrants {
    Table,
    EnvironmentId,
    UserId,
}

#[derive(DeriveIden)]
enum ProtectedBranches {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum ProtectedTags {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum CiEnvironments {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, DbBackend, Statement};

    #[test]
    fn backfill_deduplicates_and_removes_unusable_principals() {
        let usable = BTreeSet::from([2, 7]);
        let mut rows = Vec::new();
        let mirror = decode_into(
            &mut rows,
            11,
            Some("[7,2,7,9]".to_string()),
            &usable,
            "test.grants",
        )
        .unwrap();
        assert_eq!(rows, vec![(11, 7), (11, 2)]);
        assert_eq!(mirror.as_deref(), Some("[7,2]"));
    }

    #[test]
    fn backfill_rejects_unreadable_json_before_writing() {
        let error = decode_into(
            &mut Vec::new(),
            11,
            Some("not-json".to_string()),
            &BTreeSet::new(),
            "test.grants",
        )
        .unwrap_err();
        assert!(error.to_string().contains("unreadable test.grants"));
    }

    #[tokio::test]
    async fn migration_backfills_canonical_mirrors_and_is_idempotent() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE users (
                 id INTEGER PRIMARY KEY,
                 is_active BOOLEAN NOT NULL,
                 deleted_at TEXT
             );
             CREATE TABLE protected_branches (
                 id INTEGER PRIMARY KEY,
                 allowed_push_user_ids TEXT
             );
             CREATE TABLE protected_tags (
                 id INTEGER PRIMARY KEY,
                 allowed_user_ids TEXT
             );
             CREATE TABLE ci_environments (
                 id INTEGER PRIMARY KEY,
                 allowed_approver_ids TEXT
             );
             INSERT INTO users (id, is_active, deleted_at) VALUES
                 (1, 1, NULL), (2, 0, NULL), (3, 1, '2026-08-06T00:00:00Z');
             INSERT INTO protected_branches VALUES (11, '[2,1,1,3,999]');
             INSERT INTO protected_tags VALUES (12, '[3,1,2,1,999]');
             INSERT INTO ci_environments VALUES (13, '[999,1,3,2,1]');",
        )
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        for (table, column) in [
            ("protected_branches", "allowed_push_user_ids"),
            ("protected_tags", "allowed_user_ids"),
            ("ci_environments", "allowed_approver_ids"),
        ] {
            let row = db
                .query_one(Statement::from_string(
                    DbBackend::Sqlite,
                    format!("SELECT {column} FROM {table}"),
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.try_get::<String>("", column).unwrap(), "[1]");
        }
        for table in [
            "protected_branch_push_grants",
            "protected_tag_push_grants",
            "ci_environment_approver_grants",
        ] {
            let row = db
                .query_one(Statement::from_string(
                    DbBackend::Sqlite,
                    format!("SELECT COUNT(*) AS n FROM {table}"),
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.try_get::<i64>("", "n").unwrap(), 1);
        }
    }
}
