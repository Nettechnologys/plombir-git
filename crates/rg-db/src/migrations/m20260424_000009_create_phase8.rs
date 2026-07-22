//! Phase 8 migration: organizations, teams, team_members, organization_members, notifications

use sea_orm_migration::prelude::*;

/// Create Phase 8 tables.
pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260424_000009_create_phase8"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── Organizations ──────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Organization::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Organization::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Organization::Name)
                            .string()
                            .not_null()
                            .unique_key(),
                    )
                    .col(ColumnDef::new(Organization::DisplayName).string().null())
                    .col(ColumnDef::new(Organization::Description).string().null())
                    .col(
                        ColumnDef::new(Organization::OwnerId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Organization::Visibility)
                            .string()
                            .not_null()
                            .default("public"),
                    )
                    .col(
                        ColumnDef::new(Organization::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Organization::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ── Organization Members ──────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(OrganizationMember::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OrganizationMember::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OrganizationMember::OrgId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OrganizationMember::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OrganizationMember::Role)
                            .string()
                            .not_null()
                            .default("member"),
                    )
                    .col(
                        ColumnDef::new(OrganizationMember::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .index(
                        Index::create()
                            .unique()
                            .col(OrganizationMember::OrgId)
                            .col(OrganizationMember::UserId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(OrganizationMember::Table, OrganizationMember::OrgId)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ── Teams ──────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Team::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Team::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Team::OrgId).big_integer().not_null())
                    .col(ColumnDef::new(Team::Name).string().not_null())
                    .col(ColumnDef::new(Team::Description).string().null())
                    .col(
                        ColumnDef::new(Team::Permission)
                            .string()
                            .not_null()
                            .default("read"),
                    )
                    .col(
                        ColumnDef::new(Team::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Team::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .index(Index::create().unique().col(Team::OrgId).col(Team::Name))
                    .foreign_key(
                        ForeignKey::create()
                            .from(Team::Table, Team::OrgId)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ── Team Members ──────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(TeamMember::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(TeamMember::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(TeamMember::TeamId).big_integer().not_null())
                    .col(ColumnDef::new(TeamMember::UserId).big_integer().not_null())
                    .col(
                        ColumnDef::new(TeamMember::Role)
                            .string()
                            .not_null()
                            .default("member"),
                    )
                    .col(
                        ColumnDef::new(TeamMember::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .index(
                        Index::create()
                            .unique()
                            .col(TeamMember::TeamId)
                            .col(TeamMember::UserId),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(TeamMember::Table, TeamMember::TeamId)
                            .to(Team::Table, Team::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        // ── Notifications ─────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Notification::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Notification::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Notification::UserId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Notification::EventType).string().not_null())
                    .col(ColumnDef::new(Notification::Title).string().not_null())
                    .col(ColumnDef::new(Notification::Body).string().null())
                    .col(ColumnDef::new(Notification::RepoId).big_integer().null())
                    .col(
                        ColumnDef::new(Notification::IsRead)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(Notification::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ── Add org_id column to repositories ──────────────────────
        manager
            .alter_table(
                Table::alter()
                    .table(Repositories::Table)
                    .add_column(ColumnDef::new(Repositories::OrgId).big_integer().null())
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(TeamMember::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Team::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(OrganizationMember::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Organization::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Notification::Table).to_owned())
            .await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Repositories::Table)
                    .drop_column(Repositories::OrgId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }
}

// ── Organization ──────────────────────────────────────────
#[derive(Iden)]
enum Organization {
    #[iden = "organizations"]
    Table,
    Id,
    Name,
    DisplayName,
    Description,
    OwnerId,
    Visibility,
    CreatedAt,
    UpdatedAt,
}

// ── OrganizationMember ────────────────────────────────────
#[derive(Iden)]
enum OrganizationMember {
    #[iden = "organization_members"]
    Table,
    Id,
    OrgId,
    UserId,
    Role,
    CreatedAt,
}

// ── Team ──────────────────────────────────────────────────
#[derive(Iden)]
enum Team {
    #[iden = "teams"]
    Table,
    Id,
    OrgId,
    Name,
    Description,
    Permission,
    CreatedAt,
    UpdatedAt,
}

// ── TeamMember ────────────────────────────────────────────
#[derive(Iden)]
enum TeamMember {
    #[iden = "team_members"]
    Table,
    Id,
    TeamId,
    UserId,
    Role,
    CreatedAt,
}

// ── Notification ──────────────────────────────────────────
#[derive(Iden)]
enum Notification {
    #[iden = "notifications"]
    Table,
    Id,
    UserId,
    EventType,
    Title,
    Body,
    RepoId,
    IsRead,
    CreatedAt,
}

// ── Repositories (for ALTER TABLE) ─────────────────────────
#[derive(Iden)]
enum Repositories {
    Table,
    OrgId,
}
