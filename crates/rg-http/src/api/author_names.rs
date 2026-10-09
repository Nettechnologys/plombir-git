//! Who wrote something, as a reader is shown it: the author's name, and — when
//! the author is a bot — the person it acts for (card_60a80311d512).
//!
//! An agent's issue, comment or pull request used to be indistinguishable from
//! its owner's, because the agent *was* its owner. With bot accounts the author
//! is the agent itself; `author_bot_owner` is what lets a reader see on whose
//! behalf it acted without a second lookup endpoint.

use std::collections::HashMap;

use sea_orm::DatabaseConnection;

use crate::error::AppError;

#[derive(Clone)]
struct Resolved {
    name: String,
    bot_owner: Option<String>,
}

/// A per-response cache of authors: a list of fifty comments by two people asks
/// the database about two accounts, not fifty.
#[derive(Default)]
pub(crate) struct AuthorNames {
    cache: HashMap<i64, Option<Resolved>>,
}

impl AuthorNames {
    /// Resolve every account in `user_ids` — and the owners of the bots among
    /// them — in at most two queries, so the per-row lookups that follow are
    /// all cache hits.
    ///
    /// A list of fifty issues by fifty different people used to ask the
    /// database fifty times (a hundred, counting assignees and bot owners): the
    /// per-response cache only helps when the same person appears twice. A
    /// listing calls this once with every id it is about to name, and the cost
    /// of the page stops depending on how many people wrote it.
    ///
    /// An id matching no account is cached as `None`, exactly as a single
    /// [`Self::resolve`] would have cached it: "this account is gone" is an
    /// answer, not a reason to ask again.
    pub(crate) async fn prefetch(
        &mut self,
        db: &DatabaseConnection,
        user_ids: impl IntoIterator<Item = i64>,
    ) -> Result<(), AppError> {
        let mut wanted: Vec<i64> = user_ids
            .into_iter()
            .filter(|id| !self.cache.contains_key(id))
            .collect();
        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() {
            return Ok(());
        }
        let accounts = super::user_ref::accounts_by_id(db, &wanted)
            .await
            .map_err(AppError::from)?;

        let mut owner_ids: Vec<i64> = accounts
            .values()
            .filter_map(|user| user.bot_owner_id)
            .filter(|owner_id| !accounts.contains_key(owner_id))
            .collect();
        owner_ids.sort_unstable();
        owner_ids.dedup();
        let owners = if owner_ids.is_empty() {
            std::collections::HashMap::new()
        } else {
            super::user_ref::accounts_by_id(db, &owner_ids)
                .await
                .map_err(AppError::from)?
        };
        let owner_name = |owner_id: i64| {
            accounts
                .get(&owner_id)
                .or_else(|| owners.get(&owner_id))
                .map(|owner| owner.username.clone())
        };

        for id in wanted {
            let resolved = accounts.get(&id).map(|user| Resolved {
                name: user.username.clone(),
                bot_owner: user.bot_owner_id.and_then(owner_name),
            });
            self.cache.insert(id, resolved);
        }
        Ok(())
    }

    async fn resolve(
        &mut self,
        db: &DatabaseConnection,
        user_id: i64,
    ) -> Result<Option<Resolved>, AppError> {
        if let Some(cached) = self.cache.get(&user_id) {
            return Ok(cached.clone());
        }
        let resolved = match rg_db::ops::user_ops::find_by_id(db, user_id)
            .await
            .map_err(AppError::from)?
        {
            None => None,
            Some(user) => {
                let bot_owner = match user.bot_owner_id {
                    Some(owner_id) => rg_db::ops::user_ops::find_by_id(db, owner_id)
                        .await
                        .map_err(AppError::from)?
                        .map(|owner| owner.username),
                    None => None,
                };
                Some(Resolved {
                    name: user.username,
                    bot_owner,
                })
            }
        };
        self.cache.insert(user_id, resolved.clone());
        Ok(resolved)
    }

    /// The account's name; `None` when it no longer resolves.
    pub(crate) async fn name(
        &mut self,
        db: &DatabaseConnection,
        user_id: i64,
    ) -> Result<Option<String>, AppError> {
        Ok(self.resolve(db, user_id).await?.map(|user| user.name))
    }

    /// The name and, for a bot, its owner's name.
    pub(crate) async fn author(
        &mut self,
        db: &DatabaseConnection,
        user_id: i64,
    ) -> Result<(Option<String>, Option<String>), AppError> {
        Ok(match self.resolve(db, user_id).await? {
            Some(user) => (Some(user.name), user.bot_owner),
            None => (None, None),
        })
    }
}
