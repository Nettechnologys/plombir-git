//! Database operations for users.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::user::{self, ActiveModel, Entity as UserEntity, Model as User};

/// Find a user by username (case-insensitive on SQLite).
pub async fn find_by_username(db: &DatabaseConnection, username: &str) -> Result<Option<User>> {
    UserEntity::find()
        .filter(user::Column::Username.eq(username))
        .one(db)
        .await
        .context("db: find user by username")
}

/// Find a user by email.
pub async fn find_by_email(db: &DatabaseConnection, email: &str) -> Result<Option<User>> {
    UserEntity::find()
        .filter(user::Column::Email.eq(email))
        .one(db)
        .await
        .context("db: find user by email")
}

/// Find a user by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<User>> {
    UserEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find user by id")
}

pub async fn count_by_ldap_provider(db: &DatabaseConnection, provider_id: i64) -> Result<u64> {
    UserEntity::find()
        .filter(user::Column::LdapProviderId.eq(provider_id))
        .count(db)
        .await
        .context("db: count users by LDAP provider")
}

/// List all users with optional pagination.
pub async fn list_users(
    db: &DatabaseConnection,
    page: u64,
    per_page: u64,
) -> Result<(Vec<User>, i64)> {
    let paginator = UserEntity::find()
        .order_by_desc(user::Column::CreatedAt)
        .paginate(db, per_page);

    let total = paginator.num_items().await.context("db: count users")?;
    let users = paginator.fetch_page(page).await.context("db: list users")?;

    Ok((users, total as i64))
}

/// Create a new user and return the persisted model.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<User> {
    model.insert(db).await.context("db: create user")
}

/// Update a user.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<User> {
    model.update(db).await.context("db: update user")
}

///
/// CRITICAL: SeaORM single-row update (踩坑经验 #11)
///
/// To update a single row, you MUST first `find_by_id()` to get the model,
/// then convert it into an `ActiveModel`, modify fields, and call `update()`.
///
/// CORRECT pattern (used here):
///   let model = UserEntity::find_by_id(id)
///       .one(db).await?
///       .ok_or_else(|| anyhow::anyhow!("not found"))?;
///   let mut active: ActiveModel = model.into();
///   active.field = Set(value);
///   active.update(db).await
///
/// WRONG pattern for ordinary admin/profile field updates:
///   ActiveModel { id: Set(id), ... }.update(db)  // MAY skip optimistic lock
///
/// `update_many().col_expr(...)` is still appropriate for atomic counters where
/// a read-modify-write ActiveModel cycle would lose concurrent increments.
pub async fn update_by_id(
    db: &DatabaseConnection,
    id: i64,
    display_name: Option<Option<String>>,
    bio: Option<Option<String>>,
    is_admin: Option<bool>,
    is_active: Option<bool>,
) -> Result<User> {
    let model = UserEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find user for update")?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", id))?;

    let mut active: ActiveModel = model.into();

    if let Some(dn) = display_name {
        active.display_name = Set(dn);
    }
    if let Some(b) = bio {
        active.bio = Set(b);
    }
    if let Some(admin) = is_admin {
        active.is_admin = Set(admin);
    }
    if let Some(active_flag) = is_active {
        active.is_active = Set(active_flag);
    }

    active.update(db).await.context("db: update user by admin")
}

/// Create a user with high-level parameters (used by SSO).
pub async fn create_user(
    db: &DatabaseConnection,
    username: &str,
    email: &str,
    password_hash: &str,
    display_name: &str,
) -> Result<User> {
    use crate::entities::user;
    let now = chrono::Utc::now();
    let model = user::ActiveModel {
        id: NotSet,
        username: Set(username.to_string()),
        email: Set(email.to_string()),
        password_hash: Set(if password_hash.is_empty() {
            "".into()
        } else {
            password_hash.to_string()
        }),
        display_name: Set(if display_name.is_empty() {
            None
        } else {
            Some(display_name.to_string())
        }),
        avatar_url: Set(None),
        bio: Set(None),
        is_admin: Set(false),
        is_active: Set(true),
        auth_provider: Set("oauth2".into()),
        ldap_dn: Set(None),
        ldap_uid: Set(None),
        ldap_provider_id: Set(None),
        totp_secret: Set(None),
        mfa_enabled: Set(false),
        mfa_type: Set(None),
        backup_codes: Set(None),
        last_login_at: Set(None),
        login_attempts: Set(0),
        locked_until: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    create(db, model).await
}

/// Create a directory-backed user after successful LDAP authentication.
pub async fn create_ldap_user(
    db: &DatabaseConnection,
    ldap_provider_id: i64,
    username: &str,
    email: &str,
    display_name: Option<&str>,
    ldap_dn: &str,
    ldap_uid: Option<&str>,
) -> Result<User> {
    let now = chrono::Utc::now();
    create(
        db,
        user::ActiveModel {
            id: NotSet,
            username: Set(username.to_string()),
            email: Set(email.to_string()),
            password_hash: Set(String::new()),
            display_name: Set(display_name.map(str::to_string)),
            avatar_url: Set(None),
            bio: Set(None),
            is_admin: Set(false),
            is_active: Set(true),
            auth_provider: Set("ldap".into()),
            ldap_dn: Set(Some(ldap_dn.to_string())),
            ldap_uid: Set(ldap_uid.map(str::to_string)),
            ldap_provider_id: Set(Some(ldap_provider_id)),
            totp_secret: Set(None),
            mfa_enabled: Set(false),
            mfa_type: Set(None),
            backup_codes: Set(None),
            last_login_at: Set(None),
            login_attempts: Set(0),
            locked_until: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
        },
    )
    .await
}

/// Refresh non-authoritative LDAP identity metadata after a successful bind.
/// Email is deliberately not changed here because it is globally unique and
/// may require an administrator to resolve a directory collision.
pub async fn sync_ldap_identity(
    db: &DatabaseConnection,
    user_id: i64,
    ldap_provider_id: i64,
    display_name: Option<&str>,
    ldap_dn: &str,
    ldap_uid: Option<&str>,
) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.display_name = Set(display_name.map(str::to_string));
    active.ldap_dn = Set(Some(ldap_dn.to_string()));
    active.ldap_uid = Set(ldap_uid.map(str::to_string));
    active.ldap_provider_id = Set(Some(ldap_provider_id));
    active.updated_at = Set(chrono::Utc::now());
    update(db, active).await
}

/// Update the TOTP secret for a user (encrypted).
pub async fn update_totp_secret(
    db: &DatabaseConnection,
    user_id: i64,
    encrypted_secret: &str,
) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.totp_secret = Set(Some(encrypted_secret.to_string()));
    active.updated_at = Set(chrono::Utc::now());
    active
        .update(db)
        .await
        .map_err(|e| anyhow::anyhow!("db: {}", e))
}

/// Enable MFA for a user.
pub async fn enable_mfa(db: &DatabaseConnection, user_id: i64, mfa_type: &str) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.mfa_enabled = Set(true);
    active.mfa_type = Set(Some(mfa_type.to_string()));
    active.updated_at = Set(chrono::Utc::now());
    active
        .update(db)
        .await
        .map_err(|e| anyhow::anyhow!("db: {}", e))
}

/// Disable MFA for a user.
pub async fn disable_mfa(db: &DatabaseConnection, user_id: i64) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.mfa_enabled = Set(false);
    active.mfa_type = Set(None);
    active.totp_secret = Set(None);
    active.backup_codes = Set(None);
    active.updated_at = Set(chrono::Utc::now());
    active
        .update(db)
        .await
        .map_err(|e| anyhow::anyhow!("db: {}", e))
}

/// Record a successful login and reset login_attempts/locked_until.
pub async fn record_successful_login(db: &DatabaseConnection, user_id: i64) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.last_login_at = Set(Some(chrono::Utc::now()));
    active.login_attempts = Set(0);
    active.locked_until = Set(None);
    active
        .update(db)
        .await
        .map_err(|e| anyhow::anyhow!("db: {}", e))
}

/// Reset primary-factor failures while MFA is still pending. This deliberately
/// does not update `last_login_at`, which represents a completed login.
pub async fn reset_login_failures(db: &DatabaseConnection, user_id: i64) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.login_attempts = Set(0);
    active.locked_until = Set(None);
    active.updated_at = Set(chrono::Utc::now());
    active
        .update(db)
        .await
        .map_err(|e| anyhow::anyhow!("db: {}", e))
}

/// Increment failed login attempts and lock account if threshold exceeded.
pub async fn record_failed_login(
    db: &DatabaseConnection,
    user_id: i64,
    max_attempts: i32,
) -> Result<bool> {
    let now = chrono::Utc::now();
    let threshold = max_attempts.max(1);
    let locked_until = now + chrono::Duration::minutes(15);
    let result = UserEntity::update_many()
        // Keep this assignment before LoginAttempts. MySQL evaluates UPDATE
        // assignments left-to-right, while PostgreSQL/SQLite use the old row;
        // this ordering therefore makes the threshold expression portable.
        .col_expr(
            user::Column::LockedUntil,
            Expr::case(
                Expr::col(user::Column::LoginAttempts).gte(threshold - 1),
                Expr::value(locked_until),
            )
            .finally(Expr::col(user::Column::LockedUntil))
            .into(),
        )
        .col_expr(
            user::Column::LoginAttempts,
            Expr::col(user::Column::LoginAttempts).add(1),
        )
        .col_expr(user::Column::UpdatedAt, Expr::value(now))
        .filter(user::Column::Id.eq(user_id))
        .exec(db)
        .await
        .context("db: atomically record failed login")?;
    if result.rows_affected == 0 {
        anyhow::bail!("user {} not found", user_id);
    }
    let updated = find_by_id(db, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found after failed login update", user_id))?;
    Ok(updated
        .locked_until
        .is_some_and(|locked_until| locked_until > now))
}

/// Delete a user by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    let model = UserEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find user for delete")?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", id))?;

    model.delete(db).await.context("db: delete user")?;
    Ok(())
}
