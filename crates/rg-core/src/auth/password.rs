//! Password hashing and verification using Argon2id.
//!
//! Argon2 is built to be expensive: tens of milliseconds of CPU and 19 MiB of
//! memory per pass, on purpose. That makes it the one piece of work an
//! anonymous caller can make this server do at will — every password door
//! (web login, SSH, `docker login` against `/v2/auth/token`) verifies a hash,
//! and the unknown-account branch burns a dummy one so the timing stays flat.
//! Run inline on a Tokio worker, a stream of wrong passwords pinned every
//! worker and stopped HTTP, git and SSH at once.
//!
//! So the public functions here are `async`, and none of them hashes on the
//! caller's thread. Each pass runs on the blocking pool under one process-wide
//! limiter ([`PasswordWorkSaturated`] once it is full): a fixed number of
//! passes at a time, a bounded queue in front of them, a bounded wait in that
//! queue, and a per-source share where the caller knows a trustworthy source
//! address. The synchronous kernels stay private, so a caller cannot reach
//! Argon2 without going through the limiter.
//!
//! Also includes a [`PasswordValidator`] for password strength checks
//! (Phase 22-D security hardening).

use anyhow::{Context, Result};
use argon2::{
    password_hash::{
        Error as PasswordHashError, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
    Argon2,
};
use rand_core::OsRng;
#[cfg(test)]
use std::cell::Cell;
use std::collections::{hash_map::Entry, HashMap};
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::Semaphore;

/// The *stored* hash could not be used to decide anything.
///
/// This is not a wrong password. A PHC string the verifier refuses to parse, or
/// one written by an algorithm this build does not have — a half-finished
/// migration, a truncated column, a restore from a forge that hashed with
/// bcrypt — is our data being broken, and the account holder can do nothing
/// about it. Folding it into "invalid credentials" gives that user a login that
/// fails forever while every log line says they mistyped their password.
///
/// Carried inside the `anyhow::Error` so the transport layer can tell the two
/// apart through any `.context(...)` a caller added, the same way
/// [`crate::error::NotFound`] travels up to the HTTP status mapping.
#[derive(Debug, Error)]
#[error("stored password hash is unusable: {reason}")]
pub struct UnusablePasswordHash {
    pub reason: String,
}

/// Shorthand for the `anyhow` form of [`UnusablePasswordHash`].
fn unusable_hash(reason: impl std::fmt::Display) -> anyhow::Error {
    anyhow::Error::new(UnusablePasswordHash {
        reason: reason.to_string(),
    })
}

/// The password limiter would not take one more Argon2 pass.
///
/// This is not a verdict on the credential: nothing was hashed, so nothing is
/// known about it. HTTP answers `503` — the server is busy, come back — and SSH,
/// which can only accept or reject, rejects. Neither may count it as a failed
/// attempt: during a flood the account owner's *correct* password lands here
/// too, and a strike for it would let the flood lock the owner out.
///
/// Carried inside the `anyhow::Error`, like [`UnusablePasswordHash`], so it
/// survives every `.context(...)` on the way to the transport.
#[derive(Debug, Error)]
#[error("password verification is at capacity: {reason}")]
pub struct PasswordWorkSaturated {
    pub reason: &'static str,
}

fn saturated(reason: &'static str) -> anyhow::Error {
    anyhow::Error::new(PasswordWorkSaturated { reason })
}

/// Queue places in front of each running pass. With a pass at ~15 ms, a full
/// queue drains in about half a second; anything past it is shed at once
/// rather than parked behind work the client will have given up on.
const QUEUE_PER_PERMIT: usize = 32;

/// Longest a pass may wait for a free slot. The queue bound already keeps the
/// wait short on a healthy host; this is the ceiling for one whose CPU is
/// contended so hard that a pass takes far longer than it should.
const MAX_QUEUE_WAIT: Duration = Duration::from_secs(5);

/// Passes one source address may have admitted (running or queued) at once.
/// It caps concurrency, not rate: a person, or a whole office behind one NAT,
/// never has more password checks *in flight* than this, while one address
/// flooding the door can no longer take every queue place for itself.
const PER_SOURCE_IN_FLIGHT: usize = 4;

/// The process-wide limiter every Argon2 pass goes through.
///
/// Sized at half the cores the process may use (at least one), so a flood
/// keeps the other half for everything else — Tokio's workers still have to
/// get scheduled to answer `/health`, serve git and finish the handshakes the
/// flood is not part of. Memory follows the same bound: 19 MiB a pass.
fn limiter() -> &'static PasswordWorkLimiter {
    static LIMITER: OnceLock<PasswordWorkLimiter> = OnceLock::new();
    LIMITER.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        let permits = (cores / 2).max(1);
        PasswordWorkLimiter::new(
            permits,
            permits * QUEUE_PER_PERMIT,
            PER_SOURCE_IN_FLIGHT,
            MAX_QUEUE_WAIT,
        )
    })
}

/// Bounded admission in front of Tokio's blocking pool for Argon2 passes.
///
/// `spawn_blocking` alone moves the work off the async workers, but the pool
/// grows to 512 threads: a flood would then run 512 passes at once — every
/// core pegged and 10 GiB of Argon2 memory — and the workers would starve for
/// CPU instead of for threads. The semaphore bounds the passes, `capacity`
/// bounds the queue in front of it.
struct PasswordWorkLimiter {
    passes: Arc<Semaphore>,
    /// Running plus queued passes the limiter admits before it sheds.
    capacity: usize,
    per_source: usize,
    max_wait: Duration,
    admitted: Arc<Mutex<Admitted>>,
}

#[derive(Default)]
struct Admitted {
    total: usize,
    by_source: HashMap<IpAddr, usize>,
}

fn lock_admitted(admitted: &Mutex<Admitted>) -> MutexGuard<'_, Admitted> {
    // The critical sections only add and subtract; a panic cannot leave the
    // counts half-written, so a poisoned lock still holds the truth.
    admitted.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One admitted pass. Dropping it gives the place back.
struct Admission {
    admitted: Arc<Mutex<Admitted>>,
    source: Option<IpAddr>,
}

impl Drop for Admission {
    fn drop(&mut self) {
        let mut admitted = lock_admitted(&self.admitted);
        admitted.total -= 1;
        if let Some(source) = self.source {
            if let Entry::Occupied(mut in_flight) = admitted.by_source.entry(source) {
                *in_flight.get_mut() -= 1;
                if *in_flight.get() == 0 {
                    in_flight.remove();
                }
            }
        }
    }
}

impl PasswordWorkLimiter {
    fn new(passes: usize, queue: usize, per_source: usize, max_wait: Duration) -> Self {
        let passes = passes.max(1);
        Self {
            passes: Arc::new(Semaphore::new(passes)),
            capacity: passes + queue,
            per_source: per_source.max(1),
            max_wait,
            admitted: Arc::default(),
        }
    }

    /// Take a place, or say at once why there is none.
    fn admit(&self, source: Option<IpAddr>) -> Result<Admission> {
        let mut admitted = lock_admitted(&self.admitted);
        if admitted.total >= self.capacity {
            return Err(saturated("every slot and queue place is taken"));
        }
        if let Some(source) = source {
            let in_flight = admitted.by_source.entry(source).or_insert(0);
            if *in_flight >= self.per_source {
                return Err(saturated(
                    "this source address already has its share of checks in flight",
                ));
            }
            *in_flight += 1;
        }
        admitted.total += 1;
        Ok(Admission {
            admitted: Arc::clone(&self.admitted),
            source,
        })
    }

    /// Run `work` on the blocking pool once a slot is free.
    ///
    /// `Err` carrying [`PasswordWorkSaturated`] means `work` never ran.
    async fn run<T, F>(&self, source: Option<IpAddr>, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce() -> T + Send + 'static,
    {
        let admission = self.admit(source)?;
        let pass =
            match tokio::time::timeout(self.max_wait, Arc::clone(&self.passes).acquire_owned())
                .await
            {
                Ok(Ok(pass)) => pass,
                Ok(Err(closed)) => return Err(anyhow::Error::new(closed)),
                Err(_elapsed) => return Err(saturated("no slot came free within the wait budget")),
            };
        // Both the slot and the admission travel into the closure. A caller
        // that gives up — a client that hangs up mid-login — drops this
        // future, but a pass that already started runs to its end on the
        // pool, and its slot has to stay taken until it does; released with
        // the future, abandoned passes would pile up past the bound.
        tokio::task::spawn_blocking(move || {
            let _held = (pass, admission);
            work()
        })
        .await
        .context("password hashing task failed")
    }
}

/// Run one password kernel through the process-wide limiter.
async fn off_runtime<T, F>(source: Option<IpAddr>, work: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (output, burned) = limiter().run(source, move || counting_burns(work)).await?;
    credit_burns(burned);
    Ok(output)
}

/// Hash a plaintext password. Returns a PHC-format string (includes algorithm,
/// params, salt, hash). Runs off the async runtime, under the password limiter.
pub async fn hash_password(password: &str) -> Result<String> {
    let password = password.to_owned();
    off_runtime(None, move || hash_password_blocking(&password)).await?
}

fn hash_password_blocking(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("password hashing failed: {}", e))?;
    Ok(hash.to_string())
}

/// Verify a plaintext password against a stored PHC hash, off the async
/// runtime and under the password limiter.
///
/// `Ok(false)` means one thing only: the password does not match. Every other
/// way the verification can end — an unparseable PHC string, an algorithm this
/// build cannot verify, parameters it rejects — is [`UnusablePasswordHash`] and
/// comes back as `Err`, because none of them is the caller's doing. So is
/// [`PasswordWorkSaturated`], for a check that never ran.
///
/// The distinction is load-bearing: `.is_ok()` over the whole verification
/// reported "wrong password" for a hash that was never checked at all, so a
/// migration that moved the hashes to different Argon2 parameters (or left a
/// column half-written) locked accounts out with nothing in the log to say why.
pub async fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let password = password.to_owned();
    let hash = hash.to_owned();
    off_runtime(None, move || verify_password_blocking(&password, &hash)).await?
}

fn verify_password_blocking(password: &str, hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(hash).map_err(unusable_hash)?;
    match Argon2::default().verify_password(password.as_bytes(), &parsed) {
        Ok(()) => Ok(true),
        // The one rejection that is about the password itself.
        Err(PasswordHashError::Password) => Ok(false),
        Err(error) => Err(unusable_hash(error)),
    }
}

/// Stand-in hash verified when the account does not exist.
///
/// Produced by [`hash_password`], so it carries exactly the `Argon2::default()`
/// parameters real hashes are stored with; `dummy_hash_uses_current_default_params`
/// fails loudly if a dependency bump ever moves those defaults apart.
const DUMMY_PASSWORD_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$j10MFJnxYC8YdoKn2f0/pw$hiB3C8eJEitR5h7E+0P51MvixnERtz8bhn3hwLOHcq0";

#[cfg(test)]
thread_local! {
    /// Dummy verifications burned *on this thread*, for the tests that assert
    /// the work was spent.
    ///
    /// Per-thread rather than one process-global counter. The crate's tests
    /// compile into a single binary that runs them in parallel threads, and
    /// three of them burn dummy verifications — so a shared counter answers one
    /// test's "how many did *I* spend?" with another test's work, and one
    /// test's reset zeroes a count another is mid-way through asserting.
    /// Measured before this change: 1 red run in 30 of
    /// `cargo test -p rg-core --lib -- burns wrong_password_against`.
    ///
    /// The burn itself now happens on a blocking-pool thread, so
    /// [`off_runtime`] carries the count that thread spent back to the thread
    /// that awaited it (see [`counting_burns`]). A test therefore still reads
    /// its own work — as long as it awaits on one thread, which a
    /// `current_thread` runtime guarantees and a `multi_thread` one does not.
    static DUMMY_VERIFICATION_BURNS: Cell<usize> = const { Cell::new(0) };
}

/// Run `work` and report how many dummy burns it spent on this thread. Always
/// zero outside tests; the count exists only so a test can see the work.
fn counting_burns<T>(work: impl FnOnce() -> T) -> (T, usize) {
    #[cfg(test)]
    let before = dummy_verification_burns();
    let output = work();
    #[cfg(test)]
    let burned = dummy_verification_burns() - before;
    #[cfg(not(test))]
    let burned = 0;
    (output, burned)
}

/// Add burns spent on a pool thread to the awaiting thread's count.
fn credit_burns(burned: usize) {
    #[cfg(test)]
    DUMMY_VERIFICATION_BURNS.with(|burns| burns.set(burns.get() + burned));
    #[cfg(not(test))]
    debug_assert_eq!(burned, 0);
}

/// Verify `password` against `stored_hash`, spending the same Argon2 work when
/// there is no hash to verify against.
///
/// Argon2 is deliberately expensive — tens of milliseconds — so an
/// authentication path that skips it for unknown accounts answers "does this
/// account exist?" in its response time, no matter how carefully the response
/// *body* is unified. Callers that resolve a user before checking the password
/// must pass `None` instead of short-circuiting, so both outcomes cost the same.
/// Both go through the same limiter too, so a shed answer cannot tell them
/// apart either.
///
/// `source` is the client address *when the transport knows it for certain* —
/// the TCP peer of an SSH session. It caps how many checks one address may
/// have in flight. Pass `None` where the only address on hand is one the client
/// could have written itself.
///
/// `Ok(false)` covers both rejections the caller is allowed to answer with:
/// there is no such account, or the password is wrong. An unusable stored hash
/// is [`UnusablePasswordHash`] and comes back as `Err` — every caller must
/// report it (with the account it happened on) instead of folding it into a
/// rejection, because a `false` there is an answer we never actually computed.
/// [`PasswordWorkSaturated`] is the same kind of non-answer.
///
/// With `stored_hash = None` the result is always `Ok(false)` once the work
/// ran: there is nothing to verify, only work to spend.
pub async fn verify_password_or_dummy(
    password: &str,
    stored_hash: Option<&str>,
    source: Option<IpAddr>,
) -> Result<bool> {
    let password = password.to_owned();
    let stored_hash = stored_hash.map(str::to_owned);
    off_runtime(source, move || {
        verify_password_or_dummy_blocking(&password, stored_hash.as_deref())
    })
    .await?
}

fn verify_password_or_dummy_blocking(password: &str, stored_hash: Option<&str>) -> Result<bool> {
    match stored_hash {
        Some(hash) => verify_password_blocking(password, hash),
        None => {
            burn_dummy_verification_blocking(password);
            Ok(false)
        }
    }
}

/// Spend one Argon2 verification with nothing to verify against.
///
/// For the branches that reject before they ever reach a stored hash — an
/// account on a provider no password reaches, an LDAP rejection that never got
/// as far as a bind — and therefore would otherwise answer in microseconds
/// while a real account pays tens of milliseconds. There is no verdict to
/// return; the work *is* the point. `Err` is [`PasswordWorkSaturated`] (or a
/// lost pool task): the caller must answer with it rather than with its own
/// rejection, or a full limiter would answer unknown accounts fast and known
/// ones with a `503`.
pub async fn burn_dummy_verification(password: &str) -> Result<()> {
    let password = password.to_owned();
    off_runtime(None, move || burn_dummy_verification_blocking(&password)).await
}

fn burn_dummy_verification_blocking(password: &str) {
    // `black_box` keeps the optimizer from noticing the result is thrown away.
    let verified = verify_password_blocking(password, DUMMY_PASSWORD_HASH).is_ok();
    #[cfg(test)]
    DUMMY_VERIFICATION_BURNS.with(|burns| burns.set(burns.get() + 1));
    std::hint::black_box(verified);
}

/// Zero this thread's burn count. Call it at the start of the assertion, not
/// once for the whole suite: the count belongs to the thread, not the process.
#[cfg(test)]
pub(crate) fn reset_dummy_verification_burns() {
    DUMMY_VERIFICATION_BURNS.with(|burns| burns.set(0));
}

/// Dummy verifications burned on the calling thread since the last reset.
#[cfg(test)]
pub(crate) fn dummy_verification_burns() -> usize {
    DUMMY_VERIFICATION_BURNS.with(|burns| burns.get())
}

// ── Password Strength Validation (Phase 22-D) ──────────────────────────────

/// Password validation error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordError {
    TooShort { min: usize, got: usize },
    TooLong { max: usize, got: usize },
    NoUppercase,
    NoLowercase,
    NoDigit,
    NoSpecialChar,
    ContainsWhitespace,
    TooCommon(String),
    ContainsUsername,
}

impl std::fmt::Display for PasswordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort { min, got } => write!(
                f,
                "password must be at least {} characters (got {})",
                min, got
            ),
            Self::TooLong { max, got } => write!(
                f,
                "password must be at most {} characters (got {})",
                max, got
            ),
            Self::NoUppercase => write!(f, "password must contain at least one uppercase letter"),
            Self::NoLowercase => write!(f, "password must contain at least one lowercase letter"),
            Self::NoDigit => write!(f, "password must contain at least one digit"),
            Self::NoSpecialChar => write!(
                f,
                "password must contain at least one special character (!@#$%^&*...)"
            ),
            Self::ContainsWhitespace => write!(f, "password must not contain whitespace"),
            Self::TooCommon(pwd) => write!(
                f,
                "password '{}' is too common — please choose a stronger one",
                pwd
            ),
            Self::ContainsUsername => write!(f, "password must not contain the username"),
        }
    }
}

impl std::error::Error for PasswordError {}

/// Common/weak passwords to reject. Lowercase for case-insensitive matching.
/// Top 50 most common passwords from various breach analyses.
const COMMON_PASSWORDS: &[&str] = &[
    "password",
    "123456",
    "12345678",
    "qwerty",
    "abc123",
    "monkey",
    "1234567",
    "letmein",
    "trustno1",
    "dragon",
    "baseball",
    "iloveyou",
    "master",
    "sunshine",
    "ashley",
    "michael",
    "shadow",
    "123123",
    "654321",
    "superman",
    "qazwsx",
    "football",
    "password1",
    "password123",
    "welcome",
    "hello",
    "charlie",
    "donald",
    "admin",
    "administrator",
    "root",
    "toor",
    "pass",
    "test",
    "guest",
    "info",
    "mysql",
    "user",
    "ftp",
    "pi",
    "puppet",
    "ansible",
    "ec2-user",
    "vagrant",
    "ubuntu",
    "admin123",
    "root123",
    "test123",
];

/// Password validator with configurable rules.
#[derive(Debug, Clone)]
pub struct PasswordValidator {
    pub min_length: usize,
    pub max_length: usize,
    pub require_uppercase: bool,
    pub require_lowercase: bool,
    pub require_digit: bool,
    pub require_special: bool,
    pub reject_common: bool,
    pub reject_username: bool,
}

impl Default for PasswordValidator {
    fn default() -> Self {
        Self {
            min_length: 8,
            max_length: 128,
            require_uppercase: true,
            require_lowercase: true,
            require_digit: true,
            require_special: true,
            reject_common: true,
            reject_username: true,
        }
    }
}

impl PasswordValidator {
    /// Create a standard-strength validator (8+ chars, mixed case, digit, special).
    pub fn standard() -> Self {
        Self::default()
    }

    /// Create a strict validator (12+ chars, all requirements).
    pub fn strict() -> Self {
        Self {
            min_length: 12,
            ..Self::default()
        }
    }

    /// Create a lenient validator (only minimum length, no other rules).
    pub fn lenient() -> Self {
        Self {
            min_length: 6,
            require_uppercase: false,
            require_lowercase: false,
            require_digit: false,
            require_special: false,
            reject_common: false,
            reject_username: false,
            ..Self::default()
        }
    }

    /// Validate a password. Returns Ok(()) on success, Err on failure.
    pub fn validate(&self, password: &str) -> Result<(), PasswordError> {
        // Length checks
        if password.len() < self.min_length {
            return Err(PasswordError::TooShort {
                min: self.min_length,
                got: password.len(),
            });
        }
        if password.len() > self.max_length {
            return Err(PasswordError::TooLong {
                max: self.max_length,
                got: password.len(),
            });
        }

        // Whitespace check
        if password.chars().any(|c| c.is_whitespace()) {
            return Err(PasswordError::ContainsWhitespace);
        }

        // Character class checks
        if self.require_uppercase && !password.chars().any(|c| c.is_ascii_uppercase()) {
            return Err(PasswordError::NoUppercase);
        }
        if self.require_lowercase && !password.chars().any(|c| c.is_ascii_lowercase()) {
            return Err(PasswordError::NoLowercase);
        }
        if self.require_digit && !password.chars().any(|c| c.is_ascii_digit()) {
            return Err(PasswordError::NoDigit);
        }
        if self.require_special {
            // Common special characters
            const SPECIAL: &str = "!@#$%^&*()_+-=[]{}|;:,.<>?/~`'\"\\";
            if !password.chars().any(|c| SPECIAL.contains(c)) {
                return Err(PasswordError::NoSpecialChar);
            }
        }

        // Common password check (case-insensitive, with number/special suffix tolerance)
        if self.reject_common {
            let lower = password.to_lowercase();
            // Strip trailing digits and special chars to catch patterns like "password1!" or "password!!!"
            let stripped: String = lower
                .chars()
                .take_while(|c| c.is_ascii_alphabetic())
                .collect();
            if !stripped.is_empty() && COMMON_PASSWORDS.iter().any(|p| stripped == *p) {
                return Err(PasswordError::TooCommon(password.to_string()));
            }
            // Also check exact match for short common passwords like "123456"
            if COMMON_PASSWORDS.iter().any(|p| lower == *p) {
                return Err(PasswordError::TooCommon(password.to_string()));
            }
        }

        Ok(())
    }

    /// Validate a password against a username (must not contain the username).
    pub fn validate_with_username(
        &self,
        password: &str,
        username: &str,
    ) -> Result<(), PasswordError> {
        self.validate(password)?;
        if self.reject_username
            && !username.is_empty()
            && password.to_lowercase().contains(&username.to_lowercase())
        {
            return Err(PasswordError::ContainsUsername);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_hash_and_verify() {
        let hash = hash_password("hunter2").await.unwrap();
        assert!(verify_password("hunter2", &hash).await.unwrap());
        assert!(!verify_password("wrong", &hash).await.unwrap());
    }

    /// The dummy hash only masks the "unknown account" branch while it costs
    /// what a real verification costs — i.e. while its parameters are still the
    /// ones `hash_password` writes. An `argon2` bump that changes the defaults
    /// must break here, not silently re-open the oracle.
    #[test]
    fn dummy_hash_uses_current_default_params() {
        let parsed = PasswordHash::new(DUMMY_PASSWORD_HASH).expect("dummy hash must be valid PHC");
        let params = argon2::Params::try_from(&parsed).expect("dummy hash must carry params");
        let default = Argon2::default();

        assert_eq!(parsed.algorithm.as_str(), "argon2id");
        assert_eq!(params.m_cost(), default.params().m_cost());
        assert_eq!(params.t_cost(), default.params().t_cost());
        assert_eq!(params.p_cost(), default.params().p_cost());
    }

    #[tokio::test]
    async fn verify_password_or_dummy_burns_once_for_unknown_accounts() {
        let real = hash_password("correct horse battery staple").await.unwrap();

        reset_dummy_verification_burns();
        assert!(
            !verify_password_or_dummy("wrong password", Some(real.as_str()), None)
                .await
                .unwrap()
        );
        assert_eq!(
            dummy_verification_burns(),
            0,
            "known accounts must spend their real hash instead of the dummy one"
        );

        assert!(!verify_password_or_dummy("wrong password", None, None)
            .await
            .unwrap());
        assert_eq!(
            dummy_verification_burns(),
            1,
            "unknown accounts must spend exactly one dummy verification"
        );
    }

    /// A hash that cannot be parsed is not a wrong password. Reporting it as
    /// one is what let a broken `password_hash` column lock an account out
    /// while every caller kept answering "invalid credentials".
    #[tokio::test]
    async fn unparseable_stored_hash_is_an_error_not_a_rejection() {
        // Through the async door on purpose: the type has to survive the trip
        // through the blocking pool, or every caller's downcast goes blind.
        let error = verify_password("hunter2", "not-a-phc-string")
            .await
            .expect_err("a hash that is not PHC at all cannot yield a verdict");
        assert!(
            error.downcast_ref::<UnusablePasswordHash>().is_some(),
            "callers classify on the type, not the message: {error:#}"
        );
    }

    /// The migration case the classification exists for: a well-formed PHC
    /// string from *another* algorithm parses fine, and `Argon2` then refuses
    /// it — for a reason that has nothing to do with the password supplied.
    #[test]
    fn foreign_algorithm_hash_is_an_error_not_a_rejection() {
        // A valid scrypt PHC string (the shape a bcrypt/scrypt-era import or a
        // half-finished rehash leaves behind), never produced by this build.
        let foreign = "$scrypt$ln=16,r=8,p=1$aaaaaaaaaaaaaaaa$\
                       YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE";
        // Guards the test's own premise: this must fail *past* the parser, in
        // the verification, or it is only re-testing the case above.
        PasswordHash::new(foreign).expect("the fixture must be well-formed PHC");
        let error = verify_password_blocking("hunter2", foreign)
            .expect_err("a hash this build cannot verify cannot yield a verdict");
        assert!(
            error.downcast_ref::<UnusablePasswordHash>().is_some(),
            "expected UnusablePasswordHash, got: {error:#}"
        );
    }

    /// The other half of the split: a hash we *can* verify still answers
    /// `Ok(false)` for a wrong password, with or without the dummy branch.
    #[tokio::test]
    async fn wrong_password_against_a_usable_hash_stays_a_plain_rejection() {
        let hash = hash_password("hunter2").await.unwrap();
        assert!(!verify_password("wrong", &hash).await.unwrap());
        assert!(
            !verify_password_or_dummy("wrong", Some(hash.as_str()), None)
                .await
                .unwrap()
        );
        assert!(
            verify_password_or_dummy("hunter2", Some(hash.as_str()), None)
                .await
                .unwrap()
        );
        // No account to verify against is a rejection, never an error.
        assert!(!verify_password_or_dummy("hunter2", None, None)
            .await
            .unwrap());
    }

    #[test]
    fn test_password_validator_default() {
        let v = PasswordValidator::default();
        assert!(v.validate("Str0ng!Pass").is_ok());
        assert!(v.validate("weak").is_err());
        assert!(v.validate("alllowercase1!").is_err()); // no uppercase
        assert!(v.validate("ALLUPPERCASE1!").is_err()); // no lowercase
        assert!(v.validate("NoDigits!Pass").is_err()); // no digit
        assert!(v.validate("NoSpecial1Pass").is_err()); // no special
    }

    #[test]
    fn test_password_validator_too_common() {
        let v = PasswordValidator::default();
        assert!(v.validate("Good!Pass1").is_ok());
        // Common passwords with proper char classes should still be rejected
        assert!(matches!(
            v.validate("Password1!"),
            Err(PasswordError::TooCommon(_))
        ));
        assert!(matches!(
            v.validate("Admin123!"),
            Err(PasswordError::TooCommon(_))
        ));
        assert!(matches!(
            v.validate("Qwerty1!"),
            Err(PasswordError::TooCommon(_))
        ));
        assert!(matches!(
            v.validate("Welcome1!"),
            Err(PasswordError::TooCommon(_))
        ));
        assert!(matches!(
            v.validate("Test123!"),
            Err(PasswordError::TooCommon(_))
        ));
    }

    #[test]
    fn test_password_validator_username_check() {
        let v = PasswordValidator::default();
        assert!(v.validate_with_username("Str0ng!Pass", "alice").is_ok());
        assert!(matches!(
            v.validate_with_username("Alice!123", "alice"),
            Err(PasswordError::ContainsUsername)
        ));
    }

    #[test]
    fn test_password_validator_strict() {
        let v = PasswordValidator::strict();
        assert!(v.validate("VeryStr0ng!Pass").is_ok());
        assert!(matches!(
            v.validate("Str0ng!Pass"),
            Err(PasswordError::TooShort { min: 12, .. })
        ));
    }

    #[test]
    fn test_password_validator_lenient() {
        let v = PasswordValidator::lenient();
        assert!(v.validate("simple").is_ok());
        assert!(v.validate("weak").is_err()); // too short
    }

    #[test]
    fn test_password_validator_whitespace() {
        let v = PasswordValidator::default();
        assert!(matches!(
            v.validate("Str0ng! Pass"),
            Err(PasswordError::ContainsWhitespace)
        ));
        assert!(matches!(
            v.validate("Str0ng\tPass"),
            Err(PasswordError::ContainsWhitespace)
        ));
    }

    #[test]
    fn test_password_validator_length_limits() {
        let v = PasswordValidator::default();
        let long = "A".repeat(200);
        assert!(matches!(
            v.validate(&long),
            Err(PasswordError::TooLong { .. })
        ));
    }

    // ── The limiter in front of Argon2 ──────────────────────────────────────

    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn shed_reason(error: &anyhow::Error) -> Option<&'static str> {
        error
            .downcast_ref::<PasswordWorkSaturated>()
            .map(|shed| shed.reason)
    }

    /// Occupy one slot of `limiter` until the returned sender is dropped or
    /// sent to. Returns once the work is actually running on the pool.
    async fn hold_a_slot(
        limiter: &Arc<PasswordWorkLimiter>,
        source: Option<IpAddr>,
    ) -> (
        std::sync::mpsc::Sender<()>,
        tokio::task::JoinHandle<Result<()>>,
    ) {
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let held = {
            let limiter = Arc::clone(limiter);
            tokio::spawn(async move {
                limiter
                    .run(source, move || {
                        started_tx.send(()).expect("the test awaits the start");
                        // Either a release or the sender going away ends it.
                        match release_rx.recv() {
                            Ok(()) | Err(std::sync::mpsc::RecvError) => {}
                        }
                    })
                    .await
            })
        };
        started_rx.await.expect("the held pass starts");
        (release_tx, held)
    }

    /// The whole point of the module: a burst of password checks must leave
    /// the runtime thread free. On a `current_thread` runtime there is exactly
    /// one worker, so inline Argon2 makes the burst one uninterrupted poll and
    /// the heartbeat cannot beat even once until it is over.
    #[tokio::test]
    async fn an_argon2_burst_leaves_the_runtime_thread_free() {
        let beats = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let heartbeat = tokio::spawn({
            let beats = Arc::clone(&beats);
            let stop = Arc::clone(&stop);
            async move {
                while !stop.load(Ordering::SeqCst) {
                    beats.fetch_add(1, Ordering::SeqCst);
                    tokio::task::yield_now().await;
                }
            }
        });
        // Let the heartbeat get going before the burst is polled.
        tokio::task::yield_now().await;

        let before = beats.load(Ordering::SeqCst);
        let verdicts = futures::future::join_all(
            (0..4).map(|_| verify_password_or_dummy("a guess", None, None)),
        )
        .await;
        let during = beats.load(Ordering::SeqCst) - before;
        stop.store(true, Ordering::SeqCst);
        heartbeat.await.expect("heartbeat joins");

        for verdict in verdicts {
            assert!(!verdict.expect("an unknown account is a plain rejection"));
        }
        assert!(
            during > 0,
            "the runtime thread never got to run anything else while four Argon2 passes ran"
        );
    }

    /// The semaphore is the bound on CPU and on Argon2's memory: however many
    /// callers arrive, no more than `passes` hashes run at once.
    #[tokio::test]
    async fn the_limiter_never_runs_more_passes_than_it_has_slots() {
        const SLOTS: usize = 2;
        let limiter = Arc::new(PasswordWorkLimiter::new(
            SLOTS,
            64,
            64,
            Duration::from_secs(60),
        ));
        let running = Arc::new(AtomicUsize::new(0));
        let high_water = Arc::new(AtomicUsize::new(0));

        let passes = (0..12).map(|_| {
            let limiter = Arc::clone(&limiter);
            let running = Arc::clone(&running);
            let high_water = Arc::clone(&high_water);
            async move {
                limiter
                    .run(None, move || {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        high_water.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(15));
                        running.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await
            }
        });
        for pass in futures::future::join_all(passes).await {
            pass.expect("every pass fits the queue and runs");
        }

        let high_water = high_water.load(Ordering::SeqCst);
        assert!(
            (1..=SLOTS).contains(&high_water),
            "{high_water} passes ran at once with {SLOTS} slots"
        );
    }

    /// A full limiter answers at once and never runs the work. Waiting here
    /// instead would park the flood in memory and hand the real user a timeout.
    #[tokio::test]
    async fn a_full_limiter_sheds_at_once_without_running_the_work() {
        let limiter = Arc::new(PasswordWorkLimiter::new(1, 0, 8, Duration::from_secs(60)));
        let (release, held) = hold_a_slot(&limiter, None).await;

        let ran = Arc::new(AtomicBool::new(false));
        let shed = tokio::time::timeout(
            Duration::from_secs(2),
            limiter.run(None, {
                let ran = Arc::clone(&ran);
                move || ran.store(true, Ordering::SeqCst)
            }),
        )
        .await
        .expect("a full limiter must answer at once, not queue past its capacity")
        .expect_err("there is no place left for this pass");
        assert_eq!(
            shed_reason(&shed),
            Some("every slot and queue place is taken"),
            "{shed:#}"
        );
        assert!(!ran.load(Ordering::SeqCst), "shed work must never have run");

        release.send(()).expect("held pass waits for release");
        held.await.expect("held pass joins").expect("held pass ran");
        // The place came back with the pass that held it.
        limiter
            .run(None, || ())
            .await
            .expect("a freed slot admits the next pass");
    }

    /// A queued pass gives up after the wait budget instead of waiting for as
    /// long as the slot ahead of it stays busy.
    #[tokio::test]
    async fn a_queued_pass_gives_up_after_the_wait_budget() {
        let limiter = Arc::new(PasswordWorkLimiter::new(1, 4, 8, Duration::from_millis(50)));
        let (release, held) = hold_a_slot(&limiter, None).await;

        let ran = Arc::new(AtomicBool::new(false));
        let shed = tokio::time::timeout(
            Duration::from_secs(10),
            limiter.run(None, {
                let ran = Arc::clone(&ran);
                move || ran.store(true, Ordering::SeqCst)
            }),
        )
        .await
        .expect("the wait budget, not the slot ahead, decides how long a pass queues")
        .expect_err("no slot came free in time");
        assert_eq!(
            shed_reason(&shed),
            Some("no slot came free within the wait budget"),
            "{shed:#}"
        );
        assert!(!ran.load(Ordering::SeqCst));

        release.send(()).expect("held pass waits for release");
        held.await.expect("held pass joins").expect("held pass ran");
    }

    /// One address cannot take every place: past its share it is shed while
    /// other addresses — and callers with no trustworthy address — still get in.
    #[tokio::test]
    async fn one_source_cannot_take_more_than_its_share() {
        let flooder: IpAddr = "198.51.100.7".parse().unwrap();
        let bystander: IpAddr = "203.0.113.9".parse().unwrap();
        let limiter = Arc::new(PasswordWorkLimiter::new(8, 8, 1, Duration::from_secs(60)));
        let (release, held) = hold_a_slot(&limiter, Some(flooder)).await;

        let shed = limiter
            .run(Some(flooder), || ())
            .await
            .expect_err("the flooder already has its one check in flight");
        assert_eq!(
            shed_reason(&shed),
            Some("this source address already has its share of checks in flight"),
            "{shed:#}"
        );
        limiter
            .run(Some(bystander), || ())
            .await
            .expect("another address is not held to the flooder's share");
        limiter
            .run(None, || ())
            .await
            .expect("a caller without an address is held only to the global bound");

        release.send(()).expect("held pass waits for release");
        held.await.expect("held pass joins").expect("held pass ran");
        limiter
            .run(Some(flooder), || ())
            .await
            .expect("the share comes back when the check ends");
    }

    /// A caller that hangs up does not take its pass with it: the Argon2 work
    /// already on the pool keeps running, so its slot must stay taken until the
    /// work really ends — otherwise every abandoned login frees a slot early
    /// and the passes running at once grow without bound.
    #[tokio::test]
    async fn an_abandoned_caller_keeps_its_slot_until_the_pass_ends() {
        let limiter = Arc::new(PasswordWorkLimiter::new(1, 0, 8, Duration::from_secs(60)));
        let (release, held) = hold_a_slot(&limiter, None).await;
        held.abort();
        assert!(
            held.await
                .expect_err("the caller was aborted")
                .is_cancelled(),
            "the caller must really be gone for this to test anything"
        );

        let shed = limiter
            .run(None, || ())
            .await
            .expect_err("the abandoned pass is still running and still holds the slot");
        assert!(shed_reason(&shed).is_some(), "{shed:#}");

        release
            .send(())
            .expect("the abandoned pass still waits for release");
        // The pass ends on the pool on its own schedule; the slot follows it.
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match limiter.run(None, || ()).await {
                    Ok(()) => break,
                    Err(error) if shed_reason(&error).is_some() => {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(error) => panic!("unexpected limiter failure: {error:#}"),
                }
            }
        })
        .await
        .expect("the slot is released once the abandoned pass ends");
    }
}
