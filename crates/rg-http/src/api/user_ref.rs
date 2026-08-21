//! Naming a *person* in a request body.
//!
//! An account is addressable three ways: the numeric `users.id`, the username,
//! and the e-mail. Only the first is stable — and it is the one nobody outside
//! the instance administration can look up. There is no `/users/{username}`,
//! `/search` indexes repositories, issues and wikis but not accounts, and
//! `/admin/users` is the instance admin's. So a body that accepts *only*
//! `user_id` is a dead end for the person filling it in: they know the name of
//! the human they want to let in and nothing else.
//!
//! The organization member and team member endpoints were exactly that dead
//! end (card_cb9f71672b11). On the live instance the owner of an organization
//! who wanted to give `Zubrenok` access created a **team** named after them
//! instead — reading the team's name field as the field for a person — and the
//! audit log recorded `org.create → team.create → repo.create` with no
//! `org.add_member` anywhere. Nothing about the permission model failed; the
//! only thing the form ever asked for was a number the owner could not obtain.
//!
//! This module is the one place that turns any of the three into the account
//! row, so every handler that hands out access agrees on what it accepts, on
//! which failures are the caller's, and on what it can say back about who was
//! added.

use serde::Deserialize;

use rg_db::entities::user::Model as User;

/// The three interchangeable ways a request body may name an account.
///
/// Flattened into a request struct, so an endpoint keeps its own fields and
/// gains all three names at once — and a client that already sends `user_id`
/// keeps working unchanged.
#[derive(Deserialize, Default)]
pub struct UserRef {
    pub user_id: Option<i64>,
    pub username: Option<String>,
    pub email: Option<String>,
}

impl UserRef {
    /// Resolve the named account, or fail with a `400` that names what was
    /// asked for.
    ///
    /// The whole row, not just the id: every caller either needs the username
    /// for its response and its audit entry, or is about to insert a row whose
    /// foreign key is this account. Handing back a number that was never
    /// checked is what let a typo'd `user_id` reach the database and come back
    /// as a constraint failure — a `500` for a request that was simply wrong.
    ///
    /// Only the four request-shaped outcomes are the caller's: a non-positive
    /// id, and each of the three names matching nobody. The lookups themselves
    /// are ours and stay `5xx`, which is why they are `?`-propagated rather
    /// than folded into "no such user".
    pub async fn resolve(&self, db: &rg_db::DatabaseConnection) -> anyhow::Result<User> {
        if let Some(user_id) = self.user_id {
            if user_id <= 0 {
                return Err(rg_core::error::invalid_request(
                    "user_id must be a positive integer",
                ));
            }
            return rg_db::ops::user_ops::find_by_id(db, user_id)
                .await?
                .ok_or_else(|| {
                    rg_core::error::invalid_request(format!("user #{user_id} not found"))
                });
        }

        if let Some(username) = trimmed(self.username.as_deref()) {
            return rg_db::ops::user_ops::find_by_username(db, username)
                .await?
                .ok_or_else(|| {
                    rg_core::error::invalid_request(format!("user '{username}' not found"))
                });
        }

        if let Some(email) = trimmed(self.email.as_deref()) {
            return rg_db::ops::user_ops::find_by_email(db, email)
                .await?
                .ok_or_else(|| {
                    rg_core::error::invalid_request(format!("user '{email}' not found"))
                });
        }

        Err(rg_core::error::invalid_request(
            "user_id, username, or email is required",
        ))
    }
}

/// The accounts a set of rows names, keyed by id.
///
/// The mirror of [`UserRef`] on the way out: a membership row carries a
/// `user_id` and nothing else, so a list rendered from those rows alone can
/// only say "User #3" — and the reader has no endpoint to turn that number
/// into a person with. One round-trip for the whole page rather than one per
/// row.
///
/// An id matching no account is simply absent from the map. The caller renders
/// that row unnamed rather than dropping it: the row is a membership that
/// really exists, and a list that answers "who has access" must not quietly
/// shorten itself.
pub(crate) async fn accounts_by_id(
    db: &rg_db::DatabaseConnection,
    user_ids: &[i64],
) -> anyhow::Result<std::collections::HashMap<i64, User>> {
    Ok(rg_db::ops::user_ops::find_by_ids(db, user_ids)
        .await?
        .into_iter()
        .map(|user| (user.id, user))
        .collect())
}

/// A present, non-blank name. A field holding `"  "` names nobody, and falling
/// through to the next branch answers "user_id, username, or email is
/// required" — which is the truth about that body — instead of looking up the
/// empty string.
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}
