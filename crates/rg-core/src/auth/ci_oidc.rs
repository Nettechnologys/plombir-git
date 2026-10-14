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
    /// `owner/name`. A name can be reused after a rename, a transfer or a
    /// deletion, so a trust policy should also pin [`Self::repository_id`].
    pub repository: String,
    pub repository_id: i64,
    pub repository_owner: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// `branch`, `tag`, `pull_request` or `unknown`, so a policy does not have
    /// to know every ref prefix to tell `refs/pull/12/head` from a branch.
    pub ref_type: String,
    pub sha: String,
    /// The environment whose gate released the job; absent for a job without
    /// one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub pipeline_id: i64,
    pub job_id: i64,
    /// The account that triggered the pipeline; absent when none did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
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

/// What a token states about the job it was issued to.
#[derive(Debug, Clone, Copy)]
pub struct JobIdentity<'a> {
    pub owner: &'a str,
    pub repository: &'a str,
    pub repository_id: i64,
    pub pipeline_id: i64,
    pub job_id: i64,
    pub ref_name: &'a str,
    pub sha: &'a str,
    /// The environment the job was attached to when its gate decided, `None`
    /// for a job without one.
    pub environment: Option<&'a str>,
    pub actor: Option<&'a str>,
}

impl JobIdentity<'_> {
    /// `repo:<owner>/<name>:environment:<env>` for a job with an environment,
    /// `repo:<owner>/<name>:ref:<ref>` otherwise — the shapes GitHub uses.
    ///
    /// The old `repo:<id>:pipeline:<id>:job:<id>` carried neither the ref nor
    /// the environment, so a cloud role could only trust `repo:<id>:*`: every
    /// branch and every pull request of the repository. A pull request's ref
    /// is `refs/pull/<n>/head`, so it cannot claim `ref:refs/heads/main`, and
    /// the two scopes start differently, so an environment cannot pass for a
    /// ref. The environment name is interpolated as it is; a policy matches
    /// the whole subject, not a prefix of it.
    pub fn subject(&self) -> String {
        let scope = match self.environment {
            Some(environment) => format!("environment:{environment}"),
            None => format!("ref:{}", self.ref_name),
        };
        format!("repo:{}/{}:{scope}", self.owner, self.repository)
    }
}

/// The kind of ref a token's `ref_type` claim names.
pub fn ref_type(ref_name: &str) -> &'static str {
    if ref_name.starts_with("refs/heads/") {
        "branch"
    } else if ref_name.starts_with("refs/tags/") {
        "tag"
    } else if ref_name.starts_with("refs/pull/") {
        "pull_request"
    } else {
        "unknown"
    }
}

pub fn issue(
    key: &InstanceKey,
    issuer: &str,
    audience: &str,
    identity: &JobIdentity<'_>,
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
        sub: identity.subject(),
        aud: audience.to_string(),
        iat: now.timestamp(),
        nbf: now.timestamp(),
        exp: expires.timestamp(),
        jti: uuid::Uuid::new_v4().to_string(),
        repository: format!("{}/{}", identity.owner, identity.repository),
        repository_id: identity.repository_id,
        repository_owner: identity.owner.to_string(),
        ref_name: identity.ref_name.to_string(),
        ref_type: ref_type(identity.ref_name).to_string(),
        sha: identity.sha.to_string(),
        environment: identity.environment.map(str::to_string),
        pipeline_id: identity.pipeline_id,
        job_id: identity.job_id,
        actor: identity.actor.map(str::to_string),
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
    fn identity<'a>(ref_name: &'a str, environment: Option<&'a str>) -> JobIdentity<'a> {
        JobIdentity {
            owner: "acme",
            repository: "deploy",
            repository_id: 1,
            pipeline_id: 2,
            job_id: 3,
            ref_name,
            sha: "abc",
            environment,
            actor: Some("alice"),
        }
    }

    /// The subject names the ref, or the environment when the job has one, so
    /// a production role can trust exactly `environment:production` and no
    /// branch or pull request can produce that subject.
    #[test]
    fn the_subject_names_the_ref_or_the_environment() {
        assert_eq!(
            identity("refs/heads/main", None).subject(),
            "repo:acme/deploy:ref:refs/heads/main"
        );
        assert_eq!(
            identity("refs/pull/12/head", None).subject(),
            "repo:acme/deploy:ref:refs/pull/12/head"
        );
        assert_eq!(
            identity("refs/pull/12/head", Some("production")).subject(),
            "repo:acme/deploy:environment:production"
        );
        // An environment named like a ref still reads as an environment.
        assert_eq!(
            identity("refs/heads/feature", Some("refs/heads/main")).subject(),
            "repo:acme/deploy:environment:refs/heads/main"
        );
        assert_eq!(ref_type("refs/heads/main"), "branch");
        assert_eq!(ref_type("refs/tags/v1"), "tag");
        assert_eq!(ref_type("refs/pull/12/head"), "pull_request");
        assert_eq!(ref_type("HEAD"), "unknown");
    }

    #[test]
    fn tokens_are_asymmetric_audience_bound_and_publicly_verifiable() {
        let instance_key = InstanceKey::derived_from_secret("secret");
        let (token, _) = issue(
            &instance_key,
            "https://forge.example/oidc",
            "sts.example",
            &identity("refs/heads/main", Some("production")),
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
        assert_eq!(claims.sub, "repo:acme/deploy:environment:production");
        assert_eq!(claims.repository, "acme/deploy");
        assert_eq!(claims.repository_id, 1);
        assert_eq!(claims.ref_type, "branch");
        assert_eq!(claims.environment.as_deref(), Some("production"));
        assert_eq!(claims.actor.as_deref(), Some("alice"));
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
