//! Database operations for access tokens.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::access_token::{
    self, ActiveModel, Entity as TokenEntity, Model as AccessToken,
};
use crate::entities::access_token_repository;

/// Find a token by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<AccessToken>> {
    TokenEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find access token by id")
}

/// Find a token by its SHA-256 hash.
pub async fn find_by_hash(db: &DatabaseConnection, hash: &str) -> Result<Option<AccessToken>> {
    TokenEntity::find()
        .filter(access_token::Column::TokenHash.eq(hash))
        .one(db)
        .await
        .context("db: find access token by hash")
}

/// List all tokens for a user.
pub async fn list_by_user(db: &DatabaseConnection, user_id: i64) -> Result<Vec<AccessToken>> {
    TokenEntity::find()
        .filter(access_token::Column::UserId.eq(user_id))
        .order_by_asc(access_token::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list access tokens by user")
}

/// Create a token, together with the repositories it is confined to, in one
/// transaction.
///
/// `repository_ids` is written only when the model sets `repo_restricted`;
/// the rows and the flag either both land or neither does, so a failure can
/// never publish a restricted token that silently reaches everything.
pub async fn create(
    db: &DatabaseConnection,
    model: ActiveModel,
    repository_ids: &[i64],
) -> Result<AccessToken> {
    let transaction = db
        .begin()
        .await
        .context("db: begin restricted access token create")?;
    let token = model
        .insert(&transaction)
        .await
        .context("db: create access token")?;
    if token.repo_restricted && !repository_ids.is_empty() {
        let mut unique = repository_ids.to_vec();
        unique.sort_unstable();
        unique.dedup();
        access_token_repository::Entity::insert_many(unique.into_iter().map(|repository_id| {
            access_token_repository::ActiveModel {
                token_id: Set(token.id),
                repository_id: Set(repository_id),
            }
        }))
        .exec(&transaction)
        .await
        .context("db: record access token repositories")?;
    }
    transaction
        .commit()
        .await
        .context("db: commit restricted access token create")?;
    Ok(token)
}

/// The repositories a restricted token may reach, keyed by token id.
///
/// Tokens with no rows are absent from the map; whether that means "nothing"
/// or "unrestricted" is the token's own `repo_restricted` flag.
pub async fn repository_ids_by_token(
    db: &DatabaseConnection,
    token_ids: &[i64],
) -> Result<std::collections::HashMap<i64, Vec<i64>>> {
    let mut by_token: std::collections::HashMap<i64, Vec<i64>> = std::collections::HashMap::new();
    if token_ids.is_empty() {
        return Ok(by_token);
    }
    let rows = access_token_repository::Entity::find()
        .filter(access_token_repository::Column::TokenId.is_in(token_ids.iter().copied()))
        .order_by_asc(access_token_repository::Column::RepositoryId)
        .all(db)
        .await
        .context("db: list access token repositories")?;
    for row in rows {
        by_token
            .entry(row.token_id)
            .or_default()
            .push(row.repository_id);
    }
    Ok(by_token)
}

/// Record that a Personal Access Token was used, at most once per
/// [`LAST_USED_RESOLUTION`](super::LAST_USED_RESOLUTION).
///
/// `previous` is the `last_used_at` the caller just read with the credential.
/// Within the window this returns without touching the database, so a burst
/// of requests on one credential is reads only.
pub async fn touch_last_used(
    db: &DatabaseConnection,
    id: i64,
    previous: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    let now = chrono::Utc::now();
    if !super::last_used_is_due(previous, now) {
        return Ok(());
    }
    TokenEntity::update_many()
        .col_expr(access_token::Column::LastUsedAt, Expr::value(now))
        .filter(access_token::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update access token last used time")?;
    Ok(())
}

/// Delete a token by id. `Ok(false)` means no such row.
///
/// The caller's lookup and this `DELETE` are two statements, so a concurrent
/// revocation can win in between; reporting `rows_affected` is what lets the
/// route answer 404 instead of confirming a revocation it did not perform.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = TokenEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete access token")?;
    Ok(result.rows_affected > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    async fn setup_test_db() -> DatabaseConnection {
        use sea_orm::{ConnectOptions, Database, Statement};
        let mut opt = ConnectOptions::new("sqlite::memory:");
        opt.max_connections(1);
        let db = Database::connect(opt)
            .await
            .expect("connect to in-memory db");
        crate::run_migrations(&db).await.expect("run migrations");
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) VALUES(1, 'test', 'test@test.com', 'x', 0, 1, '2024-01-01', '2024-01-01')",
        )).await.expect("insert test user");
        db
    }

    /// The route above this op answers 204 on `true` and 404 on `false`, so the
    /// second delete of one id must not report the same thing as the first. A
    /// `Result<()>` here made both look identical, which is how a request that
    /// removed nothing came to confirm a revocation.
    #[tokio::test]
    async fn deleting_a_token_twice_reports_the_second_delete_removed_nothing() {
        let db = setup_test_db().await;
        let token = create(
            &db,
            ActiveModel {
                id: sea_orm::NotSet,
                user_id: sea_orm::Set(1),
                name: sea_orm::Set("laptop".to_string()),
                token_hash: sea_orm::Set("hash".to_string()),
                scopes: sea_orm::Set("repo".to_string()),
                expires_at: sea_orm::Set(None),
                last_used_at: sea_orm::Set(None),
                created_at: sea_orm::Set(Utc::now()),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("create test token");

        assert!(
            delete_by_id(&db, token.id).await.expect("first delete"),
            "the delete that actually removed the row must report it"
        );
        assert!(
            !delete_by_id(&db, token.id).await.expect("second delete"),
            "a delete that removed no row must not be indistinguishable from one that did"
        );
    }

    /// An id that never existed is the same answer as one already revoked —
    /// and neither is an error, which is what keeps the 5xx/404 split honest.
    #[tokio::test]
    async fn deleting_an_unknown_token_id_is_not_an_error() {
        let db = setup_test_db().await;
        assert!(!delete_by_id(&db, 4242).await.expect("delete unknown id"));
    }
}
