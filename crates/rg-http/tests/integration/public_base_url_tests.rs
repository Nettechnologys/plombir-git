//! Every URL the server hands a protocol client to follow next names one
//! address, and the right one for the deployment (card_f78054e9e98f).
//!
//! The LFS action hrefs, the CI OIDC issuer, the OCI token realm and a package
//! index's links used to be built by three separate rules: LFS and OIDC wrote
//! `http://` unconditionally, OCI and the package indexes guessed `https` from
//! the host name and never read `external_url`. Each was wrong on a different
//! deployment, so each deployment is a case here, and every case asks all four
//! surfaces the same question through the routed server.

use crate::common::{register_full, spawn_test_app_with_overrides, StateOverrides};
use sha2::{Digest, Sha256};

const OWNER: &str = "pub_url_owner";
const REPO: &str = "assets";

/// The four client-facing URLs one request `Host` produces on this server.
struct Advertised {
    lfs_href: String,
    oidc_issuer: String,
    oci_realm: String,
    nuget_resource: String,
}

impl Advertised {
    fn all(&self) -> [(&'static str, &str); 4] {
        [
            ("LFS action href", &self.lfs_href),
            ("CI OIDC issuer", &self.oidc_issuer),
            ("OCI token realm", &self.oci_realm),
            ("NuGet index resource", &self.nuget_resource),
        ]
    }
}

async fn advertised(overrides: StateOverrides, host: &str) -> Advertised {
    let (base, _db) = spawn_test_app_with_overrides(overrides).await;
    let (token, _) = register_full(&base, OWNER, "pub_url_owner@example.com").await;
    let client = reqwest::Client::new();
    let created = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "name": REPO, "is_private": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);

    let content = b"a large file";
    let lfs: serde_json::Value = client
        .post(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/lfs/objects/batch"
        ))
        .header(reqwest::header::HOST, host)
        .bearer_auth(&token)
        .header("Accept", "application/vnd.git-lfs+json")
        .header("Content-Type", "application/vnd.git-lfs+json")
        .body(
            serde_json::json!({
                "operation": "upload",
                "transfers": ["basic"],
                "objects": [{ "oid": hex::encode(Sha256::digest(content)), "size": content.len() }],
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let lfs_href = lfs["objects"][0]["actions"]["upload"]["href"]
        .as_str()
        .unwrap_or_else(|| panic!("no upload action: {lfs}"))
        .to_string();

    let discovery: serde_json::Value = client
        .get(format!(
            "{base}/api/v1/ci/oidc/.well-known/openid-configuration"
        ))
        .header(reqwest::header::HOST, host)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let oidc_issuer = discovery["issuer"]
        .as_str()
        .unwrap_or_else(|| panic!("no issuer: {discovery}"))
        .to_string();

    let challenge = client
        .get(format!("{base}/v2/"))
        .header(reqwest::header::HOST, host)
        .send()
        .await
        .unwrap()
        .headers()
        .get(reqwest::header::WWW_AUTHENTICATE)
        .and_then(|value| value.to_str().ok())
        .expect("the version check must issue a challenge")
        .to_string();
    let oci_realm = challenge
        .split_once("realm=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(realm, _)| realm.to_string())
        .unwrap_or_else(|| panic!("no realm in {challenge}"));

    let nuget: serde_json::Value = client
        .get(format!(
            "{base}/api/v1/repos/{OWNER}/{REPO}/packages/nuget/index.json"
        ))
        .header(reqwest::header::HOST, host)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let nuget_resource = nuget["resources"][0]["@id"]
        .as_str()
        .unwrap_or_else(|| panic!("no resource: {nuget}"))
        .to_string();

    Advertised {
        lfs_href,
        oidc_issuer,
        oci_realm,
        nuget_resource,
    }
}

fn assert_all_start_with(advertised: &Advertised, prefix: &str) {
    for (surface, url) in advertised.all() {
        assert!(
            url.starts_with(prefix),
            "{surface} must start with {prefix}, got {url}"
        );
    }
}

/// An instance serving its own `[tls]` with no `external_url`: the client
/// reached it over TLS, so every follow-up URL must be `https://` on that host —
/// not `http://` on a TLS port, which fails at the handshake.
#[tokio::test]
async fn a_tls_listener_without_external_url_advertises_https_on_the_request_host() {
    let advertised = advertised(
        StateOverrides {
            tls_enabled: true,
            ..Default::default()
        },
        "git.example.test",
    )
    .await;
    assert_all_start_with(&advertised, "https://git.example.test/");
}

/// Behind a proxy the request `Host` is whatever the proxy forwards; the
/// configured address is the only one the client can reach, OCI included.
#[tokio::test]
async fn a_configured_external_url_wins_on_every_surface() {
    let advertised = advertised(
        StateOverrides {
            external_url: Some("https://public.example.test/".to_string()),
            ..Default::default()
        },
        "internal-upstream:8080",
    )
    .await;
    assert_all_start_with(&advertised, "https://public.example.test/");
}

/// A plain-HTTP instance on a LAN name: nothing here speaks TLS, so a URL that
/// says `https://` sends the client to a handshake the server cannot answer.
#[tokio::test]
async fn a_plain_http_instance_does_not_advertise_https() {
    let advertised = advertised(StateOverrides::default(), "git.lan:8080").await;
    assert_all_start_with(&advertised, "http://git.lan:8080/");
}
