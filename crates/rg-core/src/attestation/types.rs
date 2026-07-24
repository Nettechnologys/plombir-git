//! Wire types for detached attestations.
//!
//! The signed payload is an [in-toto Statement v1] and the detached wrapper is a
//! [DSSE envelope] — both widely-implemented standards, so an attestation
//! produced here is verifiable by cosign / slsa-verifier / any DSSE library in
//! any language, using the public key already published at the instance's CI
//! OIDC JWKS endpoint.
//!
//! [in-toto Statement v1]: https://github.com/in-toto/attestation/blob/main/spec/v1/statement.md
//! [DSSE envelope]: https://github.com/secure-systems-lab/dsse/blob/master/envelope.md

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// in-toto statement type URI.
pub const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
/// DSSE `payloadType` for an in-toto JSON statement.
pub const DSSE_PAYLOAD_TYPE: &str = "application/vnd.in-toto+json";

/// A subject the attestation is *about*, bound by content digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    /// Human-readable name (the asset filename).
    pub name: String,
    /// Algorithm → lowercase-hex digest. Always carries `sha256`.
    pub digest: BTreeMap<String, String>,
}

/// The in-toto statement — the exact object that gets canonicalized and signed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Statement {
    /// Statement type URI. Serialized as `_type` per the in-toto spec.
    #[serde(rename = "_type")]
    pub statement_type: String,
    /// Subjects bound by digest (single-element for a release asset).
    pub subject: Vec<Subject>,
    /// Predicate discriminator — the pluggable-verifier registry key.
    #[serde(rename = "predicateType")]
    pub predicate_type: String,
    /// Type-specific provenance body.
    pub predicate: Value,
}

impl Statement {
    /// Build a single-subject statement binding `sha256_hex` to `name`.
    pub fn new(
        name: impl Into<String>,
        sha256_hex: impl Into<String>,
        predicate_type: impl Into<String>,
        predicate: Value,
    ) -> Self {
        let mut digest = BTreeMap::new();
        digest.insert("sha256".to_string(), sha256_hex.into());
        Statement {
            statement_type: STATEMENT_TYPE.to_string(),
            subject: vec![Subject {
                name: name.into(),
                digest,
            }],
            predicate_type: predicate_type.into(),
            predicate,
        }
    }

    /// The `sha256` digest of the first subject, if present.
    pub fn subject_sha256(&self) -> Option<&str> {
        self.subject
            .first()
            .and_then(|s| s.digest.get("sha256"))
            .map(String::as_str)
    }
}

/// One signature line in a DSSE envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    /// JWK `kid` of the signing key (matches the CI OIDC JWKS `kid`).
    pub keyid: String,
    /// Base64 (standard alphabet, padded) Ed25519 signature over the DSSE PAE.
    pub sig: String,
}

/// A DSSE envelope: the detached, transportable attestation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Envelope {
    /// Always [`DSSE_PAYLOAD_TYPE`] here.
    #[serde(rename = "payloadType")]
    pub payload_type: String,
    /// Base64 (standard alphabet, padded) of the canonical statement bytes.
    pub payload: String,
    /// Detached signatures over the payload.
    pub signatures: Vec<Signature>,
}
