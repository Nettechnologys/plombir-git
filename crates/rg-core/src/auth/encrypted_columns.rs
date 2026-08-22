//! The one list of database columns that hold [`super::encryption::encrypt`]
//! output.
//!
//! Two operations have to agree on this list exactly: the startup preflight
//! ([`super::key_check`]) samples it to answer "does the configured key still
//! open this database", and the re-encryption pass ([`super::rekey`]) rewrites
//! it to move the database onto a new key. A column present in one list and
//! missing from the other is a silent data-loss bug in the direction that
//! matters most — a rotation that reports success while leaving one column
//! sealed under a secret the operator has just been told to discard.
//!
//! So the list is not written twice. It lives here once, as a callback macro:
//! [`with_encrypted_columns`] expands a caller-supplied macro over every entry,
//! and adding a column means adding one line below, which both consumers pick
//! up on the next build.
//!
//! Each entry is `entity, ColumnVariant, field, "table.column", optional |
//! required`. The label is what an operator reads in a refusal or a rotation
//! report, so it names the SQL table and column rather than the Rust field.
//! `optional` / `required` says whether the model field is an `Option<String>`.

/// Expand `$callback!` once over the whole registry.
///
/// The callback receives every entry as a `;`-separated list, so it can match
/// with a repetition and dispatch per entry (see [`super::key_check`] and
/// [`super::rekey`] for the two shapes this is used in).
macro_rules! with_encrypted_columns {
    ($callback:ident) => {
        $callback! {
            user, TotpSecret, totp_secret,
                "users.totp_secret", optional;
            ci_secret, EncryptedValue, encrypted_value,
                "ci_secrets.encrypted_value", required;
            sso_provider, ClientSecretEnc, client_secret_enc,
                "sso_providers.client_secret_enc", optional;
            sso_provider, LdapBindPasswordEnc, ldap_bind_password_enc,
                "sso_providers.ldap_bind_password_enc", optional;
            // `oauth_accounts.access_token` / `refresh_token` used to sit here.
            // They were dropped by `m20260822_000002_drop_oauth_account_tokens`:
            // nothing read them back, so the instance was carrying somebody
            // else's live provider credentials — and rotating them on every
            // rekey — for no feature at all (card_51dd82b6dc82).
            //
            // Unlike the columns above this one exists on every instance that
            // has started once, which is what makes the preflight bite on a
            // deployment that stores nothing else encrypted.
            instance_signing_key, SeedEncrypted, seed_encrypted,
                "instance_signing_key.seed_encrypted", required;
            mirror, PasswordEncrypted, password_encrypted,
                "mirrors.password_encrypted", optional;
            // Read back on every delivery to sign it, so this one is encrypted
            // rather than hashed — see `crate::webhook::service`.
            webhook, SecretEncrypted, secret_encrypted,
                "webhooks.secret_encrypted", optional;
        }
    };
}

pub(crate) use with_encrypted_columns;

macro_rules! labels_array {
    ($($entity:ident, $col:ident, $field:ident, $label:literal, $opt:tt;)*) => {
        &[$($label),*]
    };
}

/// Every registered column label, in registry order.
///
/// Exposed so a test can assert that a rotation report covers the same set the
/// preflight samples, rather than trusting the two to have stayed in step.
pub const LABELS: &[&str] = with_encrypted_columns!(labels_array);
