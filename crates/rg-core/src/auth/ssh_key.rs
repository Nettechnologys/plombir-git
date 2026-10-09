//! SSH public key fingerprint utilities.

use anyhow::{bail, Context, Result};
use base64::Engine as _;

/// Compute the SHA-256 fingerprint from an OpenSSH public key string.
///
/// Input format: `"ssh-ed25519 AAAA... comment"`
/// Output: `"SHA256:base64url..."` (matches `ssh-keygen -l -E sha256`)
pub fn fingerprint_from_openssh(pubkey: &str) -> Result<String> {
    if pubkey.contains('\r') || pubkey.contains('\n') {
        bail!("public key must be a single line");
    }

    // Split off the key type and base64 blob
    let mut parts = pubkey.split_whitespace();
    let key_type = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("empty public key"))?;
    let b64 = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("missing key blob"))?;

    if !matches!(
        key_type,
        "ssh-ed25519"
            | "ssh-rsa"
            | "ecdsa-sha2-nistp256"
            | "ecdsa-sha2-nistp384"
            | "ecdsa-sha2-nistp521"
            | "sk-ssh-ed25519@openssh.com"
            | "sk-ecdsa-sha2-nistp256@openssh.com"
    ) {
        bail!("unsupported SSH public key type: {key_type}");
    }

    let raw = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64))
        .context("invalid base64 key blob")?;
    // The length prefix is read as a fixed-size chunk, so the blob being too
    // short to hold one is the same expression that produces it. Asking
    // `raw.len() < 4` first and then unwrapping a `try_into()` on the slice
    // spent an operator's process on a malformed public key the moment either
    // half moved.
    let Some(algorithm_len) = raw.first_chunk::<4>().copied() else {
        bail!("invalid SSH public key blob");
    };
    let algorithm_len = u32::from_be_bytes(algorithm_len) as usize;
    let Some(encoded_type) = raw.get(4..4 + algorithm_len) else {
        bail!("invalid SSH public key blob");
    };
    let encoded_type =
        std::str::from_utf8(encoded_type).context("invalid SSH public key algorithm")?;
    if encoded_type != key_type {
        bail!("SSH public key type does not match encoded key blob");
    }

    // SHA-256 hash
    let digest = sha256(&raw);

    // Standard-alphabet base64 without padding: the OpenSSH fingerprint format.
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest);
    Ok(format!("SHA256:{}", encoded))
}

fn sha256(data: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519_KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA test";

    #[test]
    fn fingerprints_valid_openssh_key() {
        let fingerprint = fingerprint_from_openssh(ED25519_KEY).unwrap();
        assert!(fingerprint.starts_with("SHA256:"));
    }

    /// card_9903905d92a3: the four-byte algorithm-name length prefix used to be
    /// read as `raw[0..4].try_into().unwrap()` behind a separate `raw.len() < 4`
    /// guard, and the name itself as a bare `&raw[4..4 + algorithm_len]` slice
    /// behind a second one. Both inputs below are public keys a stranger can
    /// paste into the key form, and each one lands on a different guard.
    #[test]
    fn a_truncated_key_blob_is_refused_rather_than_ending_the_process() {
        let engine = base64::engine::general_purpose::STANDARD;

        for (case, blob) in [
            // Three bytes: shorter than the length prefix itself.
            ("shorter than the length prefix", engine.encode([0u8, 0, 0])),
            // A prefix claiming 32 bytes of algorithm name over a blob holding
            // one.
            (
                "an algorithm name longer than the blob",
                engine.encode([0u8, 0, 0, 32, b'x']),
            ),
            // The largest length a `u32` can spell, which is where a naive
            // `4 + algorithm_len` slice indexes far past the end.
            (
                "the largest length prefix a u32 can spell",
                engine.encode([0xffu8, 0xff, 0xff, 0xff, b'x']),
            ),
        ] {
            assert!(
                fingerprint_from_openssh(&format!("ssh-ed25519 {blob}")).is_err(),
                "{case} must be a refusal, not a panic"
            );
        }
    }

    #[test]
    fn rejects_invalid_or_mismatched_key_blob() {
        assert!(fingerprint_from_openssh("ssh-ed25519 !!!").is_err());
        assert!(fingerprint_from_openssh(
            "ssh-rsa AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
        )
        .is_err());
        assert!(fingerprint_from_openssh("ssh-dss AAAA").is_err());
    }
}
