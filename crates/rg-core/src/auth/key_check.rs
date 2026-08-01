//! Startup preflight: does the configured encryption key still open the data
//! this instance already wrote?
//!
//! Every at-rest secret in ForgeKeep is AES-GCM encrypted under
//! [`encryption::derive_key`] of the instance's encryption secret, which
//! defaults to `[auth].jwt_secret`. Change that secret and *nothing* announces
//! it: the server starts, and then every path that touches an encrypted column
//! fails on its own — MFA login says "decryption failed", the CI job says it
//! cannot decrypt a secret, the mirror sync says the credential is unreadable,
//! LDAP bind says the password is missing. Each is a separate 500 in a separate
//! handler, hours or days apart, and none of them names the cause
//! (card_d740512de0a8).
//!
//! This module turns that scattered runtime failure into one refusal at
//! startup, with the fix in the message. It is deliberately *read-only* and
//! *sampling*: it opens nothing, changes nothing, and reads a handful of rows.
//!
//! ## Why a sample, and why "none of them opened"
//!
//! There is no key-check marker row to consult — the defect predates any such
//! column, and the deployments that need this check the most are the ones
//! already carrying data. So the check asks the data itself: take a few values
//! that structurally *are* our ciphertext ([`encryption::looks_like_ciphertext`]),
//! and try to open them.
//!
//! The verdict is deliberately asymmetric. One value that opens proves the key
//! is right, so any success passes. Only "there is ciphertext here and **not
//! one** of the samples opened" is treated as a wrong key, because that is what
//! a re-keyed instance looks like and essentially nothing else does: AES-GCM is
//! authenticated, so a correct key does not fail, and a single corrupt row
//! cannot fake the verdict while its neighbours still open.

use anyhow::{bail, Context, Result};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect};

use rg_db::entities::{ci_secret, instance_signing_key, mirror, oauth_account, sso_provider, user};

use crate::auth::encrypted_columns::with_encrypted_columns;
use crate::auth::encryption;

/// Rows read per column. Enough that one corrupt or hand-edited value cannot
/// decide the verdict alone, small enough to stay a constant-time startup step
/// on an instance with a million users.
const SAMPLE_LIMIT: u64 = 5;

/// What the probe saw. `probed` counts only values that structurally look like
/// our ciphertext; a column of legacy plaintext contributes nothing.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct KeyProbe {
    pub probed: usize,
    pub opened: usize,
    /// Columns that contributed at least one value the key could not open,
    /// deduplicated and in probe order — the operator-facing list of what is
    /// at stake.
    pub unopened_columns: Vec<&'static str>,
}

impl KeyProbe {
    /// No encrypted data exists yet, so no key can be wrong. True for a fresh
    /// install, which must never be blocked by this check.
    pub fn is_empty(&self) -> bool {
        self.probed == 0
    }

    /// Ciphertext exists and none of it opened — the re-keyed instance.
    pub fn key_is_wrong(&self) -> bool {
        self.probed > 0 && self.opened == 0
    }
}

/// Read a sample of the encrypted columns and report how much of it `key` opens.
pub async fn probe_encryption_key(db: &DatabaseConnection, key: &[u8; 32]) -> Result<KeyProbe> {
    let mut probe = KeyProbe::default();
    for (column, value) in collect_samples(db).await? {
        if !encryption::looks_like_ciphertext(&value) {
            continue;
        }
        probe.probed += 1;
        if encryption::decrypt(&value, key).is_ok() {
            probe.opened += 1;
        } else if !probe.unopened_columns.contains(&column) {
            probe.unopened_columns.push(column);
        }
    }
    Ok(probe)
}

/// Refuse to start when the configured encryption secret cannot open the
/// encrypted data already in `db`.
///
/// `Ok(())` on a fresh database (nothing encrypted yet) and whenever at least
/// one sampled value opens. A partial failure — some values open, some do not —
/// is a warning rather than a refusal: the key is demonstrably right, so what
/// is left is per-row damage that stopping the server would not repair.
pub async fn verify_encryption_key(db: &DatabaseConnection, encryption_secret: &str) -> Result<()> {
    let key = encryption::derive_key(encryption_secret);
    let probe = probe_encryption_key(db, &key)
        .await
        .context("could not read the encrypted columns to verify the encryption key")?;

    if probe.is_empty() {
        tracing::debug!("encryption key check: no encrypted data stored yet");
        return Ok(());
    }

    if probe.key_is_wrong() {
        bail!("{}", wrong_key_message(&probe));
    }

    if !probe.unopened_columns.is_empty() {
        tracing::warn!(
            columns = %probe.unopened_columns.join(", "),
            opened = probe.opened,
            probed = probe.probed,
            "the encryption key is correct, but some stored values did not decrypt — \
             those individual rows are damaged and must be re-entered"
        );
        return Ok(());
    }

    tracing::info!(
        opened = probe.opened,
        "encryption key check passed: stored secrets decrypt with the configured key"
    );
    Ok(())
}

/// The refusal message. Long on purpose: it is the only thing the operator sees
/// before the process exits, and the recovery ("pin the previous secret as
/// `encryption_key`") is not something anyone guesses.
fn wrong_key_message(probe: &KeyProbe) -> String {
    format!(
        "the configured encryption key does not decrypt any of the {probed} encrypted \
         value(s) already stored in this database ({columns}).\n\
         \n\
         Nothing has been changed. This is what a changed secret looks like: data at rest \
         (TOTP secrets, CI secrets, mirror and LDAP passwords, SSO client secrets, OAuth \
         tokens, this instance's provenance signing key) is encrypted with \
         [auth].encryption_key, which defaults to [auth].jwt_secret \
         when it is not set. Starting anyway would fail every one of those operations \
         separately, at runtime, with no indication why.\n\
         \n\
         If you rotated jwt_secret: keep the new signing secret and pin the OLD one as the \
         encryption key, which leaves the stored data readable:\n\
         \n\
         \x20   [auth]\n\
         \x20   jwt_secret     = \"<the new secret>\"\n\
         \x20   encryption_key = \"<the secret used before the rotation>\"\n\
         \n\
         (or set FORGEKEEP_ENCRYPTION_KEY / --encryption-key to the previous secret).\n\
         \n\
         If the previous secret is genuinely lost, the encrypted values cannot be recovered \
         by anyone: clear them and have MFA re-enrolled, CI secrets, mirror and LDAP \
         passwords and SSO client secrets re-entered. The instance's provenance signing key \
         is in that set — losing it means release attestations signed so far can no longer \
         be verified, and `forgekeep rotate-instance-key` is what mints a new identity.",
        probed = probe.probed,
        columns = probe.unopened_columns.join(", "),
    )
}

/// Sample every column that stores [`encryption::encrypt`] output.
///
/// The columns come from [`encrypted_columns`], which is also what
/// [`crate::auth::rekey`] rewrites — so a column added to one is sampled by the
/// other, and a rotation can never quietly skip what this check watches.
///
/// Each entry is `(column label, stored value)`. The label is what the operator
/// reads in the refusal, so it names the table and column, not the Rust field.
async fn collect_samples(db: &DatabaseConnection) -> Result<Vec<(&'static str, String)>> {
    let mut samples: Vec<(&'static str, String)> = Vec::new();

    // The nullable columns are filtered to non-null rows before the limit:
    // without it, `LIMIT 5` over a million users returns five rows with no MFA
    // and the check silently probes nothing.
    macro_rules! sample_one {
        ($entity:ident, $col:ident, $field:ident, $label:literal, optional) => {{
            for row in $entity::Entity::find()
                .filter($entity::Column::$col.is_not_null())
                .limit(SAMPLE_LIMIT)
                .all(db)
                .await
                .context(concat!("sampling ", $label))?
            {
                samples.extend(row.$field.map(|value| ($label, value)));
            }
        }};
        ($entity:ident, $col:ident, $field:ident, $label:literal, required) => {{
            for row in $entity::Entity::find()
                .limit(SAMPLE_LIMIT)
                .all(db)
                .await
                .context(concat!("sampling ", $label))?
            {
                samples.push(($label, row.$field));
            }
        }};
    }

    macro_rules! sample_all {
        ($($entity:ident, $col:ident, $field:ident, $label:literal, $opt:tt;)*) => {
            $( sample_one!($entity, $col, $field, $label, $opt); )*
        };
    }

    with_encrypted_columns!(sample_all);

    Ok(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ciphertext(secret: &str, plaintext: &str) -> String {
        encryption::encrypt(plaintext, &encryption::derive_key(secret)).unwrap()
    }

    /// A base32 TOTP secret and a bare password are what the columns held
    /// before they were encrypted. Reading either as "ciphertext that failed to
    /// open" would refuse the start of a perfectly healthy instance.
    #[test]
    fn legacy_plaintext_is_not_mistaken_for_ciphertext() {
        assert!(!encryption::looks_like_ciphertext("JBSWY3DPEHPK3PXP"));
        assert!(!encryption::looks_like_ciphertext("hunter2"));
        assert!(!encryption::looks_like_ciphertext(""));
        assert!(!encryption::looks_like_ciphertext(
            "s3cr3t+with/base64=chars"
        ));
        assert!(encryption::looks_like_ciphertext(&ciphertext("k", "x")));
    }

    #[test]
    fn a_probe_that_saw_nothing_never_blocks_a_start() {
        let probe = KeyProbe::default();
        assert!(probe.is_empty());
        assert!(!probe.key_is_wrong());
    }

    /// One value opening proves the key: whatever else failed is damage to that
    /// row, not the wrong key, and stopping the server would not repair it.
    #[test]
    fn one_value_opening_outvotes_the_rest() {
        let probe = KeyProbe {
            probed: 4,
            opened: 1,
            unopened_columns: vec!["ci_secrets.encrypted_value"],
        };
        assert!(!probe.key_is_wrong());
    }

    #[test]
    fn the_refusal_names_the_columns_and_the_recovery() {
        let probe = KeyProbe {
            probed: 2,
            opened: 0,
            unopened_columns: vec!["users.totp_secret"],
        };
        assert!(probe.key_is_wrong());
        let message = wrong_key_message(&probe);
        assert!(message.contains("users.totp_secret"));
        assert!(message.contains("encryption_key"));
        assert!(message.contains("FORGEKEEP_ENCRYPTION_KEY"));
    }
}
