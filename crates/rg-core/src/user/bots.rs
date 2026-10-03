//! Bot accounts — an AI agent's own identity, owned by a person
//! (card_60a80311d512).
//!
//! An agent working under its owner's Personal Access Token is indistinguishable
//! from its owner: every issue, comment and merge it makes reads as theirs, and
//! the only way to stop it is to revoke something the owner also uses. A bot is
//! an account of its own, so its actions carry its own name, its repository
//! access is granted to *it* (as a collaborator), and its tokens can be revoked
//! without touching anybody else's.
//!
//! What makes it a bot rather than a second human account:
//!
//! - it has an owner (`users.bot_owner_id`), who is the only one who can mint
//!   its tokens or delete it, and it stops authenticating the moment the owner
//!   does;
//! - it has no password and no external identity, so no login form, password
//!   reset or SSO callback can ever produce a session for it;
//! - its tokens carry the `repo` scope only — the account-management API
//!   (`/users/...`) is out of its reach, so it cannot mint credentials for
//!   itself, enrol SSH keys, or create bots of its own.

use anyhow::Result;
use sea_orm::DatabaseConnection;

use rg_db::entities::user::Model as User;
use rg_db::ops::user_ops;

/// Domain of the address every bot account is created with.
///
/// `.invalid` is reserved (RFC 2606) and never resolves, so mail addressed to a
/// bot cannot reach a real mailbox. The address exists only because
/// `users.email` is unique and required.
pub const BOT_EMAIL_DOMAIN: &str = "bots.invalid";

/// Create a bot owned by `owner_id`.
///
/// The owner must be a usable human account: a bot cannot own a bot, and an
/// account that can no longer act cannot take on something it answers for.
/// The name follows the rules of every other account name, and must be free
/// both as an account and as an organization — the two share one URL space.
pub async fn create_bot(
    db: &DatabaseConnection,
    owner_id: i64,
    username: &str,
    display_name: Option<&str>,
) -> Result<User> {
    let username = username.trim();
    let owner = user_ops::find_by_id(db, owner_id)
        .await?
        .ok_or_else(|| crate::error::not_found("user"))?;
    if !owner.is_usable() {
        return Err(crate::error::forbidden(
            "a disabled account cannot create bots",
        ));
    }
    if owner.is_bot() {
        return Err(crate::error::forbidden("a bot cannot create bots"));
    }

    super::service::validate_username(username)?;
    let display_name = display_name.map(str::trim).filter(|name| !name.is_empty());
    if display_name.is_some_and(|name| name.chars().count() > 255) {
        return Err(crate::error::invalid_request(
            "display_name must be at most 255 characters",
        ));
    }

    let taken = || crate::error::conflict(format!("username '{username}' is already taken"));
    if user_ops::find_by_username(db, username).await?.is_some()
        || rg_db::ops::org_ops::get_org_by_name(db, username)
            .await?
            .is_some()
    {
        return Err(taken());
    }

    let email = format!("{}@{BOT_EMAIL_DOMAIN}", username.to_ascii_lowercase());
    user_ops::create_bot(db, owner_id, username, &email, display_name)
        .await
        .map_err(|error| {
            if rg_db::is_unique_violation_anyhow(&error) {
                taken()
            } else {
                error
            }
        })
}

/// The bots `owner_id` owns, oldest first.
pub async fn list_bots(db: &DatabaseConnection, owner_id: i64) -> Result<Vec<User>> {
    user_ops::list_bots_by_owner(db, owner_id).await
}

/// The bot called `username` when `owner_id` owns it.
///
/// Somebody else's bot and no bot at all are the same answer — `None` — so a
/// person probing names learns nothing about accounts that are not theirs.
pub async fn find_owned_bot(
    db: &DatabaseConnection,
    owner_id: i64,
    username: &str,
) -> Result<Option<User>> {
    Ok(user_ops::find_by_username(db, username)
        .await?
        .filter(|user| user.bot_owner_id == Some(owner_id)))
}

/// Whether the owner of `bot` can still act — a bot answers for nothing on its
/// own, so it stops authenticating the moment its owner does.
///
/// `Ok(true)` for a human account, which has no owner to consult.
pub async fn owner_is_usable(db: &DatabaseConnection, account: &User) -> Result<bool> {
    let Some(owner_id) = account.bot_owner_id else {
        return Ok(true);
    };
    Ok(user_ops::find_by_id(db, owner_id)
        .await?
        .is_some_and(|owner| owner.is_usable()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{ConnectOptions, Database};

    async fn db() -> DatabaseConnection {
        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options).await.unwrap();
        rg_db::run_migrations(&db).await.unwrap();
        db
    }

    async fn human(db: &DatabaseConnection, name: &str) -> User {
        user_ops::create_user(db, name, &format!("{name}@example.test"), "x", "")
            .await
            .unwrap()
    }

    fn status(error: &anyhow::Error) -> String {
        format!("{error:#}")
    }

    #[tokio::test]
    async fn a_bot_is_an_owned_account_no_login_reaches() {
        let db = db().await;
        let alice = human(&db, "alice").await;
        let bot = create_bot(&db, alice.id, "alice-agent", Some("Alice's agent"))
            .await
            .unwrap();
        assert_eq!(bot.bot_owner_id, Some(alice.id));
        assert!(bot.is_bot());
        assert!(!bot.is_admin);
        assert_eq!(bot.auth_provider, user_ops::BOT_AUTH_PROVIDER);
        assert!(bot.password_hash.is_empty());
        assert_eq!(bot.email, "alice-agent@bots.invalid");
        assert_eq!(
            list_bots(&db, alice.id)
                .await
                .unwrap()
                .into_iter()
                .map(|b| b.username)
                .collect::<Vec<_>>(),
            vec!["alice-agent".to_string()]
        );
    }

    #[tokio::test]
    async fn a_bot_cannot_own_a_bot_and_names_stay_unique() {
        let db = db().await;
        let alice = human(&db, "alice").await;
        let bot = create_bot(&db, alice.id, "alice-agent", None)
            .await
            .unwrap();

        let nested = create_bot(&db, bot.id, "agent-of-agent", None)
            .await
            .unwrap_err();
        assert!(status(&nested).contains("a bot cannot create bots"));

        let taken = create_bot(&db, alice.id, "alice", None).await.unwrap_err();
        assert!(status(&taken).contains("already taken"));

        let reserved = create_bot(&db, alice.id, "settings", None)
            .await
            .unwrap_err();
        assert!(status(&reserved).contains("reserved"), "{reserved:#}");
    }

    #[tokio::test]
    async fn only_the_owner_finds_the_bot_and_it_dies_with_the_owner() {
        let db = db().await;
        let alice = human(&db, "alice").await;
        let mallory = human(&db, "mallory").await;
        let bot = create_bot(&db, alice.id, "alice-agent", None)
            .await
            .unwrap();

        assert!(find_owned_bot(&db, alice.id, "alice-agent")
            .await
            .unwrap()
            .is_some());
        assert!(find_owned_bot(&db, mallory.id, "alice-agent")
            .await
            .unwrap()
            .is_none());
        assert!(find_owned_bot(&db, alice.id, "alice")
            .await
            .unwrap()
            .is_none());

        assert!(owner_is_usable(&db, &bot).await.unwrap());
        user_ops::update_by_id(&db, alice.id, None, None, None, Some(false))
            .await
            .unwrap();
        assert!(!owner_is_usable(&db, &bot).await.unwrap());
        assert!(owner_is_usable(&db, &mallory).await.unwrap());
    }
}
