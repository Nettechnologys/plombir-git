//! Cross-repository scoping for the release and release-asset routes.
//!
//! The read side of this module was anchored to the repository in the path
//! (`resolve_release_in_repo` / `resolve_asset_in_repo`), but the write side
//! checked `owner/name` and then acted on a *global* id:
//!
//!   PATCH  /repos/<mine>/<mine>/releases/<id-belonging-to-someone-else>
//!   DELETE /repos/<mine>/<mine>/releases/assets/<id-belonging-to-someone-else>
//!
//! Write access to a single repository of one's own was therefore enough to
//! rename, extend, sign and delete the releases of a private repository the
//! caller cannot even read. The attestation read routes had the same gap: they
//! required read access to the repository in the path while serving the
//! envelope of whatever asset id was passed.
//!
//! Every probe below expects `404` — not `403`, which would still confirm the
//! id exists — and each test also exercises the same call against the
//! attacker's *own* release, so a green run proves the gate revokes foreign
//! access rather than that the fixture is broken.

use crate::common::{create_repo, register_user, spawn_test_app};

const PW: &str = "Qz7$wRtm";

async fn create_private_repo(base: &str, token: &str, name: &str) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_private_repo '{name}' failed");
}

async fn create_release(base: &str, token: &str, owner: &str, repo: &str, title: &str) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/releases"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": title }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create_release '{title}' failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn upload_asset(base: &str, token: &str, owner: &str, repo: &str, release_id: i64) -> i64 {
    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets"
        ))
        .bearer_auth(token)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=notes.txt")
        .body("release asset")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "upload_asset failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

/// Attacker (public repo of their own) and victim (private repo with a release).
///
/// Returns `(base, attacker_token, attacker, attacker_repo, attacker_release,
/// victim_token, victim, victim_repo, victim_release)`.
#[allow(clippy::type_complexity)]
async fn setup(
    suffix: &str,
) -> (
    String,
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
    i64,
) {
    let base = spawn_test_app().await;

    let attacker = format!("relattacker{suffix}");
    let attacker_token = register_user(
        &base,
        &attacker,
        &format!("relattacker{suffix}@example.com"),
        PW,
    )
    .await;
    let attacker_repo = format!("relattackerrepo{suffix}");
    create_repo(&base, &attacker_token, &attacker_repo).await;
    let attacker_release = create_release(
        &base,
        &attacker_token,
        &attacker,
        &attacker_repo,
        "attacker release",
    )
    .await;

    let victim = format!("relvictim{suffix}");
    let victim_token = register_user(
        &base,
        &victim,
        &format!("relvictim{suffix}@example.com"),
        PW,
    )
    .await;
    let victim_repo = format!("relvictimrepo{suffix}");
    create_private_repo(&base, &victim_token, &victim_repo).await;
    let victim_release = create_release(
        &base,
        &victim_token,
        &victim,
        &victim_repo,
        "victim release",
    )
    .await;

    (
        base,
        attacker_token,
        attacker,
        attacker_repo,
        attacker_release,
        victim_token,
        victim,
        victim_repo,
        victim_release,
    )
}

#[tokio::test]
async fn release_writes_are_scoped_to_their_repository() {
    let (
        base,
        attacker_token,
        attacker,
        attacker_repo,
        attacker_release,
        victim_token,
        victim,
        victim_repo,
        victim_release,
    ) = setup("1").await;
    let client = reqwest::Client::new();

    // The attacker routes through their own repository, where they do have
    // write access, and addresses a release id that lives in the victim's.
    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/releases/{victim_release}"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"title": "pwned"}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "PATCH of a foreign release must be 404, not a rename"
    );

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/releases/{victim_release}/assets"
        ))
        .bearer_auth(&attacker_token)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=planted.txt")
        .body("planted")
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "uploading into a foreign release must be 404"
    );

    let resp = client
        .delete(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/releases/{victim_release}"
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "DELETE of a foreign release must be 404, not a delete"
    );

    // Baseline 1 — the victim's release is still there, unrenamed and empty.
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/releases/{victim_release}"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the victim's release was deleted");
    let release: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        release["title"], "victim release",
        "the victim's release was renamed"
    );

    let assets: serde_json::Value = client
        .get(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/releases/{victim_release}/assets"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        assets.as_array().unwrap().len(),
        0,
        "an asset was planted in the victim's release"
    );

    // Baseline 2 — the very same calls still work on the attacker's own
    // release, so the 404s above are a revoked reach, not a broken route.
    let resp = client
        .patch(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/releases/{attacker_release}"
        ))
        .bearer_auth(&attacker_token)
        .json(&serde_json::json!({"title": "renamed"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the owner cannot rename their release");

    upload_asset(
        &base,
        &attacker_token,
        &attacker,
        &attacker_repo,
        attacker_release,
    )
    .await;

    let resp = client
        .delete(format!(
            "{base}/api/v1/repos/{attacker}/{attacker_repo}/releases/{attacker_release}"
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 204, "the owner cannot delete their release");
}

#[tokio::test]
async fn asset_and_attestation_routes_are_scoped_to_their_repository() {
    let (
        base,
        attacker_token,
        attacker,
        attacker_repo,
        attacker_release,
        victim_token,
        victim,
        victim_repo,
        victim_release,
    ) = setup("2").await;
    let client = reqwest::Client::new();

    let victim_asset =
        upload_asset(&base, &victim_token, &victim, &victim_repo, victim_release).await;
    let attacker_asset = upload_asset(
        &base,
        &attacker_token,
        &attacker,
        &attacker_repo,
        attacker_release,
    )
    .await;

    // The victim signs their own asset, so there is an envelope to leak.
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/releases/assets/{victim_asset}/attestation"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "the victim cannot sign their own asset");

    let attacker_route = format!("{base}/api/v1/repos/{attacker}/{attacker_repo}/releases/assets");

    // Signing a foreign asset — overwrites the victim's stored envelope.
    let resp = client
        .post(format!("{attacker_route}/{victim_asset}/attestation"))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "signing a foreign asset must be 404, not a new envelope"
    );

    // Reading it — the envelope names the private repository's asset and digest.
    let resp = client
        .get(format!("{attacker_route}/{victim_asset}/attestation"))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "the attestation of a foreign asset must not be readable"
    );

    let resp = client
        .post(format!(
            "{attacker_route}/{victim_asset}/attestation/verify"
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "verifying a foreign asset must not confirm its digest"
    );

    let resp = client
        .delete(format!("{attacker_route}/{victim_asset}"))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        404,
        "DELETE of a foreign asset must be 404, not a delete"
    );

    // Baseline 1 — the victim's asset is still downloadable and still carries
    // the envelope they signed.
    let resp = client
        .get(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/releases/assets/{victim_asset}/download"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the victim's asset was deleted");
    assert_eq!(resp.text().await.unwrap(), "release asset");

    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{victim}/{victim_repo}/releases/assets/{victim_asset}/attestation/verify"
        ))
        .bearer_auth(&victim_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "the victim's attestation is gone");
    let report: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        report["status"], "verified",
        "the victim's attestation no longer verifies: {report}"
    );

    // Baseline 2 — the same four calls still work on the attacker's own asset.
    let resp = client
        .post(format!("{attacker_route}/{attacker_asset}/attestation"))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "the owner cannot sign their own asset");

    let resp = client
        .get(format!("{attacker_route}/{attacker_asset}/attestation"))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "the owner cannot read their own attestation"
    );

    let resp = client
        .post(format!(
            "{attacker_route}/{attacker_asset}/attestation/verify"
        ))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "the owner cannot verify their own attestation"
    );

    let resp = client
        .delete(format!("{attacker_route}/{attacker_asset}"))
        .bearer_auth(&attacker_token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        204,
        "the owner cannot delete their own asset"
    );
}
