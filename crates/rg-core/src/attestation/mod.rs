//! Detached, opt-in provenance attestations for release assets.
//!
//! An attestation binds a release asset (by its SHA-256) to a signed, typed
//! provenance statement. The design is deliberately standards-shaped so it is
//! verifiable *outside* Plombir Git:
//!
//! - **Payload**: an [in-toto Statement v1](types::Statement) — `_type`,
//!   `subject[].digest.sha256`, `predicateType`, `predicate`.
//! - **Canonicalization**: [RFC 8785 JCS](jcs) before signing, so the exact
//!   bytes are reproducible across languages (deterministic provenance).
//! - **Signature**: Ed25519 over the [DSSE] Pre-Authentication Encoding (PAE),
//!   which domain-separates the payload with its `payloadType` and length
//!   prefixes. The signing key is the [instance
//!   key](crate::auth::instance_key) — its public half is already served at
//!   `/api/v1/ci/oidc/jwks`, so no new key has to be distributed to verifiers,
//!   and it is stored rather than derived from `jwt_secret` so that rotating
//!   the signing secret does not invalidate every envelope ever issued.
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

use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, Verifier};
use serde_json::Value;

use crate::auth::instance_key::InstanceKey;
pub use predicate::{PredicateVerifier, VerifierRegistry, PLOMBIR_GIT_PROVENANCE_TYPE};
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
pub fn sign_statement(key: &InstanceKey, statement: &Statement) -> Result<Envelope> {
    let value = serde_json::to_value(statement).context("serialize attestation statement")?;
    let payload = jcs::to_canonical_bytes(&value).context("canonicalize attestation payload")?;
    let msg = pae(DSSE_PAYLOAD_TYPE, &payload);

    let signature = key.sign(&msg);

    Ok(Envelope {
        payload_type: DSSE_PAYLOAD_TYPE.to_string(),
        payload: STANDARD.encode(&payload),
        signatures: vec![EnvelopeSignature {
            keyid: key.kid().to_string(),
            sig: STANDARD.encode(signature.to_bytes()),
        }],
    })
}

/// Convenience: build a Plombir Git provenance statement for an asset and sign it.
///
/// `builder_id` attributes the build to the issuing instance (e.g. its external
/// URL); `predicate_extra` is merged into the predicate for extra context
/// (release id, uploader, timestamp — all deterministic, no floats).
pub fn sign_asset_provenance(
    key: &InstanceKey,
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
        PLOMBIR_GIT_PROVENANCE_TYPE.to_string(),
        predicate,
    );
    sign_statement(key, &statement)
}

/// A successfully verified attestation.
#[derive(Debug, Clone)]
pub struct Verified {
    /// The decoded, signature-checked statement.
    pub statement: Statement,
    /// The `kid` of the signature that verified.
    pub keyid: String,
}

/// What a verification concluded — and, first of all, whether it concluded
/// anything about the asset's bytes at all.
///
/// The distinction is the whole point of this type. Folding everything that is
/// not [`Verified`](Self::Verified) into one boolean says "the bytes no longer
/// match what was signed" about an envelope this instance merely could not
/// read, which is an accusation of tampering aimed at an asset nobody has
/// touched (card_4579598691ce).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VerificationStatus {
    /// Signature, subject-digest binding and predicate all hold.
    Verified,
    /// Cryptographic evidence *contradicts* the attestation: the asset's
    /// current digest is not the digest that was signed, or a signature
    /// offered under this instance's own `kid` does not verify. This is the
    /// loud one — the only status that means "these bytes are not those
    /// bytes".
    Mismatch,
    /// The check could not reach a verdict, and therefore says nothing about
    /// the asset: an envelope this instance cannot read, a predicate type it
    /// has no verifier for, a statement carrying no subject digest, or an
    /// envelope signed before the provenance key was rotated away.
    Undeterminable,
}

/// A verification that did not end in [`VerificationStatus::Verified`],
/// carrying *which* of the two non-verified answers it is.
///
/// The message is unchanged from what the checks always reported; the status is
/// the part a caller must not have to recover by matching on that text.
#[derive(Debug)]
pub struct VerifyError {
    status: VerificationStatus,
    source: anyhow::Error,
}

impl VerifyError {
    /// Evidence contradicts the attestation — see
    /// [`VerificationStatus::Mismatch`].
    fn mismatch(source: anyhow::Error) -> Self {
        Self {
            status: VerificationStatus::Mismatch,
            source,
        }
    }

    /// No verdict could be reached — see
    /// [`VerificationStatus::Undeterminable`].
    fn undeterminable(source: anyhow::Error) -> Self {
        Self {
            status: VerificationStatus::Undeterminable,
            source,
        }
    }

    /// Which non-verified answer this is. Never
    /// [`VerificationStatus::Verified`].
    pub fn status(&self) -> VerificationStatus {
        self.status
    }
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Forwarded verbatim, alternate flag included, so `{e:#}` still prints
        // the whole `anyhow` context chain the checks build up.
        if f.alternate() {
            write!(f, "{:#}", self.source)
        } else {
            write!(f, "{}", self.source)
        }
    }
}

impl std::error::Error for VerifyError {}

/// Verify a detached envelope against the instance key and an expected asset
/// digest, then run the type-specific predicate verifier.
///
/// The error carries a [`VerificationStatus`] separating the two answers that
/// must never be reported as one:
///
/// - [`Mismatch`](VerificationStatus::Mismatch) — the statement's subject
///   SHA-256 does not equal `expected_sha256` (the asset was tampered with, or
///   the attestation belongs to different bytes), or a signature carrying this
///   instance's `kid` does not verify under it.
/// - [`Undeterminable`](VerificationStatus::Undeterminable) — the envelope is
///   not a readable DSSE/in-toto document, its statement carries no subject
///   digest, its predicate type has no registered verifier or fails its
///   type-specific checks, or it was signed under a key this instance no
///   longer holds. None of these observes the asset's bytes, so none of them
///   may be reported as tampering.
pub fn verify_envelope(
    key: &InstanceKey,
    envelope: &Envelope,
    expected_sha256: &str,
    registry: &VerifierRegistry,
) -> std::result::Result<Verified, VerifyError> {
    if envelope.payload_type != DSSE_PAYLOAD_TYPE {
        return Err(VerifyError::undeterminable(anyhow!(
            "unexpected DSSE payloadType '{}' (want '{}')",
            envelope.payload_type,
            DSSE_PAYLOAD_TYPE
        )));
    }

    let payload = STANDARD
        .decode(envelope.payload.as_bytes())
        .context("decode attestation payload")
        .map_err(VerifyError::undeterminable)?;
    let msg = pae(DSSE_PAYLOAD_TYPE, &payload);

    let verifying = key.verifying_key();
    let expected_kid = key.kid();

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
    let keyid = match verified_kid {
        Some(keyid) => keyid,
        // Naming the mismatch is the difference between "your asset is
        // suspect" and "this instance no longer holds the key that signed it".
        // The second is what an operator sees after deliberately rotating the
        // provenance key, and it used to be indistinguishable from tampering
        // (card_3aecf3708ebe). It is also not a verdict about the bytes at
        // all, which is why it carries `Undeterminable` rather than a message
        // wrapped around `false`.
        None if envelope.signatures.iter().all(|s| s.keyid != expected_kid) => {
            let offered: Vec<&str> = envelope
                .signatures
                .iter()
                .map(|s| s.keyid.as_str())
                .collect();
            return Err(VerifyError::undeterminable(anyhow!(
                "attestation was signed by a different instance key (kid {}) than this instance \
                 now holds (kid {expected_kid}) — it predates a rotation of the provenance \
                 signing key",
                if offered.is_empty() {
                    "none".to_string()
                } else {
                    offered.join(", ")
                }
            )));
        }
        // A signature offered under *this* instance's kid that does not verify
        // is the one signature failure that is evidence: the envelope's bytes
        // are not the bytes this key signed.
        None => {
            return Err(VerifyError::mismatch(anyhow!(
                "no signature verified under the instance key"
            )))
        }
    };

    let statement: Statement = serde_json::from_slice(&payload)
        .context("parse attestation statement")
        .map_err(VerifyError::undeterminable)?;

    match statement.subject_sha256() {
        Some(sha) if sha == expected_sha256 => {}
        Some(sha) => {
            return Err(VerifyError::mismatch(anyhow!(
                "attestation subject digest {sha} does not match asset digest {expected_sha256}"
            )))
        }
        None => {
            return Err(VerifyError::undeterminable(anyhow!(
                "attestation statement has no sha256 subject digest"
            )))
        }
    }

    // Everything past the digest binding is a statement about the *predicate*,
    // not about the asset's bytes — those have already been proven to be the
    // signed ones. An unknown predicate type, or a predicate body this
    // instance's verifier rejects, therefore leaves the asset unaccused.
    registry
        .verify_predicate(&statement)
        .context("predicate verification failed")
        .map_err(VerifyError::undeterminable)?;

    Ok(Verified { statement, keyid })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn instance_key() -> InstanceKey {
        InstanceKey::derived_from_secret("instance-secret")
    }

    // SHA-256 of the empty string — a convenient fixed digest for vectors.
    const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn sample_statement() -> Statement {
        Statement::new(
            "app-1.0.tar.gz",
            EMPTY_SHA256,
            PLOMBIR_GIT_PROVENANCE_TYPE.to_string(),
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
        let a = sign_statement(&instance_key(), &sample_statement()).unwrap();
        let b = sign_statement(&instance_key(), &sample_statement()).unwrap();
        assert_eq!(a, b, "Ed25519 signing must be deterministic (RFC 8032)");
    }

    #[test]
    fn sign_then_verify_roundtrips() {
        let env = sign_statement(&instance_key(), &sample_statement()).unwrap();
        let reg = VerifierRegistry::with_defaults();
        let verified = verify_envelope(&instance_key(), &env, EMPTY_SHA256, &reg).unwrap();
        assert_eq!(verified.statement.subject_sha256(), Some(EMPTY_SHA256));
        assert_eq!(verified.keyid, instance_key().kid());
    }

    /// The loud verdict, and the only one allowed to be loud: the asset's
    /// bytes are not the bytes the signature covers.
    #[test]
    fn tampered_asset_digest_is_a_mismatch() {
        let env = sign_statement(&instance_key(), &sample_statement()).unwrap();
        let reg = VerifierRegistry::with_defaults();
        let other = "0".repeat(64);
        let error = verify_envelope(&instance_key(), &env, &other, &reg)
            .expect_err("a digest that is not the signed one must not verify");
        assert_eq!(error.status(), VerificationStatus::Mismatch);
        assert!(format!("{error:#}").contains("does not match asset digest"));
    }

    /// A different instance key must not verify — and the refusal must say so
    /// in those words. "Invalid signature" about an envelope this instance
    /// signed before its key was rotated reads as tampering; naming both `kid`s
    /// is what tells the operator it is a key change (card_3aecf3708ebe).
    #[test]
    fn a_different_instance_key_fails_and_names_the_mismatch() {
        let env = sign_statement(&instance_key(), &sample_statement()).unwrap();
        let reg = VerifierRegistry::with_defaults();
        let other = InstanceKey::derived_from_secret("a-rotated-instance");
        let error = verify_envelope(&other, &env, EMPTY_SHA256, &reg)
            .expect_err("an envelope signed by another key must not verify");
        let message = format!("{error:#}");
        assert!(message.contains(instance_key().kid()), "{message}");
        assert!(message.contains(other.kid()), "{message}");
        assert!(message.contains("rotation"), "{message}");
        // And not only in words: a rotated key means this instance cannot
        // check the envelope at all, which is a different answer from "these
        // bytes were substituted" (card_4579598691ce).
        assert_eq!(error.status(), VerificationStatus::Undeterminable);
    }

    /// A signature offered under *this* instance's own kid that does not hold
    /// is evidence, not a gap: the envelope is not what this key signed.
    #[test]
    fn flipped_signature_bit_is_a_mismatch() {
        let mut env = sign_statement(&instance_key(), &sample_statement()).unwrap();
        // Corrupt one signature byte.
        let mut raw = STANDARD.decode(env.signatures[0].sig.as_bytes()).unwrap();
        raw[0] ^= 0x01;
        env.signatures[0].sig = STANDARD.encode(&raw);
        let reg = VerifierRegistry::with_defaults();
        let error = verify_envelope(&instance_key(), &env, EMPTY_SHA256, &reg)
            .expect_err("a corrupted signature must not verify");
        assert_eq!(error.status(), VerificationStatus::Mismatch);
    }

    #[test]
    fn valid_signature_with_foreign_kid_is_rejected() {
        let mut env = sign_statement(&instance_key(), &sample_statement()).unwrap();
        env.signatures[0].keyid = "deadbeef".to_string();
        let reg = VerifierRegistry::with_defaults();
        let error = verify_envelope(&instance_key(), &env, EMPTY_SHA256, &reg)
            .expect_err("a signature advertising a foreign kid must not verify");
        // This instance holds no key that signed it, so it has no verdict to
        // give about the bytes.
        assert_eq!(error.status(), VerificationStatus::Undeterminable);
    }

    /// The card's first acceptance: a well-signed envelope whose predicate type
    /// this instance has no verifier for is *unreadable to us*, not tampering.
    /// The signature holds and the subject digest binds these exact bytes —
    /// the only thing missing is our ability to interpret the predicate.
    #[test]
    fn an_unknown_predicate_type_is_undeterminable_not_a_mismatch() {
        let statement = Statement::new(
            "app-1.0.tar.gz",
            EMPTY_SHA256,
            "https://someone-elses.example/attestation/v9".to_string(),
            json!({ "whatever": true }),
        );
        let env = sign_statement(&instance_key(), &statement).unwrap();
        let reg = VerifierRegistry::with_defaults();
        let error = verify_envelope(&instance_key(), &env, EMPTY_SHA256, &reg)
            .expect_err("an unregistered predicate type is still not verified");
        assert_eq!(
            error.status(),
            VerificationStatus::Undeterminable,
            "no registered verifier is a gap in this instance, not a claim about the asset: {error:#}"
        );
        assert!(format!("{error:#}").contains("no verifier registered"));
    }

    /// Same principle one step in: the predicate type is ours, the signature
    /// and digest hold, and only the predicate body fails its own checks. That
    /// is a malformed attestation, not substituted bytes.
    #[test]
    fn a_rejected_predicate_body_is_undeterminable_not_a_mismatch() {
        let statement = Statement::new(
            "app-1.0.tar.gz",
            EMPTY_SHA256,
            PLOMBIR_GIT_PROVENANCE_TYPE.to_string(),
            json!({ "builder": { "id": "" } }),
        );
        let env = sign_statement(&instance_key(), &statement).unwrap();
        let reg = VerifierRegistry::with_defaults();
        let error = verify_envelope(&instance_key(), &env, EMPTY_SHA256, &reg)
            .expect_err("an empty builder.id must not verify");
        assert_eq!(error.status(), VerificationStatus::Undeterminable);
    }

    /// An envelope that is not a readable DSSE document says nothing about the
    /// asset either — including the case where the payload is not base64 at all.
    #[test]
    fn an_unreadable_envelope_is_undeterminable() {
        let signed = sign_statement(&instance_key(), &sample_statement()).unwrap();

        let mut wrong_type = signed.clone();
        wrong_type.payload_type = "application/vnd.something+json".to_string();
        let reg = VerifierRegistry::with_defaults();
        assert_eq!(
            verify_envelope(&instance_key(), &wrong_type, EMPTY_SHA256, &reg)
                .expect_err("a foreign payloadType must not verify")
                .status(),
            VerificationStatus::Undeterminable
        );

        let mut undecodable = signed;
        undecodable.payload = "not-base64!!".to_string();
        assert_eq!(
            verify_envelope(&instance_key(), &undecodable, EMPTY_SHA256, &reg)
                .expect_err("an undecodable payload must not verify")
                .status(),
            VerificationStatus::Undeterminable
        );
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
            &instance_key(),
            "app.tar.gz",
            EMPTY_SHA256,
            "https://forge.example",
            json!({ "release_id": 7, "uploader_id": 3 }),
        )
        .unwrap();
        let reg = VerifierRegistry::with_defaults();
        let v = verify_envelope(&instance_key(), &env, EMPTY_SHA256, &reg).unwrap();
        assert_eq!(v.statement.predicate["release_id"], json!(7));
        assert_eq!(
            v.statement.predicate["builder"]["id"],
            json!("https://forge.example")
        );
    }
}
