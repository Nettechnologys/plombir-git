//! Durable notices for access and security configuration changes. Call only
//! inside the mutation transaction. Historical affected-account copies contain
//! names/roles, never repository contents, remote URLs, or credentials.
use super::notification_delivery_ops::{self as queue, QueueEvent};
use crate::{
    entities::{organization, organization_member, repository, user},
    notification::Event,
};
use anyhow::Result;
use sea_orm::*;

pub async fn org_owners(db: &impl ConnectionTrait, org_id: i64) -> Result<Vec<i64>> {
    Ok(organization_member::Entity::find()
        .filter(organization_member::Column::OrgId.eq(org_id))
        .filter(organization_member::Column::Role.eq("owner"))
        .all(db)
        .await?
        .into_iter()
        .map(|m| m.user_id)
        .collect())
}

pub async fn repo_event(
    db: &impl ConnectionTrait,
    repo_id: i64,
    event: Event,
    action: &str,
    affected: &[i64],
) -> Result<()> {
    repo_event_with_history(db, repo_id, event, action, affected, None).await
}

/// A transfer's previous principals must not learn the new private namespace.
pub async fn repo_event_with_history(
    db: &impl ConnectionTrait,
    repo_id: i64,
    event: Event,
    action: &str,
    affected: &[i64],
    historical_name: Option<&str>,
) -> Result<()> {
    let Some(repo) = repository::Entity::find_by_id(repo_id).one(db).await? else {
        return Ok(());
    };
    let (namespace, owners) = if let Some(org_id) = repo.org_id {
        (
            organization::Entity::find_by_id(org_id)
                .one(db)
                .await?
                .map(|o| o.name)
                .unwrap_or_default(),
            org_owners(db, org_id).await?,
        )
    } else {
        (
            user::Entity::find_by_id(repo.owner_id)
                .one(db)
                .await?
                .map(|u| u.username)
                .unwrap_or_default(),
            vec![repo.owner_id],
        )
    };
    emit(
        db,
        event,
        &format!("{namespace}/{}", repo.name),
        action,
        owners,
        Some(repo_id),
        None,
        affected,
        historical_name,
    )
    .await
}

pub async fn org_event(
    db: &impl ConnectionTrait,
    org_id: i64,
    event: Event,
    action: &str,
    affected: &[i64],
) -> Result<()> {
    let Some(org) = organization::Entity::find_by_id(org_id).one(db).await? else {
        return Ok(());
    };
    emit(
        db,
        event,
        &org.name,
        action,
        org_owners(db, org_id).await?,
        None,
        Some(org_id),
        affected,
        None,
    )
    .await
}

// Owner copies must retain their authority until dispatch. Affected-account
// copies intentionally survive removal: they report that account's old grant.
#[allow(clippy::too_many_arguments)]
async fn emit(
    db: &impl ConnectionTrait,
    event: Event,
    name: &str,
    action: &str,
    owners: Vec<i64>,
    repo_id: Option<i64>,
    org_id: Option<i64>,
    affected: &[i64],
    historical_name: Option<&str>,
) -> Result<()> {
    let mut recipients = std::collections::BTreeSet::new();
    recipients.extend(owners.iter().copied());
    recipients.extend(affected.iter().copied());
    if let Some(actor) = crate::notification::current_actor_id() {
        recipients.insert(actor);
    }
    // Resolve names in one bounded query; notification text never exposes a
    // raw grant value or a bot's credentials. Bot grants notify the human owner.
    let subjects: Vec<String> = user::Entity::find()
        .filter(user::Column::Id.is_in(affected.iter().take(20).copied()))
        .order_by_asc(user::Column::Id)
        .all(db)
        .await?
        .into_iter()
        .map(|u| u.username)
        .collect();
    let users = user::Entity::find()
        .filter(user::Column::Id.is_in(recipients.iter().copied()))
        .all(db)
        .await?;
    let mut routed = std::collections::BTreeMap::new();
    for principal in users {
        let recipient = principal.bot_owner_id.unwrap_or(principal.id);
        let owner = owners.contains(&principal.id);
        let affected_account = affected.contains(&principal.id);
        let flags = routed.entry(recipient).or_insert((false, false));
        flags.0 |= owner;
        flags.1 |= affected_account;
    }
    let mut events = Vec::new();
    for (recipient, (owner, affected_account)) in routed {
        let recipient_name = if affected_account && !owner {
            historical_name.unwrap_or(name)
        } else {
            name
        };
        let mut note = QueueEvent::security(recipient, event, Some(recipient_name.into()));
        note.metadata.changed = vec![action.into()];
        note.metadata.subject = (!subjects.is_empty()).then(|| subjects.join(", "));
        if owner && !affected_account {
            note.repo_id = repo_id;
            note.admin_only = repo_id.is_some();
            if matches!(action, "repository_deleted" | "organization_deleted") {
                // The fact being reported is retirement itself; retain only
                // the bounded historical identity after the resource is gone.
                note.repo_id = None;
                note.admin_only = false;
            }
            note.metadata.organization_owner_id = if action == "organization_deleted" {
                None
            } else {
                org_id
            };
        }
        events.push(note);
    }
    for page in events.chunks(200) {
        queue::enqueue_many(db, page).await?;
    }
    Ok(())
}

/// Instance-wide trust changes reach current administrators. The affected
/// account gets a separate historical copy, subject to its own preferences.
pub async fn system_event(
    db: &impl ConnectionTrait,
    action: &str,
    affected: Option<i64>,
) -> Result<()> {
    let mut recipients: std::collections::BTreeSet<i64> = user::Entity::find()
        .filter(user::Column::IsAdmin.eq(true))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .all(db)
        .await?
        .into_iter()
        .map(|u| u.id)
        .collect();
    recipients.extend(affected);
    let mut events = Vec::new();
    for id in recipients {
        let mut event = QueueEvent::security(id, Event::AdminSecurityChanged, None);
        event.metadata.changed = vec![action.into()];
        event.metadata.subject = affected.map(|id| id.to_string());
        event.admin_only = affected != Some(id);
        events.push(event);
    }
    for page in events.chunks(200) {
        queue::enqueue_many(db, page).await?;
    }
    Ok(())
}
