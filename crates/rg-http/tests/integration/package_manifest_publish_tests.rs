//! card_d4a40e605109: the publish handler must not throw away the manifest
//! parse.
//!
//! `POST .../packages/{type}/publish` ran the adapter's `extract_metadata` and
//! then `.ok()`-ed the result, so a file whose manifest does not parse reached
//! one of two wrong endings. With `?name=&version=` in the query the caller's
//! values stood in for the metadata and the broken artifact was stored with a
//! `201`, to be served from then on to the protocol client that cannot read it.
//! Without them the caller was told `package name is required` — a message
//! naming the wrong cause, because the name was not missing, it was unreadable.
//!
//! The fix splits the two jobs the adapters had been sharing unevenly:
//! `validate` is the verdict on the artifact and is unconditional, while
//! `extract_metadata` only reads coordinates out of it and is fatal solely when
//! nothing else can supply them. Both halves are pinned here, and so is the
//! legitimate fall-through — a Maven `-sources.jar` carries no manifest by
//! design and must still publish.

use std::io::{Cursor, Write};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::StatusCode;
use sea_orm::{ConnectionTrait, FromQueryResult};

const OWNER: &str = "manifest-owner";
const REPO: &str = "manifest-repo";

fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut tar = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut tar);
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            archive.append(&header, *content).unwrap();
        }
        archive.finish().unwrap();
    }
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&tar).unwrap();
    encoder.finish().unwrap()
}

fn zip_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut archive = zip::ZipWriter::new(&mut output);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (path, content) in files {
            archive.start_file(*path, options).unwrap();
            archive.write_all(content).unwrap();
        }
        archive.finish().unwrap();
    }
    output.into_inner()
}

struct Fixture {
    base: String,
    token: String,
    db: rg_db::DatabaseConnection,
}

impl Fixture {
    async fn new() -> Self {
        let (base, db) = spawn_test_app_with_db().await;
        let (token, _) = register_full(&base, OWNER, &format!("{OWNER}@example.test")).await;
        create_repo(&base, &token, REPO).await;
        Self { base, token, db }
    }

    /// `coordinates` is what goes into the query string — `None` means the
    /// caller sent neither name nor version and is relying on extraction.
    async fn publish(
        &self,
        package_type: &str,
        filename: &str,
        body: Vec<u8>,
        coordinates: Option<(&str, &str)>,
    ) -> reqwest::Response {
        let mut url = reqwest::Url::parse(&format!(
            "{}/api/v1/repos/{OWNER}/{REPO}/packages/{package_type}/publish",
            self.base
        ))
        .expect("publish url");
        if let Some((name, version)) = coordinates {
            url.query_pairs_mut()
                .append_pair("name", name)
                .append_pair("version", version);
        }
        reqwest::Client::new()
            .post(url)
            .bearer_auth(&self.token)
            .header(
                reqwest::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            )
            .body(body)
            .send()
            .await
            .expect("publish request")
    }

    /// Nothing may be left behind by a refused publish.
    async fn stored_versions(&self) -> i64 {
        #[derive(Debug, sea_orm::FromQueryResult)]
        struct Count {
            total: i64,
        }
        Count::find_by_statement(sea_orm::Statement::from_string(
            self.db.get_database_backend(),
            "SELECT COUNT(*) AS total FROM package_versions",
        ))
        .one(&self.db)
        .await
        .expect("count package versions")
        .expect("one row")
        .total
    }
}

/// The headline case. The envelope is a perfectly good gzip and `Chart.yaml` is
/// present, so the old `validate` was satisfied; only the YAML inside is
/// rubbish. The caller names the coordinates in the query, which is exactly the
/// combination that used to answer `201`.
#[tokio::test]
async fn a_chart_whose_manifest_does_not_parse_is_refused_and_stores_nothing() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    let response = fixture
        .publish(
            "helm",
            "broken-1.0.0.tgz",
            tar_gz(&[(
                "broken/Chart.yaml",
                b"name: broken\n  version: \"1.0.0\"\n\tapiVersion: v2\n",
            )]),
            Some(("broken", "1.0.0")),
        )
        .await;

    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "a chart that no helm client can read is not publishable"
    );
    let body = response.text().await.unwrap_or_default();
    assert!(
        body.contains("Chart.yaml"),
        "the answer must name the manifest that failed, got: {body}"
    );
    assert_eq!(
        fixture.stored_versions().await,
        before,
        "a refused publish must not leave a version row behind"
    );
}

/// The other ending of the same defect: with no query params the caller used to
/// be told the name was missing. It was not missing — it was unreadable, and
/// the answer has to say which.
#[tokio::test]
async fn a_chart_that_cannot_be_read_does_not_report_a_missing_name() {
    let fixture = Fixture::new().await;

    let response = fixture
        .publish(
            "helm",
            "broken-1.0.0.tgz",
            tar_gz(&[("broken/Chart.yaml", b"[this is not: a chart\n")]),
            None,
        )
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = response.text().await.unwrap_or_default();
    assert!(
        body.contains("Chart.yaml"),
        "the answer must name the real cause, got: {body}"
    );
    assert!(
        !body.contains("package name is required"),
        "the name was not missing, it was unreadable: {body}"
    );
}

/// A chart that does parse still publishes, so the check above is a check and
/// not a blanket refusal of the format.
#[tokio::test]
async fn a_well_formed_chart_still_publishes() {
    let fixture = Fixture::new().await;

    let response = fixture
        .publish(
            "helm",
            "good-1.0.0.tgz",
            tar_gz(&[(
                "good/Chart.yaml",
                b"apiVersion: v2\nname: good\nversion: 1.0.0\n",
            )]),
            None,
        )
        .await;

    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "a chart whose Chart.yaml parses is publishable without any query params"
    );
}

/// "The adapter cannot read metadata from this kind of file" and "the adapter
/// tried to read it and failed" used to arrive at the handler as the same
/// `None`. They are different answers now, and this is the first of the two:
/// `GenericAdapter` has no manifest to read and the caller supplies everything.
#[tokio::test]
async fn a_format_with_nothing_to_extract_still_publishes_on_query_params() {
    let fixture = Fixture::new().await;

    let response = fixture
        .publish(
            "generic",
            "blob-1.0.0.bin",
            b"an opaque payload with no manifest anywhere in it".to_vec(),
            Some(("blob", "1.0.0")),
        )
        .await;

    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "a format that never had metadata to extract is not a failed extraction"
    );
}

/// The second of the two, and the reason a blanket refusal in the handler would
/// have been wrong: Maven publishes one version as several files, and only the
/// `.pom` carries coordinates. A classifier-suffixed jar defeats the
/// `{name}-{version}` filename convention by design, which is precisely why the
/// caller states the coordinates in the query.
#[tokio::test]
async fn a_maven_classifier_artifact_without_a_manifest_still_joins_its_version() {
    let fixture = Fixture::new().await;
    let coordinates = Some(("com.example:multi", "1.0.0"));

    let created = fixture
        .publish(
            "maven",
            "multi-1.0.0.pom",
            br#"<?xml version="1.0"?>
<project><groupId>com.example</groupId><artifactId>multi</artifactId><version>1.0.0</version></project>"#
                .to_vec(),
            coordinates,
        )
        .await;
    assert_eq!(created.status(), StatusCode::CREATED, "the pom creates it");

    let added = fixture
        .publish(
            "maven",
            "multi-1.0.0-sources.jar",
            zip_archive(&[("Main.java", b"class Main {}\n")]),
            coordinates,
        )
        .await;
    assert_eq!(
        added.status(),
        StatusCode::OK,
        "a sources jar carries no manifest and must still join the version"
    );
}

/// `POST /packages/docker/publish` cannot store anything a `docker pull` will
/// ever find. It used to answer `201` and put an unreachable row in the
/// registry; the refusal now names where the caller should go instead.
#[tokio::test]
async fn a_docker_publish_is_refused_and_points_at_the_oci_api() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    let response = fixture
        .publish(
            "docker",
            "image.tar",
            b"whatever a caller might upload here".to_vec(),
            Some(("image", "1.0")),
        )
        .await;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    assert_ne!(
        status,
        StatusCode::CREATED,
        "this endpoint cannot publish a docker image: {body}"
    );
    assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");
    assert!(
        body.contains("OCI") || body.contains("/v2/") || body.contains("v2 API"),
        "the refusal must send the caller to the OCI API, got: {body}"
    );
    assert_eq!(
        fixture.stored_versions().await,
        before,
        "a refused docker publish must not leave a version row behind"
    );
}

/// A tar archive with no gzip around it — a `.gem` is a plain tar.
fn tar_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut out);
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_path(path).unwrap();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            archive.append(&header, *content).unwrap();
        }
        archive.finish().unwrap();
    }
    out
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

/// card_9f264cb67dd9: `validate` is the only gate a publish always runs —
/// `extract_metadata` failing is deliberately survivable when the coordinates
/// arrive in the query string, which is how a Maven classifier artifact gets
/// published. Five adapters used to spend that one unconditional gate checking
/// that the manifest was *present*, never that it could be *read*.
///
/// The consequence is a `201` for an artifact no client can install: `cargo`,
/// `npm`, `pip`, `gem` and `dotnet` all parse the same manifest the registry
/// declined to look at, and they parse it after resolving, downloading and
/// unpacking.
///
/// Each case below is a perfectly good envelope — real gzip, real ZIP, real tar
/// — with the manifest inside it broken, published with `?name=&version=` so
/// the extraction failure is survivable and only `validate` stands between the
/// artifact and the registry.
///
/// The other half of the discrimination is not repeated here: the nine-format
/// matrix in `package_format_e2e_tests` publishes a healthy artifact of every
/// one of these formats, so a `validate` that rejected too much goes red there.
#[tokio::test]
async fn a_manifest_that_cannot_be_read_is_refused_for_every_format_that_has_one() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    // `(package type, file name, body, the word the refusal has to name)`.
    let cases: Vec<(&str, &str, Vec<u8>, &str)> = vec![
        (
            "cargo",
            "broken-1.0.0.crate",
            tar_gz(&[("broken-1.0.0/Cargo.toml", b"[package\nname = ")]),
            "cargo.toml",
        ),
        (
            "npm",
            "broken-1.0.0.tgz",
            tar_gz(&[("package/package.json", b"{ \"name\": ")]),
            "package.json",
        ),
        (
            "nuget",
            "broken.1.0.0.nupkg",
            zip_archive(&[(
                "broken.nuspec",
                br#"<?xml version="1.0"?><package><metadata><version>1.0.0</version></metadata></package>"#,
            )]),
            ".nuspec",
        ),
        (
            "pypi",
            "broken-1.0.0-py3-none-any.whl",
            zip_archive(&[("broken-1.0.0.dist-info/METADATA", b"Version: 1.0.0\n")]),
            "metadata",
        ),
        (
            "rubygems",
            "broken-1.0.0.gem",
            tar_archive(&[("metadata.gz", &gzip(b"[this is not: a gemspec\n"))]),
            "metadata",
        ),
    ];

    for (package_type, filename, body, expected) in cases {
        let response = fixture
            .publish(package_type, filename, body, Some(("broken", "1.0.0")))
            .await;

        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a {package_type} artifact whose manifest cannot be read must not be stored: {text}"
        );
        assert!(
            text.to_lowercase().contains(expected),
            "the {package_type} refusal must name the manifest that failed, got: {text}"
        );
    }

    assert_eq!(
        fixture.stored_versions().await,
        before,
        "no refused publish may leave a version row behind"
    );
}
