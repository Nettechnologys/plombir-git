//! MFA backup code operations.
use sea_orm::*;
use sha2::{Digest, Sha256};

use crate::entities::mfa_backup_code;
pub use crate::entities::mfa_backup_code::Entity;

/// Hash a backup code with SHA-256 (for storage).
pub fn hash_code(code: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Generate n random backup codes (6-digit numbers as strings).
pub fn generate_codes(n: usize) -> Vec<String> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..n)
        .map(|_| format!("{:06}", rng.gen_range(0..1_000_000)))
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
