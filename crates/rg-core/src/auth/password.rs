//! Password hashing and verification using Argon2id.
//!
//! Also includes a [`PasswordValidator`] for password strength checks
//! (Phase 22-D security hardening).

use anyhow::Result;
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use rand_core::OsRng;

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
pub fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let parsed =
        PasswordHash::new(hash).map_err(|e| anyhow::anyhow!("invalid password hash: {}", e))?;
    Ok(Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok())
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
/// Returns `false` for every failure — no such account, wrong password, or an
/// unparseable stored hash (logged, then treated as a failed login).
pub fn verify_password_or_dummy(password: &str, stored_hash: Option<&str>) -> bool {
    match stored_hash {
        Some(hash) => verify_password(password, hash).unwrap_or_else(|error| {
            tracing::warn!(error = %format!("{error:#}"), "stored password hash is unusable");
            false
        }),
        None => {
            // The point is the work, not the answer — `black_box` keeps the
            // optimizer from noticing the result is thrown away.
            std::hint::black_box(verify_password(password, DUMMY_PASSWORD_HASH).is_ok());
            false
        }
    }
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
                    assert!(!verify_password_or_dummy("wrong password", stored));
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
