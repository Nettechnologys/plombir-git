//! Database operations for bot agent profiles (ADR-0001, card_d4d689acfa70).

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::agent_profile::{self, Entity as ProfileEntity, Model as AgentProfile};
use crate::entities::agent_profile_repository;

/// The profile of bot `bot_id`, if it has one.
pub async fn find_by_bot(db: &DatabaseConnection, bot_id: i64) -> Result<Option<AgentProfile>> {
    ProfileEntity::find()
        .filter(agent_profile::Column::BotId.eq(bot_id))
        .one(db)
        .await
        .context("db: find agent profile by bot")
}

/// The repositories profile `profile_id` may work in, ascending.
pub async fn repository_ids(db: &DatabaseConnection, profile_id: i64) -> Result<Vec<i64>> {
    Ok(agent_profile_repository::Entity::find()
        .filter(agent_profile_repository::Column::ProfileId.eq(profile_id))
        .order_by_asc(agent_profile_repository::Column::RepositoryId)
        .all(db)
        .await
        .context("db: list agent profile repositories")?
        .into_iter()
        .map(|row| row.repository_id)
        .collect())
}

/// Write the profile of `model.bot_id` and replace its repository list, in
/// one transaction.
///
/// `model` is the complete row; its `id` is ignored, the bot's existing row
/// (if any) is the one updated. The list is replaced rather than merged, and
/// it lands with the row or not at all, so a failure cannot leave a profile
/// that reaches a repository its owner just took off the list.
pub async fn upsert(
    db: &DatabaseConnection,
    model: AgentProfile,
    repository_ids: &[i64],
) -> Result<AgentProfile> {
    let transaction = db.begin().await.context("db: begin agent profile write")?;
    let existing = ProfileEntity::find()
        .filter(agent_profile::Column::BotId.eq(model.bot_id))
        .one(&transaction)
        .await
        .context("db: read agent profile before write")?;
    let active = active_for_write(model, existing.as_ref());
    let profile = if existing.is_some() {
        active
            .update(&transaction)
            .await
            .context("db: update agent profile")?
    } else {
        active
            .insert(&transaction)
            .await
            .context("db: insert agent profile")?
    };
    agent_profile_repository::Entity::delete_many()
        .filter(agent_profile_repository::Column::ProfileId.eq(profile.id))
        .exec(&transaction)
        .await
        .context("db: clear agent profile repositories")?;
    let mut unique = repository_ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if !unique.is_empty() {
        agent_profile_repository::Entity::insert_many(unique.into_iter().map(|repository_id| {
            agent_profile_repository::ActiveModel {
                profile_id: Set(profile.id),
                repository_id: Set(repository_id),
            }
        }))
        .exec(&transaction)
        .await
        .context("db: record agent profile repositories")?;
    }
    transaction
        .commit()
        .await
        .context("db: commit agent profile write")?;
    Ok(profile)
}

/// Every column set from `active`, with the key column pointing at the
/// existing row on an update and left to the database on an insert.
fn active_for_write(
    model: AgentProfile,
    existing: Option<&AgentProfile>,
) -> agent_profile::ActiveModel {
    let mut active = agent_profile::ActiveModel::from(model).reset_all();
    active.id = match existing {
        Some(row) => Unchanged(row.id),
        None => NotSet,
    };
    active
}

/// Delete bot `bot_id`'s profile and, through the cascade, its repository list.
/// `false` when there was none.
pub async fn delete_by_bot(db: &DatabaseConnection, bot_id: i64) -> Result<bool> {
    let result = ProfileEntity::delete_many()
        .filter(agent_profile::Column::BotId.eq(bot_id))
        .exec(db)
        .await
        .context("db: delete agent profile")?;
    Ok(result.rows_affected > 0)
}
