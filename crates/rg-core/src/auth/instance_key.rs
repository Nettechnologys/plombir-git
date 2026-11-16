//! This instance's Ed25519 identities — the keys that sign release provenance
//! attestations and CI OIDC tokens, and back the CI OIDC JWKS.
//!
//! ## Why stored, why several, why re-read
//!
//! The key used to be derived from `jwt_secret` (`SHA-256(LEGACY_DOMAIN ||
//! jwt_secret)`), so anyone who ever knew that secret can recompute it, and a
//! later rotation of the secret changes nothing about the key. It is therefore
//! stored, and a fresh installation generates it at random.
//!
//! A replaced key is kept as a public half (`retired`, or `revoked` when it is
//! known to be compromised) instead of being overwritten, so what it signed
//! still verifies on this server and a signature under a revoked key is
//! refused by name. Only the active row keeps its sealed seed.
//!
//! The running server does not hold the key for its lifetime: [`InstanceKeys`]
//! re-reads the table at most [`CACHE_TTL`] after a rotation, so a key taken out
//! of service by the CLI stops signing on every replica without a restart.
//!
//! An instance upgraded with a key derived from `jwt_secret` rotates it away on
//! its first start ([`establish`]): retiring it keeps every stored attestation
//! verifiable here, and the derived key leaves the JWKS after [`JWKS_OVERLAP`].

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rg_db::entities::instance_signing_key::{
    self as row, ORIGIN_GENERATED, ORIGIN_LEGACY_DERIVED, STATUS_ACTIVE, STATUS_RETIRED,
    STATUS_REVOKED,
};
use rg_db::ops::instance_signing_key_ops as ops;
use sea_orm::{DatabaseConnection, TransactionTrait};
use sha2::{Digest, Sha256};

use crate::auth::encryption;

/// Domain separator of the original derivation. Frozen: it is how a key
/// adopted from an old instance is recognised and reproduced.
const LEGACY_DOMAIN: &[u8] = b"forgekeep-ci-oidc-ed25519-v1\0";

/// How long a running process may keep signing with a key after it was taken
/// out of service elsewhere.
pub const CACHE_TTL: Duration = Duration::from_secs(30);

/// How long a retired key stays in the JWKS: the 5-minute lifetime of a CI OIDC
/// token it may have signed, clock skew, and [`CACHE_TTL`] on a replica that
/// had not yet noticed the rotation. A revoked key is never published.
///
/// Seen from outside, a retired key can stay in a relying party's copy of the
/// JWKS for this overlap, plus [`CACHE_TTL`] and one read on the slowest
/// replica, plus the response's `max-age` of 300 seconds.
pub const JWKS_OVERLAP: chrono::Duration = chrono::Duration::minutes(10);

/// The instance's signing key pair plus the `kid` its public half advertises.
///
/// A distinct type rather than a `&str` secret on purpose: swapping one `&str`
/// parameter for another compiles silently at every call site, and that is how
/// the signing secret and the at-rest key once stayed tangled
/// (card_d740512de0a8).
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

    /// A key from the operating system's RNG.
    pub fn generate() -> Self {
        use rand::RngCore;
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        Self::from_seed(seed)
    }

    /// The pre-0.4.3 derivation. Used to adopt the key an old instance signed
    /// with, and by tests that want a deterministic identity without a database.
    pub fn derived_from_secret(secret: &str) -> Self {
        let mut hash = Sha256::new();
        hash.update(LEGACY_DOMAIN);
        hash.update(secret.as_bytes());
        Self::from_seed(hash.finalize().into())
    }

    /// The raw seed. Only the persistence path should need this.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    Active,
    Retired,
    Revoked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyOrigin {
    Generated,
    LegacyDerived,
}

impl KeyStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => STATUS_ACTIVE,
            Self::Retired => STATUS_RETIRED,
            Self::Revoked => STATUS_REVOKED,
        }
    }
}

impl KeyOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generated => ORIGIN_GENERATED,
            Self::LegacyDerived => ORIGIN_LEGACY_DERIVED,
        }
    }
}

/// The public half of one key this instance has held, and what it may still do.
#[derive(Clone, Debug)]
pub struct PublicKey {
    pub kid: String,
    pub verifying: VerifyingKey,
    pub status: KeyStatus,
    pub origin: KeyOrigin,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub retired_at: Option<chrono::DateTime<chrono::Utc>>,
    pub revoked_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The active signing key and every public half this instance has held.
#[derive(Clone, Debug)]
pub struct KeyRing {
    active: InstanceKey,
    keys: Vec<PublicKey>,
}

impl KeyRing {
    /// A ring holding one generated active key — for tests and other callers
    /// that have no history.
    pub fn single(active: InstanceKey) -> Self {
        let keys = vec![PublicKey {
            kid: active.kid().to_owned(),
            verifying: active.verifying_key(),
            status: KeyStatus::Active,
            origin: KeyOrigin::Generated,
            created_at: chrono::Utc::now(),
            retired_at: None,
            revoked_at: None,
        }];
        Self { active, keys }
    }

    /// A ring with arbitrary statuses, for tests of the code that reads one.
    #[cfg(test)]
    pub(crate) fn from_parts(active: InstanceKey, keys: Vec<PublicKey>) -> Self {
        Self { active, keys }
    }

    /// The key that signs.
    pub fn active(&self) -> &InstanceKey {
        &self.active
    }

    /// Every key, oldest first.
    pub fn keys(&self) -> &[PublicKey] {
        &self.keys
    }

    pub fn find(&self, kid: &str) -> Option<&PublicKey> {
        self.keys.iter().find(|key| key.kid == kid)
    }

    /// What the JWKS publishes at `now`: the active key and keys retired less
    /// than [`JWKS_OVERLAP`] ago. Never a revoked key.
    pub fn published(&self, now: chrono::DateTime<chrono::Utc>) -> Vec<&PublicKey> {
        self.keys
            .iter()
            .filter(|key| match key.status {
                KeyStatus::Active => true,
                KeyStatus::Retired => key.retired_at.is_some_and(|at| at > now - JWKS_OVERLAP),
                KeyStatus::Revoked => false,
            })
            .collect()
    }
}

/// The key ring as the running server sees it: re-read from the database at
/// most [`CACHE_TTL`] after the last read.
pub struct InstanceKeys {
    source: Option<(DatabaseConnection, [u8; 32])>,
    ttl: Duration,
    cached: Mutex<(Instant, Arc<KeyRing>)>,
}

impl InstanceKeys {
    /// Keys backed by the database, starting from an already loaded ring.
    pub fn new(db: DatabaseConnection, encryption_key: &str, ring: KeyRing) -> Self {
        Self {
            source: Some((db, encryption::derive_key(encryption_key))),
            ttl: CACHE_TTL,
            cached: Mutex::new((Instant::now(), Arc::new(ring))),
        }
    }

    /// A ring that is never re-read — for states built without a database.
    pub fn fixed(ring: KeyRing) -> Self {
        Self {
            source: None,
            ttl: Duration::MAX,
            cached: Mutex::new((Instant::now(), Arc::new(ring))),
        }
    }

    /// Override [`CACHE_TTL`]; tests use it to observe a rotation without
    /// sleeping.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// The ring, re-read when the cached copy is older than the TTL. A failed
    /// read is an error, not a fallback to the stale ring: signing with a key
    /// that may have been revoked is the failure this type exists to prevent.
    pub async fn current(&self) -> Result<Arc<KeyRing>> {
        {
            let cached = self.cached.lock().expect("instance key cache poisoned");
            if self.source.is_none() || cached.0.elapsed() < self.ttl {
                return Ok(cached.1.clone());
            }
        }
        self.fresh().await
    }

    /// The ring as stored right now.
    pub async fn fresh(&self) -> Result<Arc<KeyRing>> {
        let Some((db, cipher_key)) = &self.source else {
            return Ok(self
                .cached
                .lock()
                .expect("instance key cache poisoned")
                .1
                .clone());
        };
        // Stamped before the read, so the TTL bound is honest (TTL plus one
        // read), and a slow read that started earlier never replaces a
        // snapshot taken after it.
        let started = Instant::now();
        let ring = Arc::new(load_ring(db, cipher_key).await?);
        let mut cached = self.cached.lock().expect("instance key cache poisoned");
        if started >= cached.0 {
            *cached = (started, ring.clone());
            Ok(ring)
        } else {
            Ok(cached.1.clone())
        }
    }
}

/// Load the ring on server start, establishing a key on first use and retiring
/// a key derived from `jwt_secret`.
///
/// - No key recorded and nothing signed by the derived key → a random key.
/// - No key recorded, but a stored attestation is signed by the key derived
///   from the current `jwt_secret` (an instance upgraded from before keys were
///   stored) → adopt that key so it keeps verifying, then retire it below.
/// - Keys recorded but none active (an operator retired the active key, e.g.
///   after losing the encryption key) → always a random key: the derived key
///   may already be recorded and must never sign again.
/// - Active key `legacy_derived` → routine rotation: the derived key keeps
///   verifying its attestations here and leaves the JWKS after
///   [`JWKS_OVERLAP`], a random key signs from now on.
///
/// Replicas starting together race on the `active_slot` index and the rotation
/// compare-and-set; every loser re-reads, so they end up with one identity.
pub async fn establish(
    db: &DatabaseConnection,
    jwt_secret: &str,
    encryption_key: &str,
) -> Result<KeyRing> {
    let cipher_key = encryption::derive_key(encryption_key);
    fill_missing_identities(db, &cipher_key).await?;

    if ops::find_active(db).await?.is_none() {
        let derived = InstanceKey::derived_from_secret(jwt_secret);
        let attested = ops::all(db).await?.is_empty() && attested_by(db, derived.kid()).await?;
        let (candidate, origin) = if attested {
            (derived, ORIGIN_LEGACY_DERIVED)
        } else {
            (InstanceKey::generate(), ORIGIN_GENERATED)
        };
        if let Err(insert_error) = insert_key(db, &candidate, origin, &cipher_key).await {
            // Another process established a key between the read and the
            // insert; its row is the authority.
            if ops::find_active(db).await?.is_none() {
                return Err(insert_error.context("store the instance signing key"));
            }
        } else if attested {
            tracing::info!(
                kid = %candidate.kid(),
                "adopted the instance signing key derived from the JWT secret so stored release \
                 attestations keep verifying; it is retired immediately"
            );
        }
    }

    let active = ops::find_active(db)
        .await?
        .ok_or_else(|| anyhow!("no active instance signing key after establishing one"))?;
    if active.origin == ORIGIN_LEGACY_DERIVED {
        if let Some(rotation) =
            rotate_row(db, &cipher_key, &active, RotationReason::Routine).await?
        {
            tracing::warn!(
                retired_kid = %rotation.previous_kid,
                kid = %rotation.key.kid(),
                "retired the instance signing key derived from the JWT secret. External \
                 verifiers must refetch /api/v1/ci/oidc/jwks; attestations signed with the \
                 retired key verify on this server only — re-sign assets that need external \
                 verification. If the JWT secret may have leaked, run \
                 `plombir-git revoke-instance-key {}`",
                rotation.previous_kid
            );
        }
    }

    let ring = load_ring(db, &cipher_key).await?;
    warn_about_unknown_attestation_keys(db, &ring).await;
    Ok(ring)
}

/// Load the ring for a CLI command: fills in identities the migration could not
/// compute, then reads every key.
pub async fn load(db: &DatabaseConnection, encryption_key: &str) -> Result<KeyRing> {
    let cipher_key = encryption::derive_key(encryption_key);
    fill_missing_identities(db, &cipher_key).await?;
    load_ring(db, &cipher_key).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationReason {
    /// The old key keeps verifying what it signed and stays in the JWKS for
    /// [`JWKS_OVERLAP`].
    Routine,
    /// The old key is revoked: gone from the JWKS at once, and a signature
    /// under it is refused.
    Compromised,
}

pub struct Rotation {
    pub previous_kid: String,
    pub key: InstanceKey,
}

/// Replace the active key. Fails if another process rotated it concurrently,
/// so an operator never believes they retired a key they did not.
pub async fn rotate(
    db: &DatabaseConnection,
    encryption_key: &str,
    reason: RotationReason,
) -> Result<Rotation> {
    let cipher_key = encryption::derive_key(encryption_key);
    fill_missing_identities(db, &cipher_key).await?;
    let active = ops::find_active(db)
        .await?
        .ok_or_else(|| anyhow!("this instance has no signing key yet; start the server once"))?;
    rotate_row(db, &cipher_key, &active, reason)
        .await?
        .ok_or_else(|| {
            anyhow!(
                "the instance signing key was rotated concurrently; check `instance-key-status`"
            )
        })
}

/// Revoke a retired key, e.g. the derived key retired on upgrade once it turns
/// out the JWT secret leaked. The active key is refused: revoking it would
/// leave nothing to sign with.
pub async fn revoke(db: &DatabaseConnection, encryption_key: &str, kid: &str) -> Result<()> {
    // MySQL compares the kid column case-insensitively, PostgreSQL and SQLite
    // exactly; a kid is 16 lowercase hex digits everywhere it is written.
    let kid = kid.trim().to_ascii_lowercase();
    if kid.len() != 16 || !kid.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("a kid is 16 hexadecimal digits, as `instance-key-status` prints it");
    }
    let kid = kid.as_str();
    fill_missing_identities(db, &encryption::derive_key(encryption_key)).await?;
    let Some(key) = ops::find_by_kid(db, kid).await? else {
        bail!("this instance has no signing key with kid {kid}");
    };
    match key.status.as_str() {
        STATUS_REVOKED => Ok(()),
        STATUS_ACTIVE => bail!(
            "kid {kid} is the active signing key; replace it with \
             `plombir-git rotate-instance-key --reason compromised`"
        ),
        STATUS_RETIRED => {
            if ops::revoke_retired(db, kid).await? == 1 {
                tracing::warn!(kid, "instance signing key revoked");
                Ok(())
            } else {
                bail!("kid {kid} changed status concurrently; check `instance-key-status`")
            }
        }
        other => bail!("kid {kid} has an unknown status {other:?}"),
    }
}

/// Retire or revoke `active` and insert a new generated key, in one
/// transaction. `None` when `active` was no longer the active row.
async fn rotate_row(
    db: &DatabaseConnection,
    cipher_key: &[u8; 32],
    active: &row::Model,
    reason: RotationReason,
) -> Result<Option<Rotation>> {
    let previous_kid = active
        .kid
        .clone()
        .ok_or_else(|| anyhow!("the active instance signing key has no kid"))?;
    let key = InstanceKey::generate();
    let txn = db.begin().await.context("begin the key rotation")?;
    let revoke = reason == RotationReason::Compromised;
    if ops::retire_active(&txn, active.id, revoke).await? != 1 {
        txn.rollback().await.ok();
        return Ok(None);
    }
    insert_key(&txn, &key, ORIGIN_GENERATED, cipher_key).await?;
    txn.commit().await.context("commit the key rotation")?;
    Ok(Some(Rotation { previous_kid, key }))
}

async fn insert_key(
    db: &impl sea_orm::ConnectionTrait,
    key: &InstanceKey,
    origin: &str,
    cipher_key: &[u8; 32],
) -> Result<()> {
    let sealed = encrypt_seed(key, cipher_key)?;
    ops::insert_active(
        db,
        key.kid(),
        &hex::encode(key.verifying_key().as_bytes()),
        &sealed,
        origin,
    )
    .await
    .context("store the instance signing key")?;
    Ok(())
}

/// Compute `kid` and the public key of rows carried over from the singleton
/// table, which the migration could not open. Server start and every key CLI
/// command run it, so a database migrated offline is complete before use.
async fn fill_missing_identities(db: &DatabaseConnection, cipher_key: &[u8; 32]) -> Result<()> {
    for key in ops::all(db).await? {
        if key.kid.is_some() {
            continue;
        }
        let sealed = key.seed_encrypted.as_deref().ok_or_else(|| {
            anyhow!(
                "instance signing key {} has neither a kid nor a seed",
                key.id
            )
        })?;
        let opened = decode_seed(sealed, cipher_key)?;
        ops::fill_identity(
            db,
            key.id,
            opened.kid(),
            &hex::encode(opened.verifying_key().as_bytes()),
        )
        .await?;
    }
    Ok(())
}

async fn load_ring(db: &DatabaseConnection, cipher_key: &[u8; 32]) -> Result<KeyRing> {
    let mut active = None;
    let mut keys = Vec::new();
    for key in ops::all(db)
        .await
        .context("read the instance signing keys")?
    {
        let kid = key
            .kid
            .clone()
            .ok_or_else(|| anyhow!("instance signing key {} has no kid yet", key.id))?;
        let status = match key.status.as_str() {
            STATUS_ACTIVE => KeyStatus::Active,
            STATUS_RETIRED => KeyStatus::Retired,
            STATUS_REVOKED => KeyStatus::Revoked,
            other => bail!("instance signing key {kid} has an unknown status {other:?}"),
        };
        let origin = match key.origin.as_str() {
            ORIGIN_GENERATED => KeyOrigin::Generated,
            ORIGIN_LEGACY_DERIVED => KeyOrigin::LegacyDerived,
            other => bail!("instance signing key {kid} has an unknown origin {other:?}"),
        };
        let public: [u8; 32] = hex::decode(key.public_key.as_deref().unwrap_or_default())
            .ok()
            .and_then(|raw| raw.try_into().ok())
            .ok_or_else(|| anyhow!("instance signing key {kid} has no valid public key"))?;
        let verifying = VerifyingKey::from_bytes(&public)
            .with_context(|| format!("instance signing key {kid} has an invalid public key"))?;
        if status == KeyStatus::Active {
            let sealed = key
                .seed_encrypted
                .as_deref()
                .ok_or_else(|| anyhow!("the active instance signing key {kid} has no seed"))?;
            let opened = decode_seed(sealed, cipher_key)?;
            if opened.kid() != kid {
                bail!("the active instance signing key does not match its recorded kid {kid}");
            }
            if active.is_some() {
                bail!("more than one active instance signing key is recorded");
            }
            active = Some(opened);
        }
        keys.push(PublicKey {
            kid,
            verifying,
            status,
            origin,
            created_at: key.created_at,
            retired_at: key.retired_at,
            revoked_at: key.revoked_at,
        });
    }
    let active = active.ok_or_else(|| anyhow!("this instance has no active signing key"))?;
    Ok(KeyRing { active, keys })
}

/// Whether a stored attestation carries a signature under `kid`. The text
/// match narrows the scan; the parse decides.
async fn attested_by(db: &DatabaseConnection, kid: &str) -> Result<bool> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};
    let rows = rg_db::entities::release_asset::Entity::find()
        .filter(rg_db::entities::release_asset::Column::Attestation.contains(kid))
        .limit(20)
        .all(db)
        .await
        .context("look for stored release attestations")?;
    Ok(rows
        .iter()
        .filter_map(|row| row.attestation.as_deref())
        .filter_map(|json| serde_json::from_str::<crate::attestation::Envelope>(json).ok())
        .any(|envelope| envelope.signatures.iter().any(|sig| sig.keyid == kid)))
}

/// Name, once per start, attestations signed by a key this instance never
/// recorded — an instance that rotated `jwt_secret` before keys were stored.
/// They cannot be made to verify again, and silence there is what once made
/// the defect take days to recognise. Best-effort: a diagnostic must not stop a
/// start.
async fn warn_about_unknown_attestation_keys(db: &DatabaseConnection, ring: &KeyRing) {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};

    let rows = match rg_db::entities::release_asset::Entity::find()
        .filter(rg_db::entities::release_asset::Column::Attestation.is_not_null())
        .limit(5)
        .all(db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::debug!(error = %e, "could not sample stored attestations");
            return;
        }
    };
    let unknown: Vec<String> = rows
        .iter()
        .filter_map(|row| row.attestation.as_deref())
        .filter_map(|json| serde_json::from_str::<crate::attestation::Envelope>(json).ok())
        .flat_map(|envelope| envelope.signatures)
        .map(|signature| signature.keyid)
        .filter(|keyid| ring.find(keyid).is_none())
        .collect();
    if !unknown.is_empty() {
        tracing::warn!(
            kid = %ring.active().kid(),
            stored_kids = %unknown.join(", "),
            "release attestations stored here were signed by keys this instance never recorded — \
             its JWT secret was rotated before signing keys were stored, and those envelopes \
             cannot verify again. Re-sign the affected assets"
        );
    }
}

fn encrypt_seed(key: &InstanceKey, cipher_key: &[u8; 32]) -> Result<String> {
    encryption::encrypt(&hex::encode(key.seed()), cipher_key)
        .context("encrypt the instance signing key")
}

/// Decrypt and decode a stored seed. A failure names the encryption key,
/// because that is what it means operationally.
fn decode_seed(seed_encrypted: &str, cipher_key: &[u8; 32]) -> Result<InstanceKey> {
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
#[path = "instance_key_tests.rs"]
mod tests;
