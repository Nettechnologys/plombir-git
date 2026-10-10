//! AES-256-GCM encryption utilities for sensitive data at rest.

use aes_gcm::{
    aead::{Aead, OsRng},
    Aes256Gcm, Key, KeyInit, Nonce,
};
use base64::{
    engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD},
    Engine as _,
};
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
///
/// A multiline secret may be printed one line at a time, and shell jobs often
/// encode credentials before printing them. Mask the original, sufficiently
/// long individual lines, and their common transport encodings. Fragments
/// shorter than four bytes are deliberately skipped: replacing such fragments
/// would corrupt ordinary output without providing useful secrecy.
pub fn mask_values(input: &str, secrets: &[String]) -> String {
    let mut masked = input.to_owned();
    let mut values = std::collections::HashSet::new();
    for secret in secrets {
        for value in std::iter::once(secret.as_str()).chain(secret.lines()) {
            if value.len() < 4 {
                continue;
            }
            values.insert(value.to_owned());
            for bytes in [value.as_bytes().to_vec(), format!("{value}\n").into_bytes()] {
                for encoded in [
                    STANDARD.encode(&bytes),
                    STANDARD_NO_PAD.encode(&bytes),
                    URL_SAFE.encode(&bytes),
                    URL_SAFE_NO_PAD.encode(&bytes),
                ] {
                    // GNU `base64` wraps its output at 76 columns by default.
                    // `echo "$SECRET" | base64` also encodes a final newline.
                    if encoded.len() > 76 {
                        values.insert(
                            encoded
                                .as_bytes()
                                .chunks(76)
                                .map(|chunk| std::str::from_utf8(chunk).expect("base64 is ASCII"))
                                .collect::<Vec<_>>()
                                .join("\n"),
                        );
                    }
                    values.insert(encoded);
                }
            }
            let encoded = urlencoding::encode(value).into_owned();
            // Both `%2F` and `%2f` are valid percent encodings.
            let mut lower_hex = encoded.as_bytes().to_vec();
            for index in 0..lower_hex.len().saturating_sub(2) {
                if lower_hex[index] == b'%' {
                    lower_hex[index + 1] = lower_hex[index + 1].to_ascii_lowercase();
                    lower_hex[index + 2] = lower_hex[index + 2].to_ascii_lowercase();
                }
            }
            values.insert(String::from_utf8(lower_hex).expect("percent encoding is ASCII"));
            values.insert(encoded);
        }
    }
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    for value in values {
        masked = masked.replace(&value, "***");
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

    #[test]
    fn masks_multiline_secrets_and_encoded_forms() {
        let secret =
            "-----BEGIN PRIVATE KEY-----\nlong-private-material\n-----END PRIVATE KEY-----";
        let output = format!(
            "raw line: long-private-material\nencoded: {}\nurl: {}\nfull: {secret}",
            STANDARD.encode(secret),
            urlencoding::encode(secret)
        );
        let masked = mask_values(&output, &[secret.to_owned()]);
        assert!(!masked.contains("long-private-material"), "{masked}");
        assert!(!masked.contains(&STANDARD.encode(secret)), "{masked}");
        assert!(!masked.contains("%0A"), "{masked}");
        assert!(masked.matches("***").count() >= 3, "{masked}");
    }

    #[test]
    fn masks_padded_unpadded_and_url_safe_base64_without_short_fragments() {
        let secret = "token?";
        let output = format!(
            "{} {} {} {} token? ok",
            STANDARD.encode(secret),
            STANDARD_NO_PAD.encode(secret),
            URL_SAFE.encode(secret),
            URL_SAFE_NO_PAD.encode(secret)
        );
        let masked = mask_values(&output, &[secret.into(), "ok".into()]);
        assert!(!masked.contains("dG9rZW4"), "{masked}");
        assert!(masked.ends_with("*** ok"), "{masked}");
    }

    #[test]
    fn masks_wrapped_base64_from_echoing_a_long_secret() {
        let secret = "long-private-material-".repeat(8);
        let encoded = STANDARD.encode(format!("{secret}\n"));
        let wrapped = encoded
            .as_bytes()
            .chunks(76)
            .map(|chunk| std::str::from_utf8(chunk).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(wrapped.contains('\n'));
        assert_eq!(mask_values(&wrapped, &[secret]), "***");
    }
}
