//! Short-lived, audience-bound OIDC identity tokens for CI workloads.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{Duration, Utc};
use ed25519_dalek::{pkcs8::EncodePrivateKey, SigningKey};
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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

fn signing_key(secret: &str) -> SigningKey {
    let mut hash = Sha256::new();
    hash.update(b"ironforge-ci-oidc-ed25519-v1\0");
    hash.update(secret.as_bytes());
    SigningKey::from_bytes(&hash.finalize().into())
}

pub fn jwk(secret: &str) -> CiOidcJwk {
    let verifying = signing_key(secret).verifying_key();
    let x = URL_SAFE_NO_PAD.encode(verifying.as_bytes());
    let kid = hex::encode(&Sha256::digest(verifying.as_bytes())[..8]);
    CiOidcJwk {
        kty: "OKP",
        crv: "Ed25519",
        key_use: "sig",
        alg: "EdDSA",
        kid,
        x,
    }
}

pub fn issue(
    secret: &str,
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
    let key = signing_key(secret);
    let pem = key
        .to_pkcs8_pem(Default::default())
        .context("encode CI OIDC signing key")?;
    let jwk = jwk(secret);
    let mut header = Header::new(Algorithm::EdDSA);
    header.kid = Some(jwk.kid);
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
        let (token, _) = issue(
            "secret",
            "https://forge.example/oidc",
            "sts.example",
            1,
            2,
            3,
            "refs/heads/main",
            "abc",
        )
        .unwrap();
        let key = signing_key("secret").verifying_key();
        let pem = key.to_public_key_pem(Default::default()).unwrap();
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
