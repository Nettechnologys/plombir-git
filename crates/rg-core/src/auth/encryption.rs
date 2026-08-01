//! AES-256-GCM encryption utilities for sensitive data at rest.

use aes_gcm::{
    aead::{Aead, OsRng},
    Aes256Gcm, Key, KeyInit, Nonce,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::RngCore;
use sha2::{Digest, Sha256};

/// Bytes of framing every [`encrypt`] output carries: a 12-byte nonce prefix
/// plus the 16-byte AES-GCM authentication tag. A value shorter than this
/// cannot be one of ours, whatever else it looks like.
const FRAMING_LEN: usize = 12 + 16;

/// Derive the at-rest encryption key from the instance's encryption secret.
///
/// **The secret this takes is `[auth].encryption_key`, not `[auth].jwt_secret`.**
/// The two were one value until card_d740512de0a8: rotating the JWT signing
/// secret then silently re-keyed every encrypted column in the database — TOTP
/// secrets, CI secrets, mirror and LDAP passwords, SSO client secrets, OAuth
/// tokens — and each one surfaced as its own 500 in its own handler, long after
/// the restart that caused it. `encryption_key` still *defaults* to
/// `jwt_secret` (existing deployments have their data under it), but it is a
/// separate knob so signing-secret rotation stops being destructive, and
/// [`crate::auth::key_check`] refuses to start when the configured key no
/// longer opens what is already stored.
pub fn derive_key(secret: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result);
    key
}

/// Whether `value` has the shape of an [`encrypt`] output.
///
/// Structural only — it says nothing about *which* key would open the value,
/// and a `true` here is not a promise that [`decrypt`] succeeds. Its job is to
/// keep [`crate::auth::key_check`] from reading a legacy plaintext column (a
/// bare base32 TOTP secret, a password stored before that column was
/// encrypted) as "ciphertext this key failed to open", which would turn a
/// correctly-configured instance into a refused start.
pub fn looks_like_ciphertext(value: &str) -> bool {
    URL_SAFE_NO_PAD
        .decode(value)
        .is_ok_and(|bytes| bytes.len() > FRAMING_LEN)
}

pub fn encrypt(plaintext: &str, key: &[u8; 32]) -> Result<String, anyhow::Error> {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let mut nonce_bytes = [0u8; 12];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| anyhow::anyhow!("encryption failed: {}", e))?;

    let mut combined = Vec::with_capacity(12 + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(URL_SAFE_NO_PAD.encode(&combined))
}

pub fn decrypt(encoded: &str, key: &[u8; 32]) -> Result<String, anyhow::Error> {
    let combined = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|e| anyhow::anyhow!("base64 decode error: {}", e))?;
    if combined.len() < 12 {
        anyhow::bail!("ciphertext too short");
    }

    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| anyhow::anyhow!("decryption failed: {}", e))?;

    String::from_utf8(plaintext).map_err(|e| anyhow::anyhow!("invalid UTF-8: {}", e))
}

/// Replace sensitive values in output before it is persisted or streamed.
pub fn mask_values(input: &str, secrets: &[String]) -> String {
    let mut masked = input.to_owned();
    let mut values = secrets
        .iter()
        .filter(|value| value.len() >= 4)
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for value in values {
        masked = masked.replace(value.as_str(), "***");
    }
    masked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let key = derive_key("test-secret-key");
        let plaintext = "JBSWY3DPEHPK3PXP";
        let encrypted = encrypt(plaintext, &key).unwrap();
        let decrypted = decrypt(&encrypted, &key).unwrap();
        assert_eq!(plaintext, decrypted);
    }
}
