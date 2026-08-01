//! MFA backup code operations.
use sea_orm::*;
use sha2::{Digest, Sha256};

use crate::entities::mfa_backup_code;
pub use crate::entities::mfa_backup_code::Entity;

/// Crockford base32 — the digits plus the 22 letters that survive being read
/// off a screen and typed back in (`I`, `L`, `O` and `U` are out, so there is
/// nothing to confuse with `1`, `0` or with a word). Exactly 32 symbols, so a
/// byte masked to its low 5 bits selects one with no modulo bias.
const BACKUP_CODE_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Symbols per generated code. 5 bits each, so 14 of them carry 70 bits.
///
/// The floor that matters is 64: below it a single unsalted SHA-256 — which is
/// what a lookup-by-hash forces us to use — is brute-forceable from a database
/// dump, and the code alone is a full bypass of the second factor.
const BACKUP_CODE_LEN: usize = 14;

/// How many codes one enrolment hands out.
pub const BACKUP_CODE_COUNT: usize = 10;

/// Canonicalise a user-typed code before it is hashed.
///
/// Codes are shown once and re-typed later, often out of a paper note, so the
/// separators, spaces and lower case a user adds must not decide whether the
/// account is recoverable. `I`/`L` fold to `1` and `O` to `0` the way Crockford
/// specifies — the generator never emits those letters, so the folding costs no
/// entropy and only forgives a misread.
///
/// Legacy 6-digit codes pass through unchanged (digits have nothing to strip,
/// upcase or fold), which is what keeps their stored hashes verifiable.
fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| match c.to_ascii_uppercase() {
            'I' | 'L' => '1',
            'O' => '0',
            other => other,
        })
        .collect()
}

/// Hash a backup code with SHA-256 (for storage).
///
/// Deliberately *not* Argon2, unlike the password beside it: a code is verified
/// by looking its hash up in an index, and a per-row salt would turn that into
/// a scan with one Argon2 pass per stored code. The entropy of the code carries
/// the whole load instead — see [`BACKUP_CODE_LEN`].
pub fn hash_code(code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(normalize_code(code).as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Generate n random backup codes.
///
/// Every symbol comes from the OS CSPRNG (`OsRng`), like the PAT generator in
/// `rg-http` and `generate_jwt_secret` in `rg-cli`; there is no fallback that
/// could emit a guessable code — `fill_bytes` panics if the OS entropy source
/// is unavailable.
pub fn generate_codes(n: usize) -> Vec<String> {
    use rand::RngCore;
    let mut rng = rand::rngs::OsRng;
    (0..n)
        .map(|_| {
            let mut raw = [0u8; BACKUP_CODE_LEN];
            rng.fill_bytes(&mut raw);
            raw.iter()
                .map(|byte| BACKUP_CODE_ALPHABET[(byte & 0x1f) as usize] as char)
                .collect()
        })
        .collect()
}

/// Store backup codes for a user (replaces existing unused ones).
pub async fn set_codes(
    db: &DatabaseConnection,
    user_id: i64,
    codes: &[String],
) -> Result<(), DbErr> {
    // Delete unused codes for this user
    Entity::delete_many()
        .filter(mfa_backup_code::Column::UserId.eq(user_id))
        .filter(mfa_backup_code::Column::Used.eq(false))
        .exec(db)
        .await?;

    let now = chrono::Utc::now();
    for code in codes {
        let am = mfa_backup_code::ActiveModel {
            id: NotSet,
            user_id: Set(user_id),
            code_hash: Set(hash_code(code)),
            used: Set(false),
            used_at: Set(None),
            created_at: Set(now),
        };
        am.insert(db).await?;
    }
    Ok(())
}

/// Verify a backup code. Returns true if valid and marks it used.
pub async fn verify_and_consume(
    db: &DatabaseConnection,
    user_id: i64,
    code: &str,
) -> Result<bool, DbErr> {
    let hash = hash_code(code);
    let some = Entity::find()
        .filter(mfa_backup_code::Column::UserId.eq(user_id))
        .filter(mfa_backup_code::Column::CodeHash.eq(hash))
        .filter(mfa_backup_code::Column::Used.eq(false))
        .one(db)
        .await?;

    if let Some(m) = some {
        let mut am: mfa_backup_code::ActiveModel = m.into();
        am.used = Set(true);
        am.used_at = Set(Some(chrono::Utc::now()));
        am.update(db).await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// List backup codes status for a user.
pub async fn list_codes(
    db: &DatabaseConnection,
    user_id: i64,
) -> Result<Vec<mfa_backup_code::Model>, DbErr> {
    Entity::find()
        .filter(mfa_backup_code::Column::UserId.eq(user_id))
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entropy floor, asserted as the two things it is actually made of.
    /// A code is a full second-factor bypass stored under one unsalted SHA-256,
    /// so shrinking either number quietly is the failure this test exists to
    /// stop — 32 symbols × 14 positions = 70 bits.
    #[test]
    fn generated_codes_carry_at_least_64_bits() {
        assert_eq!(BACKUP_CODE_ALPHABET.len(), 32);
        assert_eq!(BACKUP_CODE_LEN, 14);

        let bits = BACKUP_CODE_LEN as f64 * (BACKUP_CODE_ALPHABET.len() as f64).log2();
        assert!(
            bits >= 64.0,
            "{bits} bits per backup code is below the floor"
        );
    }

    /// The alphabet is part of the contract too: an entropy count is only true
    /// if every position really draws from all 32 symbols, and the symbols must
    /// stay unambiguous for a code read off paper.
    #[test]
    fn generated_codes_use_the_declared_alphabet() {
        let alphabet: std::collections::HashSet<char> =
            BACKUP_CODE_ALPHABET.iter().map(|b| *b as char).collect();
        assert_eq!(alphabet.len(), 32, "alphabet has duplicate symbols");
        for ambiguous in ['I', 'L', 'O', 'U'] {
            assert!(
                !alphabet.contains(&ambiguous),
                "{ambiguous} is too easy to misread to be in a hand-typed code"
            );
        }

        let codes = generate_codes(BACKUP_CODE_COUNT);
        assert_eq!(codes.len(), BACKUP_CODE_COUNT);
        for code in &codes {
            assert_eq!(
                code.chars().count(),
                BACKUP_CODE_LEN,
                "wrong length: {code}"
            );
            assert!(
                code.chars().all(|c| alphabet.contains(&c)),
                "code left the declared alphabet: {code}"
            );
        }

        // Not a randomness test — just proof the generator is not emitting one
        // constant, which a mask/index slip would produce.
        assert_eq!(
            codes.iter().collect::<std::collections::HashSet<_>>().len(),
            codes.len(),
            "generator repeated a code within a single batch"
        );
    }

    /// Codes issued before this format are stored as the bare SHA-256 of their
    /// six digits. Normalisation must leave digits untouched, or every existing
    /// enrolment loses its recovery path on upgrade. The expected digests are
    /// the plain `sha256sum` of the two strings — they encode the old format,
    /// so they are the thing that must not drift.
    #[test]
    fn legacy_six_digit_codes_still_hash_to_their_stored_value() {
        assert_eq!(
            hash_code("123456"),
            "8d969eef6ecad3c29a3a629280e686cf0c3f5d5a86aff3ca12020c923adc6c92"
        );
        assert_eq!(
            hash_code("000000"),
            "91b4d142823f7d20c5f08df69122de43f35f057a988d9619f6d3138485c9a203"
        );
    }

    /// A code is re-typed off a note, so the shape it comes back in must not
    /// decide whether the account is recoverable.
    #[test]
    fn hashing_forgives_how_a_code_was_typed_back() {
        let canonical = hash_code("ABCD1234EFGH56");
        for variant in [
            "abcd1234efgh56",
            "ABCD-1234-EFGH-56",
            " ABCD 1234 EFGH 56 ",
            "ABCD1234EFGH56\n",
        ] {
            assert_eq!(hash_code(variant), canonical, "rejected variant: {variant}");
        }

        // Crockford folding: the generator never emits I/L/O, so mapping them
        // onto 1/0 forgives a misread without merging two issuable codes.
        assert_eq!(hash_code("O1I0L5"), hash_code("011015"));
    }

    /// Folding must not reach past the ambiguous letters — two distinct codes
    /// staying distinct is what the entropy count assumes.
    #[test]
    fn normalization_keeps_distinct_codes_distinct() {
        assert_ne!(hash_code("ABCD1234EFGH56"), hash_code("ABCD1234EFGH57"));
        assert_ne!(hash_code("123456"), hash_code("123457"));
    }
}
