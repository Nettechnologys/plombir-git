//! Database operations for users.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::user::{self, ActiveModel, Entity as UserEntity, Model as User};

/// Find a user by username (case-insensitive on SQLite).
pub async fn find_by_username(db: &DatabaseConnection, username: &str) -> Result<Option<User>> {
    UserEntity::find()
        .filter(user::Column::Username.eq(username))
        .one(db)
        .await
        .context("db: find user by username")
}

/// Find a user by username, ignoring one already claimed for retirement.
///
/// The lookup that decides whether a *new* thing may join the account's
/// namespace — creating or importing a repository into it, above all — has to
/// use this one rather than [`find_by_username`]: an account whose storage is
/// being retired is no longer a namespace anything may enter, and
/// `repositories.owner_id` is `ON DELETE CASCADE`, so a row that lands there
/// anyway is not merely orphaned but destroyed.
///
/// Deliberately keyed on `deleted_at`, not on `is_active`: deactivation is an
/// orthogonal toggle that says nothing about whether the namespace still
/// exists, and an organization whose owner is deactivated still accepts
/// repositories.
pub async fn find_active_by_username(
    db: &DatabaseConnection,
    username: &str,
) -> Result<Option<User>> {
    UserEntity::find()
        .filter(user::Column::Username.eq(username))
        .filter(user::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find active user by username")
}

/// Claim an account for retirement, reporting whether this call claimed it.
///
/// `deleted_at` is the account's retirement marker: set while the storage of
/// its repositories is being retired, and gone together with the row itself
/// once it is. Deleting an account spans storage no database transaction can
/// hold, so the row cannot simply disappear at the end of one — something has
/// to say "this namespace is closing" for the whole span, and this is it. The
/// column already denies every credential through
/// [`Model::is_usable`](crate::entities::user::Model::is_usable), which is the
/// behaviour a closing account wants anyway.
///
/// One conditional statement, and the row count is the answer: `false` means
/// the account is already being retired by somebody else (or is already gone),
/// so this caller does not own the deletion and must not start retiring storage
/// a concurrent deleter is also retiring.
pub async fn begin_user_retirement(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let now = chrono::Utc::now();
    let result = UserEntity::update_many()
        .col_expr(user::Column::DeletedAt, Expr::value(Some(now)))
        .col_expr(user::Column::UpdatedAt, Expr::value(now))
        .filter(user::Column::Id.eq(id))
        .filter(user::Column::DeletedAt.is_null())
        .exec(db)
        .await
        .context("db: begin user retirement")?;
    Ok(result.rows_affected > 0)
}

/// Release a retirement claim whose deletion could not finish.
///
/// A deletion that fails half-way leaves the account and its unretired
/// repositories exactly where they were, so the marker has to come off too —
/// otherwise a retryable failure would leave an account nobody can create in,
/// nobody can log in as, and no request can reopen.
pub async fn abort_user_retirement(db: &DatabaseConnection, id: i64) -> Result<()> {
    UserEntity::update_many()
        .col_expr(
            user::Column::DeletedAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .filter(user::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: abort user retirement")?;
    Ok(())
}

/// Whether an account is still open for new members of its namespace.
///
/// `false` covers both "claimed for retirement" and "already gone": to anything
/// asking whether it may still join this namespace the two are the same answer.
/// As with [`find_active_by_username`], this is not `is_active`.
pub async fn user_namespace_is_open(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let count = UserEntity::find()
        .filter(user::Column::Id.eq(id))
        .filter(user::Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: check user retirement state")?;
    Ok(count > 0)
}

/// Find a user by email.
pub async fn find_by_email(db: &DatabaseConnection, email: &str) -> Result<Option<User>> {
    UserEntity::find()
        .filter(user::Column::Email.eq(email))
        .one(db)
        .await
        .context("db: find user by email")
}

/// Find a directory identity by the provider and stable LDAP uid that own it.
///
/// The caller must not substitute a username for this pair: usernames are
/// account-local labels and can legitimately belong to an unrelated user.
pub async fn find_by_ldap_provider_and_uid(
    db: &DatabaseConnection,
    ldap_provider_id: i64,
    ldap_uid: &str,
) -> Result<Option<User>> {
    UserEntity::find()
        .filter(user::Column::AuthProvider.eq("ldap"))
        .filter(user::Column::LdapProviderId.eq(ldap_provider_id))
        .filter(user::Column::LdapUid.eq(ldap_uid))
        .one(db)
        .await
        .context("db: find user by LDAP provider and uid")
}

/// Find a user by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<User>> {
    UserEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find user by id")
}

/// Load several accounts at once, for a listing that has to name them.
///
/// A membership row carries a `user_id` and nothing else, so a list of members
/// rendered from those rows alone can only say "User #3". Resolving the names
/// one row at a time would put a query per member on the page; this is the one
/// round-trip that lets the caller build an id → account map before it maps
/// over the rows.
///
/// An id matching nothing is simply absent from the result — the caller decides
/// what an unnamed row looks like, since dropping the row would hide a
/// membership that really is there.
pub async fn find_by_ids(db: &DatabaseConnection, ids: &[i64]) -> Result<Vec<User>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    UserEntity::find()
        .filter(user::Column::Id.is_in(ids.iter().copied()))
        .all(db)
        .await
        .context("db: find users by id")
}

/// Revoke every bearer session issued for a user before this call.
///
/// Password resets and logout deliberately share this primitive: a timestamp
/// cannot order a JWT's second-resolution `iat` against a database timestamp
/// without leaving a one-second survivor or rejecting the replacement token.
/// The monotonically increasing version is exact and the expression keeps
/// concurrent revocations from losing one another's update.
///
/// Personal access tokens and SSH keys are separate, durable credentials. They
/// intentionally survive a password change; account deactivation remains the
/// explicit operation that revokes every credential type at once.
pub async fn invalidate_sessions(db: &DatabaseConnection, user_id: i64) -> Result<User> {
    let result = UserEntity::update_many()
        .col_expr(
            user::Column::SessionVersion,
            Expr::col(user::Column::SessionVersion).add(1),
        )
        .col_expr(user::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
        .filter(user::Column::Id.eq(user_id))
        .exec(db)
        .await
        .context("db: revoke user sessions")?;
    if result.rows_affected == 0 {
        anyhow::bail!("user {} not found", user_id);
    }
    find_by_id(db, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found after session revocation", user_id))
}

pub async fn count_by_ldap_provider(db: &DatabaseConnection, provider_id: i64) -> Result<u64> {
    UserEntity::find()
        .filter(user::Column::LdapProviderId.eq(provider_id))
        .count(db)
        .await
        .context("db: count users by LDAP provider")
}

/// Count active (non-deleted) users — backs the `plombir_git_users` gauge.
pub async fn count_active(db: &DatabaseConnection) -> Result<u64> {
    UserEntity::find()
        .filter(user::Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: count active users")
}

/// Whether this instance has ever had a user row, tombstones included.
///
/// Bootstrap authorization runs on every self-registration, including open
/// instances with a large account table. Fetching one primary key keeps that
/// check constant-work instead of turning it into a full-table `COUNT(*)`.
pub async fn has_any(db: &DatabaseConnection) -> Result<bool> {
    UserEntity::find()
        .select_only()
        .column(user::Column::Id)
        .limit(1)
        .into_tuple::<i64>()
        .one(db)
        .await
        .map(|id| id.is_some())
        .context("db: check whether any user exists")
}

/// List all users with pagination.
///
/// `offset` is a row offset, and the slicing is spelled out with
/// `.offset().limit()` — as every neighbour in this module does — rather than
/// through `Paginator::fetch_page`. That is not a style preference: `fetch_page`
/// takes a 0-based *page index* and builds `OFFSET page_size * page` itself, so
/// this parameter (which every caller fills from `PaginationParams::offset()`)
/// was being multiplied by the page size a second time. The real SQL offset came
/// out as `per_page² × (page − 1)`: with 25 users and `per_page = 20`,
/// `GET /api/v1/admin/users?page=2` asked for row 400, answered `200` with an
/// empty `data`, and still reported `total = 25` and `total_pages = 2`
/// (card_1e3c1cff05b4).
pub async fn list_users(
    db: &DatabaseConnection,
    offset: u64,
    limit: u64,
) -> Result<(Vec<User>, i64)> {
    let total = UserEntity::find()
        .count(db)
        .await
        .context("db: count users")?;
    let users = UserEntity::find()
        .order_by_desc(user::Column::CreatedAt)
        .order_by_desc(user::Column::Id)
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list users")?;

    Ok((users, total as i64))
}

/// Create a new user and return the persisted model.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<User> {
    model.insert(db).await.context("db: create user")
}

///
/// Update the mutable admin-owned fields of an account that is still open.
///
/// A user row remains physically present while repository storage is retired,
/// but `deleted_at` closes it to new mutations. The conditional statement is
/// therefore keyed by both the stable id and the retirement marker; a concurrent
/// physical DELETE is the same `None` outcome. The verification read is in the
/// same retryable transaction so a later retirement cannot turn a committed
/// admin update into a false 404, and it also distinguishes a MySQL no-op update
/// from an absent row.
pub async fn update_by_id(
    db: &DatabaseConnection,
    id: i64,
    display_name: Option<Option<String>>,
    bio: Option<Option<String>>,
    is_admin: Option<bool>,
    is_active: Option<bool>,
) -> Result<Option<User>> {
    let display_name = &display_name;
    let bio = &bio;
    let is_admin = &is_admin;
    let is_active = &is_active;
    crate::contention::retry_transaction("admin user update", || async move {
        let transaction = db.begin().await.context("db: begin admin user update")?;
        let result: Result<Option<User>> = async {
            let mut update = UserEntity::update_many();
            let mut has_changes = false;
            if let Some(display_name) = display_name {
                update =
                    update.col_expr(user::Column::DisplayName, Expr::value(display_name.clone()));
                has_changes = true;
            }
            if let Some(bio) = bio {
                update = update.col_expr(user::Column::Bio, Expr::value(bio.clone()));
                has_changes = true;
            }
            if let Some(is_admin) = is_admin {
                update = update.col_expr(user::Column::IsAdmin, Expr::value(*is_admin));
                has_changes = true;
            }
            if let Some(is_active) = is_active {
                update = update.col_expr(user::Column::IsActive, Expr::value(*is_active));
                has_changes = true;
            }

            let rows_affected = if has_changes {
                update
                    .filter(user::Column::Id.eq(id))
                    .filter(user::Column::DeletedAt.is_null())
                    .exec(&transaction)
                    .await
                    .context("db: update user by admin")?
                    .rows_affected
            } else {
                0
            };
            open_user_after_update(&transaction, id, rows_affected, "admin user update").await
        }
        .await;
        match result {
            Ok(updated) => {
                transaction
                    .commit()
                    .await
                    .context("db: commit admin user update")?;
                Ok(updated)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error)
                        .context(format!("db: roll back admin user update: {rollback_error}"));
                }
                Err(error)
            }
        }
    })
    .await
}

/// Create an `oauth2` account row with high-level parameters, and nothing else.
///
/// A first SSO sign-in does not come through here: it needs the account and its
/// identity link in one transaction, which is
/// `oauth_account_ops::link_with_new_user`. This is the fixture spelling.
pub async fn create_user(
    db: &DatabaseConnection,
    username: &str,
    email: &str,
    password_hash: &str,
    display_name: &str,
) -> Result<User> {
    create(
        db,
        oauth_user_model(username, email, password_hash, display_name),
    )
    .await
}

/// The row [`create_user`] inserts — `auth_provider = "oauth2"`, no second
/// factor, nothing locked.
///
/// Shared with `oauth_account_ops::link_with_new_user`, which has to insert the
/// same row inside the transaction that also writes the account's first
/// external-identity link.
pub(crate) fn oauth_user_model(
    username: &str,
    email: &str,
    password_hash: &str,
    display_name: &str,
) -> ActiveModel {
    use crate::entities::user;
    let now = chrono::Utc::now();
    user::ActiveModel {
        id: NotSet,
        username: Set(username.to_string()),
        email: Set(email.to_string()),
        password_hash: Set(if password_hash.is_empty() {
            "".into()
        } else {
            password_hash.to_string()
        }),
        display_name: Set(if display_name.is_empty() {
            None
        } else {
            Some(display_name.to_string())
        }),
        avatar_url: Set(None),
        bio: Set(None),
        is_admin: Set(false),
        is_active: Set(true),
        auth_provider: Set("oauth2".into()),
        ldap_uid: Set(None),
        ldap_provider_id: Set(None),
        totp_secret: Set(None),
        pending_totp_secret: Set(None),
        pending_totp_secret_at: Set(None),
        mfa_enabled: Set(false),
        totp_last_step: Set(None),
        last_login_at: Set(None),
        login_attempts: Set(0),
        locked_until: Set(None),
        session_version: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
        bot_owner_id: Set(None),
        password_change_required: Set(false),
        email_verified_at: Set(None),
    }
}

/// Create a bot account owned by `owner_id`.
///
/// A bot has no password and no external identity: `auth_provider = "bot"`
/// is a provider no login form reaches, so the only credential it can ever
/// present is a Personal Access Token its owner minted for it. It is never an
/// administrator, whatever the owner is.
pub async fn create_bot(
    db: &DatabaseConnection,
    owner_id: i64,
    username: &str,
    email: &str,
    display_name: Option<&str>,
) -> Result<User> {
    let now = chrono::Utc::now();
    create(
        db,
        user::ActiveModel {
            id: NotSet,
            username: Set(username.to_string()),
            email: Set(email.to_string()),
            password_hash: Set(String::new()),
            display_name: Set(display_name.map(str::to_string)),
            avatar_url: Set(None),
            bio: Set(None),
            is_admin: Set(false),
            is_active: Set(true),
            auth_provider: Set(BOT_AUTH_PROVIDER.into()),
            ldap_uid: Set(None),
            ldap_provider_id: Set(None),
            totp_secret: Set(None),
            pending_totp_secret: Set(None),
            pending_totp_secret_at: Set(None),
            mfa_enabled: Set(false),
            totp_last_step: Set(None),
            last_login_at: Set(None),
            login_attempts: Set(0),
            locked_until: Set(None),
            session_version: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            bot_owner_id: Set(Some(owner_id)),
            password_change_required: Set(false),
            email_verified_at: Set(None),
        },
    )
    .await
}

/// `users.auth_provider` of a bot account — a provider no login form reaches.
pub const BOT_AUTH_PROVIDER: &str = "bot";

/// The bots a person owns, oldest first.
pub async fn list_bots_by_owner(db: &DatabaseConnection, owner_id: i64) -> Result<Vec<User>> {
    UserEntity::find()
        .filter(user::Column::BotOwnerId.eq(owner_id))
        .order_by_asc(user::Column::CreatedAt)
        .order_by_asc(user::Column::Id)
        .all(db)
        .await
        .context("db: list bots by owner")
}

/// Create a directory-backed user after successful LDAP authentication.
pub async fn create_ldap_user(
    db: &DatabaseConnection,
    ldap_provider_id: i64,
    username: &str,
    email: &str,
    display_name: Option<&str>,
    ldap_uid: Option<&str>,
) -> Result<User> {
    let now = chrono::Utc::now();
    create(
        db,
        user::ActiveModel {
            id: NotSet,
            username: Set(username.to_string()),
            email: Set(email.to_string()),
            password_hash: Set(String::new()),
            display_name: Set(display_name.map(str::to_string)),
            avatar_url: Set(None),
            bio: Set(None),
            is_admin: Set(false),
            is_active: Set(true),
            auth_provider: Set("ldap".into()),
            ldap_uid: Set(ldap_uid.map(str::to_string)),
            ldap_provider_id: Set(Some(ldap_provider_id)),
            totp_secret: Set(None),
            pending_totp_secret: Set(None),
            pending_totp_secret_at: Set(None),
            mfa_enabled: Set(false),
            totp_last_step: Set(None),
            last_login_at: Set(None),
            login_attempts: Set(0),
            locked_until: Set(None),
            session_version: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
            bot_owner_id: Set(None),
            password_change_required: Set(false),
            email_verified_at: Set(None),
        },
    )
    .await
}

/// Refresh non-authoritative LDAP identity metadata after a successful bind.
/// Email is deliberately not changed here because it is globally unique and
/// may require an administrator to resolve a directory collision.
///
/// `None` means the account was claimed for retirement, deleted, or rebound to
/// another LDAP provider after the caller resolved it. The already-observed
/// identity must never turn that outcome into a fresh provision.
pub async fn sync_ldap_identity(
    db: &DatabaseConnection,
    user_id: i64,
    ldap_provider_id: i64,
    display_name: Option<&str>,
    ldap_uid: Option<&str>,
) -> Result<Option<User>> {
    let provider_is_compatible = Condition::any()
        .add(user::Column::LdapProviderId.eq(ldap_provider_id))
        // Accounts created before provider identity was persisted are adopted
        // only after the login path has proved there is exactly one candidate.
        .add(user::Column::LdapProviderId.is_null());
    let result = UserEntity::update_many()
        .col_expr(
            user::Column::DisplayName,
            Expr::value(display_name.map(str::to_string)),
        )
        .col_expr(
            user::Column::LdapUid,
            Expr::value(ldap_uid.map(str::to_string)),
        )
        .col_expr(
            user::Column::LdapProviderId,
            Expr::value(Some(ldap_provider_id)),
        )
        .col_expr(user::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
        .filter(user::Column::Id.eq(user_id))
        .filter(user::Column::AuthProvider.eq("ldap"))
        .filter(user::Column::DeletedAt.is_null())
        .filter(provider_is_compatible)
        .exec(db)
        .await
        .context("db: sync LDAP identity")?;

    match result.rows_affected {
        // MySQL can report zero for an unchanged UPDATE. The scoped re-read is
        // the portable distinction between that and a row which disappeared.
        0 | 1 => UserEntity::find()
            .filter(user::Column::Id.eq(user_id))
            .filter(user::Column::AuthProvider.eq("ldap"))
            .filter(user::Column::LdapProviderId.eq(ldap_provider_id))
            .filter(user::Column::DeletedAt.is_null())
            .one(db)
            .await
            .context("db: find user after LDAP identity sync"),
        rows => anyhow::bail!("db: LDAP identity sync affected {rows} rows for user {user_id}"),
    }
}

async fn open_user_after_update<C>(
    db: &C,
    user_id: i64,
    rows_affected: u64,
    operation: &str,
) -> Result<Option<User>>
where
    C: ConnectionTrait,
{
    match rows_affected {
        0 | 1 => UserEntity::find()
            .filter(user::Column::Id.eq(user_id))
            .filter(user::Column::DeletedAt.is_null())
            .one(db)
            .await
            .with_context(|| format!("db: find user after {operation}")),
        rows => anyhow::bail!("db: {operation} affected {rows} rows for user {user_id}"),
    }
}

/// Stage the TOTP secret of an enrolment in progress, for an account that is
/// still open.
///
/// Deliberately not a write to `totp_secret`: that column is what the login
/// path verifies against, and this call happens on the *preparatory* step, when
/// nothing has proved that anybody holds the new secret. Overwriting the live
/// one here left an account with `mfa_enabled = true` against a secret no
/// authenticator had — the owner had only to open the wizard and walk away
/// (card_08400088bb40). [`enable_mfa_with_backup_codes`] promotes what is
/// staged here, and only after a code computed from it has been presented.
///
/// The HTTP setup path has already read the user to build the authenticator
/// label. A concurrent account deletion can claim the row in between by setting
/// `deleted_at`, or remove it entirely. `None` keeps both ordinary outcomes out
/// of SeaORM's backend-shaped `RecordNotUpdated` error and prevents setup from
/// handing out a secret that was not stored.
pub async fn stage_pending_totp_secret(
    db: &DatabaseConnection,
    user_id: i64,
    encrypted_secret: &str,
) -> Result<Option<User>> {
    stage_pending_totp_secret_with_after_read(db, user_id, encrypted_secret, || {
        std::future::ready(Ok(()))
    })
    .await
}

/// Test seam for the read in `POST /users/mfa/setup` that precedes this write.
async fn stage_pending_totp_secret_with_after_read<F, Fut>(
    db: &DatabaseConnection,
    user_id: i64,
    encrypted_secret: &str,
    after_read: F,
) -> Result<Option<User>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let after_read = &after_read;
    crate::contention::retry_transaction("stage pending TOTP secret", || async move {
        after_read().await?;
        let transaction = db.begin().await.context("db: begin TOTP secret staging")?;
        let result: Result<Option<User>> = async {
            let update = UserEntity::update_many()
                .col_expr(
                    user::Column::PendingTotpSecret,
                    Expr::value(Some(encrypted_secret.to_string())),
                )
                .col_expr(
                    user::Column::PendingTotpSecretAt,
                    Expr::value(Some(chrono::Utc::now())),
                )
                .col_expr(user::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
                .filter(user::Column::Id.eq(user_id))
                .filter(user::Column::DeletedAt.is_null())
                .exec(&transaction)
                .await
                .context("db: stage pending TOTP secret")?;
            open_user_after_update(
                &transaction,
                user_id,
                update.rows_affected,
                "TOTP secret staging",
            )
            .await
        }
        .await;
        match result {
            Ok(updated) => {
                transaction
                    .commit()
                    .await
                    .context("db: commit TOTP secret staging")?;
                Ok(updated)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!(
                        "db: roll back TOTP secret staging: {rollback_error}"
                    ));
                }
                Err(error)
            }
        }
    })
    .await
}

/// Enable MFA for a user.
///
/// Generic over the connection so it can run inside a caller's transaction.
/// Enrolment must go through [`enable_mfa_with_backup_codes`] rather than call
/// this directly — on its own it publishes a second factor whose recovery set is
/// still a separate commit away.
pub async fn enable_mfa<C>(db: &C, user_id: i64) -> Result<User>
where
    C: ConnectionTrait,
{
    enable_mfa_with_after_read(db, user_id, || std::future::ready(Ok(())))
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))
}

/// The read-before-write half of MFA enrolment.
///
/// `after_read` is a private test seam. Production passes a ready future; the
/// contention regression commits another connection after the transaction has
/// taken its user snapshot and before its first write.
async fn enable_mfa_with_after_read<C, F, Fut>(
    db: &C,
    user_id: i64,
    after_read: F,
) -> Result<Option<User>>
where
    C: ConnectionTrait,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let model = UserEntity::find()
        .filter(user::Column::Id.eq(user_id))
        .filter(user::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: find user for MFA enable")?;
    if model.is_none() {
        return Ok(None);
    }
    after_read().await?;
    let result = UserEntity::update_many()
        .col_expr(user::Column::MfaEnabled, Expr::value(true))
        .col_expr(user::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
        .filter(user::Column::Id.eq(user_id))
        .filter(user::Column::DeletedAt.is_null())
        .exec(db)
        .await
        .context("db: enable MFA")?;
    open_user_after_update(db, user_id, result.rows_affected, "MFA enable").await
}

/// Turn the second factor on and publish its backup-code set in one commit.
///
/// Enrolment used to be two independent commits with the codes carried back by
/// the response, and the order made the failure worse than a rollback: the flag
/// landed first, so a failing code write left the account with a second factor
/// and its owner with no recovery material — under a `500` that told them the
/// enrolment had not happened. Both writes land or neither does, so the answer
/// the caller receives is the state that is stored.
///
/// `codes` are the plaintext codes the response will show; only their hashes are
/// stored (see [`crate::ops::mfa_backup_code_ops::set_codes`]).
pub async fn enable_mfa_with_backup_codes(
    db: &DatabaseConnection,
    user_id: i64,
    codes: &[String],
) -> Result<Option<User>> {
    enable_mfa_with_backup_codes_with_after_read(db, user_id, codes, || std::future::ready(Ok(())))
        .await
}

/// Move a staged enrolment secret into the column the login path verifies
/// against, inside the caller's transaction.
///
/// Conditional in SQL rather than read-then-write, for the two reasons that
/// matter here: the copy cannot lose a secret staged between the read and the
/// write, and an enrolment with nothing staged — which is what an account
/// enabling MFA through a path that never called `setup` looks like — leaves
/// `totp_secret` alone instead of nulling the live factor. A row that had no
/// pending secret is therefore untouched, and `rows_affected` is not an error
/// condition.
async fn promote_pending_totp_secret<C>(db: &C, user_id: i64) -> Result<()>
where
    C: ConnectionTrait,
{
    UserEntity::update_many()
        .col_expr(
            user::Column::TotpSecret,
            Expr::col(user::Column::PendingTotpSecret).into(),
        )
        .col_expr(
            user::Column::PendingTotpSecret,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            user::Column::PendingTotpSecretAt,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        // The spent-step marker belongs to the secret that is being retired. A
        // code from the *new* authenticator is not a replay of anything, so
        // carrying the marker over would refuse the first code of a rotation for
        // up to 30 seconds — a wrong answer, and one that arrives right after
        // the owner has changed how they get in.
        .col_expr(user::Column::TotpLastStep, Expr::value(Option::<i64>::None))
        .col_expr(user::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
        .filter(user::Column::Id.eq(user_id))
        .filter(user::Column::DeletedAt.is_null())
        .filter(user::Column::PendingTotpSecret.is_not_null())
        .exec(db)
        .await
        .context("db: promote the pending TOTP secret")?;
    Ok(())
}

/// The retryable transaction behind [`enable_mfa_with_backup_codes`].
async fn enable_mfa_with_backup_codes_with_after_read<F, Fut>(
    db: &DatabaseConnection,
    user_id: i64,
    codes: &[String],
    after_read: F,
) -> Result<Option<User>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let after_read = &after_read;
    crate::contention::retry_transaction("enable MFA", || async move {
        let transaction = db.begin().await.context("db: begin MFA enrolment")?;
        let result: Result<Option<User>> = async {
            let Some(user) = enable_mfa_with_after_read(&transaction, user_id, after_read).await?
            else {
                return Ok(None);
            };
            // After the read seam, deliberately: the first statement of this
            // transaction has to stay the user read the contention tests take
            // their snapshot at, and a write issued ahead of it takes a lock
            // that turns a competing commit into `database is locked` instead
            // of the retry this transaction is built to answer with. Same
            // commit either way — `user` above is the pre-promotion snapshot,
            // and nothing reads its secret.
            promote_pending_totp_secret(&transaction, user_id).await?;
            crate::ops::mfa_backup_code_ops::set_codes(&transaction, user_id, codes)
                .await
                .context("db: store MFA backup codes")?;
            Ok(Some(user))
        }
        .await;
        match result {
            Ok(user) => {
                transaction
                    .commit()
                    .await
                    .context("db: commit MFA enrolment")?;
                Ok(user)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error)
                        .context(format!("db: roll back MFA enrolment: {rollback_error}"));
                }
                Err(error)
            }
        }
    })
    .await
}

/// Spend a TOTP time step, reporting whether this call is the one that spent it.
///
/// A compare-and-swap, not a write: the `WHERE` names the exact state the caller
/// believed it was acting on — an account that has never spent a step, or has
/// spent an *older* one — so the database picks the winner in one statement.
/// The predecessor was nothing at all. `verify_code` is a pure function of the
/// secret and the clock, so with `skew = 1` over a 30-second step an intercepted
/// code passed the second factor for as long as it stayed inside its ~90-second
/// window, and two concurrent `POST /users/mfa/verify` carrying one code were
/// answered two sessions. Same shape as
/// [`crate::ops::mfa_backup_code_ops::verify_and_consume`] and
/// [`crate::ops::password_reset_token_ops::consume`], for the same reason: the
/// backup code got its `used` column, and the TOTP step had no state to hold at
/// all.
///
/// The comparison is strictly monotonic (`<`, not `!=`), which also closes the
/// other half of the window: after step *n* is spent, a code from *n-1* that is
/// still inside the skew window is refused rather than accepted as "a different
/// step". A clock that walks backwards therefore costs an honest holder a
/// 30-second wait — the same tradeoff `session_version` makes, and the safe side
/// of it.
///
/// `rows_affected` is a safe answer on every backend here because the `WHERE`
/// guarantees the row it matches really changes (`totp_last_step` goes `NULL` or
/// something smaller → `step`), so MySQL's *changed* rows count and
/// PostgreSQL/SQLite's *matched* rows count agree.
///
/// `false` covers both "already spent" and "no such user" — deliberately: the
/// caller must answer it with the same `401 invalid TOTP code` a wrong code
/// gets, or a distinguishable reply tells whoever replayed the code that it was
/// genuine and merely late.
pub async fn consume_totp_step(
    db: &DatabaseConnection,
    user_id: i64,
    step: u64,
) -> Result<bool, DbErr> {
    // The column is a signed 64-bit integer on every backend; a `u64` step is
    // seconds-since-epoch divided by 30, so the cast cannot lose a bit this side
    // of the year 292-billion.
    let step = step as i64;
    let result = UserEntity::update_many()
        .col_expr(user::Column::TotpLastStep, Expr::value(step))
        .filter(user::Column::Id.eq(user_id))
        .filter(
            Condition::any()
                .add(user::Column::TotpLastStep.is_null())
                .add(user::Column::TotpLastStep.lt(step)),
        )
        .exec(db)
        .await?;
    Ok(result.rows_affected == 1)
}

/// Disable MFA for an account that is still open.
pub async fn disable_mfa(db: &DatabaseConnection, user_id: i64) -> Result<Option<User>> {
    disable_mfa_with_after_read(db, user_id, || std::future::ready(Ok(()))).await
}

/// The retryable transaction behind [`disable_mfa`].
///
/// `after_read` is a private test seam. Production passes a ready future; the
/// contention regression commits another connection after this transaction has
/// taken its read snapshot and before its first write.
async fn disable_mfa_with_after_read<F, Fut>(
    db: &DatabaseConnection,
    user_id: i64,
    after_read: F,
) -> Result<Option<User>>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let after_read = &after_read;
    crate::contention::retry_transaction("disable MFA", || async move {
        let transaction = db.begin().await.context("db: begin MFA removal")?;
        let result: Result<Option<User>> = async {
            let model = UserEntity::find()
                .filter(user::Column::Id.eq(user_id))
                .filter(user::Column::DeletedAt.is_null())
                .one(&transaction)
                .await
                .context("db: find user for MFA disable")?;
            if model.is_none() {
                return Ok(None);
            }
            after_read().await?;
            let result = UserEntity::update_many()
                .col_expr(user::Column::MfaEnabled, Expr::value(false))
                .col_expr(
                    user::Column::TotpSecret,
                    Expr::value(Option::<String>::None),
                )
                // An enrolment that was half-finished when the factor came off
                // goes with it. Leaving it staged would let a code from a QR
                // scanned before the removal arm the account again, through an
                // `enable` that then has no live factor to ask a password for.
                .col_expr(
                    user::Column::PendingTotpSecret,
                    Expr::value(Option::<String>::None),
                )
                .col_expr(
                    user::Column::PendingTotpSecretAt,
                    Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
                )
                .col_expr(user::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
                .filter(user::Column::Id.eq(user_id))
                .filter(user::Column::DeletedAt.is_null())
                .exec(&transaction)
                .await
                .context("db: disable MFA")?;
            let Some(user) =
                open_user_after_update(&transaction, user_id, result.rows_affected, "MFA disable")
                    .await?
            else {
                return Ok(None);
            };

            // The unused backup codes go with the factor they recover. Each one is a
            // full bypass of the second factor, so leaving them behind keeps a live
            // credential for something the owner has just asked to remove — and
            // `GET /users/mfa/backup` went on reporting them as ten usable codes on
            // an account with no second factor at all (card_0a4c00fd1b89).
            //
            // Used codes are history rather than credentials and stay, which is the
            // same line `set_codes` already draws; passing an empty set is its
            // documented spelling of "revoke every unused code". In the same
            // transaction as the flag, for the reason enrolment is: the two halves
            // must not be separately observable, or a failure between them leaves an
            // account whose recovery material outlives the factor by exactly as long
            // as nobody notices.
            crate::ops::mfa_backup_code_ops::set_codes(&transaction, user_id, &[])
                .await
                .context("db: revoke MFA backup codes")?;
            Ok(Some(user))
        }
        .await;
        match result {
            Ok(user) => {
                transaction
                    .commit()
                    .await
                    .context("db: commit MFA removal")?;
                Ok(user)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error)
                        .context(format!("db: roll back MFA removal: {rollback_error}"));
                }
                Err(error)
            }
        }
    })
    .await
}

#[derive(Clone, Copy)]
enum AuthenticationFinalization {
    /// The password or directory bind succeeded. Whether that completes login
    /// is decided from the row in the UPDATE itself: an account that currently
    /// carries MFA gets only its primary-factor failures cleared.
    PrimaryFactor,
    /// A factor which completes authentication succeeded.
    Completed,
    /// A non-interactive password door succeeded. Clear its strikes, but do not
    /// claim that a human browser login completed.
    FailuresOnly,
}

async fn finalize_authentication_state_if_open(
    db: &DatabaseConnection,
    user_id: i64,
    finalization: AuthenticationFinalization,
) -> Result<Option<User>> {
    let operation = match finalization {
        AuthenticationFinalization::PrimaryFactor => "primary login",
        AuthenticationFinalization::Completed => "completed login",
        AuthenticationFinalization::FailuresOnly => "login-failure reset",
    };
    crate::contention::retry_transaction(operation, || async move {
        let transaction = db
            .begin()
            .await
            .with_context(|| format!("db: begin {operation}"))?;
        let result: Result<Option<User>> = async {
            let now = chrono::Utc::now();
            let update = match finalization {
                // A new primary-factor challenge must not erase failures of the
                // second factor. Otherwise four wrong TOTP codes followed by a
                // fresh password challenge reset the shared counter to zero and
                // the fifth never locks the account. The same statement still
                // clears password failures and records completion when MFA is
                // currently off.
                AuthenticationFinalization::PrimaryFactor => UserEntity::update_many()
                    .col_expr(user::Column::UpdatedAt, Expr::value(now))
                    .col_expr(
                        user::Column::LoginAttempts,
                        Expr::case(
                            Expr::col(user::Column::MfaEnabled).eq(false),
                            Expr::value(0),
                        )
                        .finally(Expr::col(user::Column::LoginAttempts))
                        .into(),
                    )
                    .col_expr(
                        user::Column::LockedUntil,
                        Expr::case(
                            Expr::col(user::Column::MfaEnabled).eq(false),
                            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
                        )
                        .finally(Expr::col(user::Column::LockedUntil))
                        .into(),
                    )
                    .col_expr(
                        user::Column::LastLoginAt,
                        Expr::case(
                            Expr::col(user::Column::MfaEnabled).eq(false),
                            Expr::value(Some(now)),
                        )
                        .finally(Expr::col(user::Column::LastLoginAt))
                        .into(),
                    ),
                AuthenticationFinalization::Completed => UserEntity::update_many()
                    .col_expr(user::Column::UpdatedAt, Expr::value(now))
                    .col_expr(user::Column::LoginAttempts, Expr::value(0))
                    .col_expr(
                        user::Column::LockedUntil,
                        Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
                    )
                    .col_expr(user::Column::LastLoginAt, Expr::value(Some(now))),
                AuthenticationFinalization::FailuresOnly => UserEntity::update_many()
                    .col_expr(user::Column::UpdatedAt, Expr::value(now))
                    .col_expr(user::Column::LoginAttempts, Expr::value(0))
                    .col_expr(
                        user::Column::LockedUntil,
                        Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
                    ),
            };
            let update = update
                .filter(user::Column::Id.eq(user_id))
                .filter(user::Column::DeletedAt.is_null())
                .exec(&transaction)
                .await
                .with_context(|| format!("db: {operation}"))?;
            open_user_after_update(&transaction, user_id, update.rows_affected, operation).await
        }
        .await;
        match result {
            Ok(updated) => {
                transaction
                    .commit()
                    .await
                    .with_context(|| format!("db: commit {operation}"))?;
                Ok(updated)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error)
                        .context(format!("db: roll back {operation}: {rollback_error}"));
                }
                Err(error)
            }
        }
    })
    .await
}

/// Finalize a password or LDAP first factor while the account remains open.
///
/// The MFA flag is read by the same conditional UPDATE which resets the
/// failures. A non-MFA account records a completed login; an MFA account does
/// not advance `last_login_at` until its second factor succeeds. The returned
/// row is the fresh source of both that decision and the session generation.
pub async fn finalize_primary_login(db: &DatabaseConnection, user_id: i64) -> Result<Option<User>> {
    finalize_authentication_state_if_open(db, user_id, AuthenticationFinalization::PrimaryFactor)
        .await
}

/// Record a completed login only while the account remains open.
///
/// `None` is a typed lifecycle outcome: retirement or physical deletion won
/// after the credential was verified. Callers must stop before publishing a
/// success audit, login-log row, challenge, or session.
pub async fn record_successful_login(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Option<User>> {
    finalize_authentication_state_if_open(db, user_id, AuthenticationFinalization::Completed).await
}

/// Reset failures only while the account remains open.
///
/// `None` covers both a retirement claim and a physical DELETE. The update and
/// its verification read share one retryable transaction so a successful reset
/// cannot be reported absent merely because retirement started immediately
/// after it, while a MySQL unchanged-update result still re-reads as success.
pub async fn reset_login_failures_if_open(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Option<User>> {
    finalize_authentication_state_if_open(db, user_id, AuthenticationFinalization::FailuresOnly)
        .await
}

/// Finalize a standing credential or derived-capability proof while its owner
/// remains open.
///
/// PAT, SSH-key and LFS action-URL verification, and the job-log socket's
/// periodic re-check, happen before an authenticated continuation is
/// published, and the owner may be retired or
/// deleted in between. This is the fresh read of the owner that decides it:
/// `None` is retirement or physical deletion, a database failure is still
/// `Err`, and nothing about the account changes.
///
/// It is a read on purpose (card_b83b9bc36e3a). It used to be a no-op
/// `UPDATE … SET session_version = session_version` so it would take the row
/// lock `begin_user_retirement` takes — which made every `git clone`, LFS
/// object, registry layer and bot call a write transaction serialised on
/// SQLite's single writer. The lock bought no ordering a read does not give:
/// retirement claims the account with one autocommit `UPDATE`, so the read
/// either sees that claim committed and refuses, or runs before it and the
/// request is ordered ahead of the retirement — exactly the two outcomes the
/// update had, since it too released its lock before the request went on.
/// Nothing in retirement waits for, or re-checks, requests that already
/// passed this point under either design.
pub async fn finalize_standing_credential_owner(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Option<User>> {
    UserEntity::find_by_id(user_id)
        .filter(user::Column::DeletedAt.is_null())
        .one(db)
        .await
        .context("db: read standing credential owner")
}

/// Increment failed login attempts and lock account if threshold exceeded.
pub async fn record_failed_login(
    db: &DatabaseConnection,
    user_id: i64,
    max_attempts: i32,
) -> Result<bool> {
    let now = chrono::Utc::now();
    let threshold = max_attempts.max(1);
    let locked_until = now + chrono::Duration::minutes(15);
    let result = UserEntity::update_many()
        // Keep this assignment before LoginAttempts. MySQL evaluates UPDATE
        // assignments left-to-right, while PostgreSQL/SQLite use the old row;
        // this ordering therefore makes the threshold expression portable.
        .col_expr(
            user::Column::LockedUntil,
            Expr::case(
                Expr::col(user::Column::LoginAttempts).gte(threshold - 1),
                Expr::value(locked_until),
            )
            .finally(Expr::col(user::Column::LockedUntil))
            .into(),
        )
        .col_expr(
            user::Column::LoginAttempts,
            Expr::col(user::Column::LoginAttempts).add(1),
        )
        .col_expr(user::Column::UpdatedAt, Expr::value(now))
        .filter(user::Column::Id.eq(user_id))
        .exec(db)
        .await
        .context("db: atomically record failed login")?;
    if result.rows_affected == 0 {
        anyhow::bail!("user {} not found", user_id);
    }
    let updated = find_by_id(db, user_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found after failed login update", user_id))?;
    Ok(updated
        .locked_until
        .is_some_and(|locked_until| locked_until > now))
}

/// Delete a user by ID, returning whether a row existed.
///
/// `repo_stars.user_id` is `ON DELETE CASCADE`, so this also retracts every star
/// the account gave — and those stars sit on repositories belonging to *other*
/// accounts, which are not going anywhere. `repositories.stars_count` is
/// declared to be `COUNT(*)` over `repo_stars`, and the only other writer of it
/// is `toggle_star`, which refreshes exactly the one repository somebody just
/// starred: a repository nobody stars again would advertise the departed
/// account's star forever (card_957cc2683f70).
///
/// The inventory therefore has to be read *before* the delete — afterwards the
/// cascade has already taken the rows that name the affected repositories — and
/// the refresh has to run in the same transaction, so a failure on either side
/// rolls the deletion back instead of committing it next to counters nothing
/// will ever repair.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    delete_by_id_with_after_read(db, id, || std::future::ready(Ok(()))).await
}

/// The retryable transaction behind [`delete_by_id`].
///
/// `after_read` is a private test seam. Production passes a ready future; the
/// contention regression commits another connection after this transaction has
/// taken its read snapshot and before its first write.
async fn delete_by_id_with_after_read<F, Fut>(
    db: &DatabaseConnection,
    id: i64,
    after_read: F,
) -> Result<bool>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let after_read = &after_read;
    crate::contention::retry_transaction("delete user", || async move {
        let transaction = db.begin().await.context("db: begin user delete")?;
        let Some(model) = UserEntity::find_by_id(id)
            .one(&transaction)
            .await
            .context("db: find user for delete")?
        else {
            transaction
                .commit()
                .await
                .context("db: commit absent user delete")?;
            return Ok(false);
        };

        let starred = crate::ops::repo_star_ops::list_starred_repo_ids(&transaction, id).await?;
        after_read().await?;

        crate::serialized_user_grants::remove_user(&transaction, id).await?;
        model
            .delete(&transaction)
            .await
            .context("db: delete user")?;
        crate::ops::repo_ops::refresh_stars_counts(&transaction, &starred).await?;
        transaction
            .commit()
            .await
            .context("db: commit user delete")?;
        Ok(true)
    })
    .await
}

/// Replace the password of `user_id` — its holder's own change, proved with
/// the password it had.
///
/// `expected_hash` is the hash the caller verified the current password
/// against, and the write is conditional on it still being the stored one. A
/// reset or an administrator's password that landed between the check and
/// this write is not overwritten by a request that proved a password which no
/// longer exists. `None` covers that, and an account that is gone or retiring.
///
/// In the same commit: every bearer session issued before is revoked
/// (`session_version`), any reset link still in a mailbox stops working, and
/// the requirement to change an administrator-chosen password is met.
/// Personal access tokens and SSH keys survive, as they survive a reset — see
/// [`invalidate_sessions`].
pub async fn change_password(
    db: &DatabaseConnection,
    user_id: i64,
    expected_hash: &str,
    new_hash: &str,
) -> Result<Option<User>> {
    write_password(
        db,
        user_id,
        Some(expected_hash),
        new_hash,
        false,
        "change password",
    )
    .await
}

/// Set a password an administrator chose for `user_id`, which its holder has
/// to replace before it opens a session.
///
/// Local accounts only: a directory or identity-provider account has no
/// password here to set. Revokes the account's sessions and reset links in the
/// same commit. `None` when there is no such local, open account.
pub async fn set_password_by_administrator(
    db: &DatabaseConnection,
    user_id: i64,
    new_hash: &str,
) -> Result<Option<User>> {
    write_password(
        db,
        user_id,
        None,
        new_hash,
        true,
        "administrator password reset",
    )
    .await
}

async fn write_password(
    db: &DatabaseConnection,
    user_id: i64,
    expected_hash: Option<&str>,
    new_hash: &str,
    change_required: bool,
    what: &'static str,
) -> Result<Option<User>> {
    crate::contention::retry_transaction(what, || async move {
        let transaction = db
            .begin()
            .await
            .with_context(|| format!("db: begin {what}"))?;
        let result: Result<Option<User>> = async {
            let mut update = UserEntity::update_many()
                .col_expr(
                    user::Column::PasswordHash,
                    Expr::value(new_hash.to_string()),
                )
                .col_expr(
                    user::Column::PasswordChangeRequired,
                    Expr::value(change_required),
                )
                .col_expr(
                    user::Column::SessionVersion,
                    Expr::col(user::Column::SessionVersion).add(1),
                )
                .col_expr(user::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
                .filter(user::Column::Id.eq(user_id))
                .filter(user::Column::AuthProvider.eq("local"))
                .filter(user::Column::IsActive.eq(true))
                .filter(user::Column::DeletedAt.is_null());
            if let Some(expected_hash) = expected_hash {
                update = update.filter(user::Column::PasswordHash.eq(expected_hash));
            }
            let updated = update
                .exec(&transaction)
                .await
                .with_context(|| format!("db: write {what}"))?;
            match updated.rows_affected {
                0 => return Ok(None),
                1 => {}
                rows => anyhow::bail!("db: {what} affected {rows} rows for user {user_id}"),
            }
            crate::ops::password_reset_token_ops::invalidate_user_tokens(&transaction, user_id)
                .await
                .with_context(|| format!("db: invalidate reset links on {what}"))?;
            UserEntity::find_by_id(user_id)
                .one(&transaction)
                .await
                .with_context(|| format!("db: reload user after {what}"))
        }
        .await;
        match result {
            Ok(Some(user)) => {
                transaction
                    .commit()
                    .await
                    .with_context(|| format!("db: commit {what}"))?;
                Ok(Some(user))
            }
            Ok(None) => {
                transaction
                    .rollback()
                    .await
                    .with_context(|| format!("db: roll back {what}"))?;
                Ok(None)
            }
            Err(error) => {
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!("db: roll back {what}: {rollback_error}"));
                }
                Err(error)
            }
        }
    })
    .await
}

/// Move `user_id` to `email`, an address its holder has just proved — so the
/// same write marks it proved.
///
/// `None` when the account is gone or retiring. Another account holding the
/// address surfaces as the unique violation it is, for the caller to answer.
pub async fn update_email(
    db: &DatabaseConnection,
    user_id: i64,
    email: &str,
) -> Result<Option<User>> {
    let now = chrono::Utc::now();
    let updated = UserEntity::update_many()
        .col_expr(user::Column::Email, Expr::value(email.to_string()))
        .col_expr(user::Column::EmailVerifiedAt, Expr::value(now))
        .col_expr(user::Column::UpdatedAt, Expr::value(now))
        .filter(user::Column::Id.eq(user_id))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .exec(db)
        .await
        .context("db: change a user's email")?;
    if updated.rows_affected == 0 {
        return Ok(None);
    }
    find_by_id(db, user_id).await
}

/// Record that `user_id` proved it receives mail at `email`.
///
/// Conditional on the address still being `email`: a link mailed to the old
/// address must not mark a new one proved. `None` when nothing matched — the
/// account is gone, retiring, or its address changed since the link was sent.
pub async fn mark_email_verified(
    db: &DatabaseConnection,
    user_id: i64,
    email: &str,
) -> Result<Option<User>> {
    let now = chrono::Utc::now();
    let updated = UserEntity::update_many()
        .col_expr(user::Column::EmailVerifiedAt, Expr::value(now))
        .col_expr(user::Column::UpdatedAt, Expr::value(now))
        .filter(user::Column::Id.eq(user_id))
        .filter(user::Column::Email.eq(email))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .exec(db)
        .await
        .context("db: mark a user's email verified")?;
    if updated.rows_affected == 0 {
        return Ok(None);
    }
    find_by_id(db, user_id).await
}

/// How many open accounts hold the instance-administrator flag.
pub async fn count_active_admins(db: &DatabaseConnection) -> Result<u64> {
    UserEntity::find()
        .filter(user::Column::IsAdmin.eq(true))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: count instance administrators")
}

#[cfg(test)]
mod contention_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use sea_orm::ConnectionTrait;
    use tokio::sync::Notify;

    async fn scratch_db(name: &str) -> (DatabaseConnection, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("create temporary database directory");
        let db = crate::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                directory.path().join(name).display()
            ),
            crate::TEST_CONNECT_TIMEOUT_SECS,
            60,
            4,
        )
        .await
        .expect("connect to temporary database");
        crate::run_migrations(&db)
            .await
            .expect("migrate temporary database");
        (db, directory)
    }

    /// The interleaving is explicit rather than timing-based: the delete takes
    /// its read snapshot, another pooled connection commits, and only then is
    /// the first attempt allowed to issue its DELETE. Without the outer retry
    /// that attempt returns `SQLITE_BUSY_SNAPSHOT` and the user survives.
    #[tokio::test]
    async fn a_writer_commit_after_the_inventory_restarts_the_whole_user_delete() {
        let (db, _directory) = scratch_db("user-delete.db").await;
        let user = create_user(
            &db,
            "snapshot-delete",
            "snapshot-delete@example.invalid",
            "",
            "Snapshot Delete",
        )
        .await
        .expect("seed the account to delete");

        let attempts = AtomicUsize::new(0);
        let snapshot_taken = Notify::new();
        let resume_delete = Notify::new();
        let attempts_ref = &attempts;
        let snapshot_taken_ref = &snapshot_taken;
        let resume_delete_ref = &resume_delete;

        let deletion = delete_by_id_with_after_read(&db, user.id, || {
            let attempts = attempts_ref;
            let snapshot_taken = snapshot_taken_ref;
            let resume_delete = resume_delete_ref;
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    snapshot_taken.notify_one();
                    resume_delete.notified().await;
                }
                Ok(())
            }
        });
        let displacer = async {
            snapshot_taken.notified().await;
            db.execute_unprepared(&format!(
                "UPDATE users SET display_name = 'committed after the snapshot' WHERE id = {}",
                user.id
            ))
            .await
            .expect("commit the write that makes the delete snapshot stale");
            resume_delete.notify_one();
        };

        let (deleted, ()) = tokio::join!(deletion, displacer);
        assert!(
            deleted.expect("a transiently contended user delete must succeed"),
            "the account disappeared before this delete could remove it"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the stale first snapshot was not discarded and rebuilt exactly once"
        );
        assert!(
            find_by_id(&db, user.id)
                .await
                .expect("read the account after deletion")
                .is_none(),
            "the retried delete reported success but left the account behind"
        );
    }

    #[tokio::test]
    async fn a_writer_commit_after_the_user_read_restarts_the_whole_mfa_enrolment() {
        let (db, _directory) = scratch_db("mfa-enable.db").await;
        let user = create_user(
            &db,
            "snapshot-mfa-enable",
            "snapshot-mfa-enable@example.invalid",
            "",
            "Snapshot MFA Enable",
        )
        .await
        .expect("seed the account whose second factor is enabled");
        let codes = vec!["alpha-one".to_string(), "beta-two".to_string()];

        let attempts = AtomicUsize::new(0);
        let snapshot_taken = Notify::new();
        let resume_enrolment = Notify::new();
        let attempts_ref = &attempts;
        let snapshot_taken_ref = &snapshot_taken;
        let resume_enrolment_ref = &resume_enrolment;

        let enrolment = enable_mfa_with_backup_codes_with_after_read(&db, user.id, &codes, || {
            let attempts = attempts_ref;
            let snapshot_taken = snapshot_taken_ref;
            let resume_enrolment = resume_enrolment_ref;
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    snapshot_taken.notify_one();
                    resume_enrolment.notified().await;
                }
                Ok(())
            }
        });
        let displacer = async {
            snapshot_taken.notified().await;
            db.execute_unprepared(&format!(
                "UPDATE users SET display_name = 'committed after the snapshot' WHERE id = {}",
                user.id
            ))
            .await
            .expect("commit the write that makes the MFA-enrolment snapshot stale");
            resume_enrolment.notify_one();
        };

        let (enrolled, ()) = tokio::join!(enrolment, displacer);
        let enrolled = enrolled
            .expect("transient contention must not prevent MFA enrolment")
            .expect("the open account must still exist after MFA enrolment");
        assert!(
            enrolled.mfa_enabled,
            "the returned account must have MFA on"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the stale first snapshot was not discarded and rebuilt exactly once"
        );
        let stored = find_by_id(&db, user.id)
            .await
            .expect("read the account after MFA enrolment")
            .expect("MFA enrolment must not delete the account");
        assert!(stored.mfa_enabled, "the stored account must have MFA on");
        assert_eq!(
            stored.display_name.as_deref(),
            Some("committed after the snapshot"),
            "the retry reused the stale user model instead of reading a fresh snapshot"
        );
        assert_eq!(
            crate::ops::mfa_backup_code_ops::list_codes(&db, user.id)
                .await
                .expect("list backup codes after MFA enrolment")
                .len(),
            codes.len(),
            "the retried enrolment did not publish its complete recovery set"
        );
    }

    #[tokio::test]
    async fn an_ordinary_mfa_enrolment_failure_is_not_retried() {
        let (db, _directory) = scratch_db("mfa-enable-error.db").await;
        let user = create_user(
            &db,
            "failed-mfa-enable",
            "failed-mfa-enable@example.invalid",
            "",
            "Failed MFA Enable",
        )
        .await
        .expect("seed the account whose enrolment fails");
        let codes = vec!["never-live".to_string()];

        let attempts = AtomicUsize::new(0);
        let error = enable_mfa_with_backup_codes_with_after_read(&db, user.id, &codes, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(anyhow::anyhow!("ordinary injected failure")))
        })
        .await
        .expect_err("the injected failure must reach the caller");

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a non-contention failure must not consume the retry budget"
        );
        assert!(format!("{error:#}").contains("ordinary injected failure"));
        let stored = find_by_id(&db, user.id)
            .await
            .expect("read the account after the refused enrolment")
            .expect("the refused enrolment must keep the account");
        assert!(
            !stored.mfa_enabled,
            "the refused attempt must roll the MFA flag back"
        );
        assert!(
            crate::ops::mfa_backup_code_ops::list_codes(&db, user.id)
                .await
                .expect("list backup codes after the refused enrolment")
                .is_empty(),
            "the refused attempt published recovery credentials"
        );
    }

    #[tokio::test]
    async fn a_writer_commit_after_the_user_read_restarts_the_whole_mfa_removal() {
        let (db, _directory) = scratch_db("mfa-disable.db").await;
        let user = create_user(
            &db,
            "snapshot-mfa-disable",
            "snapshot-mfa-disable@example.invalid",
            "",
            "Snapshot MFA Disable",
        )
        .await
        .expect("seed the account whose second factor is removed");
        let codes = vec!["alpha-one".to_string(), "beta-two".to_string()];
        enable_mfa_with_backup_codes(&db, user.id, &codes)
            .await
            .expect("enrol the second factor and its backup codes");
        assert_eq!(
            crate::ops::mfa_backup_code_ops::list_codes(&db, user.id)
                .await
                .expect("list seeded backup codes")
                .len(),
            codes.len(),
            "the fixture did not publish the credentials the removal must revoke"
        );

        let attempts = AtomicUsize::new(0);
        let snapshot_taken = Notify::new();
        let resume_removal = Notify::new();
        let attempts_ref = &attempts;
        let snapshot_taken_ref = &snapshot_taken;
        let resume_removal_ref = &resume_removal;

        let removal = disable_mfa_with_after_read(&db, user.id, || {
            let attempts = attempts_ref;
            let snapshot_taken = snapshot_taken_ref;
            let resume_removal = resume_removal_ref;
            async move {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    snapshot_taken.notify_one();
                    resume_removal.notified().await;
                }
                Ok(())
            }
        });
        let displacer = async {
            snapshot_taken.notified().await;
            db.execute_unprepared(&format!(
                "UPDATE users SET display_name = 'committed after the snapshot' WHERE id = {}",
                user.id
            ))
            .await
            .expect("commit the write that makes the MFA-removal snapshot stale");
            resume_removal.notify_one();
        };

        let (removed, ()) = tokio::join!(removal, displacer);
        let removed = removed
            .expect("transient contention must not prevent MFA removal")
            .expect("the open account must still exist after MFA removal");
        assert!(
            !removed.mfa_enabled,
            "the returned account must have MFA off"
        );
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            2,
            "the stale first snapshot was not discarded and rebuilt exactly once"
        );
        let stored = find_by_id(&db, user.id)
            .await
            .expect("read the account after MFA removal")
            .expect("MFA removal must not delete the account");
        assert!(!stored.mfa_enabled, "the stored account must have MFA off");
        assert!(
            crate::ops::mfa_backup_code_ops::list_codes(&db, user.id)
                .await
                .expect("list backup codes after MFA removal")
                .is_empty(),
            "unused backup codes outlived the retried MFA removal"
        );
    }

    #[tokio::test]
    async fn an_ordinary_mfa_removal_failure_is_not_retried() {
        let (db, _directory) = scratch_db("mfa-disable-error.db").await;
        let user = create_user(
            &db,
            "failed-mfa-disable",
            "failed-mfa-disable@example.invalid",
            "",
            "Failed MFA Disable",
        )
        .await
        .expect("seed the account whose removal fails");
        let codes = vec!["still-live".to_string()];
        enable_mfa_with_backup_codes(&db, user.id, &codes)
            .await
            .expect("enrol the second factor and its backup code");

        let attempts = AtomicUsize::new(0);
        let error = disable_mfa_with_after_read(&db, user.id, || {
            attempts.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(anyhow::anyhow!("ordinary injected failure")))
        })
        .await
        .expect_err("the injected failure must reach the caller");

        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "a non-contention failure must not consume the retry budget"
        );
        assert!(format!("{error:#}").contains("ordinary injected failure"));
        assert!(
            find_by_id(&db, user.id)
                .await
                .expect("read the account after the refused removal")
                .expect("the refused removal must keep the account")
                .mfa_enabled,
            "the refused attempt must roll the MFA flag back"
        );
        assert_eq!(
            crate::ops::mfa_backup_code_ops::list_codes(&db, user.id)
                .await
                .expect("list backup codes after the refused removal")
                .len(),
            codes.len(),
            "the refused attempt must keep the live recovery credentials"
        );
    }

    #[tokio::test]
    async fn a_retirement_claim_before_totp_storage_is_a_typed_absent_outcome() {
        let (db, _directory) = scratch_db("mfa-setup-retirement.db").await;
        let user = create_user(
            &db,
            "retiring-mfa-setup",
            "retiring-mfa-setup@example.invalid",
            "",
            "Retiring MFA Setup",
        )
        .await
        .expect("seed the account whose TOTP setup loses to retirement");

        let updated = stage_pending_totp_secret_with_after_read(
            &db,
            user.id,
            "must-never-be-stored",
            || async {
                assert!(
                    begin_user_retirement(&db, user.id).await?,
                    "the injected account retirement must win"
                );
                Ok(())
            },
        )
        .await
        .expect("retirement is an outcome, not a TOTP database error");

        assert!(updated.is_none(), "setup accepted a retiring account");
        let stored = find_by_id(&db, user.id)
            .await
            .expect("read the retiring account")
            .expect("retirement keeps the row until storage is retired");
        assert!(stored.deleted_at.is_some());
        assert_eq!(
            stored.pending_totp_secret, None,
            "the losing setup staged a TOTP secret"
        );
        assert_eq!(
            stored.totp_secret, None,
            "the losing setup published a TOTP secret"
        );
    }

    #[tokio::test]
    async fn a_retirement_commit_after_the_user_read_aborts_the_whole_mfa_enrolment() {
        let (db, _directory) = scratch_db("mfa-enable-retirement.db").await;
        let user = create_user(
            &db,
            "retiring-mfa-enable",
            "retiring-mfa-enable@example.invalid",
            "",
            "Retiring MFA Enable",
        )
        .await
        .expect("seed the account whose MFA enrolment loses to retirement");
        let codes = vec!["never-live-one".to_string(), "never-live-two".to_string()];
        let attempts = AtomicUsize::new(0);
        let attempts_ref = &attempts;

        let enrolled = enable_mfa_with_backup_codes_with_after_read(&db, user.id, &codes, || {
            let attempts = attempts_ref;
            async {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    assert!(
                        begin_user_retirement(&db, user.id).await?,
                        "the injected account retirement must win"
                    );
                }
                Ok(())
            }
        })
        .await
        .expect("retirement is an outcome, not an MFA enrolment database error");

        assert!(enrolled.is_none(), "enrolment accepted a retiring account");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "the retry re-entered the mutation after observing the retirement marker"
        );
        let stored = find_by_id(&db, user.id)
            .await
            .expect("read the retiring account")
            .expect("retirement keeps the row until storage is retired");
        assert!(stored.deleted_at.is_some());
        assert!(!stored.mfa_enabled, "the losing enrolment enabled MFA");
        assert!(
            crate::ops::mfa_backup_code_ops::list_codes(&db, user.id)
                .await
                .expect("list backup codes after the losing enrolment")
                .is_empty(),
            "the losing enrolment published recovery credentials"
        );
    }

    #[tokio::test]
    async fn a_retirement_commit_after_the_user_read_aborts_the_whole_mfa_removal() {
        let (db, _directory) = scratch_db("mfa-disable-retirement.db").await;
        let user = create_user(
            &db,
            "retiring-mfa-disable",
            "retiring-mfa-disable@example.invalid",
            "",
            "Retiring MFA Disable",
        )
        .await
        .expect("seed the account whose MFA removal loses to retirement");
        let codes = vec!["still-live-one".to_string(), "still-live-two".to_string()];
        enable_mfa_with_backup_codes(&db, user.id, &codes)
            .await
            .expect("enrol the second factor")
            .expect("the account is open before retirement");
        let attempts = AtomicUsize::new(0);
        let attempts_ref = &attempts;

        let removed = disable_mfa_with_after_read(&db, user.id, || {
            let attempts = attempts_ref;
            async {
                if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    assert!(
                        begin_user_retirement(&db, user.id).await?,
                        "the injected account retirement must win"
                    );
                }
                Ok(())
            }
        })
        .await
        .expect("retirement is an outcome, not an MFA removal database error");

        assert!(removed.is_none(), "removal accepted a retiring account");
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            1,
            "the retry re-entered the mutation after observing the retirement marker"
        );
        let stored = find_by_id(&db, user.id)
            .await
            .expect("read the retiring account")
            .expect("retirement keeps the row until storage is retired");
        assert!(stored.deleted_at.is_some());
        assert!(
            stored.mfa_enabled,
            "the losing removal changed the MFA flag"
        );
        assert_eq!(
            crate::ops::mfa_backup_code_ops::list_codes(&db, user.id)
                .await
                .expect("list backup codes after the losing removal")
                .len(),
            codes.len(),
            "the losing removal partially revoked the recovery set"
        );
    }

    #[tokio::test]
    async fn primary_login_finalization_reads_mfa_and_records_completion_atomically() {
        let (db, _directory) = scratch_db("primary-login-finalization.db").await;
        let plain = create_user(
            &db,
            "plain-login",
            "plain-login@example.invalid",
            "",
            "Plain Login",
        )
        .await
        .expect("seed the plain login account");
        let mfa = create_user(
            &db,
            "mfa-login",
            "mfa-login@example.invalid",
            "",
            "MFA Login",
        )
        .await
        .expect("seed the MFA login account");
        enable_mfa(&db, mfa.id)
            .await
            .expect("enable the second factor");
        record_failed_login(&db, mfa.id, 5)
            .await
            .expect("seed a failed MFA attempt");

        let plain = finalize_primary_login(&db, plain.id)
            .await
            .expect("finalize the plain login")
            .expect("the plain account remains open");
        assert!(!plain.mfa_enabled);
        assert!(
            plain.last_login_at.is_some(),
            "a primary factor completes login when MFA is off"
        );

        let mfa = finalize_primary_login(&db, mfa.id)
            .await
            .expect("finalize the MFA primary factor")
            .expect("the MFA account remains open");
        assert!(mfa.mfa_enabled);
        assert_eq!(
            mfa.login_attempts, 1,
            "a fresh primary-factor challenge erased the second-factor failure"
        );
        assert_eq!(
            mfa.last_login_at, None,
            "a primary factor must not claim a completed MFA login"
        );
        let mfa = record_successful_login(&db, mfa.id)
            .await
            .expect("finalize the completed MFA login")
            .expect("the MFA account remains open");
        assert!(mfa.last_login_at.is_some());
    }

    #[tokio::test]
    async fn login_finalizers_treat_retirement_and_delete_as_typed_absence() {
        let (db, _directory) = scratch_db("login-finalizer-lifecycle.db").await;
        let retiring = create_user(
            &db,
            "retiring-login",
            "retiring-login@example.invalid",
            "",
            "Retiring Login",
        )
        .await
        .expect("seed the retiring login account");
        assert!(begin_user_retirement(&db, retiring.id)
            .await
            .expect("claim the account for retirement"));
        assert!(finalize_primary_login(&db, retiring.id)
            .await
            .expect("retirement is an outcome, not a database error")
            .is_none());
        assert!(record_successful_login(&db, retiring.id)
            .await
            .expect("retirement is an outcome, not a database error")
            .is_none());

        let deleted = create_user(
            &db,
            "deleted-login",
            "deleted-login@example.invalid",
            "",
            "Deleted Login",
        )
        .await
        .expect("seed the deleted login account");
        assert!(delete_by_id(&db, deleted.id)
            .await
            .expect("delete the login account"));
        assert!(record_successful_login(&db, deleted.id)
            .await
            .expect("physical deletion is an outcome, not a database error")
            .is_none());
    }
}
