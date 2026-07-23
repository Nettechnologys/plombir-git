//! WebAuthn / passkey ceremony helpers, wrapping the `webauthn-rs` crate.
//!
//! ForgeKeep keeps no server-side session store, so the in-progress ceremony
//! state (`PasskeyRegistration` / `PasskeyAuthentication`) is persisted between
//! the *begin* and *finish* HTTP round-trips inside a short-lived, HttpOnly,
//! signed cookie — the same pattern the MFA login challenge uses
//! (`jwt::generate_mfa_challenge`). Signing gives integrity + expiry so a client
//! cannot swap in a challenge the server never issued.
//!
//! Login uses the **non-discoverable** (allow-list) flavour: the user supplies a
//! username, we look up their stored passkeys and hand the authenticator an
//! explicit credential allow-list. The WebAuthn user handle therefore never has
//! to look a user up, so it is derived deterministically from the numeric user
//! id (UUIDv5) instead of costing a stored column.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use chrono::Utc;
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use webauthn_rs::prelude::*;

pub use webauthn_rs::prelude::{
    AuthenticationResult, CreationChallengeResponse, CredentialID, Passkey, PasskeyAuthentication,
    PasskeyRegistration, PublicKeyCredential, RegisterPublicKeyCredential, RequestChallengeResponse,
    Url, Uuid, Webauthn,
};

/// Fixed namespace for deriving a stable per-user WebAuthn user handle from the
/// numeric user id. It only needs to be stable and unique per user; it is never
/// used to resolve a user at login (we authenticate against an explicit
/// credential allow-list keyed by username).
const USER_HANDLE_NAMESPACE: &[u8] = b"forgekeep:webauthn:user-handle:v1";

/// Default relying-party display name shown by some authenticators.
const RP_NAME: &str = "ForgeKeep";

/// Derive the stable WebAuthn user handle for a user id.
///
/// A SHA-256 of a fixed namespace + the user id, truncated to 16 bytes — a
/// deterministic UUID that needs no stored column and no extra `uuid` crate
/// feature (`v5` is not enabled workspace-wide).
pub fn user_handle(user_id: i64) -> Uuid {
    let mut hasher = Sha256::new();
    hasher.update(USER_HANDLE_NAMESPACE);
    hasher.update(user_id.to_le_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    Uuid::from_bytes(bytes)
}

/// Build a [`Webauthn`] instance for the given relying-party id and origin.
///
/// `rp_id` must be a registrable suffix of the origin host (e.g. `git.example.com`
/// for `https://git.example.com`, or `localhost` for `http://localhost:5173`).
pub fn build(rp_id: &str, rp_origin: &str) -> Result<Webauthn> {
    let origin = Url::parse(rp_origin)
        .with_context(|| format!("invalid webauthn origin: {rp_origin}"))?;
    let builder = WebauthnBuilder::new(rp_id, &origin)
        .map_err(|e| anyhow!("webauthn builder init failed: {e}"))?
        .rp_name(RP_NAME);
    builder
        .build()
        .map_err(|e| anyhow!("webauthn build failed: {e}"))
}

/// Begin a passkey registration ceremony. `exclude` lists the user's existing
/// credential ids so an already-registered authenticator is not enrolled twice.
pub fn start_registration(
    webauthn: &Webauthn,
    user_id: i64,
    username: &str,
    display_name: &str,
    exclude: Vec<CredentialID>,
) -> Result<(CreationChallengeResponse, PasskeyRegistration)> {
    let exclude = (!exclude.is_empty()).then_some(exclude);
    webauthn
        .start_passkey_registration(user_handle(user_id), username, display_name, exclude)
        .map_err(|e| anyhow!("start passkey registration failed: {e}"))
}

/// Complete a passkey registration ceremony, yielding the credential to store.
pub fn finish_registration(
    webauthn: &Webauthn,
    credential: &RegisterPublicKeyCredential,
    state: &PasskeyRegistration,
) -> Result<Passkey> {
    webauthn
        .finish_passkey_registration(credential, state)
        .map_err(|e| anyhow!("finish passkey registration failed: {e}"))
}

/// Begin a passkey authentication ceremony against the user's stored passkeys.
pub fn start_authentication(
    webauthn: &Webauthn,
    passkeys: &[Passkey],
) -> Result<(RequestChallengeResponse, PasskeyAuthentication)> {
    webauthn
        .start_passkey_authentication(passkeys)
        .map_err(|e| anyhow!("start passkey authentication failed: {e}"))
}

/// Complete a passkey authentication ceremony.
pub fn finish_authentication(
    webauthn: &Webauthn,
    credential: &PublicKeyCredential,
    state: &PasskeyAuthentication,
) -> Result<AuthenticationResult> {
    webauthn
        .finish_passkey_authentication(credential, state)
        .map_err(|e| anyhow!("finish passkey authentication failed: {e}"))
}

/// URL-safe base64 (no padding) encoding of a credential id, for DB storage and
/// uniqueness checks.
pub fn credential_id_b64(cred_id: &CredentialID) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(cred_id.as_ref())
}

/// Serialize a passkey to JSON for durable storage.
pub fn passkey_to_json(passkey: &Passkey) -> Result<String> {
    serde_json::to_string(passkey).context("serialize passkey")
}

/// Deserialize a passkey previously stored via [`passkey_to_json`].
pub fn passkey_from_json(json: &str) -> Result<Passkey> {
    serde_json::from_str(json).context("deserialize passkey")
}

// ── Signed ceremony-state cookie (begin ⇄ finish bridge) ──────────────────

#[derive(Serialize, Deserialize)]
struct Sealed<T> {
    data: T,
    exp: i64,
}

fn seal_key(purpose: &str, secret: &str) -> String {
    format!("forgekeep:webauthn:{purpose}:{secret}")
}

/// Sign arbitrary ceremony state into a compact, expiring token (JWT-shaped,
/// HS256) with a purpose-scoped, domain-separated key so a registration blob can
/// never be replayed as an authentication blob or as a session JWT.
pub fn seal_state<T: Serialize>(
    data: T,
    purpose: &str,
    secret: &str,
    ttl_secs: i64,
) -> Result<String> {
    let claims = Sealed {
        data,
        exp: Utc::now().timestamp() + ttl_secs,
    };
    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(seal_key(purpose, secret).as_bytes()),
    )
    .context("seal webauthn state")
}

/// Verify and decode ceremony state produced by [`seal_state`]. Returns `None`
/// if the token is missing, tampered, expired, or was sealed for a different
/// purpose.
pub fn unseal_state<T: DeserializeOwned>(token: &str, purpose: &str, secret: &str) -> Option<T> {
    decode::<Sealed<T>>(
        token,
        &DecodingKey::from_secret(seal_key(purpose, secret).as_bytes()),
        &Validation::default(),
    )
    .ok()
    .map(|decoded| decoded.claims.data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_handle_is_stable_and_distinct() {
        assert_eq!(user_handle(42), user_handle(42));
        assert_ne!(user_handle(42), user_handle(43));
    }

    #[test]
    fn build_accepts_localhost() {
        let wa = build("localhost", "http://localhost:5173");
        assert!(wa.is_ok());
    }

    #[test]
    fn build_rejects_mismatched_rp_id() {
        // rp_id must be a suffix of the origin host.
        assert!(build("example.com", "http://localhost:5173").is_err());
    }

    #[derive(Serialize, Deserialize, PartialEq, Debug)]
    struct Demo {
        v: u32,
    }

    #[test]
    fn sealed_state_roundtrips_and_is_purpose_scoped() {
        let token = seal_state(Demo { v: 7 }, "reg", "secret", 300).unwrap();
        let back: Demo = unseal_state(&token, "reg", "secret").unwrap();
        assert_eq!(back, Demo { v: 7 });

        // Wrong purpose / wrong secret must fail closed.
        assert!(unseal_state::<Demo>(&token, "auth", "secret").is_none());
        assert!(unseal_state::<Demo>(&token, "reg", "other").is_none());
    }

    #[test]
    fn sealed_state_expires() {
        // Past jsonwebtoken's default 60s exp leeway.
        let token = seal_state(Demo { v: 1 }, "reg", "secret", -120).unwrap();
        assert!(unseal_state::<Demo>(&token, "reg", "secret").is_none());
    }
}
