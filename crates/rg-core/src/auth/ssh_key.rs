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
    if raw.len() < 4 {
        bail!("invalid SSH public key blob");
    }
    let algorithm_len = u32::from_be_bytes(raw[0..4].try_into().unwrap()) as usize;
    if raw.len() < 4 + algorithm_len {
        bail!("invalid SSH public key blob");
    }
    let encoded_type = std::str::from_utf8(&raw[4..4 + algorithm_len])
        .context("invalid SSH public key algorithm")?;
    if encoded_type != key_type {
        bail!("SSH public key type does not match encoded key blob");
    }

    // SHA-256 hash
    let digest = sha256(&raw);

    // base64url without padding (standard SSH fingerprint format)
    let encoded = base64_encode_nopad(&digest);
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

fn base64_encode_nopad(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = chunk.get(1).copied().unwrap_or(0);
        let b2 = chunk.get(2).copied().unwrap_or(0);
        out.push(CHARS[((b0 >> 2) & 0x3f) as usize] as char);
        out.push(CHARS[(((b0 << 4) | (b1 >> 4)) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[(((b1 << 2) | (b2 >> 6)) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(CHARS[(b2 & 0x3f) as usize] as char);
        }
    }
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
