//! OAuth account operations.
use sea_orm::sea_query::Expr;
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

/// Create an OAuth account link, converging with a concurrent first callback.
///
/// The row carries no credentials — the provider's tokens were dropped by
/// `m20260822_000002_drop_oauth_account_tokens` — so an existing link has
/// nothing left to overwrite and the "update" half is just its `updated_at`,
/// which is when this identity last signed in.
///
/// `(provider, provider_user_id)` is UNIQUE
/// (`m20260608_000002_oauth_accounts_unique`). Two callbacks of the same first
/// login can both attempt the insert; one of them meets the constraint. That
/// loss says the row this call wanted now exists — which is the outcome the
/// caller asked for — so it is resolved by re-reading and touching the winner's
/// row, not by handing the caller a constraint error.
///
/// Only a UNIQUE violation is treated that way. A foreign key failure (the
/// `user_id` does not exist), a broken connection or any other insert error
/// stays an error: the row genuinely was not written.
///
/// `None` means the winner's row was explicitly removed before this callback
/// could touch it. That is a state conflict for the callback, not permission to
/// recreate the newer unlink.
pub async fn link(
    db: &DatabaseConnection,
    user_id: i64,
    provider: &str,
    provider_user_id: &str,
    provider_username: &str,
    email: &str,
) -> Result<Option<oauth_account::Model>, DbErr> {
    let now = chrono::Utc::now();
    let am = oauth_account::ActiveModel {
        id: NotSet,
        user_id: Set(user_id),
        provider: Set(provider.to_string()),
        provider_user_id: Set(provider_user_id.to_string()),
        provider_username: Set(provider_username.to_string()),
        email: Set(email.to_string()),
        created_at: Set(now),
        updated_at: Set(now),
    };
    match am.insert(db).await {
        Ok(inserted) => Ok(Some(inserted)),
        Err(error) if crate::is_unique_violation(&error) => {
            // Lost the race for the first row. Whoever won holds this exact
            // identity, so treat it the way the existing-row branch would.
            match find_by_provider_and_uid(db, provider, provider_user_id).await? {
                Some(existing) => touch_existing(db, existing.id).await,
                // The row is not there after all, so the collision was on some
                // other constraint. Report the original failure rather than
                // inventing a reason for it.
                None => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

/// Record that an already-observed link was used again.
///
/// The callback's lookup and this write are separate statements. An explicit
/// unlink can therefore win between them; `None` represents that ordinary
/// absence without leaking SeaORM's backend-shaped `RecordNotUpdated`. This is
/// update-only by construction, so it cannot answer the unlink by recreating
/// the row.
pub async fn touch_existing(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<oauth_account::Model>, DbErr> {
    let result = Entity::update_many()
        .col_expr(
            oauth_account::Column::UpdatedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(oauth_account::Column::Id.eq(id))
        .exec(db)
        .await?;
    if result.rows_affected > 1 {
        return Err(DbErr::Custom(format!(
            "OAuth account touch affected {} rows for id {id}",
            result.rows_affected
        )));
    }

    // MySQL may report zero affected rows for a no-op UPDATE. Re-read the same
    // stable identity on every backend to distinguish that from a winning
    // unlink. A DELETE after the UPDATE but before this read is also absence,
    // which is the correct callback outcome.
    Entity::find_by_id(id).one(db).await
}

/// Delete an OAuth account by id, scoped to its owner. Returns true if a row
/// was removed.
///
/// The scoping filter and the delete are one statement, so the row cannot go
/// away between them: two concurrent unlinks of the same link produce one
/// `true` and one `false`, and only the `true` may be reported as an unlink.
/// A `user_id` that does not own `id` is likewise `false` — indistinguishable
/// from "no such link", which is what the caller answers either way.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64, user_id: i64) -> Result<bool, DbErr> {
    let res = Entity::delete_many()
        .filter(oauth_account::Column::Id.eq(id))
        .filter(oauth_account::Column::UserId.eq(user_id))
        .exec(db)
        .await?;
    Ok(res.rows_affected > 0)
}
