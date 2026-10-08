//! Database operations for organizations, teams, and org/team membership.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
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
/// Organization names an account holds too, sorted.
///
/// Owners share the first URL segment, and `resolve_owner` answers a name with
/// the account before the organization — so every name listed here addresses
/// the account, and the organization behind it is unreachable by name
/// (card_4b0594a02218). The doors refuse new collisions; this finds the ones
/// that came through before they did.
pub async fn list_names_held_by_an_account_too(db: &DatabaseConnection) -> Result<Vec<String>> {
    use sea_orm::sea_query::Query;

    let accounts = Query::select()
        .column(crate::entities::user::Column::Username)
        .from(crate::entities::user::Entity)
        .to_owned();
    let mut names: Vec<String> = organization::Entity::find()
        .select_only()
        .column(organization::Column::Name)
        .filter(organization::Column::Name.in_subquery(accounts))
        .into_tuple()
        .all(db)
        .await
        .context("db: list organization names an account holds too")?;
    names.sort();
    Ok(names)
}

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
///
/// `offset` is a row offset, and the slicing is spelled out with
/// `.offset().limit()` — as every neighbour in this module does — rather than
/// through `Paginator::fetch_page`. That is not a style preference: `fetch_page`
/// takes a 0-based *page index* and builds `OFFSET page_size * page` itself, so
/// a parameter named `offset` fed into it silently squared the unit. The admin
/// handler passes `PaginationParams::offset()`, which made the real SQL offset
/// `per_page² × (page − 1)` — at `per_page = 20`, page 2 asked for row 400 and
/// an instance with 100 organizations served nothing past the first page while
/// `total` kept reporting all of them (card_1e3c1cff05b4).
pub async fn list_all_orgs(
    db: &DatabaseConnection,
    offset: u64,
    limit: u64,
) -> Result<(Vec<organization::Model>, i64)> {
    let total = organization::Entity::find()
        .count(db)
        .await
        .context("db: count orgs")?;
    let orgs = organization::Entity::find()
        .order_by_desc(organization::Column::CreatedAt)
        .order_by_desc(organization::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
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

/// List the organizations this user **owns**, as opposed to belongs to.
///
/// [`list_user_orgs`] answers membership, which is the wrong question when an
/// account is about to disappear: `organizations.owner_id` carries no foreign
/// key at all, so deleting the owner leaves the organization row alive pointing
/// at a user id nothing resolves. The caller needs the owned set to refuse
/// before that happens.
pub async fn list_orgs_owned_by(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<organization::Model>> {
    organization::Entity::find()
        .filter(organization::Column::OwnerId.eq(user_id))
        .order_by_asc(organization::Column::Name)
        .all(db)
        .await
        .context("db: list orgs owned by user")
}

/// Update an organization.
pub async fn update_org(
    db: &DatabaseConnection,
    id: i64,
    display_name: Option<&str>,
    description: Option<&str>,
    visibility: Option<&str>,
) -> Result<Option<organization::Model>> {
    let mut update = organization::Entity::update_many().col_expr(
        organization::Column::UpdatedAt,
        Expr::value(chrono::Utc::now()),
    );
    if let Some(dn) = display_name {
        update = update.col_expr(
            organization::Column::DisplayName,
            Expr::value(Some(dn.to_string())),
        );
    }
    if let Some(desc) = description {
        update = update.col_expr(
            organization::Column::Description,
            Expr::value(Some(desc.to_string())),
        );
    }
    if let Some(vis) = visibility {
        update = update.col_expr(
            organization::Column::Visibility,
            Expr::value(vis.to_string()),
        );
    }

    let result = update
        .filter(organization::Column::Id.eq(id))
        // A routed DELETE first claims the namespace while it retires storage.
        // Once claimed, this organization is already absent to every new
        // request even though the row deliberately remains until retirement
        // completes.
        .filter(organization::Column::DeletedAt.is_null())
        .exec(db)
        .await
        .context("db: update org")?;
    match result.rows_affected {
        0 | 1 => {}
        count => anyhow::bail!("db: update org affected {count} rows for id {id}"),
    }

    // MySQL may report zero affected rows for a no-op update, so the row count
    // alone cannot distinguish an existing organization from a winning delete.
    // Re-read the same active identity on every backend instead.
    organization::Entity::find()
        .filter(organization::Column::Id.eq(id))
        .filter(organization::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find updated org")
}

/// Delete an organization, reporting whether this call removed it.
///
/// This used to read the row first and turn a miss into an `anyhow!` — a
/// second statement that bought nothing: the caller has already read the org
/// to check ownership, and between that read and this delete a concurrent
/// request can win. One statement, and the row count is the answer: `false`
/// means somebody else deleted it, and the caller decides what to tell the
/// client.
pub async fn delete_org(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = organization::Entity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete org")?;
    Ok(result.rows_affected > 0)
}

/// Claim an organization for retirement, reporting whether this call claimed it.
///
/// `deleted_at` is the organization's retirement marker: set while its
/// repositories are being retired, and gone together with the row itself once
/// they are. Deleting an organization spans storage that no database
/// transaction can hold, so the row cannot simply disappear at the end of one —
/// something has to say "this namespace is closing" for the whole span, and
/// this is it.
///
/// One conditional statement, and the row count is the answer: `false` means
/// the organization is already being retired by somebody else (or is already
/// gone), so this caller does not own the deletion and must not start retiring
/// storage a concurrent deleter is also retiring.
pub async fn begin_org_retirement(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let now = chrono::Utc::now();
    let result = organization::Entity::update_many()
        .col_expr(organization::Column::DeletedAt, Expr::value(Some(now)))
        .col_expr(organization::Column::UpdatedAt, Expr::value(now))
        .filter(organization::Column::Id.eq(id))
        .filter(organization::Column::DeletedAt.is_null())
        .exec(db)
        .await
        .context("db: begin org retirement")?;
    Ok(result.rows_affected > 0)
}

/// Release a retirement claim whose deletion could not finish.
///
/// A deletion that fails half-way leaves the organization and its unretired
/// repositories exactly where they were, so the marker has to come off too —
/// otherwise a retryable failure would leave a namespace nobody can create in
/// and no request can reopen.
pub async fn abort_org_retirement(db: &DatabaseConnection, id: i64) -> Result<()> {
    organization::Entity::update_many()
        .col_expr(
            organization::Column::DeletedAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .filter(organization::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: abort org retirement")?;
    Ok(())
}

/// Get an organization by name, ignoring one already claimed for retirement.
///
/// The lookup that decides whether a *new* thing may join the organization's
/// namespace — creating a repository, above all — has to use this one rather
/// than [`get_org_by_name`]: an organization whose storage is being retired is
/// no longer a namespace anything may enter.
pub async fn find_active_org_by_name(
    db: &DatabaseConnection,
    name: &str,
) -> Result<Option<organization::Model>> {
    organization::Entity::find()
        .filter(organization::Column::Name.eq(name))
        .filter(organization::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: get active org by name")
}

/// Whether an organization is still open for new members of its namespace.
///
/// `false` covers both "claimed for retirement" and "already gone": to anything
/// asking whether it may still join this namespace the two are the same answer.
pub async fn org_is_active(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let count = organization::Entity::find()
        .filter(organization::Column::Id.eq(id))
        .filter(organization::Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: check org retirement state")?;
    Ok(count > 0)
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

/// Remove a member from an organization. Returns whether a row was removed.
pub async fn remove_org_member(db: &DatabaseConnection, org_id: i64, user_id: i64) -> Result<bool> {
    let result = organization_member::Entity::delete_many()
        .filter(organization_member::Column::OrgId.eq(org_id))
        .filter(organization_member::Column::UserId.eq(user_id))
        .exec(db)
        .await
        .context("db: remove org member")?;
    Ok(result.rows_affected > 0)
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

/// Delete a team. Returns whether a team was actually removed.
///
/// "There is no such team" is reported as `Ok(false)`, not as an error: this
/// crate cannot depend on `rg-core` (the dependency runs the other way), so it
/// has no access to `rg_core::error::NotFound`, and an untyped
/// `anyhow!("team … not found")` here is indistinguishable at the HTTP layer
/// from the `.context("db: …")` failures below it — which is exactly how a
/// failed delete came to be answered with `404` (card_a253a34cf2f9). An `Err`
/// from this function therefore always means the database itself failed; the
/// absent-row case travels in the value and gets typed one level up.
pub async fn delete_team(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let Some(model) = team::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find team for delete")?
    else {
        return Ok(false);
    };

    model.delete(db).await.context("db: delete team")?;
    Ok(true)
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

/// Remove a member from a team. Returns whether a row was removed.
pub async fn remove_team_member(
    db: &DatabaseConnection,
    team_id: i64,
    user_id: i64,
) -> Result<bool> {
    let result = team_member::Entity::delete_many()
        .filter(team_member::Column::TeamId.eq(team_id))
        .filter(team_member::Column::UserId.eq(user_id))
        .exec(db)
        .await
        .context("db: remove team member")?;
    Ok(result.rows_affected > 0)
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

/// Decode the single row a `COUNT(*)` aggregate is obliged to return.
///
/// An absent row and a `cnt` that will not decode are *failures of the check*,
/// not a count of zero. The two membership predicates below sit under
/// `can_write_repo` / `can_admin_repo` and serve HTTP, OCI, Git-HTTP and SSH
/// alike, so folding either into `0` is how a schema drift or a backend type
/// mismatch reaches the caller as "access denied" — the check never ran, and
/// the answer says it ran and refused. `0` is a legitimate answer only once it
/// has been decoded as one.
fn decode_count(row: Option<QueryResult>, what: &str) -> Result<i64> {
    let row = row.with_context(|| format!("db: {what}: aggregate returned no row"))?;
    row.try_get::<i64>("", "cnt")
        .with_context(|| format!("db: {what}: decode `cnt`"))
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

    let count = decode_count(result, "check write team membership")?;
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

    let count = decode_count(result, "check admin team membership")?;
    Ok(count > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `decode_count` is the whole fix, so test it where a caller cannot: with
    /// an aggregate row that is missing, and with one whose `cnt` is not an
    /// integer — the shapes a schema drift or a backend type mismatch produces.
    async fn memory_db() -> DatabaseConnection {
        Database::connect("sqlite::memory:")
            .await
            .expect("open an in-memory database")
    }

    async fn one_row(db: &DatabaseConnection, sql: &str) -> Option<QueryResult> {
        db.query_one(Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("run the aggregate")
    }

    #[tokio::test]
    async fn a_decoded_zero_is_still_an_answer() {
        let db = memory_db().await;
        let row = one_row(&db, "SELECT 0 AS cnt").await;
        assert_eq!(
            decode_count(row, "check write team membership").expect("zero decodes"),
            0,
            "an honest count of zero must stay a count, not become an error"
        );
    }

    #[tokio::test]
    async fn a_positive_count_decodes() {
        let db = memory_db().await;
        let row = one_row(&db, "SELECT 3 AS cnt").await;
        assert_eq!(
            decode_count(row, "check write team membership").expect("three decodes"),
            3
        );
    }

    #[tokio::test]
    async fn an_absent_aggregate_row_is_an_error_not_a_denial() {
        let error = decode_count(None, "check write team membership")
            .expect_err("a COUNT(*) that returned no row did not answer the question");
        assert!(
            format!("{error:#}").contains("aggregate returned no row"),
            "the failure must name itself, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn an_undecodable_count_is_an_error_not_a_denial() {
        let db = memory_db().await;
        let sql = "SELECT 'not a number' AS cnt";

        // The row this test feeds in is exactly the one the old fold could not
        // tell apart from an empty team: assert here that it *is* that row, so
        // the regression is pinned to the input rather than to our wording.
        let old_fold = one_row(&db, sql)
            .await
            .and_then(|row| row.try_get::<i64>("", "cnt").ok())
            .unwrap_or(0);
        assert_eq!(
            old_fold, 0,
            "the pre-fix expression answered `0` here — that is the bug being guarded"
        );

        let error = decode_count(one_row(&db, sql).await, "check admin team membership")
            .expect_err("a `cnt` that will not decode is a failed check, not a count of zero");
        assert!(
            format!("{error:#}").contains("decode `cnt`"),
            "the failure must name itself, got: {error:#}"
        );
    }
}
