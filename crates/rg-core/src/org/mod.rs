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
        return Err(crate::error::invalid_request(
            "visibility must be 'public' or 'private'",
        ));
    }

    // The one answer both the pre-read and a losing insert give, so a caller
    // cannot tell which of the two noticed — and so the response code does not
    // become a side channel for "you lost the race".
    //
    // `Conflict`, not `InvalidRequest`: `validate_username` above owns
    // everything about the name the caller can fix, and it answers 400. What is
    // left is an organization that already holds the name — the same reading as
    // the taken-username branch of registration, which answers 409.
    let already_taken =
        || crate::error::conflict(format!("organization name '{name}' is already taken"));

    // Check if org name is already taken
    if org_ops::get_org_by_name(db, name).await?.is_some() {
        return Err(already_taken());
    }

    org_ops::create_org(db, name, display_name, description, owner_id, visibility)
        .await
        .map_err(|error| {
            if rg_db::is_unique_violation_anyhow(&error) {
                already_taken()
            } else {
                error
            }
        })
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

/// Who is asking for an organization to be deleted.
///
/// Deletion used to take the actor as a bare `i64`, which is the same type as
/// the organization id sitting right next to it at every call site — and the
/// admin route did pass the org id into that position. The two answers to "may
/// this go away" are genuinely different rules, so they are spelled as
/// different variants instead of being distinguished by which number was typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgDeleteActor {
    /// A regular user. Allowed only when they own the organization.
    Owner(i64),
    /// An instance administrator. The route-level `InstanceAdmin` gate *is* the
    /// authorization here; owning the organization is deliberately not required,
    /// otherwise an admin could only ever delete their own organizations.
    InstanceAdmin,
}

/// Delete an organization (only its owner, or an instance admin, can do this).
pub async fn delete_org(db: &DatabaseConnection, id: i64, actor: OrgDeleteActor) -> Result<()> {
    let org = org_ops::get_org(db, id)
        .await?
        .ok_or_else(|| crate::error::not_found("organization"))?;

    match actor {
        OrgDeleteActor::Owner(user_id) if org.owner_id != user_id => {
            return Err(crate::error::forbidden(
                "only the organization owner can delete it",
            ));
        }
        OrgDeleteActor::Owner(_) | OrgDeleteActor::InstanceAdmin => {}
    }

    // The ownership check above read the org in a statement of its own. Two
    // concurrent deletes both pass it, and only one of them removes the row —
    // the loser gets the same 404 as a request for an organization that was
    // never there, rather than a success it did not cause.
    if !org_ops::delete_org(db, id).await? {
        return Err(crate::error::not_found("organization"));
    }
    Ok(())
}

/// Add a member to an organization.
pub async fn add_org_member(
    db: &DatabaseConnection,
    org_id: i64,
    user_id: i64,
    role: &str,
) -> Result<rg_db::entities::organization_member::Model> {
    if role != "owner" && role != "admin" && role != "member" {
        return Err(crate::error::invalid_request(
            "role must be 'owner', 'admin', or 'member'",
        ));
    }
    let member = org_ops::add_org_member(db, org_id, user_id, role).await?;
    // Org membership grants access across all org repos — flush perm cache.
    crate::repo::service::invalidate_perm_cache_all(db);
    Ok(member)
}

/// Remove a member from an organization.
pub async fn remove_org_member(db: &DatabaseConnection, org_id: i64, user_id: i64) -> Result<()> {
    if !org_ops::remove_org_member(db, org_id, user_id).await? {
        return Err(crate::error::not_found("organization member"));
    }
    crate::repo::service::invalidate_perm_cache_all(db);
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
        return Err(crate::error::invalid_request(
            "permission must be 'read', 'write', or 'admin'",
        ));
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
///
/// Mirrors [`delete_org`]: the "no such team" outcome carries
/// [`crate::error::NotFound`] so the HTTP layer can answer `404` to *that* and
/// nothing else — a failed delete stays a 5xx the client retries instead of
/// reading as "the team was already gone".
pub async fn delete_team(db: &DatabaseConnection, id: i64) -> Result<()> {
    if !org_ops::delete_team(db, id).await? {
        return Err(crate::error::not_found("team"));
    }
    crate::repo::service::invalidate_perm_cache_all(db);
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
        return Err(crate::error::invalid_request(
            "role must be 'member' or 'maintainer'",
        ));
    }
    let member = org_ops::add_team_member(db, team_id, user_id, role).await?;
    // Team membership can grant repo access — flush perm cache.
    crate::repo::service::invalidate_perm_cache_all(db);
    Ok(member)
}

/// Remove a member from a team.
pub async fn remove_team_member(db: &DatabaseConnection, team_id: i64, user_id: i64) -> Result<()> {
    if !org_ops::remove_team_member(db, team_id, user_id).await? {
        return Err(crate::error::not_found("team member"));
    }
    crate::repo::service::invalidate_perm_cache_all(db);
    Ok(())
}

/// List team members.
pub async fn list_team_members(
    db: &DatabaseConnection,
    team_id: i64,
) -> Result<Vec<rg_db::entities::team_member::Model>> {
    org_ops::list_team_members(db, team_id).await
}
