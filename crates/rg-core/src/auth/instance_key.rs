//! This instance's long-lived Ed25519 identity — the key that signs release
//! provenance attestations and backs the CI OIDC JWKS.
//!
//! ## Why this is a stored key and not a derived one
//!
//! It used to be derived: `SHA-256("forgekeep-ci-oidc-ed25519-v1\0" ||
//! jwt_secret)`. That made the instance's *public* identity a function of a
//! secret operators are told to rotate the moment a token leaks, and rotating
//! it silently invalidated everything the old key had ever signed
//! (card_3aecf3708ebe):
//!
//! - every DSSE envelope already stored in `release_assets.attestation` stopped
//!   verifying, so `POST .../attestation/verify` answered "invalid" about a
//!   signature this very server had issued;
//! - the `kid` published at `/api/v1/ci/oidc/jwks` changed under every external
//!   verifier that had already fetched it.
//!
//! A signing key whose public half has been published cannot be re-derived from
//! a rotatable config value. So it is established once, stored, and outlives
//! every later change to `jwt_secret`.
//!
//! ## Why the first key is adopted, not generated
//!
//! On first start the seed is not random — it is exactly the value the old
//! derivation would have produced from the current `jwt_secret`. Adoption is
//! therefore a byte-for-byte no-op at the moment it happens: the same key, the
//! same `kid`, the same signatures still verifying. Generating a fresh key here
//! would have made *this release* the event that breaks every stored
//! attestation, which is the exact failure being fixed. Deliberate replacement
//! is a separate, explicit act — [`rotate`].
//!
//! The seed is encrypted at rest under `[auth].encryption_key`, like every
//! other stored secret: a database dump must not hand over the private half of
//! a key that external verifiers trust.

use anyhow::{bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use sea_orm::DatabaseConnection;
use sha2::{Digest, Sha256};

use crate::auth::encryption;

/// Domain separator of the original derivation. Frozen: changing these bytes
/// changes the identity of every instance that has not yet stored a key.
const LEGACY_DOMAIN: &[u8] = b"forgekeep-ci-oidc-ed25519-v1\0";

/// The instance's Ed25519 key pair plus the `kid` its public half advertises.
///
/// Carried in application state and passed by reference to everything that
/// signs or verifies. It is a distinct type rather than the `&str` secret it
/// replaces on purpose: swapping one `&str` parameter for another compiles
/// silently at every call site, and that is precisely how the signing secret
/// and the at-rest key stayed tangled for so long (card_d740512de0a8).
#[derive(Clone)]
pub struct InstanceKey {
    signing: SigningKey,
    kid: String,
}

impl InstanceKey {
    /// Build the key pair from a raw 32-byte Ed25519 seed.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        let signing = SigningKey::from_bytes(&seed);
        let kid = key_id(&signing.verifying_key());
        Self { signing, kid }
    }

    /// The pre-card_3aecf3708ebe derivation: the key an instance holds until it
    /// stores one. Public because that is also the right key for a test that
    /// wants a deterministic identity without a database.
    pub fn derived_from_secret(secret: &str) -> Self {
        let mut hash = Sha256::new();
        hash.update(LEGACY_DOMAIN);
        hash.update(secret.as_bytes());
        Self::from_seed(hash.finalize().into())
    }

    /// The raw seed. Only the persistence path below should need this.
    pub fn seed(&self) -> [u8; 32] {
        self.signing.to_bytes()
    }

    /// The JWKS `kid` of the public half — the first 8 bytes of its SHA-256.
    pub fn kid(&self) -> &str {
        &self.kid
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.signing.verifying_key()
    }

    pub fn sign(&self, message: &[u8]) -> Signature {
        self.signing.sign(message)
    }

    /// The private half, for the one caller that has to hand it to a JWT
    /// encoder in PKCS#8 form.
    pub(crate) fn signing_key(&self) -> &SigningKey {
        &self.signing
    }
}

/// Never let the private half reach a log line through a stray `{:?}`.
impl std::fmt::Debug for InstanceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstanceKey")
            .field("kid", &self.kid)
            .finish_non_exhaustive()
    }
}

fn key_id(verifying: &VerifyingKey) -> String {
    hex::encode(&Sha256::digest(verifying.as_bytes())[..8])
}

/// Load this instance's key, establishing it on first use.
///
/// - Row present → decrypt the stored seed. That key is the instance's identity
///   regardless of what `jwt_secret` is today, which is the whole point.
/// - Row absent → adopt the key the old derivation produces from
///   `jwt_secret` *now*, store it, and use it. Identical key, identical `kid`,
///   every existing attestation keeps verifying — and from this moment on the
///   signing secret can be rotated without touching the instance's identity.
///
/// Two servers starting against the same database race on that insert; the
/// loser re-reads and both end up with the same key, never with two identities.
pub async fn load_or_adopt(
    db: &DatabaseConnection,
    jwt_secret: &str,
    encryption_key: &str,
) -> Result<InstanceKey> {
    let cipher_key = encryption::derive_key(encryption_key);

    if let Some(row) = rg_db::ops::instance_signing_key_ops::find(db)
        .await
        .context("read the stored instance signing key")?
    {
        return decode_row(&row.seed_encrypted, &cipher_key);
    }

    let adopted = InstanceKey::derived_from_secret(jwt_secret);
    let encrypted = encrypt_seed(&adopted, &cipher_key)?;

    match rg_db::ops::instance_signing_key_ops::insert(db, &encrypted).await {
        Ok(_) => {
            tracing::info!(
                kid = %adopted.kid(),
                "adopted this instance's provenance signing key from the JWT secret and stored it; \
                 rotating the JWT secret no longer changes the key that signs release attestations"
            );
            warn_about_orphaned_attestations(db, &adopted).await;
            Ok(adopted)
        }
        // Another process inserted between the read and the write. Its row is
        // the authority — deriving the same value twice is expected, but the
        // stored row is what every future start will read.
        Err(insert_error) => {
            let row = rg_db::ops::instance_signing_key_ops::find(db)
                .await
                .context("re-read the instance signing key after a concurrent insert")?
                .ok_or_else(|| {
                    anyhow::anyhow!("could not store the instance signing key: {insert_error}")
                })?;
            decode_row(&row.seed_encrypted, &cipher_key)
        }
    }
}

/// Name, once, the one case adoption cannot repair.
///
/// An instance that rotated `jwt_secret` *before* this key was stored has
/// attestations signed by a key derived from the previous secret; the key
/// adopted now comes from the current one, so those envelopes will never
/// verify again. Adoption cannot guess the old secret, but it can refuse to be
/// silent about it — that silence is exactly what made the defect take days to
/// recognise. Best-effort: a diagnostic must not be able to stop a start, so a
/// failed read is dropped rather than propagated.
async fn warn_about_orphaned_attestations(db: &DatabaseConnection, adopted: &InstanceKey) {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};

    let rows = match rg_db::entities::release_asset::Entity::find()
        .filter(rg_db::entities::release_asset::Column::Attestation.is_not_null())
        .limit(5)
        .all(db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(error = %e, "could not sample stored attestations while adopting the instance key");
            return;
        }
    };

    let foreign: Vec<String> = rows
        .iter()
        .filter_map(|row| row.attestation.as_deref())
        .filter_map(|json| serde_json::from_str::<crate::attestation::Envelope>(json).ok())
        .flat_map(|envelope| envelope.signatures)
        .map(|signature| signature.keyid)
        .filter(|keyid| keyid != adopted.kid())
        .collect();

    if !foreign.is_empty() {
        tracing::warn!(
            adopted_kid = %adopted.kid(),
            stored_kids = %foreign.join(", "),
            "release attestations already stored here were signed by a different key than the one \
             just adopted — this instance's JWT secret was rotated before the provenance key was \
             made independent of it, and those envelopes cannot be made to verify again. \
             Re-sign the affected assets"
        );
    }
}

/// Replace the instance's identity with a freshly generated key.
///
/// Destructive by nature and never automatic: every attestation signed with the
/// previous key stops verifying, and every external verifier holding the old
/// JWKS has to refetch it. It exists so a *leaked* signing key can actually be
/// retired — which, now that the key no longer follows `jwt_secret`, has no
/// other route.
pub async fn rotate(db: &DatabaseConnection, encryption_key: &str) -> Result<InstanceKey> {
    use rand::RngCore;

    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let key = InstanceKey::from_seed(seed);

    let encrypted = encrypt_seed(&key, &encryption::derive_key(encryption_key))?;
    rg_db::ops::instance_signing_key_ops::replace(db, &encrypted)
        .await
        .context("store the rotated instance signing key")?;

    tracing::warn!(
        kid = %key.kid(),
        "the instance provenance signing key was replaced; attestations signed with the previous \
         key no longer verify and external verifiers must refetch the JWKS"
    );
    Ok(key)
}

fn encrypt_seed(key: &InstanceKey, cipher_key: &[u8; 32]) -> Result<String> {
    encryption::encrypt(&hex::encode(key.seed()), cipher_key)
        .context("encrypt the instance signing key")
}

/// Decrypt and decode a stored seed.
///
/// A failure here is an operator-facing event, not a corrupt-data footnote: it
/// means the configured `encryption_key` is not the one this row was written
/// with, so the message names that rather than leaving a hex-decode error to be
/// puzzled over.
fn decode_row(seed_encrypted: &str, cipher_key: &[u8; 32]) -> Result<InstanceKey> {
    let hex_seed = encryption::decrypt(seed_encrypted, cipher_key).context(
        "the stored instance signing key could not be decrypted with the configured \
         [auth].encryption_key — pin the key this database was written with",
    )?;
    let raw = hex::decode(hex_seed.trim()).context("decode the stored instance signing key")?;
    let seed: [u8; 32] = match raw.try_into() {
        Ok(seed) => seed,
        Err(raw) => bail!(
            "the stored instance signing key is {} bytes, expected 32",
            (raw as Vec<u8>).len()
        ),
    };
    Ok(InstanceKey::from_seed(seed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_database;

    const JWT: &str = "the-signing-secret";
    const ENC: &str = "the-at-rest-key";

    /// The identity an upgrading instance adopts must be the identity it
    /// already had, or the upgrade itself invalidates every stored attestation.
    #[tokio::test]
    async fn the_first_load_adopts_the_key_the_old_derivation_produced() {
        let db = migrated_memory_database().await;
        let loaded = load_or_adopt(&db, JWT, ENC).await.unwrap();
        assert_eq!(loaded.kid(), InstanceKey::derived_from_secret(JWT).kid());
        assert_eq!(loaded.seed(), InstanceKey::derived_from_secret(JWT).seed());
    }

    /// The card's acceptance, at the level where the defect lived: once stored,
    /// the key stops being a function of the JWT secret.
    #[tokio::test]
    async fn rotating_the_jwt_secret_leaves_the_stored_key_alone() {
        let db = migrated_memory_database().await;
        let before = load_or_adopt(&db, JWT, ENC).await.unwrap();
        let after = load_or_adopt(&db, "a-completely-different-secret", ENC)
            .await
            .unwrap();
        assert_eq!(before.kid(), after.kid());
        assert_eq!(before.seed(), after.seed());
    }

    /// A second start must not mint a second identity.
    #[tokio::test]
    async fn loading_twice_is_idempotent_and_writes_one_row() {
        let db = migrated_memory_database().await;
        load_or_adopt(&db, JWT, ENC).await.unwrap();
        load_or_adopt(&db, JWT, ENC).await.unwrap();
        let row = rg_db::ops::instance_signing_key_ops::find(&db)
            .await
            .unwrap()
            .expect("key row");
        assert_eq!(row.id, rg_db::ops::instance_signing_key_ops::SINGLETON_ID);
        assert!(row.rotated_at.is_none());
    }

    /// The escape hatch has to actually change the identity — and say so in the
    /// row, so "why did every attestation stop verifying" has a dated answer.
    #[tokio::test]
    async fn rotation_replaces_the_identity_and_is_recorded() {
        let db = migrated_memory_database().await;
        let original = load_or_adopt(&db, JWT, ENC).await.unwrap();
        let rotated = rotate(&db, ENC).await.unwrap();
        assert_ne!(original.kid(), rotated.kid());

        let reloaded = load_or_adopt(&db, JWT, ENC).await.unwrap();
        assert_eq!(reloaded.kid(), rotated.kid());
        let row = rg_db::ops::instance_signing_key_ops::find(&db)
            .await
            .unwrap()
            .expect("key row");
        assert!(row.rotated_at.is_some());
    }

    /// The stored seed is ciphertext, not the key sitting in a text column.
    #[tokio::test]
    async fn the_seed_is_encrypted_at_rest() {
        let db = migrated_memory_database().await;
        let key = load_or_adopt(&db, JWT, ENC).await.unwrap();
        let row = rg_db::ops::instance_signing_key_ops::find(&db)
            .await
            .unwrap()
            .expect("key row");
        assert!(encryption::looks_like_ciphertext(&row.seed_encrypted));
        assert!(!row.seed_encrypted.contains(&hex::encode(key.seed())));
        assert!(load_or_adopt(&db, JWT, "the-wrong-at-rest-key")
            .await
            .is_err());
    }

    /// The one case adoption cannot repair must at least be named. An instance
    /// that rotated `jwt_secret` before the key was stored keeps envelopes no
    /// key it can derive will ever verify — silence there is what made the
    /// original defect take days to recognise.
    #[tokio::test]
    async fn adoption_warns_about_attestations_signed_by_an_earlier_key() {
        let db = migrated_memory_database().await;
        // The subject here is "does adoption notice a foreign kid", not
        // referential integrity: the row only has to carry an envelope, so the
        // asset gets a synthetic parent instead of a repository/release chain
        // that proves nothing about this code.
        sea_orm::ConnectionTrait::execute_unprepared(&db, "PRAGMA foreign_keys = OFF")
            .await
            .expect("suspend foreign keys for the fixture");

        let earlier = InstanceKey::derived_from_secret("the-secret-before-the-rotation");
        let statement = crate::attestation::Statement::new(
            "app.tar.gz",
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            crate::attestation::FORGEKEEP_PROVENANCE_TYPE.to_string(),
            serde_json::json!({ "builder": { "id": "https://forge.example" } }),
        );
        let envelope = crate::attestation::sign_statement(&earlier, &statement).unwrap();
        rg_db::ops::release_ops::create_asset(
            &db,
            rg_db::entities::release_asset::ActiveModel {
                release_id: sea_orm::Set(1),
                filename: sea_orm::Set("app.tar.gz".into()),
                size: sea_orm::Set(1),
                content_type: sea_orm::Set("application/octet-stream".into()),
                download_count: sea_orm::Set(0),
                uploader_id: sea_orm::Set(Some(1)),
                created_at: sea_orm::Set(chrono::Utc::now()),
                sha256: sea_orm::Set(Some("e3b0".into())),
                attestation: sea_orm::Set(Some(serde_json::to_string(&envelope).unwrap())),
                ..Default::default()
            },
        )
        .await
        .expect("store an attestation signed by the earlier key");

        let (logs, _guard) = crate::test_support::CapturedLogs::capture();
        let adopted = load_or_adopt(&db, JWT, ENC).await.unwrap();
        let rendered = logs.rendered();
        assert!(rendered.contains(earlier.kid()), "{rendered}");
        assert!(rendered.contains(adopted.kid()), "{rendered}");
    }

    /// `{:?}` on application state must not print the private half.
    #[test]
    fn debug_shows_the_kid_and_nothing_secret() {
        let key = InstanceKey::derived_from_secret(JWT);
        let rendered = format!("{key:?}");
        assert!(rendered.contains(key.kid()));
        assert!(!rendered.contains(&hex::encode(key.seed())));
    }
}
