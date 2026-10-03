//! Whether this instance accepts self-service registrations at all.
//!
//! `POST /users/register` used to be unconditional: the only thing standing
//! between a public instance and an account farm was `[rate_limit].auth_max`,
//! which is a throttle (10 accounts a minute, indefinitely) and not a refusal.
//! An instance meant for two people has to be able to say "no new accounts from
//! outside", and until this module existed there was no way to say it.
//!
//! Two things this mode deliberately is **not**:
//!
//! * **Not a bool.** `[auth].registration` is a string so that `"invite"` can
//!   join `"open"` / `"closed"` later without breaking every config file that
//!   already spells the setting out. Self-service sign-up is a product feature
//!   with more than two states, not a stopgap flag for launch week.
//! * **Not a gate on LDAP / SSO auto-provision.** Those channels create accounts
//!   too (`plombir_git_auth_events_total{event="provision"}`), and closing
//!   self-service registration must not simultaneously lock out a whole
//!   company's directory. That is a separate switch with a separate decision
//!   behind it, and it exists: [`crate::user::provisioning`] reads
//!   `sso_providers.auto_provision` / `allowed_email_domains`, per provider.
//!   The reasoning that used to live here — "the operator who wired a provider
//!   in already decided who may have an account" — was true of a private
//!   directory and false of `github.com`, where the identity is free and the
//!   decision had never been made by anyone (card_0ae3deacd3f0).

use anyhow::Result;
use sea_orm::DatabaseConnection;

/// Serialises the bootstrap registration on an empty instance.
///
/// The window is narrow but real: between "the table is empty" and "the first
/// row is committed", every concurrent request sees an empty table. Without
/// this, concurrent registrations can all claim the one-shot instance-admin
/// capability. Held across the first insert only; once any row exists, open
/// registrations release it before password hashing and closed registrations
/// refuse before spending that work.
static BOOTSTRAP_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// How this instance answers `POST /users/register`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RegistrationMode {
    /// Anyone who can reach the endpoint may create an account (the historical
    /// behaviour, and still the default so an existing deployment that upgrades
    /// does not silently lose its sign-up page).
    #[default]
    Open,
    /// Nobody may create an account through the endpoint — except the very
    /// first one, on an instance that has never had a user. See
    /// [`authorize`].
    Closed,
}

impl RegistrationMode {
    /// The values `[auth].registration` / `PLOMBIR_GIT_REGISTRATION` accept.
    pub const ACCEPTED: [&'static str; 2] = ["open", "closed"];

    /// Parse a configured value, case- and whitespace-insensitively.
    ///
    /// Returns the accepted vocabulary in the error rather than falling back to
    /// a default: an operator who writes `registration = "close"` is asking for
    /// a *closed* instance, and answering that with a silently open one is the
    /// exact failure this setting exists to prevent.
    pub fn parse(value: &str) -> std::result::Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "open" => Ok(Self::Open),
            "closed" => Ok(Self::Closed),
            other => Err(format!(
                "expected one of {} (got {other:?})",
                Self::ACCEPTED
                    .iter()
                    .map(|mode| format!("\"{mode}\""))
                    .collect::<Vec<_>>()
                    .join(" / ")
            )),
        }
    }

    /// The configured spelling, for logs and round-tripping.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closed => "closed",
        }
    }

    pub fn is_closed(self) -> bool {
        matches!(self, Self::Closed)
    }
}

/// Permission to run one self-service registration.
///
/// Keep it alive across the registration itself: on a closed instance it
/// carries the bootstrap lock, and dropping it early re-opens the race the lock
/// is there to close.
pub struct RegistrationPermit {
    _bootstrap: Option<tokio::sync::MutexGuard<'static, ()>>,
}

impl RegistrationPermit {
    /// Whether this capability belongs to the account that initialises the
    /// instance. Kept crate-private so callers cannot choose their own role;
    /// [`authorize`] is the only constructor.
    pub(super) fn grants_instance_admin(&self) -> bool {
        self._bootstrap.is_some()
    }
}

/// Decide whether a self-service registration may proceed **before** the
/// request costs anything — no password hashing, no row written.
///
/// * `Ok(Some(permit))` — proceed, holding `permit` until the registration is
///   done.
/// * `Ok(None)` — refuse; the instance is closed and already has accounts.
/// * `Err(_)` — the question could not be answered. The caller must turn this
///   into a 5xx, never into a registration: a database that cannot be counted
///   must not be read as "no users yet, let everyone in".
pub async fn authorize(
    db: &DatabaseConnection,
    mode: RegistrationMode,
) -> Result<Option<RegistrationPermit>> {
    // Every mode needs the same one-shot answer on an empty database: this is
    // where the first account receives the capability that makes the instance
    // operable. The guard stays inside that permit until its insert commits.
    let guard = BOOTSTRAP_LOCK.lock().await;
    if !rg_db::ops::user_ops::has_any(db).await? {
        return Ok(Some(RegistrationPermit {
            _bootstrap: Some(guard),
        }));
    }
    drop(guard);

    match mode {
        RegistrationMode::Open => Ok(Some(RegistrationPermit { _bootstrap: None })),
        RegistrationMode::Closed => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_documented_spellings_parse() {
        assert_eq!(RegistrationMode::parse("open"), Ok(RegistrationMode::Open));
        assert_eq!(
            RegistrationMode::parse("closed"),
            Ok(RegistrationMode::Closed)
        );
        assert_eq!(
            RegistrationMode::parse("  CLOSED \n"),
            Ok(RegistrationMode::Closed)
        );
    }

    /// The dangerous direction: a typo must not resolve to `open`.
    #[test]
    fn an_unrecognised_value_is_an_error_naming_the_vocabulary() {
        let err = RegistrationMode::parse("close").unwrap_err();
        assert!(err.contains("\"open\""), "no vocabulary: {err}");
        assert!(err.contains("\"closed\""), "no vocabulary: {err}");
        assert!(err.contains("close"), "does not echo the input: {err}");

        assert!(RegistrationMode::parse("").is_err());
        assert!(RegistrationMode::parse("true").is_err());
    }

    #[test]
    fn the_default_is_open_so_an_upgrade_changes_nothing() {
        assert_eq!(RegistrationMode::default(), RegistrationMode::Open);
        assert!(!RegistrationMode::default().is_closed());
        assert_eq!(RegistrationMode::Closed.as_str(), "closed");
    }

    async fn migrated_db() -> DatabaseConnection {
        let db = sea_orm::Database::connect("sqlite::memory:")
            .await
            .expect("connect test database");
        rg_db::run_migrations(&db).await.expect("run migrations");
        db
    }

    #[tokio::test]
    async fn an_open_instance_marks_only_the_empty_database_permit_as_bootstrap() {
        let db = migrated_db().await;

        let permit = authorize(&db, RegistrationMode::Open)
            .await
            .unwrap()
            .expect("open registration is allowed");
        assert!(permit.grants_instance_admin());

        super::super::service::register(
            &db,
            permit,
            "founder",
            "founder@example.com",
            "Qz7$wRtm",
            "secret",
        )
        .await
        .expect("bootstrap registration");

        let permit = authorize(&db, RegistrationMode::Open)
            .await
            .unwrap()
            .expect("open registration stays allowed");
        assert!(!permit.grants_instance_admin());
    }

    #[tokio::test]
    async fn a_closed_instance_admits_the_first_account_and_then_nobody() {
        let db = migrated_db().await;

        let permit = authorize(&db, RegistrationMode::Closed)
            .await
            .expect("the count must be readable")
            .expect("an empty closed instance must be initialisable");

        super::super::service::register(
            &db,
            permit,
            "founder",
            "founder@example.com",
            "Qz7$wRtm",
            "secret",
        )
        .await
        .expect("bootstrap registration");

        assert!(
            authorize(&db, RegistrationMode::Closed)
                .await
                .unwrap()
                .is_none(),
            "the bootstrap window must close behind the first account"
        );
    }

    /// A tombstoned account is still an account: the window is "this instance
    /// has never had a user", not "it has none right now". Nothing soft-deletes
    /// a user today (see `rg_db::entities::user::Model::is_usable`), which is
    /// exactly why this is asserted now rather than discovered later.
    #[tokio::test]
    async fn a_soft_deleted_account_does_not_re_open_the_bootstrap_window() {
        use sea_orm::ActiveModelTrait;

        let db = migrated_db().await;
        let permit = authorize(&db, RegistrationMode::Closed)
            .await
            .expect("the count must be readable")
            .expect("an empty closed instance must be initialisable");
        let user = super::super::service::register(
            &db,
            permit,
            "founder",
            "founder@example.com",
            "Qz7$wRtm",
            "secret",
        )
        .await
        .expect("bootstrap registration");

        let model = rg_db::ops::user_ops::find_by_id(&db, user.user_id)
            .await
            .expect("read back the founder")
            .expect("the founder exists");
        let mut tombstoned: rg_db::entities::user::ActiveModel = model.into();
        tombstoned.deleted_at = sea_orm::Set(Some(chrono::Utc::now()));
        tombstoned.update(&db).await.expect("tombstone the founder");

        assert!(
            authorize(&db, RegistrationMode::Closed)
                .await
                .unwrap()
                .is_none(),
            "a tombstone must not hand the instance back to the next stranger"
        );
    }
}
