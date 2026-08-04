//! Move a database from one at-rest encryption key to another.
//!
//! Until this existed the encryption key was permanent for the life of a
//! database: [`super::key_check`] made rotating it *safe* (the server refuses
//! to start under a key that opens nothing) but not *possible*, and the README
//! said so outright. That left one answer to a leaked encryption key — wipe
//! every encrypted value, have every user re-enrol MFA, and re-enter every CI
//! secret, mirror and LDAP password and SSO client secret by hand — for an
//! incident whose whole point is that it is recoverable.
//!
//! This module is the other answer: read every column in
//! [`super::encrypted_columns`], open each value with the old key, seal it with
//! the new one, and write it back in a single transaction.
//!
//! ## What it refuses to do
//!
//! * **Guess.** A value that the old key does not open is *not* re-encrypted
//!   and *not* deleted: it is counted and reported. Legacy plaintext (a bare
//!   base32 TOTP secret from before the column was encrypted) is left exactly
//!   as it is, because rewriting it would encrypt a value the readers still
//!   expect in the clear.
//! * **Half-finish.** Everything happens in one transaction, so a failure in
//!   the sixth column cannot leave the first five under a different key than
//!   the rest.
//! * **Run blind.** [`rekey`] in `dry_run` mode does the entire traversal,
//!   reports exactly what each column would do, and rolls back.
//! * **Rotate onto nothing.** If no stored value opens with the old key, the
//!   apply path rolls back and fails — that is what a mistyped `--old` looks
//!   like, and committing it would replace readable ciphertext with nothing.
//!
//! Re-running after a crash is safe: a value that already opens with the *new*
//! key is recognised and left alone rather than counted as damage.

use anyhow::{bail, Context, Result};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    QueryOrder, Set, TransactionTrait,
};

use rg_db::entities::{
    ci_secret, instance_signing_key, mirror, oauth_account, sso_provider, user, webhook,
};

use crate::auth::encrypted_columns::with_encrypted_columns;
use crate::auth::encryption;

/// Rows read per round trip. Large enough that a normal instance finishes in a
/// couple of queries per column, small enough that a table of OAuth tokens on a
/// big deployment never has to fit in memory at once.
const PAGE_SIZE: u64 = 500;

/// What the pass did to one column.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnRekey {
    pub column: &'static str,
    /// Values that opened with the old key and were sealed with the new one
    /// (counted in `dry_run` too, where nothing is written).
    pub rewritten: usize,
    /// Values that already open with the new key — a re-run, not damage.
    pub already_new: usize,
    /// Values that are not our ciphertext at all (legacy plaintext), left as is.
    pub plaintext: usize,
    /// Ciphertext neither key opens. Left as is and reported; re-encrypting is
    /// impossible and deleting is not this command's call.
    pub unreadable: usize,
}

impl ColumnRekey {
    fn new(column: &'static str) -> Self {
        Self {
            column,
            ..Default::default()
        }
    }

    /// Whether this column had anything at all to look at.
    pub fn is_empty(&self) -> bool {
        self.rewritten + self.already_new + self.plaintext + self.unreadable == 0
    }
}

/// The whole pass, one entry per registered column, in registry order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RekeyReport {
    /// True when nothing was written — the traversal ran and rolled back.
    pub dry_run: bool,
    pub columns: Vec<ColumnRekey>,
}

impl RekeyReport {
    fn total(&self, field: fn(&ColumnRekey) -> usize) -> usize {
        self.columns.iter().map(field).sum()
    }

    pub fn rewritten(&self) -> usize {
        self.total(|c| c.rewritten)
    }

    pub fn already_new(&self) -> usize {
        self.total(|c| c.already_new)
    }

    pub fn plaintext(&self) -> usize {
        self.total(|c| c.plaintext)
    }

    pub fn unreadable(&self) -> usize {
        self.total(|c| c.unreadable)
    }

    /// Ciphertext exists and the old key opened none of it — the signature of a
    /// wrong `--old`, and the one outcome that must never be committed.
    pub fn old_key_is_wrong(&self) -> bool {
        self.rewritten() == 0 && self.already_new() == 0 && self.unreadable() > 0
    }

    /// Columns that hold at least one value neither key opens.
    pub fn unreadable_columns(&self) -> Vec<&'static str> {
        self.columns
            .iter()
            .filter(|c| c.unreadable > 0)
            .map(|c| c.column)
            .collect()
    }
}

/// What a single stored value turned out to be.
enum Verdict {
    /// Opened with the old key; carries the value re-sealed under the new one.
    Rewrite(String),
    AlreadyNew,
    Plaintext,
    Unreadable,
}

fn classify(value: &str, old_key: &[u8; 32], new_key: &[u8; 32]) -> Result<Verdict> {
    if !encryption::looks_like_ciphertext(value) {
        return Ok(Verdict::Plaintext);
    }
    match encryption::decrypt(value, old_key) {
        Ok(plaintext) => Ok(Verdict::Rewrite(encryption::encrypt(&plaintext, new_key)?)),
        // AES-GCM is authenticated, so "the old key failed" is not a maybe. The
        // second attempt separates an interrupted earlier run (already on the
        // new key) from a value nothing can open.
        Err(_) if encryption::decrypt(value, new_key).is_ok() => Ok(Verdict::AlreadyNew),
        Err(_) => Ok(Verdict::Unreadable),
    }
}

/// Re-encrypt every at-rest secret from `old_secret` to `new_secret`.
///
/// Offline by design: run it with the server stopped, or a handler that writes
/// an encrypted column mid-pass leaves a value under the old key.
///
/// With `dry_run` the traversal is identical and the transaction is rolled
/// back, so the returned report describes exactly what the real run would do.
pub async fn rekey(
    db: &DatabaseConnection,
    old_secret: &str,
    new_secret: &str,
    dry_run: bool,
) -> Result<RekeyReport> {
    if new_secret.trim().is_empty() {
        bail!("refusing to re-encrypt the database under an empty encryption key");
    }
    if old_secret == new_secret {
        bail!(
            "the new encryption key is identical to the old one — nothing to re-encrypt. \
             Generate a fresh one with `forgekeep gen-secret`."
        );
    }

    // On databases booted since card_82df8d4bb730 the marker is the definitive
    // proof of the old key, including on a database with no user secrets yet.
    // A re-run after a completed rotation is intentionally different: the old
    // key must fail while `new_secret` opens the marker, and the traversal below
    // will report every row as `already_new`. Preserve the old-key error when
    // neither key opens the database.
    if let Err(old_key_error) = crate::auth::key_check::verify_encryption_key(db, old_secret).await
    {
        if crate::auth::key_check::verify_encryption_key(db, new_secret)
            .await
            .is_err()
        {
            return Err(old_key_error);
        }
    }

    let old_key = encryption::derive_key(old_secret);
    let new_key = encryption::derive_key(new_secret);

    let txn = db
        .begin()
        .await
        .context("open the re-encryption transaction")?;

    let mut report = RekeyReport {
        dry_run,
        columns: Vec::new(),
    };

    macro_rules! rekey_one {
        ($entity:ident, $col:ident, $field:ident, $label:literal, optional) => {{
            let mut column = ColumnRekey::new($label);
            let mut pages = $entity::Entity::find()
                .filter($entity::Column::$col.is_not_null())
                .order_by_asc($entity::Column::Id)
                .paginate(&txn, PAGE_SIZE);
            while let Some(rows) = pages
                .fetch_and_next()
                .await
                .context(concat!("reading ", $label))?
            {
                for row in rows {
                    let Some(value) = row.$field.clone() else {
                        continue;
                    };
                    match classify(&value, &old_key, &new_key)? {
                        Verdict::Rewrite(sealed) => {
                            column.rewritten += 1;
                            if !dry_run {
                                let mut active: $entity::ActiveModel = row.into();
                                active.$field = Set(Some(sealed));
                                active
                                    .update(&txn)
                                    .await
                                    .context(concat!("re-encrypting ", $label))?;
                            }
                        }
                        Verdict::AlreadyNew => column.already_new += 1,
                        Verdict::Plaintext => column.plaintext += 1,
                        Verdict::Unreadable => column.unreadable += 1,
                    }
                }
            }
            report.columns.push(column);
        }};
        ($entity:ident, $col:ident, $field:ident, $label:literal, required) => {{
            let mut column = ColumnRekey::new($label);
            let mut pages = $entity::Entity::find()
                .order_by_asc($entity::Column::Id)
                .paginate(&txn, PAGE_SIZE);
            while let Some(rows) = pages
                .fetch_and_next()
                .await
                .context(concat!("reading ", $label))?
            {
                for row in rows {
                    let value = row.$field.clone();
                    match classify(&value, &old_key, &new_key)? {
                        Verdict::Rewrite(sealed) => {
                            column.rewritten += 1;
                            if !dry_run {
                                let mut active: $entity::ActiveModel = row.into();
                                active.$field = Set(sealed);
                                active
                                    .update(&txn)
                                    .await
                                    .context(concat!("re-encrypting ", $label))?;
                            }
                        }
                        Verdict::AlreadyNew => column.already_new += 1,
                        Verdict::Plaintext => column.plaintext += 1,
                        Verdict::Unreadable => column.unreadable += 1,
                    }
                }
            }
            report.columns.push(column);
        }};
    }

    macro_rules! rekey_all {
        ($($entity:ident, $col:ident, $field:ident, $label:literal, $opt:tt;)*) => {
            $( rekey_one!($entity, $col, $field, $label, $opt); )*
        };
    }

    with_encrypted_columns!(rekey_all);

    if dry_run {
        txn.rollback()
            .await
            .context("roll back the dry-run transaction")?;
        return Ok(report);
    }

    if report.old_key_is_wrong() {
        txn.rollback()
            .await
            .context("roll back after refusing to rotate")?;
        bail!("{}", wrong_old_key_message(&report));
    }

    crate::auth::key_check::replace_encryption_key_check(&txn, new_secret).await?;

    txn.commit()
        .await
        .context("commit the re-encrypted values")?;

    tracing::warn!(
        rewritten = report.rewritten(),
        already_new = report.already_new(),
        unreadable = report.unreadable(),
        "the at-rest encryption key was rotated; the server must now be started with the new key"
    );

    Ok(report)
}

/// The refusal for a rotation whose old key opens nothing. Same shape as the
/// startup preflight's message: what was found, what was *not* changed, and the
/// two things it is actually likely to be.
fn wrong_old_key_message(report: &RekeyReport) -> String {
    format!(
        "the old encryption key does not open any of the {unreadable} encrypted value(s) \
         stored in this database ({columns}).\n\
         \n\
         Nothing has been changed. Committing this would have left every one of those values \
         sealed under a key you are about to stop using.\n\
         \n\
         Either --old is not the key this database was written with, or it was never set \
         explicitly and defaulted to something else: [auth].encryption_key falls back to \
         [auth].jwt_secret when it is absent, so on an instance that has rotated jwt_secret \
         the key that opens the data is the *previous* signing secret.",
        unreadable = report.unreadable(),
        columns = report.unreadable_columns().join(", "),
    )
}
