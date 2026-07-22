//! Database operations for organizations, teams, and org/team membership.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::{organization, organization_member, team, team_member};

// ── Organization ops ──────────────────────────────────────────

/// Create a new organization.
pub async fn create_org(
    db: &DatabaseConnection,
    name: &str,
    display_name: Option<&str>,
    description: Option<&str>,
    owner_id: i64,
    visibility: &str,
) -> Result<organization::Model> {
    let now = chrono::Utc::now();
    let model = organization::ActiveModel {
        name: Set(name.to_string()),
        display_name: Set(display_name.map(|s| s.to_string())),
        description: Set(description.map(|s| s.to_string())),
        owner_id: Set(owner_id),
        visibility: Set(visibility.to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    let result = model.insert(db).await.context("db: create org")?;

    // Auto-add the owner as an org member with "owner" role
    add_org_member(db, result.id, owner_id, "owner").await?;

    Ok(result)
}

/// Get an organization by ID.
pub async fn get_org(db: &DatabaseConnection, id: i64) -> Result<Option<organization::Model>> {
    organization::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: get org")
}

/// Get an organization by name.
pub async fn get_org_by_name(
    db: &DatabaseConnection,
    name: &str,
) -> Result<Option<organization::Model>> {
    organization::Entity::find()
        .filter(organization::Column::Name.eq(name))
        .one(db)
        .await
        .context("db: get org by name")
}

/// List all organizations with pagination (admin use).
pub async fn list_all_orgs(
    db: &DatabaseConnection,
    offset: u64,
    limit: u64,
) -> Result<(Vec<organization::Model>, i64)> {
    let paginator = organization::Entity::find()
        .order_by_desc(organization::Column::CreatedAt)
        .paginate(db, limit);

    let total = paginator.num_items().await.context("db: count orgs")?;
    let orgs = paginator
        .fetch_page(offset)
        .await
        .context("db: list all orgs")?;

    Ok((orgs, total as i64))
}

/// List organizations owned by or belonging to a user.
pub async fn list_user_orgs(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<organization::Model>> {
    // Find org IDs where the user is a member
    let memberships = organization_member::Entity::find()
        .filter(organization_member::Column::UserId.eq(user_id))
        .all(db)
        .await
        .context("db: list user org memberships")?;

    let org_ids: Vec<i64> = memberships.iter().map(|m| m.org_id).collect();
    if org_ids.is_empty() {
        return Ok(Vec::new());
    }

    organization::Entity::find()
        .filter(organization::Column::Id.is_in(org_ids))
        .all(db)
        .await
        .context("db: list user orgs")
}

/// Update an organization.
pub async fn update_org(
    db: &DatabaseConnection,
    id: i64,
    display_name: Option<&str>,
    description: Option<&str>,
    visibility: Option<&str>,
) -> Result<organization::Model> {
    let model = organization::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find org for update")?
        .ok_or_else(|| anyhow::anyhow!("org {} not found", id))?;

    let mut active: organization::ActiveModel = model.into();
    if let Some(dn) = display_name {
        active.display_name = Set(Some(dn.to_string()));
    }
    if let Some(desc) = description {
        active.description = Set(Some(desc.to_string()));
    }
    if let Some(vis) = visibility {
        active.visibility = Set(vis.to_string());
    }
    active.updated_at = Set(chrono::Utc::now());

    active.update(db).await.context("db: update org")
}

/// Delete an organization.
pub async fn delete_org(db: &DatabaseConnection, id: i64) -> Result<()> {
    let model = organization::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find org for delete")?
        .ok_or_else(|| anyhow::anyhow!("org {} not found", id))?;

    model.delete(db).await.context("db: delete org")?;
    Ok(())
}

// ── Organization Member ops ──────────────────────────────────

/// Add a member to an organization.
pub async fn add_org_member(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
    role: &str,
) -> Result<organization_member::Model> {
    let model = organization_member::ActiveModel {
        org_id: Set(org_id),
        user_id: Set(user_id),
        role: Set(role.to_string()),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    model.insert(db).await.context("db: add org member")
}

/// Remove a member from an organization.
pub async fn remove_org_member(db: &DatabaseConnection, org_id: i64, user_id: i64) -> Result<()> {
    let member = organization_member::Entity::find()
        .filter(organization_member::Column::OrgId.eq(org_id))
        .filter(organization_member::Column::UserId.eq(user_id))
        .one(db)
        .await
        .context("db: find org member")?;

    if let Some(m) = member {
        m.delete(db).await.context("db: remove org member")?;
    }
    Ok(())
}

/// List members of an organization.
pub async fn list_org_members(
    db: &DatabaseConnection,
    org_id: i64,
) -> Result<Vec<organization_member::Model>> {
    organization_member::Entity::find()
        .filter(organization_member::Column::OrgId.eq(org_id))
        .all(db)
        .await
        .context("db: list org members")
}

/// Check if a user is a member of an organization.
pub async fn is_org_member(db: &DatabaseConnection, org_id: i64, user_id: i64) -> Result<bool> {
    let member = organization_member::Entity::find()
        .filter(organization_member::Column::OrgId.eq(org_id))
        .filter(organization_member::Column::UserId.eq(user_id))
        .one(db)
        .await
        .context("db: check org membership")?;

    Ok(member.is_some())
}

/// Find a specific org member (returns the membership model).
pub async fn find_org_member(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
) -> Result<Option<organization_member::Model>> {
    organization_member::Entity::find()
        .filter(organization_member::Column::OrgId.eq(org_id))
        .filter(organization_member::Column::UserId.eq(user_id))
        .one(db)
        .await
        .context("db: find org member")
}

// ── Team ops ────────────────────────────────────────────────

/// Create a team within an organization.
pub async fn create_team(
    db: &DatabaseConnection,
    org_id: i64,
    name: &str,
    description: Option<&str>,
    permission: &str,
) -> Result<team::Model> {
    let now = chrono::Utc::now();
    let model = team::ActiveModel {
        org_id: Set(org_id),
        name: Set(name.to_string()),
        description: Set(description.map(|s| s.to_string())),
        permission: Set(permission.to_string()),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };
    model.insert(db).await.context("db: create team")
}

/// Get a team by ID.
pub async fn get_team(db: &DatabaseConnection, id: i64) -> Result<Option<team::Model>> {
    team::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: get team")
}

pub async fn find_team_by_name(
    db: &DatabaseConnection,
    org_id: i64,
    name: &str,
) -> Result<Option<team::Model>> {
    team::Entity::find()
        .filter(team::Column::OrgId.eq(org_id))
        .filter(team::Column::Name.eq(name))
        .one(db)
        .await
        .context("db: find team by name")
}

/// List teams for an organization.
pub async fn list_org_teams(db: &DatabaseConnection, org_id: i64) -> Result<Vec<team::Model>> {
    team::Entity::find()
        .filter(team::Column::OrgId.eq(org_id))
        .all(db)
        .await
        .context("db: list org teams")
}

/// Delete a team.
pub async fn delete_team(db: &DatabaseConnection, id: i64) -> Result<()> {
    let model = team::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find team for delete")?
        .ok_or_else(|| anyhow::anyhow!("team {} not found", id))?;

    model.delete(db).await.context("db: delete team")?;
    Ok(())
}

// ── Team Member ops ──────────────────────────────────────────

/// Add a member to a team.
pub async fn add_team_member(
    db: &DatabaseConnection,
    team_id: i64,
    user_id: i64,
    role: &str,
) -> Result<team_member::Model> {
    let model = team_member::ActiveModel {
        team_id: Set(team_id),
        user_id: Set(user_id),
        role: Set(role.to_string()),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    model.insert(db).await.context("db: add team member")
}

/// Remove a member from a team.
pub async fn remove_team_member(db: &DatabaseConnection, team_id: i64, user_id: i64) -> Result<()> {
    let member = team_member::Entity::find()
        .filter(team_member::Column::TeamId.eq(team_id))
        .filter(team_member::Column::UserId.eq(user_id))
        .one(db)
        .await
        .context("db: find team member")?;

    if let Some(m) = member {
        m.delete(db).await.context("db: remove team member")?;
    }
    Ok(())
}

/// List members of a team.
pub async fn list_team_members(
    db: &DatabaseConnection,
    team_id: i64,
) -> Result<Vec<team_member::Model>> {
    team_member::Entity::find()
        .filter(team_member::Column::TeamId.eq(team_id))
        .all(db)
        .await
        .context("db: list team members")
}

/// Check if a user is a member of a team.
pub async fn is_team_member(db: &DatabaseConnection, team_id: i64, user_id: i64) -> Result<bool> {
    let member = team_member::Entity::find()
        .filter(team_member::Column::TeamId.eq(team_id))
        .filter(team_member::Column::UserId.eq(user_id))
        .one(db)
        .await
        .context("db: check team membership")?;

    Ok(member.is_some())
}

/// List teams that a user belongs to (across all orgs).
pub async fn list_user_teams(db: &DatabaseConnection, user_id: i64) -> Result<Vec<team::Model>> {
    let memberships = team_member::Entity::find()
        .filter(team_member::Column::UserId.eq(user_id))
        .all(db)
        .await
        .context("db: list user team memberships")?;

    let team_ids: Vec<i64> = memberships.iter().map(|m| m.team_id).collect();
    if team_ids.is_empty() {
        return Ok(Vec::new());
    }

    team::Entity::find()
        .filter(team::Column::Id.is_in(team_ids))
        .all(db)
        .await
        .context("db: list user teams")
}

/// Check if a user is a member of any team with write/admin permission in an org.
/// Single query replacement for the N+1 pattern (list teams + is_team_member per team).
pub async fn is_member_of_write_team(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
) -> Result<bool> {
    let backend = db.get_database_backend();
    let result = db
        .query_one(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(
                backend,
                r#"SELECT COUNT(*) as cnt
               FROM team_members tm
               JOIN teams t ON t.id = tm.team_id
               WHERE t.org_id = ? AND tm.user_id = ? AND t.permission IN ('write', 'admin')"#,
            ),
            [Value::from(org_id), Value::from(user_id)],
        ))
        .await
        .context("db: check write team membership")?;

    let count: i64 = result
        .and_then(|row| row.try_get::<i64>("", "cnt").ok())
        .unwrap_or(0);
    Ok(count > 0)
}

/// Check whether a user belongs to an admin-permission team in an organization.
pub async fn is_member_of_admin_team(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
) -> Result<bool> {
    let backend = db.get_database_backend();
    let result = db
        .query_one(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(
                backend,
                r#"SELECT COUNT(*) as cnt
               FROM team_members tm
               JOIN teams t ON t.id = tm.team_id
               WHERE t.org_id = ? AND tm.user_id = ? AND t.permission = 'admin'"#,
            ),
            [Value::from(org_id), Value::from(user_id)],
        ))
        .await
        .context("db: check admin team membership")?;
    let count: i64 = result
        .and_then(|row| row.try_get::<i64>("", "cnt").ok())
        .unwrap_or(0);
    Ok(count > 0)
}
