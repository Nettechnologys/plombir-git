//! Pluggable predicate-verifier registry, keyed by the statement's
//! `predicateType` discriminator.
//!
//! Signature + digest verification (see [`super::verify_envelope`]) is
//! type-agnostic: it proves the bytes were signed by the instance key and that
//! they bind the asset's SHA-256. Anything *type-specific* — "does this SLSA
//! provenance carry
//! a builder id?", "is this a Sigstore bundle?" — lives behind this registry so
//! new attestation types can be added without touching the crypto core.

use anyhow::{bail, Result};
use std::collections::HashMap;

use super::types::Statement;

/// A verifier for one `predicateType`. Runs *after* the signature and the
/// subject-digest binding have already been checked.
pub trait PredicateVerifier: Send + Sync {
    /// The `predicateType` discriminator this verifier handles.
    fn predicate_type(&self) -> &str;
    /// Validate the predicate body's shape/semantics. Return `Err` to reject.
    fn verify(&self, statement: &Statement) -> Result<()>;
}

/// Registry dispatching a statement to the verifier registered for its
/// `predicateType`. Unknown types are rejected (fail-closed): a verifier that
/// silently accepts predicate types it does not understand is not a verifier.
#[derive(Default)]
pub struct VerifierRegistry {
    verifiers: HashMap<String, Box<dyn PredicateVerifier>>,
}

impl VerifierRegistry {
    /// An empty registry — every predicate type is unknown (rejected).
    pub fn new() -> Self {
        Self::default()
    }

    /// Registry pre-loaded with the built-in Plombir Git provenance verifiers:
    /// the type issued today and the one issued before the rename.
    pub fn with_defaults() -> Self {
        let mut reg = Self::new();
        for predicate_type in [PLOMBIR_GIT_PROVENANCE_TYPE, FORGEKEEP_PROVENANCE_TYPE] {
            reg.register(Box::new(SlsaProvenanceVerifier { predicate_type }));
        }
        reg
    }

    /// Register (or replace) the verifier for a predicate type.
    pub fn register(&mut self, verifier: Box<dyn PredicateVerifier>) {
        self.verifiers
            .insert(verifier.predicate_type().to_string(), verifier);
    }

    /// Dispatch `statement` to its registered verifier; error if none.
    pub fn verify_predicate(&self, statement: &Statement) -> Result<()> {
        match self.verifiers.get(&statement.predicate_type) {
            Some(v) => v.verify(statement),
            None => bail!(
                "no verifier registered for predicate type '{}'",
                statement.predicate_type
            ),
        }
    }

    /// Whether a verifier is registered for `predicate_type`.
    pub fn handles(&self, predicate_type: &str) -> bool {
        self.verifiers.contains_key(predicate_type)
    }
}

/// The Plombir Git provenance predicate type (SLSA-provenance shaped) that
/// every new attestation is issued under.
///
/// It names the predicate's format, not its issuer: every instance stamps the
/// same string, and who signed is told by `builder.id` and the key's `kid`.
/// The URI sits under a domain the project controls so that nobody else can
/// define what it means.
pub const PLOMBIR_GIT_PROVENANCE_TYPE: &str = "https://plombir.com/git/provenance/v1";

/// The same predicate format under the project's former name, issued until
/// card_f835b97ca951. Verify-only: it sits inside every DSSE envelope signed
/// before then, and the verifier is looked up by it, so dropping it would
/// leave those attestations unverifiable. Nothing signs under it any more:
/// the project does not hold `forgekeep.dev`.
const FORGEKEEP_PROVENANCE_TYPE: &str = "https://forgekeep.dev/provenance/v1";

/// Verifier for the Plombir Git provenance predicate, under either of its
/// types: requires a `builder.id` string so a consumer can attribute the build
/// to an issuing instance.
pub struct SlsaProvenanceVerifier {
    predicate_type: &'static str,
}

impl PredicateVerifier for SlsaProvenanceVerifier {
    fn predicate_type(&self) -> &str {
        self.predicate_type
    }

    fn verify(&self, statement: &Statement) -> Result<()> {
        let builder_id = statement
            .predicate
            .get("builder")
            .and_then(|b| b.get("id"))
            .and_then(|id| id.as_str());
        match builder_id {
            Some(id) if !id.is_empty() => Ok(()),
            _ => bail!("provenance predicate missing non-empty builder.id"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn statement(predicate_type: &str, predicate: serde_json::Value) -> Statement {
        Statement::new(
            "asset.bin",
            "0".repeat(64),
            predicate_type.to_string(),
            predicate,
        )
    }

    #[test]
    fn accepts_well_formed_provenance() {
        let reg = VerifierRegistry::with_defaults();
        let st = statement(
            PLOMBIR_GIT_PROVENANCE_TYPE,
            json!({ "builder": { "id": "https://forge.example/instance" } }),
        );
        assert!(reg.verify_predicate(&st).is_ok());
    }

    #[test]
    fn rejects_provenance_without_builder_id() {
        let reg = VerifierRegistry::with_defaults();
        let st = statement(PLOMBIR_GIT_PROVENANCE_TYPE, json!({ "builder": {} }));
        assert!(reg.verify_predicate(&st).is_err());
    }

    /// Attestations issued before the rename carry the former type and must
    /// keep verifying, under the same predicate checks as the current one.
    #[test]
    fn the_former_type_is_still_verified_with_the_same_checks() {
        let reg = VerifierRegistry::with_defaults();
        assert!(reg.handles("https://forgekeep.dev/provenance/v1"));
        let st = statement(
            FORGEKEEP_PROVENANCE_TYPE,
            json!({ "builder": { "id": "https://forge.example/instance" } }),
        );
        assert!(reg.verify_predicate(&st).is_ok());
        let st = statement(FORGEKEEP_PROVENANCE_TYPE, json!({ "builder": {} }));
        assert!(reg.verify_predicate(&st).is_err());
    }

    #[test]
    fn rejects_unknown_predicate_type() {
        let reg = VerifierRegistry::with_defaults();
        let st = statement("https://unknown.example/type", json!({}));
        assert!(reg.verify_predicate(&st).is_err());
    }
}
