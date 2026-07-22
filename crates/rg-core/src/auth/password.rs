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
