//! Short-lived, audience-bound OIDC identity tokens for CI workloads.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{Duration, Utc};
use ed25519_dalek::pkcs8::EncodePrivateKey;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::auth::instance_key::InstanceKey;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CiOidcClaims {
    pub iss: String,
    pub sub: String,
    pub aud: String,
    pub iat: i64,
    pub nbf: i64,
    pub exp: i64,
    pub jti: String,
    pub repository_id: i64,
    pub pipeline_id: i64,
    pub job_id: i64,
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub sha: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CiOidcJwk {
    pub kty: &'static str,
    pub crv: &'static str,
    #[serde(rename = "use")]
    pub key_use: &'static str,
    pub alg: &'static str,
    pub kid: String,
    pub x: String,
}

/// The JWKS entry published for the instance key.
///
/// Sibling subsystems (release-asset attestation) sign and verify with the
/// *same* [`InstanceKey`] that backs this document — its public half is already
/// served at `/api/v1/ci/oidc/jwks`, so any external verifier can check those
/// signatures without a second key to distribute. That sharing is also why the
/// key must outlive `jwt_secret`: see [`crate::auth::instance_key`].
pub fn jwk(key: &InstanceKey) -> CiOidcJwk {
    let verifying = key.verifying_key();
    CiOidcJwk {
        kty: "OKP",
        crv: "Ed25519",
        key_use: "sig",
        alg: "EdDSA",
        kid: key.kid().to_string(),
        x: URL_SAFE_NO_PAD.encode(verifying.as_bytes()),
    }
}

// Wide by design: assembles the full OIDC claim set for a CI job token.
#[allow(clippy::too_many_arguments)]
pub fn issue(
    key: &InstanceKey,
    issuer: &str,
    audience: &str,
    repo_id: i64,
    pipeline_id: i64,
    job_id: i64,
    ref_name: &str,
    sha: &str,
) -> Result<(String, i64)> {
    let now = Utc::now();
    let expires = now + Duration::minutes(5);
    let pem = key
        .signing_key()
        .to_pkcs8_pem(Default::default())
        .context("encode CI OIDC signing key")?;
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some(key.kid().to_string());
    header.typ = Some("JWT".into());
    let claims = CiOidcClaims {
        iss: issuer.trim_end_matches('/').to_string(),
        sub: format!("repo:{repo_id}:pipeline:{pipeline_id}:job:{job_id}"),
        aud: audience.to_string(),
        iat: now.timestamp(),
        nbf: now.timestamp(),
        exp: expires.timestamp(),
        jti: uuid::Uuid::new_v4().to_string(),
        repository_id: repo_id,
        pipeline_id,
        job_id,
        ref_name: ref_name.to_string(),
        sha: sha.to_string(),
    };
    let token = encode(&header, &claims, &EncodingKey::from_ed_pem(pem.as_bytes())?)
        .context("issue CI OIDC token")?;
    Ok((token, expires.timestamp()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::pkcs8::EncodePublicKey;
    use jsonwebtoken::{decode, DecodingKey, Validation};
    #[test]
    fn tokens_are_asymmetric_audience_bound_and_publicly_verifiable() {
        let instance_key = InstanceKey::derived_from_secret("secret");
        let (token, _) = issue(
            &instance_key,
            "https://forge.example/oidc",
            "sts.example",
            1,
            2,
            3,
            "refs/heads/main",
            "abc",
        )
        .unwrap();
        let pem = instance_key
            .verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap();
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_audience(&["sts.example"]);
        validation.set_issuer(&["https://forge.example/oidc"]);
        let claims = decode::<CiOidcClaims>(
            &token,
            &DecodingKey::from_ed_pem(pem.as_bytes()).unwrap(),
            &validation,
        )
        .unwrap()
        .claims;
        assert_eq!(claims.pipeline_id, 2);
        assert!(decode::<CiOidcClaims>(
            &token,
            &DecodingKey::from_ed_pem(pem.as_bytes()).unwrap(),
            &{
                let mut v = Validation::new(Algorithm::EdDSA);
                v.set_audience(&["other"]);
                v
            }
        )
        .is_err());
    }
}
