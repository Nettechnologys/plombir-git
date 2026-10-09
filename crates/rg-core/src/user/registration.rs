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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use sea_orm::DatabaseConnection;
use subtle::ConstantTimeEq;

/// File name of the one-time setup token, beside the at-rest encryption key.
pub const SETUP_TOKEN_FILE_NAME: &str = "setup_token";

/// The one-time secret that authorises the bootstrap registration.
///
/// The first account on an empty instance becomes its administrator, whatever
/// `[auth].registration` says — otherwise a closed instance could never be
/// initialised. Until this token existed that made the first start a race:
/// whoever reached `POST /users/register` first owned the instance, and a
/// deployment whose port was reachable for the minute between `docker compose
/// up` and the operator's own registration belonged to its first visitor
/// (security audit finding #13). The server now generates this secret at the
/// first start, writes it `0600` next to the encryption key, prints it once in
/// the startup log, and admits the bootstrap registration only when the request
/// presents it. `plombir-git create-admin` is the other, token-less way in: it
/// runs on the host, which is already the position of trust the token proves.
///
/// Cloning shares the secret: the server, every handler and the permit that
/// consumes it hold the same `Arc`.
#[derive(Clone, Default)]
pub struct SetupToken {
    inner: Option<Arc<SetupTokenInner>>,
}

struct SetupTokenInner {
    secret: String,
    /// Where the secret is kept on disk, removed once the bootstrap account
    /// exists so a stale token does not outlive its purpose.
    path: Option<PathBuf>,
}

impl SetupToken {
    /// The server's spelling: the bootstrap registration must present `secret`.
    pub fn required(secret: String, path: Option<PathBuf>) -> Self {
        Self {
            inner: Some(Arc::new(SetupTokenInner { secret, path })),
        }
    }

    /// No token guards the bootstrap registration.
    ///
    /// For an instance that already has accounts — the window is shut, there is
    /// nothing to guard — and for test harnesses whose fixtures register their
    /// own first user. A production server with an empty database must never
    /// be built this way; `plombir-git serve` decides between the two from the
    /// user table itself.
    pub fn not_required() -> Self {
        Self { inner: None }
    }

    /// Whether a bootstrap registration has to present a token at all.
    pub fn is_required(&self) -> bool {
        self.inner.is_some()
    }

    /// A fresh secret: 32 random bytes, base64url without padding, so it can be
    /// pasted into a form field, a header or a shell variable unchanged.
    pub fn generate_secret() -> String {
        use base64::Engine;
        use rand::RngCore;

        let mut bytes = [0_u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// The file a server keeps the token in, beside its encryption key file.
    pub fn path_beside(encryption_key_file: &Path) -> PathBuf {
        encryption_key_file.with_file_name(SETUP_TOKEN_FILE_NAME)
    }

    /// Whether `presented` is the secret. Constant-time on equal lengths, and a
    /// token of the wrong length is not the token.
    fn accepts(&self, presented: Option<&str>) -> bool {
        let Some(inner) = self.inner.as_ref() else {
            return true;
        };
        let Some(presented) = presented.map(str::trim).filter(|value| !value.is_empty()) else {
            return false;
        };
        presented.len() == inner.secret.len()
            && bool::from(presented.as_bytes().ct_eq(inner.secret.as_bytes()))
    }

    /// The token has done its one job: forget the file. The in-memory secret
    /// stays — harmless, since `authorize` never reaches the token check once
    /// a user row exists — so a concurrent clone keeps a consistent view.
    fn consume(&self) {
        let Some(path) = self.inner.as_ref().and_then(|inner| inner.path.as_deref()) else {
            return;
        };
        match std::fs::remove_file(path) {
            Ok(()) => tracing::info!(
                path = %path.display(),
                "the first administrator exists; the one-time setup token was removed"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                path = %path.display(),
                error = %error,
                "could not remove the one-time setup token; it no longer opens anything, delete it by hand"
            ),
        }
    }
}

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
    /// behaviour, and the default until security audit finding #13: a forge
    /// that is reachable before its operator has read the config must not be
    /// an account farm by default).
    Open,
    /// Nobody may create an account through the endpoint — except the very
    /// first one, on an instance that has never had a user, and only with the
    /// one-time [`SetupToken`]. See [`authorize`].
    ///
    /// The default. An install that never set `[auth].registration` and
    /// upgrades past this change becomes closed; its sign-up page says so, and
    /// `registration = "open"` restores the old behaviour in one line.
    #[default]
    Closed,
    /// Anyone may register, and the account exists once the link mailed to
    /// its address has been followed. Until then nothing is created, and the
    /// answer to a taken address is the same as to a free one — the only way
    /// to stop registration from telling strangers which addresses have
    /// accounts here (card_45f98ab2fe1a). Needs outbound mail; the server
    /// refuses to start with this mode and no `[smtp]`.
    ///
    /// The bootstrap account on an empty instance is still created at once:
    /// the operator registering first must not depend on mail working.
    VerifyEmail,
}

impl RegistrationMode {
    /// The values `[auth].registration` / `PLOMBIR_GIT_REGISTRATION` accept.
    pub const ACCEPTED: [&'static str; 3] = ["open", "closed", "verify-email"];

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
            "verify-email" => Ok(Self::VerifyEmail),
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
            Self::VerifyEmail => "verify-email",
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
    /// The token this bootstrap registration presented, to be consumed once
    /// its row is committed. `None` for every later registration.
    setup: Option<SetupToken>,
    confirm_email: bool,
}

impl RegistrationPermit {
    /// Whether this registration waits for its address to be proved before
    /// any account exists — [`RegistrationMode::VerifyEmail`], and not the
    /// bootstrap account.
    pub fn needs_email_confirmation(&self) -> bool {
        self.confirm_email
    }

    /// Whether this capability belongs to the account that initialises the
    /// instance. Kept crate-private so callers cannot choose their own role;
    /// [`authorize`] is the only constructor.
    pub(super) fn grants_instance_admin(&self) -> bool {
        self._bootstrap.is_some()
    }

    /// The registration's row is committed: retire the setup token and release
    /// the bootstrap lock. Dropping the permit releases the lock too; this is
    /// the spelling for the success path, where the token has been spent.
    pub(super) fn finish(mut self) {
        if let Some(setup) = self.setup.take() {
            setup.consume();
        }
    }
}

/// The answer [`authorize`] gives before a registration costs anything.
pub enum Decision {
    /// Proceed, holding the permit until the registration is done.
    Permit(RegistrationPermit),
    /// Refuse: the instance is closed and already has accounts.
    Closed,
    /// Refuse: the instance has no account yet, and the request did not carry
    /// the one-time [`SetupToken`] that the first registration needs.
    SetupTokenRequired,
}

impl Decision {
    /// The permit, for callers that treat every refusal alike.
    pub fn permit(self) -> Option<RegistrationPermit> {
        match self {
            Self::Permit(permit) => Some(permit),
            Self::Closed | Self::SetupTokenRequired => None,
        }
    }
}

/// Decide whether a self-service registration may proceed **before** the
/// request costs anything — no password hashing, no row written.
///
/// `presented_setup_token` is what the request carried (body field or
/// `X-Setup-Token` header); it is only looked at on an empty instance.
///
/// * `Ok(Decision::Permit(permit))` — proceed, holding `permit` until the
///   registration is done.
/// * `Ok(Decision::Closed)` / `Ok(Decision::SetupTokenRequired)` — refuse.
/// * `Err(_)` — the question could not be answered. The caller must turn this
///   into a 5xx, never into a registration: a database that cannot be counted
///   must not be read as "no users yet, let everyone in".
pub async fn authorize(
    db: &DatabaseConnection,
    mode: RegistrationMode,
    setup: &SetupToken,
    presented_setup_token: Option<&str>,
) -> Result<Decision> {
    // Every mode needs the same one-shot answer on an empty database: this is
    // where the first account receives the capability that makes the instance
    // operable. The guard stays inside that permit until its insert commits.
    let guard = BOOTSTRAP_LOCK.lock().await;
    if !rg_db::ops::user_ops::has_any(db).await? {
        if !setup.accepts(presented_setup_token) {
            drop(guard);
            return Ok(Decision::SetupTokenRequired);
        }
        return Ok(Decision::Permit(RegistrationPermit {
            _bootstrap: Some(guard),
            setup: setup.is_required().then(|| setup.clone()),
            confirm_email: false,
        }));
    }
    drop(guard);

    match mode {
        RegistrationMode::Open => Ok(Decision::Permit(RegistrationPermit {
            _bootstrap: None,
            setup: None,
            confirm_email: false,
        })),
        RegistrationMode::VerifyEmail => Ok(Decision::Permit(RegistrationPermit {
            _bootstrap: None,
            setup: None,
            confirm_email: true,
        })),
        RegistrationMode::Closed => Ok(Decision::Closed),
    }
}

/// Whether a self-service registration would be let through now — what the
/// sign-up link and page show (card_e1baa94866ed).
///
/// The same answer [`authorize`] gives, without its lock: a closed instance
/// still takes the account that initialises it, and only that one — with the
/// setup token, since finding #13. It is a hint for the UI, not a permit — the
/// register route asks [`authorize`] again, so a race between this read and a
/// first sign-up costs a `403`, not an account.
pub async fn accepts_registrations(
    db: &DatabaseConnection,
    mode: RegistrationMode,
) -> Result<bool> {
    if !mode.is_closed() {
        return Ok(true);
    }
    Ok(!rg_db::ops::user_ops::has_any(db).await?)
}

/// Whether the instance is still waiting for its first account — what the
/// register page asks before it shows a setup-token field.
pub async fn setup_required(db: &DatabaseConnection, setup: &SetupToken) -> Result<bool> {
    if !setup.is_required() {
        return Ok(false);
    }
    Ok(!rg_db::ops::user_ops::has_any(db).await?)
}

#[cfg(test)]
mod tests {
    fn no_directories() -> super::super::service::LdapDirectories<'static> {
        static POLICY: std::sync::OnceLock<crate::auth::ldap::LdapTransportPolicy> =
            std::sync::OnceLock::new();
        super::super::service::LdapDirectories {
            encryption_key: "test-encryption-key",
            transport_policy: POLICY.get_or_init(Default::default),
        }
    }

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

    /// Security audit finding #13: an instance nobody configured refuses
    /// strangers. The upgrade note that goes with it lives in deploy/README.md.
    #[test]
    fn the_default_is_closed_so_an_unconfigured_instance_refuses_strangers() {
        assert_eq!(RegistrationMode::default(), RegistrationMode::Closed);
        assert!(RegistrationMode::default().is_closed());
        assert_eq!(RegistrationMode::Closed.as_str(), "closed");
        assert_eq!(RegistrationMode::Open.as_str(), "open");
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

        let permit = authorize(
            &db,
            RegistrationMode::Open,
            &SetupToken::not_required(),
            None,
        )
        .await
        .unwrap()
        .permit()
        .expect("open registration is allowed");
        assert!(permit.grants_instance_admin());

        super::super::service::register(
            &db,
            permit,
            "founder",
            "founder@example.com",
            "Qz7$wRtm",
            "secret",
            no_directories(),
        )
        .await
        .expect("bootstrap registration");

        let permit = authorize(
            &db,
            RegistrationMode::Open,
            &SetupToken::not_required(),
            None,
        )
        .await
        .unwrap()
        .permit()
        .expect("open registration stays allowed");
        assert!(!permit.grants_instance_admin());
    }

    #[tokio::test]
    async fn a_closed_instance_admits_the_first_account_and_then_nobody() {
        let db = migrated_db().await;

        let permit = authorize(
            &db,
            RegistrationMode::Closed,
            &SetupToken::not_required(),
            None,
        )
        .await
        .expect("the count must be readable")
        .permit()
        .expect("an empty closed instance must be initialisable");

        super::super::service::register(
            &db,
            permit,
            "founder",
            "founder@example.com",
            "Qz7$wRtm",
            "secret",
            no_directories(),
        )
        .await
        .expect("bootstrap registration");

        assert!(
            authorize(
                &db,
                RegistrationMode::Closed,
                &SetupToken::not_required(),
                None
            )
            .await
            .unwrap()
            .permit()
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
        let permit = authorize(
            &db,
            RegistrationMode::Closed,
            &SetupToken::not_required(),
            None,
        )
        .await
        .expect("the count must be readable")
        .permit()
        .expect("an empty closed instance must be initialisable");
        let user = super::super::service::register(
            &db,
            permit,
            "founder",
            "founder@example.com",
            "Qz7$wRtm",
            "secret",
            no_directories(),
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
            authorize(
                &db,
                RegistrationMode::Closed,
                &SetupToken::not_required(),
                None
            )
            .await
            .unwrap()
            .permit()
            .is_none(),
            "a tombstone must not hand the instance back to the next stranger"
        );
    }

    fn required_token(dir: &tempfile::TempDir) -> (SetupToken, PathBuf, String) {
        let secret = SetupToken::generate_secret();
        let path = dir.path().join(SETUP_TOKEN_FILE_NAME);
        std::fs::write(&path, &secret).expect("write the token file");
        (
            SetupToken::required(secret.clone(), Some(path.clone())),
            path,
            secret,
        )
    }

    async fn bootstrap_with(db: &DatabaseConnection, permit: RegistrationPermit) {
        super::super::service::register(
            db,
            permit,
            "founder",
            "founder@example.com",
            "Qz7$wRtm",
            "secret",
            no_directories(),
        )
        .await
        .expect("bootstrap registration");
    }

    /// The headline: an empty instance is not first-come-first-served any more.
    #[tokio::test]
    async fn the_bootstrap_registration_requires_the_setup_token() {
        let dir = tempfile::tempdir().unwrap();
        let (setup, _, _) = required_token(&dir);
        let db = migrated_db().await;

        for mode in [
            RegistrationMode::Open,
            RegistrationMode::Closed,
            RegistrationMode::VerifyEmail,
        ] {
            assert!(
                matches!(
                    authorize(&db, mode, &setup, None).await.unwrap(),
                    Decision::SetupTokenRequired
                ),
                "{mode:?}: a missing token must be refused on an empty instance"
            );
            assert!(
                matches!(
                    authorize(&db, mode, &setup, Some("")).await.unwrap(),
                    Decision::SetupTokenRequired
                ),
                "{mode:?}: an empty token is a missing token"
            );
        }
    }

    #[tokio::test]
    async fn a_wrong_setup_token_is_refused_and_leaves_the_window_open() {
        let dir = tempfile::tempdir().unwrap();
        let (setup, path, secret) = required_token(&dir);
        let db = migrated_db().await;

        let mut wrong = secret.clone();
        wrong.replace_range(..1, if secret.starts_with('A') { "B" } else { "A" });
        assert!(matches!(
            authorize(&db, RegistrationMode::Closed, &setup, Some(&wrong))
                .await
                .unwrap(),
            Decision::SetupTokenRequired
        ));
        let truncated = &secret[..secret.len() - 1];
        assert!(matches!(
            authorize(&db, RegistrationMode::Closed, &setup, Some(truncated))
                .await
                .unwrap(),
            Decision::SetupTokenRequired
        ));
        assert!(path.exists(), "a refused attempt must not spend the token");
        assert!(
            setup_required(&db, &setup).await.unwrap(),
            "the instance is still waiting for its first account"
        );

        // The right one, with surrounding whitespace a paste may carry.
        let permit = authorize(
            &db,
            RegistrationMode::Closed,
            &setup,
            Some(&format!(" {secret}\n")),
        )
        .await
        .unwrap()
        .permit()
        .expect("the right token opens the bootstrap window");
        assert!(permit.grants_instance_admin());
    }

    #[tokio::test]
    async fn the_setup_token_is_consumed_by_the_bootstrap_account() {
        let dir = tempfile::tempdir().unwrap();
        let (setup, path, secret) = required_token(&dir);
        let db = migrated_db().await;

        let permit = authorize(&db, RegistrationMode::Closed, &setup, Some(&secret))
            .await
            .unwrap()
            .permit()
            .expect("the token admits the first account");
        bootstrap_with(&db, permit).await;

        assert!(
            !path.exists(),
            "the token file must be gone once it was used"
        );
        assert!(
            !setup_required(&db, &setup).await.unwrap(),
            "no setup is pending once an account exists"
        );
        // Presenting it again buys nothing: the window is shut, the mode rules.
        assert!(matches!(
            authorize(&db, RegistrationMode::Closed, &setup, Some(&secret))
                .await
                .unwrap(),
            Decision::Closed
        ));
    }

    #[tokio::test]
    async fn after_the_bootstrap_the_mode_decides_closed_refuses_open_admits() {
        let dir = tempfile::tempdir().unwrap();
        let (setup, _, secret) = required_token(&dir);
        let db = migrated_db().await;
        let permit = authorize(&db, RegistrationMode::Open, &setup, Some(&secret))
            .await
            .unwrap()
            .permit()
            .expect("the token admits the first account");
        bootstrap_with(&db, permit).await;

        assert!(
            matches!(
                authorize(&db, RegistrationMode::Closed, &setup, None)
                    .await
                    .unwrap(),
                Decision::Closed
            ),
            "closed: the bootstrap window closed behind the first account"
        );
        let permit = authorize(&db, RegistrationMode::Open, &setup, None)
            .await
            .unwrap()
            .permit()
            .expect("open: later registrations need no token");
        assert!(
            !permit.grants_instance_admin(),
            "the bootstrap capability was spent on the founder"
        );
        let permit = authorize(&db, RegistrationMode::VerifyEmail, &setup, None)
            .await
            .unwrap()
            .permit()
            .expect("verify-email: later registrations need no token either");
        assert!(permit.needs_email_confirmation());
    }

    #[test]
    fn a_generated_secret_is_url_safe_and_32_bytes_long() {
        let secret = SetupToken::generate_secret();
        assert_eq!(secret.len(), 43, "32 bytes, base64url, no padding");
        assert!(secret
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        assert_ne!(secret, SetupToken::generate_secret());
        assert_eq!(
            SetupToken::path_beside(Path::new("/data/encryption_key")),
            PathBuf::from("/data/setup_token")
        );
    }
}
