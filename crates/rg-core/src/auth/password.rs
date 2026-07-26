//! Password hashing and verification using Argon2id.
//!
//! Also includes a [`PasswordValidator`] for password strength checks
//! (Phase 22-D security hardening).

use anyhow::Result;
use argon2::{
    password_hash::{
        Error as PasswordHashError, PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
    Argon2,
};
use rand_core::OsRng;
use thiserror::Error;

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

/// Hash a plaintext password. Returns a PHC-format string (includes algorithm, params, salt, hash).
pub fn hash_password(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("password hashing failed: {}", e))?;
    Ok(hash.to_string())
}

/// Verify a plaintext password against a stored PHC hash.
///
/// `Ok(false)` means one thing only: the password does not match. Every other
/// way the verification can end — an unparseable PHC string, an algorithm this
/// build cannot verify, parameters it rejects — is [`UnusablePasswordHash`] and
/// comes back as `Err`, because none of them is the caller's doing.
///
/// The distinction is load-bearing: `.is_ok()` over the whole verification
/// reported "wrong password" for a hash that was never checked at all, so a
/// migration that moved the hashes to different Argon2 parameters (or left a
/// column half-written) locked accounts out with nothing in the log to say why.
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
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

/// Verify `password` against `stored_hash`, spending the same Argon2 work when
/// there is no hash to verify against.
///
/// Argon2 is deliberately expensive — tens of milliseconds — so an
/// authentication path that skips it for unknown accounts answers "does this
/// account exist?" in its response time, no matter how carefully the response
/// *body* is unified. Callers that resolve a user before checking the password
/// must pass `None` instead of short-circuiting, so both outcomes cost the same.
///
/// `Ok(false)` covers both rejections the caller is allowed to answer with:
/// there is no such account, or the password is wrong. An unusable stored hash
/// is [`UnusablePasswordHash`] and comes back as `Err` — every caller must
/// report it (with the account it happened on) instead of folding it into a
/// rejection, because a `false` there is an answer we never actually computed.
///
/// With `stored_hash = None` the result is always `Ok(false)`: there is nothing
/// to verify, only work to spend.
pub fn verify_password_or_dummy(password: &str, stored_hash: Option<&str>) -> Result<bool> {
    match stored_hash {
        Some(hash) => verify_password(password, hash),
        None => {
            burn_dummy_verification(password);
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
/// return and nothing that can fail: the work *is* the point.
pub fn burn_dummy_verification(password: &str) {
    // `black_box` keeps the optimizer from noticing the result is thrown away.
    std::hint::black_box(verify_password(password, DUMMY_PASSWORD_HASH).is_ok());
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

    #[test]
    fn test_hash_and_verify() {
        let hash = hash_password("hunter2").unwrap();
        assert!(verify_password("hunter2", &hash).unwrap());
        assert!(!verify_password("wrong", &hash).unwrap());
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

    #[test]
    fn dummy_verification_costs_what_a_real_one_costs() {
        use std::time::Instant;

        let real = hash_password("correct horse battery staple").unwrap();
        // Minimum of a few runs: the true cost is the floor, everything above it
        // is scheduler noise from the parallel test runner.
        let floor = |stored: Option<&str>| {
            (0..3)
                .map(|_| {
                    let started = Instant::now();
                    assert!(!verify_password_or_dummy("wrong password", stored).unwrap());
                    started.elapsed()
                })
                .min()
                .unwrap()
        };

        let known = floor(Some(real.as_str())).as_secs_f64();
        let unknown = floor(None).as_secs_f64();

        // Wide band on purpose — this asserts "one full Argon2 either way",
        // not a benchmark. The bug it guards against was a ~0x short-circuit.
        assert!(
            unknown > known * 0.5 && unknown < known * 2.0,
            "unknown-account verification took {unknown:.4}s vs {known:.4}s for a known one — \
             the branches no longer cost the same"
        );
    }

    /// A hash that cannot be parsed is not a wrong password. Reporting it as
    /// one is what let a broken `password_hash` column lock an account out
    /// while every caller kept answering "invalid credentials".
    #[test]
    fn unparseable_stored_hash_is_an_error_not_a_rejection() {
        let error = verify_password("hunter2", "not-a-phc-string")
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
        let error = verify_password("hunter2", foreign)
            .expect_err("a hash this build cannot verify cannot yield a verdict");
        assert!(
            error.downcast_ref::<UnusablePasswordHash>().is_some(),
            "expected UnusablePasswordHash, got: {error:#}"
        );
    }

    /// The other half of the split: a hash we *can* verify still answers
    /// `Ok(false)` for a wrong password, with or without the dummy branch.
    #[test]
    fn wrong_password_against_a_usable_hash_stays_a_plain_rejection() {
        let hash = hash_password("hunter2").unwrap();
        assert!(!verify_password("wrong", &hash).unwrap());
        assert!(!verify_password_or_dummy("wrong", Some(hash.as_str())).unwrap());
        assert!(verify_password_or_dummy("hunter2", Some(hash.as_str())).unwrap());
        // No account to verify against is a rejection, never an error.
        assert!(!verify_password_or_dummy("hunter2", None).unwrap());
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
}
