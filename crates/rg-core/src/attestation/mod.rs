//! Detached, opt-in provenance attestations for release assets.
//!
//! An attestation binds a release asset (by its SHA-256) to a signed, typed
//! provenance statement. The design is deliberately standards-shaped so it is
//! verifiable *outside* ForgeKeep:
//!
//! - **Payload**: an [in-toto Statement v1](types::Statement) — `_type`,
//!   `subject[].digest.sha256`, `predicateType`, `predicate`.
//! - **Canonicalization**: [RFC 8785 JCS](jcs) before signing, so the exact
//!   bytes are reproducible across languages (deterministic provenance).
//! - **Signature**: Ed25519 over the [DSSE] Pre-Authentication Encoding (PAE),
//!   which domain-separates the payload with its `payloadType` and length
//!   prefixes. The signing key is the instance key from [`crate::auth::ci_oidc`]
//!   — its public half is already served at `/api/v1/ci/oidc/jwks`, so no new
//!   key has to be distributed to verifiers.
//! - **Wrapper**: a [DSSE envelope](types::Envelope) stored detached from the
//!   asset bytes.
//! - **Verification**: signature + digest binding first, then a pluggable
//!   [predicate verifier](predicate::VerifierRegistry) dispatched on
//!   `predicateType`.
//!
//! [DSSE]: https://github.com/secure-systems-lab/dsse

mod jcs;
pub mod predicate;
pub mod types;

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, Signer, Verifier};
use serde_json::Value;

use crate::auth::ci_oidc;
pub use predicate::{PredicateVerifier, VerifierRegistry, FORGEKEEP_PROVENANCE_TYPE};
pub use types::{Envelope, Signature as EnvelopeSignature, Statement, Subject, DSSE_PAYLOAD_TYPE};

/// DSSE Pre-Authentication Encoding: `"DSSEv1" SP len(type) SP type SP
/// len(payload) SP payload`. This is what gets signed — the `payloadType` and
/// the length framing provide domain separation, so a signature over one
/// payload type can never be replayed as another.
fn pae(payload_type: &str, payload: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(payload.len() + payload_type.len() + 32);
    msg.extend_from_slice(b"DSSEv1 ");
    msg.extend_from_slice(payload_type.len().to_string().as_bytes());
    msg.push(b' ');
    msg.extend_from_slice(payload_type.as_bytes());
    msg.push(b' ');
    msg.extend_from_slice(payload.len().to_string().as_bytes());
    msg.push(b' ');
    msg.extend_from_slice(payload);
    msg
}

/// Sign an in-toto statement, producing a detached DSSE envelope.
///
/// The payload is JCS-canonicalized so the bytes are deterministic; the
/// signature is Ed25519 over the DSSE PAE of those bytes with the instance key.
pub fn sign_statement(secret: &str, statement: &Statement) -> Result<Envelope> {
    let value = serde_json::to_value(statement).context("serialize attestation statement")?;
    let payload = jcs::to_canonical_bytes(&value).context("canonicalize attestation payload")?;
    let msg = pae(DSSE_PAYLOAD_TYPE, &payload);

    let key = ci_oidc::signing_key(secret);
    let signature = key.sign(&msg);
    let keyid = ci_oidc::jwk(secret).kid;

    Ok(Envelope {
        payload_type: DSSE_PAYLOAD_TYPE.to_string(),
        payload: STANDARD.encode(&payload),
        signatures: vec![EnvelopeSignature {
            keyid,
            sig: STANDARD.encode(signature.to_bytes()),
        }],
    })
}

/// Convenience: build a ForgeKeep provenance statement for an asset and sign it.
///
/// `builder_id` attributes the build to the issuing instance (e.g. its external
/// URL); `predicate_extra` is merged into the predicate for extra context
/// (release id, uploader, timestamp — all deterministic, no floats).
pub fn sign_asset_provenance(
    secret: &str,
    filename: &str,
    sha256_hex: &str,
    builder_id: &str,
    predicate_extra: Value,
) -> Result<Envelope> {
    let mut predicate = serde_json::json!({ "builder": { "id": builder_id } });
    if let Value::Object(extra) = predicate_extra {
        if let Value::Object(base) = &mut predicate {
            for (k, v) in extra {
                base.insert(k, v);
            }
        }
    }
    let statement = Statement::new(
        filename,
        sha256_hex,
        FORGEKEEP_PROVENANCE_TYPE.to_string(),
        predicate,
    );
    sign_statement(secret, &statement)
}

/// A successfully verified attestation.
#[derive(Debug, Clone)]
pub struct Verified {
    /// The decoded, signature-checked statement.
    pub statement: Statement,
    /// The `kid` of the signature that verified.
    pub keyid: String,
}

/// Verify a detached envelope against the instance key and an expected asset
/// digest, then run the type-specific predicate verifier.
///
/// Fails (returns `Err`) if:
/// - no signature verifies under the instance key,
/// - the payload isn't a well-formed in-toto statement,
/// - the statement's subject SHA-256 doesn't equal `expected_sha256` (the asset
///   was tampered with, or the attestation belongs to different bytes),
/// - the predicate type has no registered verifier or fails its checks.
pub fn verify_envelope(
    secret: &str,
    envelope: &Envelope,
    expected_sha256: &str,
    registry: &VerifierRegistry,
) -> Result<Verified> {
    if envelope.payload_type != DSSE_PAYLOAD_TYPE {
        bail!(
            "unexpected DSSE payloadType '{}' (want '{}')",
            envelope.payload_type,
            DSSE_PAYLOAD_TYPE
        );
    }

    let payload = STANDARD
        .decode(envelope.payload.as_bytes())
        .context("decode attestation payload")?;
    let msg = pae(DSSE_PAYLOAD_TYPE, &payload);

    let verifying = ci_oidc::verifying_key(secret);
    let expected_kid = ci_oidc::jwk(secret).kid;

    // A signature verifies only when both the bytes AND the advertised kid match
    // the instance key — so a valid signature carrying someone else's kid is
    // still rejected.
    let mut verified_kid: Option<String> = None;
    for sig in &envelope.signatures {
        if sig.keyid != expected_kid {
            continue;
        }
        let raw = match STANDARD.decode(sig.sig.as_bytes()) {
            Ok(r) => r,
            Err(_) => continue,
        };
        let signature = match Signature::from_slice(&raw) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if verifying.verify(&msg, &signature).is_ok() {
            verified_kid = Some(sig.keyid.clone());
            break;
        }
    }
    let keyid = verified_kid.context("no signature verified under the instance key")?;

    let statement: Statement =
        serde_json::from_slice(&payload).context("parse attestation statement")?;

    match statement.subject_sha256() {
        Some(sha) if sha == expected_sha256 => {}
        Some(sha) => {
            bail!("attestation subject digest {sha} does not match asset digest {expected_sha256}")
        }
        None => bail!("attestation statement has no sha256 subject digest"),
    }

    registry
        .verify_predicate(&statement)
        .context("predicate verification failed")?;

    Ok(Verified { statement, keyid })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SECRET: &str = "instance-secret";
    // SHA-256 of the empty string — a convenient fixed digest for vectors.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn sample_statement() -> Statement {
        Statement::new(
            "app-1.0.tar.gz",
            EMPTY_SHA256,
            FORGEKEEP_PROVENANCE_TYPE.to_string(),
            json!({ "builder": { "id": "https://forge.example/instance" } }),
        )
    }

    #[test]
    fn canonical_payload_is_a_frozen_cross_language_vector() {
        // This exact byte string is the cross-language contract: any JCS
        // implementation must produce it, and the signature is over its DSSE
        // PAE. If this literal changes, existing attestations stop verifying.
        let value = serde_json::to_value(sample_statement()).unwrap();
        let bytes = jcs::to_canonical_bytes(&value).unwrap();
        let expected = concat!(
            r#"{"_type":"https://in-toto.io/Statement/v1","#,
            r#""predicate":{"builder":{"id":"https://forge.example/instance"}},"#,
            r#""predicateType":"https://forgekeep.dev/provenance/v1","#,
            r#""subject":[{"digest":{"sha256":""#,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            r#""},"name":"app-1.0.tar.gz"}]}"#,
        );
        assert_eq!(String::from_utf8(bytes).unwrap(), expected);
    }

    #[test]
    fn sign_is_deterministic() {
        let a = sign_statement(SECRET, &sample_statement()).unwrap();
        let b = sign_statement(SECRET, &sample_statement()).unwrap();
        assert_eq!(a, b, "Ed25519 signing must be deterministic (RFC 8032)");
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let env = sign_statement(SECRET, &sample_statement()).unwrap();
        let reg = VerifierRegistry::with_defaults();
        let verified = verify_envelope(SECRET, &env, EMPTY_SHA256, &reg).unwrap();
        assert_eq!(verified.statement.subject_sha256(), Some(EMPTY_SHA256));
        assert_eq!(verified.keyid, ci_oidc::jwk(SECRET).kid);
    }

    #[test]
    fn tampered_asset_digest_fails() {
        let env = sign_statement(SECRET, &sample_statement()).unwrap();
        let reg = VerifierRegistry::with_defaults();
        let other = "0".repeat(64);
        assert!(verify_envelope(SECRET, &env, &other, &reg).is_err());
    }

    #[test]
    fn wrong_key_fails() {
        let env = sign_statement(SECRET, &sample_statement()).unwrap();
        let reg = VerifierRegistry::with_defaults();
        assert!(verify_envelope("different-secret", &env, EMPTY_SHA256, &reg).is_err());
    }

    #[test]
    fn flipped_signature_bit_fails() {
        let mut env = sign_statement(SECRET, &sample_statement()).unwrap();
        // Corrupt one signature byte.
        let mut raw = STANDARD.decode(env.signatures[0].sig.as_bytes()).unwrap();
        raw[0] ^= 0x01;
        env.signatures[0].sig = STANDARD.encode(&raw);
        let reg = VerifierRegistry::with_defaults();
        assert!(verify_envelope(SECRET, &env, EMPTY_SHA256, &reg).is_err());
    }

    #[test]
    fn valid_signature_with_foreign_kid_is_rejected() {
        let mut env = sign_statement(SECRET, &sample_statement()).unwrap();
        env.signatures[0].keyid = "deadbeef".to_string();
        let reg = VerifierRegistry::with_defaults();
        assert!(verify_envelope(SECRET, &env, EMPTY_SHA256, &reg).is_err());
    }

    #[test]
    fn pae_domain_separates_payload_type() {
        // Different payloadType ⇒ different signed message.
        assert_ne!(pae("a", b"x"), pae("b", b"x"));
        // Length framing prevents ("ab","c") colliding with ("a","bc").
        assert_ne!(pae("ab", b"c"), pae("a", b"bc"));
    }

    #[test]
    fn provenance_helper_merges_extra_and_verifies() {
        let env = sign_asset_provenance(
            SECRET,
            "app.tar.gz",
            EMPTY_SHA256,
            "https://forge.example",
            json!({ "release_id": 7, "uploader_id": 3 }),
        )
        .unwrap();
        let reg = VerifierRegistry::with_defaults();
        let v = verify_envelope(SECRET, &env, EMPTY_SHA256, &reg).unwrap();
        assert_eq!(v.statement.predicate["release_id"], json!(7));
        assert_eq!(
            v.statement.predicate["builder"]["id"],
            json!("https://forge.example")
        );
    }
}
