//! Database operations for runners.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::runner::{ActiveModel, Column, Entity as RunnerEntity, Model as Runner};

/// Register a new runner.
///
/// Generates a token, stores only its SHA-256, and returns the row together
/// with the plaintext — which is the caller's one and only chance to see it.
///
/// Returning the pair rather than a model carrying the token is what makes the
/// hashing hard to undo by accident: there is no field left to read the secret
/// back out of, so a future handler that wants to show a runner's token has to
/// notice it cannot.
pub async fn register_runner(
    db: &DatabaseConnection,
    name: &str,
    labels: &str,
    version: Option<&str>,
    os: Option<&str>,
    arch: Option<&str>,
) -> Result<(Runner, String)> {
    let now = Utc::now();
    let token = generate_token();

    let active_model = ActiveModel {
        id: NotSet,
        name: Set(name.to_string()),
        token_hash: Set(hash_token(&token)),
        status: Set("offline".to_string()),
        labels: Set(labels.to_string()),
        last_seen_at: Set(now),
        version: Set(version.map(|v| v.to_string())),
        os: Set(os.map(|v| v.to_string())),
        arch: Set(arch.map(|v| v.to_string())),
        created_at: Set(now),
        updated_at: Set(now),
    };

    let runner = active_model
        .insert(db)
        .await
        .context("db: register runner")?;

    Ok((runner, token))
}

/// Update runner heartbeat and the liveness of its executing job atomically.
///
/// Runner authentication refreshes this on every authenticated request, and
/// the external runner also calls `/heartbeat` every 30 seconds while a job is
/// executing. The job timestamp is part of the same fact: committing only the
/// runner half would let the job watchdog reclaim healthy work.
pub async fn update_heartbeat(db: &DatabaseConnection, runner_id: i64) -> Result<()> {
    let now = Utc::now();
    let txn = db.begin().await.context("db: begin runner heartbeat")?;

    RunnerEntity::update_many()
        .col_expr(Column::LastSeenAt, Expr::value(now))
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Id.eq(runner_id))
        .exec(&txn)
        .await
        .context("db: update runner heartbeat")?;

    crate::ops::pipeline_ops::touch_running_jobs_for_runner(&txn, runner_id).await?;
    txn.commit().await.context("db: commit runner heartbeat")?;
    Ok(())
}

/// Update runner status.
pub async fn update_status(db: &impl ConnectionTrait, runner_id: i64, status: &str) -> Result<()> {
    let now = Utc::now().naive_utc();

    RunnerEntity::update_many()
        .col_expr(Column::Status, Expr::value(status.to_string()))
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Id.eq(runner_id))
        .exec(db)
        .await
        .context("db: update runner status")?;

    Ok(())
}

/// Retire a runner the watchdog found unreachable: release the jobs it was
/// holding and mark it `offline`, both or neither.
///
/// The two writes are only correct together, and the watchdog used to make them
/// separately, in the order that disarms its own retry: `status = 'offline'`
/// first, then the job reset. `find_offline_runners` selects on
/// `status IN ('online', 'busy')`, so a reset that failed after the status
/// landed left jobs pinned to a runner that branch would never look at again —
/// the loop had written the row out of its own selection (card_4d1d8b9fba56).
///
/// One transaction rather than a reordering, mirroring [`deregister_runner`]:
/// the pair is the same pair, and the failure of either write must leave the
/// runner in a status the next tick still selects, sixty seconds later.
///
/// Returns how many jobs were handed back to the queue.
pub async fn retire_unreachable_runner(db: &DatabaseConnection, runner_id: i64) -> Result<u64> {
    let txn = db
        .begin()
        .await
        .context("db: begin offline runner retirement")?;

    let released = crate::ops::pipeline_ops::reset_runner_jobs(&txn, runner_id).await?;
    update_status(&txn, runner_id, "offline").await?;

    txn.commit()
        .await
        .context("db: commit offline runner retirement")?;
    Ok(released)
}

/// Find a runner by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Runner>> {
    RunnerEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find runner by id")
}

/// Find a runner by the token it presented.
///
/// The argument is the plaintext bearer token off the wire; the stored column
/// holds only its hash, so the comparison happens between hashes.
pub async fn find_by_token(db: &DatabaseConnection, token: &str) -> Result<Option<Runner>> {
    RunnerEntity::find()
        .filter(Column::TokenHash.eq(hash_token(token)))
        .one(db)
        .await
        .context("db: find runner by token")
}

/// List all runners (for admin).
pub async fn list_all(db: &DatabaseConnection) -> Result<Vec<Runner>> {
    RunnerEntity::find()
        .order_by_desc(Column::LastSeenAt)
        .all(db)
        .await
        .context("db: list all runners")
}

/// Deregister a runner: hand its in-flight jobs back to the pool and delete the
/// runner row, as one transaction.
///
/// The two writes are not independent. `pipeline_jobs.runner_id` points at the
/// row being deleted, so committing the delete without the reset strands every
/// `assigned`/`running` job on a runner that no longer exists — the scheduler
/// hands those jobs to nobody, and only the watchdog's stuck-job sweep
/// eventually notices. Committing the reset without the delete is the harmless
/// half, but a caller that did the two separately had no way to tell which half
/// it got. Inside one transaction there is no half: either the pool got the
/// jobs back and the runner is gone, or nothing changed and the error says so.
///
/// Returns `false` when no runner row matched — the caller's 404 — and rolls
/// the reset back rather than leaving jobs reset in the name of a runner that
/// was not there to deregister.
pub async fn deregister_runner(db: &DatabaseConnection, runner_id: i64) -> Result<bool> {
    let txn = db.begin().await.context("db: begin transaction")?;

    crate::ops::pipeline_ops::reset_runner_jobs(&txn, runner_id).await?;

    let deleted = RunnerEntity::delete_by_id(runner_id)
        .exec(&txn)
        .await
        .context("db: delete runner")?
        .rows_affected
        > 0;

    if !deleted {
        txn.rollback().await.context("db: rollback transaction")?;
        return Ok(false);
    }

    txn.commit().await.context("db: commit transaction")?;
    Ok(true)
}

/// Generate a unique token for runner authentication.
fn generate_token() -> String {
    // Use UUID v4 to generate a unique token (36 chars with hyphens)
    // Remove hyphens to get 32-char token
    uuid::Uuid::new_v4().to_string().replace('-', "")
}

/// Hash a runner token for storage and lookup.
///
/// Plain SHA-256 with no salt, exactly as `access_token.token_hash` and
/// `password_reset_token.token_hash`: the lookup is *by* the hash, so there is
/// nowhere to put a per-row salt. That is sound here because the input is not
/// user-chosen — a v4 UUID carries 122 bits out of the CSPRNG, which no
/// dictionary or rainbow table reaches.
pub fn hash_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The floor this whole change rests on: the token is a v4 UUID's 122 bits
    /// of CSPRNG output, so the unsalted hash has nothing to attack. If the
    /// generator is ever weakened to something guessable, that is the moment
    /// the storage scheme stops being sound — so the format is pinned here.
    #[test]
    fn a_generated_token_is_32_hex_characters_and_never_repeats() {
        let a = generate_token();
        let b = generate_token();

        assert_eq!(a.len(), 32, "token: {a}");
        assert!(
            a.chars().all(|c| c.is_ascii_hexdigit()),
            "token is not hex: {a}"
        );
        assert_ne!(a, b, "two registrations produced the same token");
    }

    #[test]
    fn hashing_is_deterministic_and_hides_the_token() {
        let token = generate_token();
        let hash = hash_token(&token);

        assert_eq!(hash, hash_token(&token));
        assert_ne!(hash, token);
        assert_eq!(hash.len(), 64, "expected SHA-256 hex, got {hash}");
        assert_ne!(hash, hash_token(&generate_token()));
    }
}
