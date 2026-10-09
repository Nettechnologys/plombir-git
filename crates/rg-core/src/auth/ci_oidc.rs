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
    /// `owner/name`, for policies that would rather name repositories than
    /// carry their numeric ids.
    pub repository: String,
    pub repository_id: i64,
    pub repository_owner: String,
    #[serde(rename = "ref")]
    pub ref_name: String,
    /// `branch`, `tag`, `pull_request`, or `unknown` — derived from `ref`.
    /// `ref` alone forces a policy to know every prefix spelling; this names
    /// the kind so `refs/pull/12/head` is not mistaken for a branch.
    pub ref_type: String,
    pub sha: String,
    /// The job's `environment:` name, omitted for a job without one. Its
    /// presence in the subject is what lets a production role require
    /// `sub = repo:owner/name:environment:production` instead of accepting
    /// every branch of the repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<String>,
    pub pipeline_id: i64,
    pub job_id: i64,
    /// The account that triggered the pipeline, when the trigger had one
    /// (webhooks and pushes from a deleted account do not). Omitted otherwise,
    /// never invented.
    #[serde(skip_serializing_if = "Option::is_none")]
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

/// The facts about a job a token has to state.
///
/// Borrowed fields rather than owned ones so the HTTP handler can assemble it
/// straight from the rows it already loaded, without cloning the claim set into
/// a parallel struct.
#[derive(Debug, Clone, Copy)]
pub struct JobIdentity<'a> {
    pub owner: &'a str,
    pub repository: &'a str,
    pub repository_id: i64,
    pub pipeline_id: i64,
    pub job_id: i64,
    pub ref_name: &'a str,
    pub sha: &'a str,
    pub environment: Option<&'a str>,
    pub actor: Option<&'a str>,
}

impl JobIdentity<'_> {
    /// The GitHub-style subject: `repo:owner/name:environment:env` for a job
    /// that declared an environment, `repo:owner/name:ref:<ref>` otherwise.
    ///
    /// The two scopes have disjoint prefixes, so a repository cannot alias one
    /// for the other — an environment literally named `refs/heads/main` still
    /// appears under `environment:`. The environment name is interpolated
    /// verbatim (as GitHub does); a policy should match an exact value, not a
    /// bare prefix, when the name contains `:`.
    pub fn subject(&self) -> String {
        let scope = match self.environment {
            Some(environment) => format!("environment:{environment}"),
            None => format!("ref:{}", self.ref_name),
        };
        format!("repo:{}/{}:{scope}", self.owner, self.repository)
    }
}

/// `branch`, `tag`, `pull_request`, or `unknown`, from the full ref.
///
/// Kept public and separate from [`issue`] so the docs and tests pin the same
/// mapping the tokens carry.
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

    fn identity<'a>(environment: Option<&'a str>) -> JobIdentity<'a> {
        JobIdentity {
            owner: "acme",
            repository: "widget",
            repository_id: 1,
            pipeline_id: 2,
            job_id: 3,
            ref_name: "refs/heads/main",
            sha: "abc",
            environment,
            actor: Some("octocat"),
        }
    }

    fn decode_claims(token: &str, key: &InstanceKey) -> CiOidcClaims {
        let pem = key
            .verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap();
        let mut validation = Validation::new(Algorithm::EdDSA);
        validation.set_audience(&["sts.example"]);
        validation.set_issuer(&["https://forge.example/oidc"]);
        validation.required_spec_claims.clear();
        decode::<CiOidcClaims>(
            token,
            &DecodingKey::from_ed_pem(pem.as_bytes()).unwrap(),
            &validation,
        )
        .unwrap()
        .claims
    }

    #[test]
    fn tokens_are_asymmetric_audience_bound_and_publicly_verifiable() {
        let instance_key = InstanceKey::derived_from_secret("secret");
        let (token, _) = issue(
            &instance_key,
            "https://forge.example/oidc",
            "sts.example",
            &identity(None),
        )
        .unwrap();
        let pem = instance_key
            .verifying_key()
            .to_public_key_pem(Default::default())
            .unwrap();
        let claims = decode_claims(&token, &instance_key);
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

    #[test]
    fn a_branch_job_is_addressed_by_repository_and_ref() {
        let instance_key = InstanceKey::derived_from_secret("secret");
        let (token, _) = issue(
            &instance_key,
            "https://forge.example/oidc",
            "sts.example",
            &identity(None),
        )
        .unwrap();
        let claims = decode_claims(&token, &instance_key);
        assert_eq!(claims.sub, "repo:acme/widget:ref:refs/heads/main");
        assert_eq!(claims.ref_name, "refs/heads/main");
        assert_eq!(claims.ref_type, "branch");
        assert_eq!(claims.sha, "abc");
        assert_eq!(claims.repository, "acme/widget");
        assert_eq!(claims.repository_id, 1);
        assert_eq!(claims.repository_owner, "acme");
        assert_eq!(claims.pipeline_id, 2);
        assert_eq!(claims.job_id, 3);
        assert_eq!(claims.actor.as_deref(), Some("octocat"));
        assert_eq!(claims.environment, None);
    }

    #[test]
    fn an_environment_job_is_addressed_by_environment_not_ref() {
        let instance_key = InstanceKey::derived_from_secret("secret");
        let (token, _) = issue(
            &instance_key,
            "https://forge.example/oidc",
            "sts.example",
            &identity(Some("production")),
        )
        .unwrap();
        let claims = decode_claims(&token, &instance_key);
        assert_eq!(claims.sub, "repo:acme/widget:environment:production");
        assert_eq!(claims.environment.as_deref(), Some("production"));
        // The ref still travels in the claims, but it no longer decides the
        // subject: `refs/heads/anything` cannot select the production role.
        assert_eq!(claims.ref_name, "refs/heads/main");
        assert_eq!(claims.ref_type, "branch");
    }

    #[test]
    fn ref_type_names_the_kind_of_ref() {
        assert_eq!(ref_type("refs/heads/main"), "branch");
        assert_eq!(ref_type("refs/tags/v1.2.3"), "tag");
        assert_eq!(ref_type("refs/pull/42/head"), "pull_request");
        assert_eq!(ref_type("refs/pull/42/merge"), "pull_request");
        assert_eq!(ref_type("HEAD"), "unknown");
    }

    #[test]
    fn an_environment_named_like_a_ref_cannot_alias_a_ref_subject() {
        let instance_key = InstanceKey::derived_from_secret("secret");
        let (token, _) = issue(
            &instance_key,
            "https://forge.example/oidc",
            "sts.example",
            &identity(Some("refs/heads/main")),
        )
        .unwrap();
        let claims = decode_claims(&token, &instance_key);
        assert_eq!(claims.sub, "repo:acme/widget:environment:refs/heads/main");
        assert_eq!(
            identity(None).subject(),
            "repo:acme/widget:ref:refs/heads/main"
        );
    }

    #[test]
    fn optional_claims_are_absent_rather_than_null() {
        let instance_key = InstanceKey::derived_from_secret("secret");
        let mut job = identity(None);
        job.actor = None;
        let (token, _) = issue(
            &instance_key,
            "https://forge.example/oidc",
            "sts.example",
            &job,
        )
        .unwrap();
        let claims = decode_claims(&token, &instance_key);
        assert_eq!(claims.actor, None);
        let json = serde_json::to_value(&claims).unwrap();
        assert!(json.get("environment").is_none());
        assert!(json.get("actor").is_none());
    }
}
