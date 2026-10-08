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
//!
//! The e-mail is read and **refused** (card_9e97b992d4a6): the instance does not
//! confirm addresses, so an address identifies the person who typed it first,
//! and a grant addressed to it can land in a stranger's account. It comes back
//! only together with address confirmation, and then only for confirmed
//! addresses.

use serde::Deserialize;

use rg_db::entities::user::Model as User;

/// Why an e-mail does not name a person here. Shared so every surface that
/// reads a [`UserRef`] gives the same reason.
pub(crate) const EMAIL_NOT_ACCEPTED: &str =
    "a person cannot be named by e-mail address here: addresses are not confirmed on this \
     instance, so an address names whoever registered it first; use the username";

/// The ways a request body may name an account: `user_id` or `username`, and
/// an `email` that is refused with a reason (see the module note).
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
    /// Only the request-shaped outcomes are the caller's: a non-positive id,
    /// an id or username matching nobody, and an e-mail at all. The lookups themselves
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

        // An address is refused, not resolved (card_9e97b992d4a6). Nothing on
        // this instance confirms that the account holding an address owns it —
        // local registration takes whatever was typed — so "add
        // bob@corp.example" would hand the grant to whoever registered that
        // address first, not to Bob. The field stays in the body so the caller
        // is told why, instead of the address being dropped and the request
        // answered as if it named nobody.
        if trimmed(self.email.as_deref()).is_some() {
            return Err(rg_core::error::invalid_request(EMAIL_NOT_ACCEPTED));
        }

        Err(rg_core::error::invalid_request(
            "user_id or username is required",
        ))
    }
}

/// The people an allow-list names, resolved to the ids it is stored as.
///
/// An allow-list is the other shape access is handed out in: not one person per
/// request, but a whole set replacing the previous one — the direct-push
/// exceptions of a protected branch, the exceptions of a protected tag pattern.
/// It was numeric-only on both ends, which put the operator in the same dead end
/// [`UserRef`] exists for, and one step worse: the settings form's placeholder
/// read `42, 108`, so the screen asked in numbers for people it had no endpoint
/// to look up.
///
/// Order is the caller's and repeats are dropped — the grant tables key on
/// `(rule, user)`, so a list naming the same person twice is one grant, not a
/// constraint failure. A blank entry is refused rather than skipped: silently
/// dropping it would let `alice,,bob` store fewer people than it names.
pub(crate) async fn resolve_identifiers(
    db: &rg_db::DatabaseConnection,
    identifiers: &[String],
) -> anyhow::Result<Vec<i64>> {
    let mut ids: Vec<i64> = Vec::with_capacity(identifiers.len());
    for identifier in identifiers {
        if identifier.trim().is_empty() {
            return Err(rg_core::error::invalid_request(
                "an entry of the allow-list names nobody",
            ));
        }
        let user = UserRef::from_identifier(identifier).resolve(db).await?;
        if !ids.contains(&user.id) {
            ids.push(user.id);
        }
    }
    Ok(ids)
}

/// The allow-list a request carries, whichever of the two ways it named it.
///
/// The named list wins when it is present, and the numeric one is what a client
/// written before names were accepted still sends. They are not merged: a body
/// carrying both would otherwise store the union of two lists the operator
/// believes are one, and "the list is exactly this" is the whole contract of an
/// allow-list — it is how a grant is revoked.
///
/// `None` from both is `None`, which every caller reads as "leave the stored
/// list alone" rather than as "admit nobody".
pub(crate) async fn resolve_allow_list(
    db: &rg_db::DatabaseConnection,
    named: Option<&[String]>,
    ids: Option<Vec<i64>>,
) -> anyhow::Result<Option<Vec<i64>>> {
    match named {
        Some(named) => Ok(Some(resolve_identifiers(db, named).await?)),
        None => Ok(ids),
    }
}

/// One person on an allow-list, named.
///
/// The mirror of [`resolve_allow_list`] on the way out, and the same shape the
/// collaborator listing answers in: the id stays because it is what the rule
/// stores, and the name travels with it because the client has nowhere to look
/// it up. `username` is `None` for an id that resolves to no account — the
/// grant is real and stays visible, unnamed, rather than shortening a list that
/// answers "who may push here".
#[derive(Debug, serde::Serialize)]
pub struct AllowedUser {
    pub user_id: i64,
    pub username: Option<String>,
    pub display_name: Option<String>,
}

/// Name every id of an allow-list, in the order the rule stores them.
pub(crate) async fn name_allow_list(
    db: &rg_db::DatabaseConnection,
    user_ids: &[i64],
) -> anyhow::Result<Vec<AllowedUser>> {
    let accounts = accounts_by_id(db, user_ids).await?;
    Ok(user_ids
        .iter()
        .map(|user_id| {
            let account = accounts.get(user_id);
            AllowedUser {
                user_id: *user_id,
                username: account.map(|user| user.username.clone()),
                display_name: account.and_then(|user| user.display_name.clone()),
            }
        })
        .collect())
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

impl UserRef {
    /// Read one free-text identifier the way a person types it.
    ///
    /// A form field asks for "user", not for one of three keys, so something
    /// has to decide which key the answer becomes: a bare run of digits is an
    /// id (anyone who genuinely has one keeps working), anything containing
    /// `@` is an e-mail, everything else is a username. The browser side makes
    /// the same three-way choice in `web/src/lib/api/userRef.ts::buildUserRef`;
    /// this is the server's copy for endpoints whose body carries the
    /// identifier as a single string rather than as the flattened [`UserRef`].
    /// An `@` still becomes an e-mail, so the person who typed one is told why
    /// it is refused rather than that no such username exists.
    ///
    /// A number that does not fit `i64` is kept as a username rather than
    /// rejected here — [`UserRef::resolve`] is where "matches nobody" is
    /// answered, in one place and with one message.
    pub fn from_identifier(identifier: &str) -> Self {
        let identifier = identifier.trim();
        if !identifier.is_empty() && identifier.bytes().all(|byte| byte.is_ascii_digit()) {
            if let Ok(user_id) = identifier.parse::<i64>() {
                return Self {
                    user_id: Some(user_id),
                    ..Self::default()
                };
            }
        }
        if identifier.contains('@') {
            return Self {
                email: Some(identifier.to_string()),
                ..Self::default()
            };
        }
        Self {
            username: Some(identifier.to_string()),
            ..Self::default()
        }
    }
}

/// A present, non-blank name. A field holding `"  "` names nobody, and falling
/// through to the next branch answers "user_id or username is
/// required" — which is the truth about that body — instead of looking up the
/// empty string.
fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}
