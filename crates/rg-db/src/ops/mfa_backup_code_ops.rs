//! MFA backup code operations.
use sea_orm::sea_query::Expr;
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

/// Store backup codes for a user, replacing the unused ones in one commit.
///
/// Generic over the connection so the replacement can join a caller's
/// transaction instead of committing on its own; passing a `DatabaseTransaction`
/// nests a savepoint. [`crate::ops::user_ops::enable_mfa_with_backup_codes`] uses
/// that to put "the second factor is on" and "these are the codes" in one
/// commit.
///
/// The transaction is the point of this function, not tidiness. A set is shown
/// to its owner exactly once — in the response that carries it — so a set
/// published half-written leaves the account with a second factor and the owner
/// with recovery material they never saw, under a `500` claiming nothing
/// happened. Delete-then-insert is also a read-modify-write of the whole set, so
/// serialising it is what stops two concurrent re-issues from interleaving into a
/// stored set that matches neither of the two answers handed out.
pub async fn set_codes<C>(db: &C, user_id: i64, codes: &[String]) -> Result<(), DbErr>
where
    C: ConnectionTrait + TransactionTrait,
{
    let txn = db.begin().await?;

    // Used codes are history, not credentials, and stay: only the live set is
    // being replaced.
    Entity::delete_many()
        .filter(mfa_backup_code::Column::UserId.eq(user_id))
        .filter(mfa_backup_code::Column::Used.eq(false))
        .exec(&txn)
        .await?;

    let now = chrono::Utc::now();
    // One statement for the whole set: a row per round-trip gave the failure ten
    // places to land inside the window the delete had already opened.
    // `on_empty_do_nothing` keeps "revoke every unused code" a legitimate
    // request rather than an INSERT with no VALUES.
    Entity::insert_many(codes.iter().map(|code| mfa_backup_code::ActiveModel {
        id: NotSet,
        user_id: Set(user_id),
        code_hash: Set(hash_code(code)),
        used: Set(false),
        used_at: Set(None),
        created_at: Set(now),
    }))
    .on_empty_do_nothing()
    .exec(&txn)
    .await?;

    txn.commit().await
}

/// [`set_codes`] as a transaction of its own, run again when the backend
/// refuses it for contention — the entry point for a caller that holds no
/// transaction (card_d71c4875993c).
///
/// The refusal is not exotic. On InnoDB the delete takes next-key locks on the
/// `user_id` index, including the gap after the account's last entry, and the
/// insert of the account next door needs an insert-intention lock in that same
/// gap. Two re-issues for neighbouring accounts therefore deadlock with no row
/// in common, and InnoDB rolls one back with 1213 and asks for the transaction
/// to be run again. Re-running is safe: the set is generated before the call,
/// and each attempt deletes and inserts from scratch.
///
/// A caller that already holds a transaction passes it to [`set_codes`] and
/// retries at its own level — a deadlock rolls back the whole transaction, so
/// a retry of the inner savepoint alone would run on a transaction that is gone.
pub async fn reissue_codes(
    db: &DatabaseConnection,
    user_id: i64,
    codes: &[String],
) -> anyhow::Result<()> {
    crate::contention::retry_transaction("re-issue MFA backup codes", || async {
        set_codes(db, user_id, codes)
            .await
            .map_err(anyhow::Error::from)
    })
    .await
}

/// Spend a backup code, reporting whether this call is the one that spent it.
///
/// A compare-and-swap, not a read followed by a write: the `WHERE` names the
/// exact state the caller believed it was acting on — this user's code, still
/// unspent — so the database picks the winner in one statement. The
/// predecessor read the row with `one(db)`, decided `used == false` in
/// application memory, and only then issued an `UPDATE` filtered on the id
/// alone. Two `POST /users/mfa/verify` carrying the same code both read the
/// live row, both marked it used, and both were answered a session: one
/// single-use code, two passes of the second factor. Same shape as
/// [`crate::ops::password_reset_token_ops::consume`], for the same reason.
///
/// `rows_affected` is a safe answer on every backend here because the `WHERE`
/// guarantees the row it matches really changes (`used` goes `false → true`),
/// so MySQL's "changed rows" count and PostgreSQL/SQLite's "matched rows" count
/// agree.
///
/// `false` covers both "no such code" and "already spent" — deliberately, and
/// not only because the caller does not need to tell them apart: answering them
/// differently would tell whoever is guessing that a code existed.
pub async fn verify_and_consume(
    db: &DatabaseConnection,
    user_id: i64,
    code: &str,
) -> Result<bool, DbErr> {
    let hash = hash_code(code);
    let result = Entity::update_many()
        .col_expr(mfa_backup_code::Column::Used, Expr::value(true))
        .col_expr(
            mfa_backup_code::Column::UsedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(mfa_backup_code::Column::UserId.eq(user_id))
        .filter(mfa_backup_code::Column::CodeHash.eq(hash))
        .filter(mfa_backup_code::Column::Used.eq(false))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

/// List backup codes status for a user.
///
/// Generic over the connection for the same reason [`set_codes`] is: a caller
/// holding a transaction has to be able to read the set it just wrote.
pub async fn list_codes<C: ConnectionTrait>(
    db: &C,
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
