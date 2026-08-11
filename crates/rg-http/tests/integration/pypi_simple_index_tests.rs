//! The PyPI Simple Repository API answers the URLs PEP 503 defines.
//!
//! pip does not ask for `.../simple/mypkg`. It builds `<index-url>/<normalized
//! name>/` — trailing slash included, name lower-cased with every run of
//! `-`/`_`/`.` collapsed to a single `-`. A registry that only serves the bare,
//! as-published spelling answers none of the requests a real client sends, and
//! in production the miss falls through to the SPA fallback, so pip is handed
//! an HTML page that is not an index.

use std::io::{Cursor, Write};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use reqwest::StatusCode;
use sea_orm::ConnectionTrait;
use sha2::{Digest as _, Sha256};

/// A wheel is a ZIP holding `{name}-{version}.dist-info/METADATA`.
fn wheel(dist_info: &str, metadata: &str) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut archive = zip::ZipWriter::new(&mut output);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        archive
            .start_file(format!("{dist_info}/METADATA"), options)
            .unwrap();
        archive.write_all(metadata.as_bytes()).unwrap();
        archive.finish().unwrap();
    }
    output.into_inner()
}

/// The multipart envelope current Twine builds in `Repository::_upload`.
fn twine_form(
    package_name: &str,
    version: &str,
    filename: &str,
    body: Vec<u8>,
    sha256_digest: String,
) -> reqwest::multipart::Form {
    reqwest::multipart::Form::new()
        .text(":action", "file_upload")
        .text("protocol_version", "1")
        .text("metadata_version", "2.1")
        .text("name", package_name.to_string())
        .text("version", version.to_string())
        .text("filetype", "bdist_wheel")
        .text("pyversion", "py3")
        .text("sha256_digest", sha256_digest)
        .part(
            "content",
            reqwest::multipart::Part::bytes(body)
                .file_name(filename.to_string())
                .mime_str("application/octet-stream")
                .expect("literal MIME type"),
        )
}

struct Fixture {
    base: String,
    client: reqwest::Client,
    /// Held for the lifetime of the test: the temporary database lives as long
    /// as this handle. The failure-status test also drives it directly, to break
    /// exactly the table the handler queries.
    db: rg_db::DatabaseConnection,
}

impl Fixture {
    fn simple(&self, tail: &str) -> String {
        format!(
            "{}/api/v1/repos/pypi-owner/pypi-repo/packages/pypi/simple{tail}",
            self.base
        )
    }

    async fn get(&self, url: &str) -> (StatusCode, String) {
        let resp = self.client.get(url).send().await.unwrap();
        let status = resp.status();
        (status, resp.text().await.unwrap())
    }
}

/// Publish one wheel under `published_name` and hand back the fixture.
async fn fixture_with_package(published_name: &str, version: &str) -> Fixture {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "pypi-owner", "pypi-owner@example.com").await;
    create_repo(&base, &token, "pypi-repo").await;

    let file_stem = published_name.replace('-', "_");
    let filename = format!("{file_stem}-{version}-py3-none-any.whl");
    let body = wheel(
        &format!("{file_stem}-{version}.dist-info"),
        &format!("Metadata-Version: 2.1\nName: {published_name}\nVersion: {version}\n"),
    );

    let client = reqwest::Client::new();
    let published = client
        .post(format!(
            "{base}/api/v1/repos/pypi-owner/pypi-repo/packages/pypi/publish"
        ))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        published.status(),
        StatusCode::CREATED,
        "publish failed: {}",
        published.text().await.unwrap()
    );

    Fixture { base, client, db }
}

/// Acceptance for card_bb095aad4f76: the real Twine wire shape closes the
/// publish -> simple index -> download round trip. Exercising one route spelling
/// on the first upload and the other on the duplicate makes either route's
/// removal fail this test rather than leaving a decorative alias unproved.
#[tokio::test]
async fn twine_multipart_upload_round_trips_through_the_simple_index() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "twine-owner", "twine-owner@example.com").await;
    create_repo(&base, &token, "twine-repo").await;

    let package_name = "matrix-twine";
    let version = "1.2.3";
    let filename = "matrix_twine-1.2.3-py3-none-any.whl";
    let body = wheel(
        "matrix_twine-1.2.3.dist-info",
        "Metadata-Version: 2.1\nName: matrix-twine\nVersion: 1.2.3\n",
    );
    let digest = hex::encode(Sha256::digest(&body));
    let client = reqwest::Client::new();
    let legacy = format!("{base}/api/v1/repos/twine-owner/twine-repo/packages/pypi/legacy");

    let uploaded = client
        .post(format!("{legacy}/"))
        .bearer_auth(&token)
        .multipart(twine_form(
            package_name,
            version,
            filename,
            body.clone(),
            digest.clone(),
        ))
        .send()
        .await
        .unwrap();
    let status = uploaded.status();
    let response_body = uploaded.text().await.unwrap();
    assert_eq!(
        status,
        StatusCode::OK,
        "Twine upload failed: {response_body}"
    );

    let index = client
        .get(format!(
            "{base}/api/v1/repos/twine-owner/twine-repo/packages/pypi/simple/matrix-twine/"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    let index = index.text().await.unwrap();
    let href = index
        .split_once("<a href=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(url, _)| url.split('#').next().unwrap().to_string())
        .unwrap_or_else(|| panic!("no download link in the index page: {index}"));
    let downloaded = client.get(href).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), body.as_slice());

    let duplicate = client
        .post(&legacy)
        .bearer_auth(&token)
        .multipart(twine_form(package_name, version, filename, body, digest))
        .send()
        .await
        .unwrap();
    assert_eq!(
        duplicate.status(),
        StatusCode::CONFLICT,
        "repeating the same distribution must be a conflict: {}",
        duplicate.text().await.unwrap()
    );
}

/// Acceptance for card_33fcd7258af5: PEP 440 declares `1.0` and `v1.0.0` to be
/// one public version, and a public version must be unique inside a
/// distribution. A second spelling is therefore a conflicting immutable
/// publication, not a second release and not an additional-file upload.
#[tokio::test]
async fn equivalent_pep440_version_spelling_is_a_conflict_without_a_second_row() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "pep440-owner", "pep440-owner@example.com").await;
    create_repo(&base, &token, "pep440-repo").await;
    let client = reqwest::Client::new();
    let legacy = format!("{base}/api/v1/repos/pep440-owner/pep440-repo/packages/pypi/legacy/");

    // Twine reads both the version field and the metadata out of the same
    // distribution, so the two spellings differ everywhere a real upload would.
    let upload = |version: &'static str| {
        let client = client.clone();
        let token = token.clone();
        let legacy = legacy.clone();
        async move {
            let filename = format!("matrix_pep440-{version}-py3-none-any.whl");
            let body = wheel(
                &format!("matrix_pep440-{version}.dist-info"),
                &format!("Metadata-Version: 2.1\nName: matrix-pep440\nVersion: {version}\n"),
            );
            let digest = hex::encode(Sha256::digest(&body));
            client
                .post(legacy)
                .bearer_auth(token)
                .multipart(twine_form(
                    "matrix-pep440",
                    version,
                    &filename,
                    body,
                    digest,
                ))
                .send()
                .await
                .unwrap()
        }
    };

    let first = upload("1.0").await;
    let first_status = first.status();
    let first_body = first.text().await.unwrap();
    assert_eq!(first_status, StatusCode::OK, "{first_body}");

    let equivalent = upload("v1.0.0").await;
    assert_eq!(
        equivalent.status(),
        StatusCode::CONFLICT,
        "an equivalent PEP 440 spelling was accepted: {}",
        equivalent.text().await.unwrap()
    );

    let repo = rg_core::repo::service::find_repo_by_owner_name(&db, "pep440-owner", "pep440-repo")
        .await
        .unwrap()
        .unwrap();
    let registry = rg_db::ops::package_registry_ops::find_by_repo_and_type(&db, repo.id, "pypi")
        .await
        .unwrap()
        .unwrap();
    let package =
        rg_db::ops::package_ops::find_by_registry_and_name(&db, registry.id, "matrix-pep440")
            .await
            .unwrap()
            .unwrap();
    let versions = rg_db::ops::package_version_ops::list_by_package(&db, package.id)
        .await
        .unwrap();
    assert_eq!(
        versions.len(),
        1,
        "equivalent spelling created a second row"
    );
    assert_eq!(
        versions[0].version, "1.0",
        "the first spelling stays canonical storage"
    );
    assert_eq!(versions[0].protocol_version_key.as_deref(), Some("1"));
    let files = rg_db::ops::package_file_ops::list_by_version(&db, versions[0].id)
        .await
        .unwrap();
    assert_eq!(
        files.len(),
        1,
        "the refused publish left a second distribution file"
    );
}

#[tokio::test]
async fn twine_upload_rejects_a_false_sha256_without_publishing() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "digest-owner", "digest-owner@example.com").await;
    create_repo(&base, &token, "digest-repo").await;

    let body = wheel(
        "bad_digest-1.0.0.dist-info",
        "Metadata-Version: 2.1\nName: bad-digest\nVersion: 1.0.0\n",
    );
    let client = reqwest::Client::new();
    let response = client
        .post(format!(
            "{base}/api/v1/repos/digest-owner/digest-repo/packages/pypi/legacy/"
        ))
        .bearer_auth(&token)
        .multipart(twine_form(
            "bad-digest",
            "1.0.0",
            "bad_digest-1.0.0-py3-none-any.whl",
            body,
            "0".repeat(64),
        ))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let response_body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response_body}");
    assert!(response_body.contains("sha256_digest"), "{response_body}");

    let index = client
        .get(format!(
            "{base}/api/v1/repos/digest-owner/digest-repo/packages/pypi/simple/"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    let index = index.text().await.unwrap();
    assert!(!index.contains("bad-digest"), "{index}");
}

/// The project page pip actually requests: `.../simple/<name>/`.
#[tokio::test]
async fn the_project_page_answers_the_url_pip_builds() {
    let fx = fixture_with_package("matrix-pypi", "1.0.0").await;

    for tail in ["/matrix-pypi/", "/matrix-pypi"] {
        let (status, body) = fx.get(&fx.simple(tail)).await;
        assert_eq!(status, StatusCode::OK, "GET simple{tail}: {body}");
        assert!(
            body.contains("matrix_pypi-1.0.0-py3-none-any.whl"),
            "GET simple{tail} must be the package index, not a fallback page: {body}"
        );
    }
}

/// PEP 503 has the client normalize the name, so a project published as
/// `Matrix_PyPI` is fetched as `matrix-pypi/` and has to be found under it —
/// with download links that still carry the stored spelling, since that is what
/// the download route matches on.
#[tokio::test]
async fn a_normalized_name_finds_the_project_it_was_published_under() {
    let fx = fixture_with_package("Matrix_PyPI", "2.0.0").await;

    let (status, body) = fx.get(&fx.simple("/matrix-pypi/")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("/packages/pypi/Matrix_PyPI/2.0.0/"),
        "the download link must name the package as it is stored: {body}"
    );

    let href = body
        .split_once("<a href=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(url, _)| url.split('#').next().unwrap().to_string())
        .unwrap_or_else(|| panic!("no download link in the index page: {body}"));
    let (status, _) = fx.get(&href).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the linked file must download: {href}"
    );
}

/// The root index — the URL a user hands to `--index-url` — lists every project
/// and links each one with the trailing slash its page is served at.
#[tokio::test]
async fn the_root_index_lists_projects_and_links_them_with_a_slash() {
    let fx = fixture_with_package("Matrix_PyPI", "1.0.0").await;

    for tail in ["/", ""] {
        let (status, body) = fx.get(&fx.simple(tail)).await;
        assert_eq!(status, StatusCode::OK, "GET simple{tail}: {body}");
        assert!(
            body.contains("Matrix_PyPI"),
            "GET simple{tail} must list the published project: {body}"
        );
        assert!(
            body.contains("/packages/pypi/simple/matrix-pypi/\""),
            "GET simple{tail} must link the project page by its normalized name, slash included: {body}"
        );
    }

    // The link the root index advertises has to be one the router serves.
    let (status, body) = fx.get(&fx.simple("/matrix-pypi/")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// A repository with no PyPI packages is an empty index, not a 404: in
/// production a 404 falls through to the SPA and pip parses the app shell.
#[tokio::test]
async fn an_empty_registry_is_an_empty_index_rather_than_a_fallback_page() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "pypi-owner", "pypi-owner@example.com").await;
    create_repo(&base, &token, "pypi-repo").await;
    let fx = Fixture {
        base,
        client: reqwest::Client::new(),
        db,
    };

    let (status, body) = fx.get(&fx.simple("/")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("<title>Simple index</title>"), "{body}");
}

/// Acceptance for card_287a67fe4f81: the normalized-name fallback must not turn
/// a failed lookup into "no such project".
///
/// The project page looks the literal spelling up first and, on a miss, scans
/// the published names for one that normalizes to the requested form. Both
/// queries hit the `packages` table, so a database failure failed *both* — and
/// the scan swallowed its own failure (`.ok()?`), leaving the handler to report
/// the first miss as a `404`. For `pip`/`uv` a `404` means "this project does
/// not exist here", which is cached and not retried, so an outage looked like a
/// deleted package; the body also carried the `db: …` context (H-05).
///
/// The outage is simulated by dropping `packages` rather than by closing the
/// pool: the repository resolution and the read gate run first and must keep
/// working, or the request would never reach the code under test and the
/// assertion would pass against the unfixed handler.
#[tokio::test]
async fn a_failed_project_scan_is_not_reported_as_an_absent_project() {
    let fx = fixture_with_package("Matrix_PyPI", "3.0.0").await;

    // Baselines on a healthy database — without them this test cannot tell "the
    // status was fixed" from "the route 5xx's on everything".
    let (status, body) = fx.get(&fx.simple("/matrix-pypi/")).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the published project is still found by its normalized name: {body}"
    );
    let (status, body) = fx.get(&fx.simple("/definitely-absent/")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a project that really is not published stays a 404: {body}"
    );
    let absent: serde_json::Value = serde_json::from_str(&body).expect("404 body is JSON");
    let absent = absent["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !absent.contains("db:") && !absent.contains("packages"),
        "the 404 body must be the fixed message, got: {absent}"
    );

    // Break exactly the table both lookups read.
    fx.db
        .execute_unprepared("DROP TABLE packages")
        .await
        .expect("drop packages");

    let (status, body) = fx.get(&fx.simple("/matrix-pypi/")).await;
    assert!(
        status.is_server_error(),
        "a failed project scan must be a 5xx, not {status} — a 404 tells pip the \
         project does not exist and it will not retry (body: {body})"
    );
    let failed: serde_json::Value = serde_json::from_str(&body).expect("5xx body is JSON");
    let message = failed["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("packages"),
        "the response body must not carry internal error detail, got: {message}"
    );
}

// ── PEP 740 attestations (card_b25bd1cbc60c) ──────────────

/// One PEP 740 attestation describing `filename` / `sha256`.
fn attestation(filename: &str, sha256: &str, predicate_type: &str) -> serde_json::Value {
    use base64::Engine as _;
    let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
    let statement = serde_json::json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{ "name": filename, "digest": { "sha256": sha256 } }],
        "predicateType": predicate_type,
        "predicate": {},
    });
    serde_json::json!({
        "version": 1,
        "verification_material": {
            "certificate": b64(b"a DER certificate"),
            "transparency_entries": [{ "logIndex": "1" }],
        },
        "envelope": {
            "statement": b64(&serde_json::to_vec(&statement).unwrap()),
            "signature": b64(b"a DSSE signature"),
        },
    })
}

struct AttestedUpload {
    base: String,
    owner: String,
    token: String,
    client: reqwest::Client,
    /// Held for the lifetime of the test: the temporary database lives as long
    /// as this handle.
    _db: rg_db::DatabaseConnection,
    filename: String,
    body: Vec<u8>,
    digest: String,
}

impl AttestedUpload {
    async fn new(owner: &str) -> Self {
        let (base, db) = spawn_test_app_with_db().await;
        let (token, _) = register_full(&base, owner, &format!("{owner}@example.com")).await;
        create_repo(&base, &token, "attest-repo").await;
        let body = wheel(
            "matrix_attest-1.0.0.dist-info",
            "Metadata-Version: 2.1\nName: matrix-attest\nVersion: 1.0.0\n",
        );
        let digest = hex::encode(Sha256::digest(&body));
        Self {
            base,
            owner: owner.to_string(),
            token,
            client: reqwest::Client::new(),
            _db: db,
            filename: "matrix_attest-1.0.0-py3-none-any.whl".to_string(),
            body,
            digest,
        }
    }

    fn url(&self, tail: &str) -> String {
        format!(
            "{}/api/v1/repos/{}/attest-repo/packages/pypi{tail}",
            self.base, self.owner
        )
    }

    async fn get(&self, url: &str) -> (StatusCode, String) {
        let response = self.client.get(url).send().await.unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    }

    async fn upload(&self, attestations: Option<serde_json::Value>) -> (StatusCode, String) {
        let mut form = twine_form(
            "matrix-attest",
            "1.0.0",
            &self.filename,
            self.body.clone(),
            self.digest.clone(),
        );
        if let Some(attestations) = attestations {
            form = form.text("attestations", attestations.to_string());
        }
        let response = self
            .client
            .post(self.url("/legacy/"))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await
            .unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    }
}

/// Acceptance for card_b25bd1cbc60c: `twine upload --attestations` used to be
/// answered 200 with the attestations dropped on the floor. The evidence must
/// survive the upload, be reachable, and be advertised where a client looks.
#[tokio::test]
async fn twine_attestations_survive_the_upload_and_are_served() {
    let fx = AttestedUpload::new("attest-owner").await;
    let attestation = attestation(
        &fx.filename,
        &fx.digest,
        "https://docs.pypi.org/attestations/publish/v1",
    );

    let (status, body) = fx
        .upload(Some(serde_json::json!([attestation.clone()])))
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // The Simple page is the only place a client learns the provenance exists.
    let (status, index) = fx.get(&fx.url("/simple/matrix-attest/")).await;
    assert_eq!(status, StatusCode::OK, "{index}");
    let provenance_url = index
        .split_once("data-provenance=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(url, _)| url.to_string())
        .unwrap_or_else(|| panic!("the project page advertises no provenance: {index}"));
    assert!(
        provenance_url.ends_with(&format!("{}.provenance", fx.filename)),
        "{provenance_url}"
    );
    // The provenance file is evidence, not a distribution: pip must never be
    // offered it as something to install.
    assert_eq!(
        index.matches("<a href=").count(),
        1,
        "only the wheel is an installable link: {index}"
    );
    // Storing a second file under the version must not cost the wheel its own
    // digest — the version-level fallback only applies to a lone file, so the
    // per-file digest is what has to carry this link now.
    assert!(
        index.contains(&format!("#sha256={}\"", fx.digest)),
        "the wheel keeps its own checksum next to the link: {index}"
    );

    // And the advertised URL has to be one this server answers, with the
    // attestation the publisher sent still inside it.
    let (status, served) = fx.get(&provenance_url).await;
    assert_eq!(status, StatusCode::OK, "{served}");
    let provenance: serde_json::Value = serde_json::from_str(&served).expect("provenance is JSON");
    assert_eq!(provenance["version"], 1);
    let bundle = &provenance["attestation_bundles"][0];
    assert_eq!(bundle["attestations"][0], attestation);
    // A repository write token is not a Trusted Publisher, and the document
    // must not pass one off as the other.
    assert_eq!(bundle["publisher"]["trusted_publisher"], false);
    assert_eq!(
        bundle["publisher"]["repository"],
        "attest-owner/attest-repo"
    );
}

/// An attestation that does not describe this upload is evidence for something
/// else. Refusing it after the wheel is stored would leave a distribution whose
/// publisher believes it is attested, so the refusal must come first.
#[tokio::test]
async fn a_foreign_attestation_publishes_nothing_at_all() {
    let fx = AttestedUpload::new("foreign-owner").await;

    let (status, body) = fx
        .upload(Some(serde_json::json!([attestation(
            "matrix_attest-9.9.9-py3-none-any.whl",
            &fx.digest,
            "https://slsa.dev/provenance/v1"
        )])))
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("subject names"), "{body}");

    let (status, index) = fx.get(&fx.url("/simple/")).await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert!(
        !index.contains("matrix-attest"),
        "the refused upload must leave no package behind: {index}"
    );
}

/// The silent-loss shape this card is about: the field arrives, the server has
/// no way to honour it, and the publisher is told everything went fine.
#[tokio::test]
async fn a_corrupt_attestations_field_is_refused_rather_than_ignored() {
    let fx = AttestedUpload::new("corrupt-owner").await;

    let (status, body) = fx.upload(Some(serde_json::json!([{ "version": 1 }]))).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an unusable attestation must not answer 200: {body}"
    );

    let (status, index) = fx.get(&fx.url("/simple/")).await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert!(!index.contains("matrix-attest"), "{index}");
}

/// Acceptance for card_68133b6fb79d: Twine sends `gpg_signature` as a file
/// part next to `content`. Until ForgeKeep can preserve and serve that sidecar,
/// rejecting the request is the only honest result; the wheel must not be
/// published after its detached signature was refused.
#[tokio::test]
async fn a_gpg_signed_upload_is_refused_before_publishing_distribution() {
    let fx = AttestedUpload::new("gpg-owner").await;
    let form = twine_form(
        "matrix-attest",
        "1.0.0",
        &fx.filename,
        fx.body.clone(),
        fx.digest.clone(),
    )
    .part(
        "gpg_signature",
        reqwest::multipart::Part::bytes(b"a detached OpenPGP signature".to_vec())
            .file_name(format!("{}.asc", fx.filename))
            .mime_str("application/octet-stream")
            .expect("literal MIME type"),
    );
    let response = fx
        .client
        .post(fx.url("/legacy/"))
        .bearer_auth(&fx.token)
        .multipart(form)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("GPG signatures are not supported"), "{body}");

    let (status, index) = fx.get(&fx.url("/simple/")).await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert!(
        !index.contains("matrix-attest"),
        "the rejected signed upload must leave no package behind: {index}"
    );
}

/// The mirror of the case above: an upload that carries no attestations is
/// still a perfectly good upload, and its page must not advertise provenance
/// the registry does not hold.
#[tokio::test]
async fn an_unattested_upload_advertises_no_provenance() {
    let fx = AttestedUpload::new("plain-owner").await;

    let (status, body) = fx.upload(None).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let (status, index) = fx.get(&fx.url("/simple/matrix-attest/")).await;
    assert_eq!(status, StatusCode::OK, "{index}");
    assert!(!index.contains("data-provenance"), "{index}");

    let (status, missing) = fx
        .get(&fx.url(&format!("/matrix-attest/1.0.0/{}.provenance", fx.filename)))
        .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "nothing is stored, so nothing is served: {missing}"
    );
}
