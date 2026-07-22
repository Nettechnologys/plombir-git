//! Organization service — business logic for org/team management.

use anyhow::Result;
use sea_orm::DatabaseConnection;

use rg_db::ops::org_ops;

/// Create a new organization.
pub async fn create_org(
    db: &DatabaseConnection,
    name: &str,
    display_name: Option<&str>,
    description: Option<&str>,
    owner_id: i64,
    visibility: &str,
) -> Result<rg_db::entities::organization::Model> {
    // Validate org name (same rules as username)
    crate::validate_username(name)?;

    // Check visibility
    if visibility != "public" && visibility != "private" {
        anyhow::bail!("visibility must be 'public' or 'private'");
    }

    // Check if org name is already taken
    if org_ops::get_org_by_name(db, name).await?.is_some() {
        anyhow::bail!("organization name '{}' is already taken", name);
    }

    org_ops::create_org(db, name, display_name, description, owner_id, visibility).await
}

/// Get an organization by name.
pub async fn get_org_by_name(
    db: &DatabaseConnection,
    name: &str,
) -> Result<Option<rg_db::entities::organization::Model>> {
    org_ops::get_org_by_name(db, name).await
}

/// Get an organization by ID.
pub async fn get_org(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<rg_db::entities::organization::Model>> {
    org_ops::get_org(db, id).await
}

/// List organizations for a user.
pub async fn list_user_orgs(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<rg_db::entities::organization::Model>> {
    org_ops::list_user_orgs(db, user_id).await
}

/// Update an organization.
pub async fn update_org(
    db: &DatabaseConnection,
    id: i64,
    display_name: Option<&str>,
    description: Option<&str>,
    visibility: Option<&str>,
) -> Result<rg_db::entities::organization::Model> {
    org_ops::update_org(db, id, display_name, description, visibility).await
}

/// Delete an organization (only owner can do this).
pub async fn delete_org(db: &DatabaseConnection, id: i64, requesting_user_id: i64) -> Result<()> {
    let org = org_ops::get_org(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("organization not found"))?;

    if org.owner_id != requesting_user_id {
        anyhow::bail!("only the organization owner can delete it");
    }

    org_ops::delete_org(db, id).await
}

/// Add a member to an organization.
pub async fn add_org_member(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
    role: &str,
) -> Result<rg_db::entities::organization_member::Model> {
    if role != "owner" && role != "admin" && role != "member" {
        anyhow::bail!("role must be 'owner', 'admin', or 'member'");
    }
    let member = org_ops::add_org_member(db, org_id, user_id, role).await?;
    // Org membership grants access across all org repos — flush perm cache.
    crate::repo::service::invalidate_perm_cache_all();
    Ok(member)
}

/// Remove a member from an organization.
pub async fn remove_org_member(db: &DatabaseConnection, org_id: i64, user_id: i64) -> Result<()> {
    org_ops::remove_org_member(db, org_id, user_id).await?;
    crate::repo::service::invalidate_perm_cache_all();
    Ok(())
}

/// List organization members.
pub async fn list_org_members(
    db: &DatabaseConnection,
    org_id: i64,
) -> Result<Vec<rg_db::entities::organization_member::Model>> {
    org_ops::list_org_members(db, org_id).await
}

/// Check if user is a member of the org.
pub async fn is_org_member(db: &DatabaseConnection, org_id: i64, user_id: i64) -> Result<bool> {
    org_ops::is_org_member(db, org_id, user_id).await
}

/// Find a specific org member.
pub async fn find_org_member(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
) -> Result<Option<rg_db::entities::organization_member::Model>> {
    org_ops::find_org_member(db, org_id, user_id).await
}

/// Check if a user is a member of a team.
pub async fn is_team_member(db: &DatabaseConnection, team_id: i64, user_id: i64) -> Result<bool> {
    org_ops::is_team_member(db, team_id, user_id).await
}

// ── Team service ─────────────────────────────────────────────

/// Create a team.
pub async fn create_team(
    db: &DatabaseConnection,
    org_id: i64,
    name: &str,
    description: Option<&str>,
    permission: &str,
) -> Result<rg_db::entities::team::Model> {
    if permission != "read" && permission != "write" && permission != "admin" {
        anyhow::bail!("permission must be 'read', 'write', or 'admin'");
    }
    org_ops::create_team(db, org_id, name, description, permission).await
}

/// List teams for an organization.
pub async fn list_org_teams(
    db: &DatabaseConnection,
    org_id: i64,
) -> Result<Vec<rg_db::entities::team::Model>> {
    org_ops::list_org_teams(db, org_id).await
}

/// Get a team by ID.
pub async fn get_team(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<rg_db::entities::team::Model>> {
    org_ops::get_team(db, id).await
}

/// Delete a team.
pub async fn delete_team(db: &DatabaseConnection, id: i64) -> Result<()> {
    org_ops::delete_team(db, id).await?;
    crate::repo::service::invalidate_perm_cache_all();
    Ok(())
}

/// Add a member to a team.
pub async fn add_team_member(
    db: &DatabaseConnection,
    team_id: i64,
    user_id: i64,
    role: &str,
) -> Result<rg_db::entities::team_member::Model> {
    if role != "member" && role != "maintainer" {
        anyhow::bail!("role must be 'member' or 'maintainer'");
    }
    let member = org_ops::add_team_member(db, team_id, user_id, role).await?;
    // Team membership can grant repo access — flush perm cache.
    crate::repo::service::invalidate_perm_cache_all();
    Ok(member)
}

/// Remove a member from a team.
pub async fn remove_team_member(db: &DatabaseConnection, team_id: i64, user_id: i64) -> Result<()> {
    org_ops::remove_team_member(db, team_id, user_id).await?;
    crate::repo::service::invalidate_perm_cache_all();
    Ok(())
}

/// List team members.
pub async fn list_team_members(
    db: &DatabaseConnection,
    team_id: i64,
) -> Result<Vec<rg_db::entities::team_member::Model>> {
    org_ops::list_team_members(db, team_id).await
}
