//! Database operations for webhooks and webhook deliveries.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::webhook::{
    self, ActiveModel as WebhookActiveModel, Entity as WebhookEntity, Model as Webhook,
};
use crate::entities::webhook_delivery::{
    ActiveModel as DeliveryActiveModel, Entity as DeliveryEntity, Model as WebhookDelivery,
};
use crate::entities::{organization, repository};

// ── Webhook CRUD ──────────────────────────────────────────────────────────

/// Find a webhook by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Webhook>> {
    WebhookEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find webhook by id")
}

/// List webhooks for a repo.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Webhook>> {
    WebhookEntity::find()
        .filter(webhook::Column::RepoId.eq(repo_id))
        .order_by_desc(webhook::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list webhooks by repo")
}

/// Repository events fan out to the repository, its organization (if any),
/// and the instance. The scope predicate is part of the SQL query so a hook
/// registered in another organization cannot leak a private repo event.
pub async fn list_active_for_event(
    db: &DatabaseConnection,
    repo_id: i64,
    org_id: Option<i64>,
    event: &str,
) -> Result<Vec<Webhook>> {
    let mut scopes = Condition::any()
        .add(
            Condition::all()
                .add(webhook::Column::ScopeKind.eq("repo"))
                .add(webhook::Column::RepoId.eq(repo_id)),
        )
        .add(webhook::Column::ScopeKind.eq("system"));
    if let Some(org_id) = org_id {
        scopes = scopes.add(
            Condition::all()
                .add(webhook::Column::ScopeKind.eq("org"))
                .add(webhook::Column::ScopeId.eq(org_id)),
        );
    }
    WebhookEntity::find()
        .filter(scopes)
        .filter(webhook::Column::Active.eq(true))
        .filter(webhook::Column::Events.contains(event))
        .all(db)
        .await
        .context("db: list scoped active webhooks for event")
}

pub async fn list_by_scope(
    db: &DatabaseConnection,
    scope_kind: &str,
    scope_id: i64,
) -> Result<Vec<Webhook>> {
    WebhookEntity::find()
        .filter(webhook::Column::ScopeKind.eq(scope_kind))
        .filter(webhook::Column::ScopeId.eq(scope_id))
        .order_by_desc(webhook::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list webhooks by scope")
}

/// Create a new webhook.
pub async fn create_webhook(db: &DatabaseConnection, model: WebhookActiveModel) -> Result<Webhook> {
    let owner = match &model.scope_kind {
        ActiveValue::Set(kind) | ActiveValue::Unchanged(kind) => match kind.as_str() {
            "org" => Some("organization"),
            "repo" => Some("repository"),
            _ => None,
        },
        ActiveValue::NotSet => None,
    };
    let Some(owner) = owner else {
        return model.insert(db).await.context("db: create webhook");
    };
    // Serialize a scoped hook insert with retirement of its owner. The HTTP
    // scope check may have completed before retirement took the owner row,
    // and the retirement deletes the owner's hooks in its own transaction.
    let owner_id = match &model.scope_id {
        ActiveValue::Set(id) | ActiveValue::Unchanged(id) => *id,
        ActiveValue::NotSet => anyhow::bail!("{owner} webhook scope id was not set"),
    };
    let transaction = db
        .begin()
        .await
        .context("db: begin scoped webhook create")?;
    let lock = db.get_database_backend() != DatabaseBackend::Sqlite;
    let active = if owner == "organization" {
        let mut query = organization::Entity::find_by_id(owner_id)
            .filter(organization::Column::DeletedAt.is_null());
        if lock {
            query = query.lock_exclusive();
        }
        query.one(&transaction).await.map(|row| row.is_some())
    } else {
        let mut query = repository::Entity::find_by_id(owner_id)
            .filter(repository::Column::DeletedAt.is_null());
        if lock {
            query = query.lock_exclusive();
        }
        query.one(&transaction).await.map(|row| row.is_some())
    }
    .with_context(|| format!("db: check {owner} webhook owner"))?;
    if !active {
        anyhow::bail!("{owner} is no longer active");
    }
    let hook = model
        .insert(&transaction)
        .await
        .context("db: create scoped webhook")?;
    transaction
        .commit()
        .await
        .context("db: commit scoped webhook create")?;
    Ok(hook)
}

/// The owner whose retirement takes its webhooks and their journals along.
#[derive(Clone, Copy, Debug)]
pub enum WebhookOwner {
    Organization(i64),
    Repository(i64),
}

/// Delete every hook of `owner` and every delivery recorded for them.
///
/// Runs on the caller's transaction, after the owner row is written, so the
/// hooks leave together with their owner or not at all. `scope_id` is
/// polymorphic and carries no foreign key, and neither does
/// `webhook_deliveries.webhook_id`, so nothing in the schema does this.
///
/// The owner lookup stays in SQL: expanding all hook ids into an `IN` list
/// would exceed SQLite's bind limit for a large owner. On a server backend the
/// hooks are locked first, because the journal writer
/// ([`create_delivery`]) holds its hook's lock until it commits — without it a
/// writer could commit between the delivery cleanup and the hook delete and
/// leave an orphan behind.
pub async fn delete_owned_hooks(
    transaction: &DatabaseTransaction,
    owner: WebhookOwner,
) -> Result<()> {
    let backend = transaction.get_database_backend();
    let (kind, owner_column, id) = match owner {
        WebhookOwner::Organization(id) => ("org", webhook::Column::ScopeId, id),
        // Repository events select hooks by `repo_id`, so retire by the same
        // column the dispatcher reads.
        WebhookOwner::Repository(id) => ("repo", webhook::Column::RepoId, id),
    };
    if backend != DatabaseBackend::Sqlite {
        WebhookEntity::find()
            .select_only()
            .column(webhook::Column::Id)
            .filter(webhook::Column::ScopeKind.eq(kind))
            .filter(owner_column.eq(id))
            .lock_exclusive()
            .into_tuple::<i64>()
            .all(transaction)
            .await
            .with_context(|| format!("db: lock {kind} webhooks for delete"))?;
    }
    let sql = match owner {
        WebhookOwner::Organization(_) => {
            "DELETE FROM webhook_deliveries WHERE webhook_id IN (\
             SELECT id FROM webhooks WHERE scope_kind = 'org' AND scope_id = ?)"
        }
        WebhookOwner::Repository(_) => {
            "DELETE FROM webhook_deliveries WHERE webhook_id IN (\
             SELECT id FROM webhooks WHERE scope_kind = 'repo' AND repo_id = ?)"
        }
    };
    transaction
        .execute(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(backend, sql),
            [id.into()],
        ))
        .await
        .with_context(|| format!("db: delete {kind} webhook deliveries"))?;
    WebhookEntity::delete_many()
        .filter(webhook::Column::ScopeKind.eq(kind))
        .filter(owner_column.eq(id))
        .exec(transaction)
        .await
        .with_context(|| format!("db: delete {kind} webhooks"))?;
    Ok(())
}

/// Update a webhook in one conditional statement.
///
/// The repository-scoped read happens before this call. `None` therefore means
/// a concurrent delete won, while database failures remain errors.
#[allow(clippy::too_many_arguments)]
pub async fn update_webhook(
    db: &DatabaseConnection,
    id: i64,
    url: String,
    content_type: String,
    secret_encrypted: Option<String>,
    active: bool,
    events: String,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Webhook>> {
    let result = WebhookEntity::update_many()
        .col_expr(webhook::Column::Url, Expr::value(url))
        .col_expr(webhook::Column::ContentType, Expr::value(content_type))
        .col_expr(
            webhook::Column::SecretEncrypted,
            Expr::value(secret_encrypted),
        )
        .col_expr(webhook::Column::Active, Expr::value(active))
        .col_expr(webhook::Column::Events, Expr::value(events))
        .col_expr(webhook::Column::UpdatedAt, Expr::value(updated_at))
        .filter(webhook::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update webhook")?;
    match result.rows_affected {
        0 | 1 => find_by_id(db, id).await,
        rows => anyhow::bail!("db: webhook update affected {rows} rows for id {id}"),
    }
}

/// Delete a webhook by id together with its delivery journal, reporting
/// whether this call removed it.
///
/// `false` means the row was already gone when the DELETE ran — the caller's
/// own lookup happened in a separate statement, so a concurrent delete can win
/// in between. Only the request that actually removed the row may report a
/// deletion.
///
/// The journal goes in the same transaction: each delivery keeps the full
/// request payload and the receiver's response, and once the hook is gone no
/// route addresses them, so they would sit unreachable until the retention
/// sweep. On a server backend the hook is locked first so a delivery whose
/// network request finishes now either commits before this cleanup or finds
/// no hook ([`create_delivery`]); on SQLite the first statement is a write,
/// which takes the single writer slot outright.
pub async fn delete_webhook_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let transaction = db.begin().await.context("db: begin webhook delete")?;
    if db.get_database_backend() != DatabaseBackend::Sqlite {
        WebhookEntity::find_by_id(id)
            .lock_exclusive()
            .one(&transaction)
            .await
            .context("db: lock webhook for delete")?;
    }
    DeliveryEntity::delete_many()
        .filter(crate::entities::webhook_delivery::Column::WebhookId.eq(id))
        .exec(&transaction)
        .await
        .context("db: delete webhook deliveries")?;
    let result = WebhookEntity::delete_by_id(id)
        .exec(&transaction)
        .await
        .context("db: delete webhook")?;
    if result.rows_affected == 0 {
        transaction
            .rollback()
            .await
            .context("db: roll back missing webhook delete")?;
        return Ok(false);
    }
    transaction
        .commit()
        .await
        .context("db: commit webhook delete")?;
    Ok(true)
}

// ── Webhook Delivery ──────────────────────────────────────────────────────

/// Create a new webhook delivery record.
pub async fn create_delivery(
    db: &DatabaseConnection,
    model: DeliveryActiveModel,
) -> Result<WebhookDelivery> {
    let webhook_id = match &model.webhook_id {
        ActiveValue::Set(id) | ActiveValue::Unchanged(id) => *id,
        ActiveValue::NotSet => anyhow::bail!("webhook delivery has no webhook id"),
    };
    let transaction = db.begin().await.context("db: begin webhook delivery")?;
    // A delivery may finish its network request after its hook is retired.
    // Lock the hook until the journal insert commits so deletion can neither
    // miss this row nor leave a new orphan after its cleanup statement.
    let mut query = WebhookEntity::find_by_id(webhook_id);
    if db.get_database_backend() != DatabaseBackend::Sqlite {
        query = query.lock_exclusive();
    }
    if query
        .one(&transaction)
        .await
        .context("db: check webhook delivery owner")?
        .is_none()
    {
        anyhow::bail!("webhook was removed before its delivery was recorded");
    }
    let delivery = model
        .insert(&transaction)
        .await
        .context("db: create webhook delivery")?;
    transaction
        .commit()
        .await
        .context("db: commit webhook delivery")?;
    Ok(delivery)
}

/// List deliveries for a webhook.
pub async fn list_deliveries_by_webhook(
    db: &DatabaseConnection,
    webhook_id: i64,
) -> Result<Vec<WebhookDelivery>> {
    deliveries_query(webhook_id)
        .all(db)
        .await
        .context("db: list webhook deliveries")
}

/// The newest 50 deliveries of one webhook, newest first.
///
/// `id` breaks ties between deliveries recorded in the same instant — a burst
/// of pushes fans out faster than the timestamp's resolution — and lets the
/// `(webhook_id, created_at, id)` index hand the rows over in order, so the
/// engine reads 50 payloads instead of every payload the webhook ever sent.
pub(crate) fn deliveries_query(webhook_id: i64) -> Select<DeliveryEntity> {
    DeliveryEntity::find()
        .filter(crate::entities::webhook_delivery::Column::WebhookId.eq(webhook_id))
        .order_by_desc(crate::entities::webhook_delivery::Column::CreatedAt)
        .order_by_desc(crate::entities::webhook_delivery::Column::Id)
        .limit(50)
}

/// Find a delivery by id.
pub async fn find_delivery_by_id(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<WebhookDelivery>> {
    DeliveryEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find webhook delivery by id")
}

/// Delete up to `limit` deliveries recorded before `cutoff`, oldest first, and
/// return how many went.
///
/// The retention sweep's batch. A delivery keeps its full request payload and
/// the receiver's response body, the hook's page shows the latest fifty, and
/// nothing else reads the rest — so without a bound the table grows by a full
/// payload per event per hook for as long as the instance lives. Two short
/// statements per batch rather than one unbounded `DELETE`: the ids are read
/// first and deleted by key, so no single statement holds the write lock for
/// the whole backlog (and MySQL refuses `LIMIT` inside an `IN` subquery). The
/// caller bounds `limit`.
pub async fn delete_deliveries_before(
    db_read: &DatabaseConnection,
    db_write: &DatabaseConnection,
    cutoff: chrono::DateTime<chrono::Utc>,
    limit: u64,
) -> Result<u64> {
    use crate::entities::webhook_delivery::Column;
    let ids: Vec<i64> = DeliveryEntity::find()
        .select_only()
        .column(Column::Id)
        .filter(Column::CreatedAt.lt(cutoff))
        .order_by_asc(Column::CreatedAt)
        .order_by_asc(Column::Id)
        .limit(limit)
        .into_tuple()
        .all(db_read)
        .await
        .context("db: list expired webhook deliveries")?;
    if ids.is_empty() {
        return Ok(0);
    }
    let result = DeliveryEntity::delete_many()
        .filter(Column::Id.is_in(ids))
        .exec(db_write)
        .await
        .context("db: delete expired webhook deliveries")?;
    Ok(result.rows_affected)
}
