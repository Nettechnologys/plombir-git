//! Old repository addresses and where they lead (card_e83bf21a5e5b).
//!
//! Written by the ownership transaction of a rename or a transfer, released
//! when a repository takes the name, read only where no live repository
//! answers. See the migration `m20261009_000920_repository_redirects`.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::*;

use crate::entities::repository::{self, Model as Repo};
use crate::entities::repository_redirect::{self, Entity as RedirectEntity};

/// The namespace a repository address lives in, by id — a renamed owner keeps
/// its redirects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Namespace {
    User(i64),
    Org(i64),
}

impl Namespace {
    /// The namespace a repository row lives in.
    pub fn of(owner_id: i64, org_id: Option<i64>) -> Self {
        match org_id {
            Some(org_id) => Namespace::Org(org_id),
            None => Namespace::User(owner_id),
        }
    }

    fn key(self) -> (&'static str, i64) {
        match self {
            Namespace::User(id) => ("user", id),
            Namespace::Org(id) => ("org", id),
        }
    }
}

/// Drop the redirect at `namespace/name`: a repository is taking the address.
pub async fn release<C: ConnectionTrait>(db: &C, namespace: Namespace, name: &str) -> Result<()> {
    let (kind, id) = namespace.key();
    RedirectEntity::delete_many()
        .filter(repository_redirect::Column::NamespaceKind.eq(kind))
        .filter(repository_redirect::Column::NamespaceId.eq(id))
        .filter(repository_redirect::Column::Name.eq(name))
        .exec(db)
        .await
        .context("db: release repository redirect")?;
    Ok(())
}

/// Record that `repo_id` left `from_namespace/from_name` for
/// `to_namespace/to_name`, inside the transaction that moves it: the
/// destination's redirect is released (the repository owns that address now)
/// and the source address leads to the repository from here on.
pub async fn record_move<C: ConnectionTrait>(
    db: &C,
    repo_id: i64,
    from_namespace: Namespace,
    from_name: &str,
    to_namespace: Namespace,
    to_name: &str,
) -> Result<()> {
    release(db, to_namespace, to_name).await?;
    if (from_namespace, from_name) == (to_namespace, to_name) {
        return Ok(());
    }
    release(db, from_namespace, from_name).await?;
    let (kind, id) = from_namespace.key();
    repository_redirect::ActiveModel {
        id: NotSet,
        namespace_kind: Set(kind.to_string()),
        namespace_id: Set(id),
        name: Set(from_name.to_string()),
        repo_id: Set(repo_id),
        created_at: Set(Utc::now()),
    }
    .insert(db)
    .await
    .context("db: record repository redirect")?;
    Ok(())
}

/// The live repository `namespace/name` leads to, if a rename or a transfer
/// left that address. A deleted repository leads nowhere.
pub async fn target(
    db: &DatabaseConnection,
    namespace: Namespace,
    name: &str,
) -> Result<Option<Repo>> {
    let (kind, id) = namespace.key();
    let Some(redirect) = RedirectEntity::find()
        .filter(repository_redirect::Column::NamespaceKind.eq(kind))
        .filter(repository_redirect::Column::NamespaceId.eq(id))
        .filter(repository_redirect::Column::Name.eq(name))
        .one(db)
        .await
        .context("db: find repository redirect")?
    else {
        return Ok(None);
    };
    repository::Entity::find_by_id(redirect.repo_id)
        .filter(repository::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find redirected repository")
}
