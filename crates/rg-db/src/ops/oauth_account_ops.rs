//! OAuth account operations.
use sea_orm::*;

use crate::entities::oauth_account;
pub use crate::entities::oauth_account::Entity;

/// Find an OAuth account by provider + provider_user_id.
pub async fn find_by_provider_and_uid(
    db: &DatabaseConnection,
    provider: &str,
    provider_user_id: &str,
) -> Result<Option<oauth_account::Model>, DbErr> {
    Entity::find()
        .filter(oauth_account::Column::Provider.eq(provider))
        .filter(oauth_account::Column::ProviderUserId.eq(provider_user_id))
        .one(db)
        .await
}

/// Find all OAuth accounts for a user.
pub async fn find_by_user_id(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<oauth_account::Model>, DbErr> {
    Entity::find()
        .filter(oauth_account::Column::UserId.eq(user_id))
        .all(db)
        .await
}

pub async fn count_by_provider(db: &DatabaseConnection, provider: &str) -> Result<u64, DbErr> {
    Entity::find()
        .filter(oauth_account::Column::Provider.eq(provider))
        .count(db)
        .await
}

/// Upsert an OAuth account (insert or update tokens).
///
/// `(provider, provider_user_id)` is UNIQUE
/// (`m20260608_000002_oauth_accounts_unique`), and the lookup below is a
/// separate statement from the insert that follows it. Two callbacks of the
/// same *first* login both read `None` and both insert; one of them meets the
/// constraint. That loss says the row this call wanted now exists — which is
/// the outcome the caller asked for — so it is resolved by re-reading the
/// winner's row and applying this call's tokens to it, not by handing the
/// caller a constraint error.
///
/// Only a UNIQUE violation is treated that way. A foreign key failure (the
/// `user_id` does not exist), a broken connection or any other insert error
/// stays an error: the row genuinely was not written.
#[allow(clippy::too_many_arguments)]
pub async fn upsert(
    db: &DatabaseConnection,
    user_id: i64,
    provider: &str,
    provider_user_id: &str,
    provider_username: &str,
    email: &str,
    access_token: Option<&str>,
    refresh_token: Option<&str>,
    token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<oauth_account::Model, DbErr> {
    if let Some(existing) = find_by_provider_and_uid(db, provider, provider_user_id).await? {
        return apply_tokens(db, existing, access_token, refresh_token, token_expires_at).await;
    }

    // Insert new
    let now = chrono::Utc::now();
    let am = oauth_account::ActiveModel {
        id: NotSet,
        user_id: Set(user_id),
        provider: Set(provider.to_string()),
        provider_user_id: Set(provider_user_id.to_string()),
        provider_username: Set(provider_username.to_string()),
        email: Set(email.to_string()),
        access_token: Set(access_token.map(str::to_string)),
        refresh_token: Set(refresh_token.map(str::to_string)),
        token_expires_at: Set(token_expires_at),
        created_at: Set(now),
        updated_at: Set(now),
    };
    match am.insert(db).await {
        Ok(inserted) => Ok(inserted),
        Err(error) if crate::is_unique_violation(&error) => {
            // Lost the race for the first row. Whoever won holds this exact
            // identity, so update it the way the existing-row branch would.
            match find_by_provider_and_uid(db, provider, provider_user_id).await? {
                Some(existing) => {
                    apply_tokens(db, existing, access_token, refresh_token, token_expires_at).await
                }
                // The row is not there after all, so the collision was on some
                // other constraint. Report the original failure rather than
                // inventing a reason for it.
                None => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// Write this call's tokens onto an existing OAuth account row.
///
/// A `None` token leaves the stored one alone: a refresh response that omits a
/// new refresh token means "keep using the one you have", and overwriting it
/// with `NULL` would end the session at the next refresh.
async fn apply_tokens(
    db: &DatabaseConnection,
    existing: oauth_account::Model,
    access_token: Option<&str>,
    refresh_token: Option<&str>,
    token_expires_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<oauth_account::Model, DbErr> {
    let mut am: oauth_account::ActiveModel = existing.into();
    if let Some(tok) = access_token {
        am.access_token = Set(Some(tok.to_string()));
    }
    if let Some(tok) = refresh_token {
        am.refresh_token = Set(Some(tok.to_string()));
    }
    if let Some(exp) = token_expires_at {
        am.token_expires_at = Set(Some(exp));
    }
    am.updated_at = Set(chrono::Utc::now());
    am.update(db).await
}

/// Delete an OAuth account by id (must belong to user).
pub async fn delete_by_id(db: &DatabaseConnection, id: i64, user_id: i64) -> Result<(), DbErr> {
    let some = Entity::find()
        .filter(oauth_account::Column::Id.eq(id))
        .filter(oauth_account::Column::UserId.eq(user_id))
        .one(db)
        .await?;
    if let Some(m) = some {
        Entity::delete_by_id(m.id).exec(db).await?;
    }
    Ok(())
}
