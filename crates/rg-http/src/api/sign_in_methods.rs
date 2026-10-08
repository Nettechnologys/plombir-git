//! Whether an account keeps a way to sign in after it drops one.
//!
//! An account created by a first SSO sign-in has no password, and nothing can
//! give it one: `forgot-password` serves `auth_provider = "local"` accounts
//! only. Its ways in are its provider links and its passkeys. Unlinking the
//! last provider used to be harmless anyway, because the next sign-in through
//! that provider found the account by its email and linked it again. That
//! merge was the pre-hijack of card_4753cfe7b985 and is gone, so dropping the
//! last way in is now permanent — the account and its repositories stay behind
//! with nobody able to open them. Both doors that remove a way in ask here
//! first.

use crate::error::AppError;
use crate::AppState;

/// The kind of way in a request is about to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WayIn {
    /// An `oauth_accounts` row.
    ProviderLink,
    /// A `passkey_credentials` row.
    Passkey,
}

/// Refuse to remove the `kind` row `id` of account `user_id` when it is that
/// account's last way to sign in.
///
/// A password counts only on a local account, because only that branch of the
/// login dispatch ever reads it; a directory account signs in through its
/// directory and keeps that whatever it unlinks here.
///
/// The question and the delete that follows it are separate statements, so two
/// concurrent removals of an account's last two ways in can both pass. That
/// race needs the account's own session on both sides, and it is the owner
/// locking themselves out; it is accepted rather than paid for with a lock.
pub(crate) async fn refuse_removing_the_last_way_in(
    state: &AppState,
    user_id: i64,
    kind: WayIn,
    id: i64,
) -> Result<(), AppError> {
    let Some(user) = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
    else {
        // Nothing to lock out. The caller's own lookup answers this case.
        return Ok(());
    };
    let has_password = user.auth_provider == "local" && !user.password_hash.is_empty();
    if has_password || user.auth_provider == "ldap" {
        return Ok(());
    }

    let other_links = rg_db::ops::oauth_account_ops::find_by_user_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .into_iter()
        .filter(|link| !(kind == WayIn::ProviderLink && link.id == id))
        .count();
    let other_passkeys = rg_db::ops::passkey_credential_ops::list_by_user(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .into_iter()
        .filter(|key| !(kind == WayIn::Passkey && key.id == id))
        .count();

    if other_links + other_passkeys == 0 {
        return Err(AppError::conflict(
            "this is the last way to sign in to this account; link another provider or add a \
             passkey before removing it",
        ));
    }
    Ok(())
}
