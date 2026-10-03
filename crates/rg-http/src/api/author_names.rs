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
