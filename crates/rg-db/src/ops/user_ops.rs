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

/// Count active (non-deleted) users — backs the `forgekeep_users` gauge.
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

/// Update a user.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<User> {
    model.update(db).await.context("db: update user")
}

///
/// CRITICAL: SeaORM single-row update (pitfall #11)
///
/// To update a single row, you MUST first `find_by_id()` to get the model,
/// then convert it into an `ActiveModel`, modify fields, and call `update()`.
///
/// CORRECT pattern (used here):
///   let Some(model) = UserEntity::find_by_id(id).one(db).await? else {
///       return Ok(None);
///   };
///   let mut active: ActiveModel = model.into();
///   active.field = Set(value);
///   active.update(db).await.map(Some)
///
/// WRONG pattern for ordinary admin/profile field updates:
///   ActiveModel { id: Set(id), ... }.update(db)  // MAY skip optimistic lock
///
/// `update_many().col_expr(...)` is still appropriate for atomic counters where
/// a read-modify-write ActiveModel cycle would lose concurrent increments.
///
/// An absent row is returned as `Ok(None)` so the higher layer can give that
/// outcome its domain meaning without confusing it with a database failure.
pub async fn update_by_id(
    db: &DatabaseConnection,
    id: i64,
    display_name: Option<Option<String>>,
    bio: Option<Option<String>>,
    is_admin: Option<bool>,
    is_active: Option<bool>,
) -> Result<Option<User>> {
    let Some(model) = UserEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find user for update")?
    else {
        return Ok(None);
    };

    let mut active: ActiveModel = model.into();

    if let Some(dn) = display_name {
        active.display_name = Set(dn);
    }
    if let Some(b) = bio {
        active.bio = Set(b);
    }
    if let Some(admin) = is_admin {
        active.is_admin = Set(admin);
    }
    if let Some(active_flag) = is_active {
        active.is_active = Set(active_flag);
    }

    active
        .update(db)
        .await
        .context("db: update user by admin")
        .map(Some)
}

/// Create a user with high-level parameters (used by SSO).
pub async fn create_user(
    db: &DatabaseConnection,
    username: &str,
    email: &str,
    password_hash: &str,
    display_name: &str,
) -> Result<User> {
    use crate::entities::user;
    let now = chrono::Utc::now();
    let model = user::ActiveModel {
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
        mfa_enabled: Set(false),
        totp_last_step: Set(None),
        last_login_at: Set(None),
        login_attempts: Set(0),
        locked_until: Set(None),
        session_version: Set(0),
        created_at: Set(now),
        updated_at: Set(now),
        deleted_at: Set(None),
    };
    create(db, model).await
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
            mfa_enabled: Set(false),
            totp_last_step: Set(None),
            last_login_at: Set(None),
            login_attempts: Set(0),
            locked_until: Set(None),
            session_version: Set(0),
            created_at: Set(now),
            updated_at: Set(now),
            deleted_at: Set(None),
        },
    )
    .await
}

/// Refresh non-authoritative LDAP identity metadata after a successful bind.
/// Email is deliberately not changed here because it is globally unique and
/// may require an administrator to resolve a directory collision.
pub async fn sync_ldap_identity(
    db: &DatabaseConnection,
    user_id: i64,
    ldap_provider_id: i64,
    display_name: Option<&str>,
    ldap_uid: Option<&str>,
) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.display_name = Set(display_name.map(str::to_string));
    active.ldap_uid = Set(ldap_uid.map(str::to_string));
    active.ldap_provider_id = Set(Some(ldap_provider_id));
    active.updated_at = Set(chrono::Utc::now());
    update(db, active).await
}

/// Update the TOTP secret for a user (encrypted).
pub async fn update_totp_secret(
    db: &DatabaseConnection,
    user_id: i64,
    encrypted_secret: &str,
) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.totp_secret = Set(Some(encrypted_secret.to_string()));
    active.updated_at = Set(chrono::Utc::now());
    active
        .update(db)
        .await
        .map_err(|e| anyhow::anyhow!("db: {}", e))
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
    enable_mfa_with_after_read(db, user_id, || std::future::ready(Ok(()))).await
}

/// The read-before-write half of MFA enrolment.
///
/// `after_read` is a private test seam. Production passes a ready future; the
/// contention regression commits another connection after the transaction has
/// taken its user snapshot and before its first write.
async fn enable_mfa_with_after_read<C, F, Fut>(db: &C, user_id: i64, after_read: F) -> Result<User>
where
    C: ConnectionTrait,
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    after_read().await?;
    let mut active: ActiveModel = model.into();
    active.mfa_enabled = Set(true);
    active.updated_at = Set(chrono::Utc::now());
    active.update(db).await.context("db: enable MFA")
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
) -> Result<User> {
    enable_mfa_with_backup_codes_with_after_read(db, user_id, codes, || std::future::ready(Ok(())))
        .await
}

/// The retryable transaction behind [`enable_mfa_with_backup_codes`].
async fn enable_mfa_with_backup_codes_with_after_read<F, Fut>(
    db: &DatabaseConnection,
    user_id: i64,
    codes: &[String],
    after_read: F,
) -> Result<User>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let after_read = &after_read;
    crate::contention::retry_transaction("enable MFA", || async move {
        let transaction = db.begin().await.context("db: begin MFA enrolment")?;
        let result: Result<User> = async {
            let user = enable_mfa_with_after_read(&transaction, user_id, after_read).await?;
            crate::ops::mfa_backup_code_ops::set_codes(&transaction, user_id, codes)
                .await
                .context("db: store MFA backup codes")?;
            Ok(user)
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

/// Disable MFA for a user.
pub async fn disable_mfa(db: &DatabaseConnection, user_id: i64) -> Result<User> {
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
) -> Result<User>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<()>>,
{
    let after_read = &after_read;
    crate::contention::retry_transaction("disable MFA", || async move {
        let transaction = db.begin().await.context("db: begin MFA removal")?;
        let result: Result<User> = async {
            let model = UserEntity::find_by_id(user_id)
                .one(&transaction)
                .await?
                .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
            after_read().await?;
            let mut active: ActiveModel = model.into();
            active.mfa_enabled = Set(false);
            active.totp_secret = Set(None);
            active.updated_at = Set(chrono::Utc::now());
            let user = active
                .update(&transaction)
                .await
                .context("db: disable MFA")?;

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
            Ok(user)
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

/// Record a successful login and reset login_attempts/locked_until.
pub async fn record_successful_login(db: &DatabaseConnection, user_id: i64) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.last_login_at = Set(Some(chrono::Utc::now()));
    active.login_attempts = Set(0);
    active.locked_until = Set(None);
    active
        .update(db)
        .await
        .map_err(|e| anyhow::anyhow!("db: {}", e))
}

/// Reset primary-factor failures while MFA is still pending. This deliberately
/// does not update `last_login_at`, which represents a completed login.
pub async fn reset_login_failures(db: &DatabaseConnection, user_id: i64) -> Result<User> {
    let model = UserEntity::find_by_id(user_id)
        .one(db)
        .await?
        .ok_or_else(|| anyhow::anyhow!("user {} not found", user_id))?;
    let mut active: ActiveModel = model.into();
    active.login_attempts = Set(0);
    active.locked_until = Set(None);
    active.updated_at = Set(chrono::Utc::now());
    active
        .update(db)
        .await
        .map_err(|e| anyhow::anyhow!("db: {}", e))
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
        let enrolled = enrolled.expect("transient contention must not prevent MFA enrolment");
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
        let removed = removed.expect("transient contention must not prevent MFA removal");
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
}
