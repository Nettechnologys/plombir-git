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

/// Helm models `Metadata.Version` as a string and validates that string with
/// Masterminds/semver. Query coordinates must not let a non-Helm chart bypass
/// either half of that contract.
#[tokio::test]
async fn a_chart_version_must_be_a_string_and_valid_helm_semver() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    for (filename, chart_yaml, expected_error) in [
        (
            "numeric-1.0.tgz",
            b"apiVersion: v2\nname: numeric\nversion: 1.0\n".as_slice(),
            "must be a string",
        ),
        (
            "invalid.tgz",
            b"apiVersion: v2\nname: invalid\nversion: legacy-row\n".as_slice(),
            "valid Helm semantic version",
        ),
    ] {
        let response = fixture
            .publish(
                "helm",
                filename,
                tar_gz(&[("chart/Chart.yaml", chart_yaml)]),
                Some(("query-cannot-bypass", "1.0.0")),
            )
            .await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{filename}");
        let body = response.text().await.unwrap_or_default();
        assert!(
            body.contains(expected_error),
            "{filename} hid the Chart.yaml version error: {body}"
        );
        assert_eq!(
            fixture.stored_versions().await,
            before,
            "{filename} left a live package version behind"
        );
    }
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

/// card_c0f6fbd66e2d: a Maven coordinate cannot be completed with a made-up
/// `unknown` group. Query coordinates do not make the POM valid: the Maven PUT
/// route always supplies them from the layout path, so accepting that bypass
/// would still store a POM that Maven itself cannot consume.
#[tokio::test]
async fn a_pom_without_a_project_or_parent_group_id_is_refused() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    for (artifact_id, through_maven_layout) in [
        ("missing-group-derived", false),
        ("missing-group-layout", true),
    ] {
        let pom = format!(
            "<?xml version=\"1.0\"?><project><artifactId>{artifact_id}</artifactId>\
             <version>1.0.0</version></project>"
        );
        let filename = format!("{artifact_id}-1.0.0.pom");
        let response = if through_maven_layout {
            reqwest::Client::new()
                .put(format!(
                    "{}/api/v1/repos/{OWNER}/{REPO}/packages/maven/com/example/\
                     {artifact_id}/1.0.0/{filename}",
                    fixture.base
                ))
                .bearer_auth(&fixture.token)
                .body(pom.into_bytes())
                .send()
                .await
                .expect("Maven layout publish request")
        } else {
            fixture
                .publish("maven", &filename, pom.into_bytes(), None)
                .await
        };

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a POM with no groupId is not publishable: {body}"
        );
        assert!(
            body.contains("<groupId>"),
            "the refusal must name the missing coordinate, got: {body}"
        );
        assert_eq!(
            fixture.stored_versions().await,
            before,
            "a refused POM must not leave a version row behind"
        );
    }
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

/// card_4df8ddf63daa: a `Cargo.toml` that *parses* can still declare a feature
/// table the sparse index cannot carry.
///
/// `[features] default = ["std", 42]` used to publish as `default = ["std"]`
/// and `default = 42` as `default = []` — the index then describes a feature
/// graph the published crate does not have, and cargo resolves against the
/// index rather than the `.crate` file, so the lie holds until the build dies
/// at `unknown feature` or at an optional dependency that was never enabled.
/// The refusal has to land at publish, before a version row exists, and name
/// the feature it refused.
#[tokio::test]
async fn a_cargo_feature_table_the_index_cannot_carry_is_refused_and_stores_nothing() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    for (label, features, expected) in [
        ("mixed array", "default = [\"std\", 42]", "entry 1"),
        ("scalar feature", "default = 42", "must be an array"),
        (
            "table feature",
            "fast = { dep = \"rand\" }",
            "must be an array",
        ),
    ] {
        let manifest =
            format!("[package]\nname = \"lossy\"\nversion = \"1.0.0\"\n\n[features]\n{features}\n");
        let response = fixture
            .publish(
                "cargo",
                "lossy-1.0.0.crate",
                tar_gz(&[("lossy-1.0.0/Cargo.toml", manifest.as_bytes())]),
                // Query coordinates are what makes `extract_metadata` failing
                // survivable, so this is the combination that used to store the
                // crate with a feature table quietly rewritten.
                Some(("lossy", "1.0.0")),
            )
            .await;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{label}: a crate whose features cannot reach the index is not publishable: {body}"
        );
        assert!(
            body.contains("feature") && body.contains(expected),
            "{label}: the refusal must name the feature it refused, got: {body}"
        );
        assert_eq!(
            fixture.stored_versions().await,
            before,
            "{label}: a refused publish must not leave a version row behind"
        );
    }

    // And the discrimination — a feature table of proper string arrays, mixing
    // both index schemas, still publishes.
    let good = fixture
        .publish(
            "cargo",
            "well-formed-1.0.0.crate",
            tar_gz(&[(
                "well-formed-1.0.0/Cargo.toml",
                b"[package]\nname = \"well-formed\"\nversion = \"1.0.0\"\n\n\
                  [dependencies]\nrand = { version = \"0.8\", optional = true }\n\n\
                  [features]\ndefault = [\"std\"]\nstd = []\nfast = [\"dep:rand\"]\n",
            )]),
            None,
        )
        .await;
    assert_eq!(
        good.status(),
        StatusCode::CREATED,
        "a well-formed feature table must still publish"
    );
}

/// Acceptance for card_6133980e7d76, through the real publish route: the
/// dependency spec is the other half of the resolver input the feature table
/// is, and it coerced instead of refusing.
///
/// `features = ["small_rng", 42]` published as `["small_rng"]`, `optional =
/// "true"` as `false` — a dependency the crate meant to gate behind a feature
/// becomes mandatory — `default-features = 0` as `true`, and `version = 0.8`
/// (a float) as `*`. A whole section that is not a table published as *no*
/// dependencies at all. Cargo resolves against the index and never against the
/// `.crate`, so each of those resolves cleanly and dies at the client's build.
#[tokio::test]
async fn a_cargo_dependency_the_index_cannot_carry_is_refused_and_stores_nothing() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    for (label, body, expected) in [
        (
            "mixed feature array",
            "[dependencies]\nrand = { version = \"0.8\", features = [\"small_rng\", 42] }\n",
            "feature 1 must be a string",
        ),
        (
            "string-typed optional",
            "[dependencies]\nrand = { version = \"0.8\", optional = \"true\" }\n",
            "field 'optional' must be a boolean",
        ),
        (
            "integer-typed default-features",
            "[dependencies]\nrand = { version = \"0.8\", default-features = 0 }\n",
            "field 'default-features' must be a boolean",
        ),
        (
            "float version",
            "[dependencies]\nrand = { version = 0.8 }\n",
            "field 'version' must be a string",
        ),
        (
            "spec that is neither string nor table",
            "[dependencies]\nserde = 1.0\n",
            "must be a version string or a table",
        ),
        (
            "section that is not a table",
            "[target.'cfg(unix)']\ndependencies = 5\n",
            "`[target.'cfg(unix)'.dependencies]` must be a table of dependencies",
        ),
    ] {
        let manifest = format!("[package]\nname = \"lossy\"\nversion = \"1.0.0\"\n\n{body}");
        let response = fixture
            .publish(
                "cargo",
                "lossy-1.0.0.crate",
                tar_gz(&[("lossy-1.0.0/Cargo.toml", manifest.as_bytes())]),
                // Query coordinates are what makes `extract_metadata` failing
                // survivable, so this is the combination that used to store the
                // crate with its dependency spec quietly rewritten.
                Some(("lossy", "1.0.0")),
            )
            .await;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{label}: a crate whose dependencies cannot reach the index is not publishable: {body}"
        );
        assert!(
            body.contains(expected),
            "{label}: the refusal must name what it refused, got: {body}"
        );
        assert_eq!(
            fixture.stored_versions().await,
            before,
            "{label}: a refused publish must not leave a version row behind"
        );
    }

    // And the discrimination — every field at its right type, plus the two
    // absences that are legitimate defaults, across all three section kinds.
    let good = fixture
        .publish(
            "cargo",
            "well-formed-1.0.0.crate",
            tar_gz(&[(
                "well-formed-1.0.0/Cargo.toml",
                b"[package]\nname = \"well-formed\"\nversion = \"1.0.0\"\n\n\
                  [dependencies]\nserde = \"1.0\"\n\
                  rand = { version = \"0.8\", features = [\"small_rng\"], optional = true, \
                  default-features = false }\n\
                  json = { version = \"1.0\", package = \"serde_json\" }\n\n\
                  [dev-dependencies]\ntempfile = \"3\"\n\n\
                  [target.'cfg(unix)'.dependencies]\nnix = \"0.27\"\n",
            )]),
            None,
        )
        .await;
    assert_eq!(
        good.status(),
        StatusCode::CREATED,
        "a well-formed dependency table must still publish"
    );
}

/// Acceptance for card_69cfa8de4fd1, through the real publish route: a gemspec
/// declaring one good and one malformed runtime dependency.
///
/// The gem used to publish, and both resolver endpoints then answered `200`
/// with `rack` alone. Bundler resolves that cleanly — a dependency list is
/// never checked against the gem that declared it — so the missing gem surfaces
/// as a `NameError` at runtime, arbitrarily far from the push that caused it.
/// The refusal has to land at publish, before a version row exists, and name
/// the element by its position in the declared list.
#[tokio::test]
async fn a_rubygems_dependency_the_index_cannot_carry_is_refused_and_stores_nothing() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    for (label, deps, expected) in [
        (
            "non-string name beside a good one",
            "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
             - !ruby/object:Gem::Dependency\n  name: 42\n  type: :runtime\n",
            "`dependencies[1].name`",
        ),
        (
            "missing name beside a good one",
            "- !ruby/object:Gem::Dependency\n  name: rack\n  type: :runtime\n\
             - !ruby/object:Gem::Dependency\n  type: :runtime\n",
            "`dependencies[1].name`",
        ),
        (
            "dependencies is not a list",
            "  rack: '>= 2.0'\n",
            "`dependencies` must be a list",
        ),
    ] {
        let gemspec = format!(
            "--- !ruby/object:Gem::Specification\nname: lossy\nversion: '1.0.0'\ndependencies:\n{deps}"
        );
        let response = fixture
            .publish(
                "rubygems",
                "lossy-1.0.0.gem",
                tar_archive(&[("metadata.gz", &gzip(gemspec.as_bytes()))]),
                // Query coordinates are what makes `extract_metadata` failing
                // survivable, so this is the combination that used to store the
                // gem with a dependency quietly dropped.
                Some(("lossy", "1.0.0")),
            )
            .await;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{label}: a gem whose dependencies cannot reach the index is not publishable: {body}"
        );
        assert!(
            body.contains(expected),
            "{label}: the refusal must name what it refused, got: {body}"
        );
        assert_eq!(
            fixture.stored_versions().await,
            before,
            "{label}: a refused publish must not leave a version row behind"
        );
    }

    // And the discrimination — the shape `gem build` writes, both dependency
    // kinds in one list and the development one malformed, still publishes: a
    // development dependency never reaches the index.
    let good = fixture
        .publish(
            "rubygems",
            "well-formed-1.0.0.gem",
            tar_archive(&[(
                "metadata.gz",
                &gzip(
                    b"--- !ruby/object:Gem::Specification\nname: well-formed\nversion: '1.0.0'\n\
                      dependencies:\n\
                      - !ruby/object:Gem::Dependency\n  name: rack\n  \
                      requirement: !ruby/object:Gem::Requirement\n    requirements:\n    \
                      - - \">=\"\n      - !ruby/object:Gem::Version\n        version: '2.0'\n  \
                      type: :runtime\n\
                      - !ruby/object:Gem::Dependency\n  name: 42\n  type: :development\n",
                ),
            )]),
            None,
        )
        .await;
    assert_eq!(
        good.status(),
        StatusCode::CREATED,
        "a well-formed runtime dependency list must still publish"
    );
}

/// Acceptance for card_d5dd8df595e7 and card_b3299f2efc4e — the same defect
/// filed twice, from the cargo sweep and from the rubygems one — through the
/// real publish route.
///
/// `"dependencies": "left-pad"` used to publish as a package that depends on
/// nothing: the value could not travel verbatim (npm fails on the whole
/// document, not the one odd package), so it was dropped, and the empty table
/// written straight after it said the registry had looked and found none. npm
/// resolves against the abbreviated packument and never against the `.tgz`, so
/// the install succeeds and the failure lands at a `require()` far from here.
/// The refusal has to land at publish, before a version row exists, and name
/// the section.
#[tokio::test]
async fn an_npm_dependency_table_the_packument_cannot_carry_is_refused_and_stores_nothing() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    for (label, field, spelled, expected) in [
        (
            "dependencies as a string",
            "dependencies",
            r#""left-pad""#,
            "a string",
        ),
        (
            "dependencies as a list",
            "dependencies",
            r#"["left-pad"]"#,
            "a list",
        ),
        (
            "devDependencies as a number",
            "devDependencies",
            "7",
            "a number",
        ),
        (
            "peerDependencies as a boolean",
            "peerDependencies",
            "false",
            "a boolean",
        ),
        (
            "peerDependenciesMeta as a list",
            "peerDependenciesMeta",
            r#"["left-pad"]"#,
            "a list",
        ),
        (
            "optionalDependencies as a string",
            "optionalDependencies",
            r#""left-pad""#,
            "a string",
        ),
    ] {
        let manifest =
            format!(r#"{{ "name": "lossy", "version": "1.0.0", "{field}": {spelled} }}"#);
        let response = fixture
            .publish(
                "npm",
                "lossy-1.0.0.tgz",
                tar_gz(&[("package/package.json", manifest.as_bytes())]),
                // Query coordinates are what makes `extract_metadata` failing
                // survivable, so this is the combination that used to store the
                // package with its dependency table quietly emptied.
                Some(("lossy", "1.0.0")),
            )
            .await;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{label}: a package whose dependency table cannot reach the packument is not \
             publishable: {body}"
        );
        assert!(
            body.contains(&format!("`{field}`")) && body.contains(expected),
            "{label}: the refusal must name the section it refused, got: {body}"
        );
        assert_eq!(
            fixture.stored_versions().await,
            before,
            "{label}: a refused publish must not leave a version row behind"
        );
    }

    // And the discrimination — every table spelled as a table, plus one the
    // manifest declares as an explicit `null`, which is the same claim as not
    // declaring it at all.
    let good = fixture
        .publish(
            "npm",
            "well-formed-1.0.0.tgz",
            tar_gz(&[(
                "package/package.json",
                br#"{ "name": "well-formed", "version": "1.0.0",
                     "dependencies": { "left-pad": "^1.3.0" },
                     "peerDependencies": { "react": ">=18" },
                     "optionalDependencies": null }"#,
            )]),
            None,
        )
        .await;
    assert_eq!(
        good.status(),
        StatusCode::CREATED,
        "a well-formed dependency table must still publish"
    );
}

/// Acceptance for card_0e2114db179c — the write half of the defect
/// `card_3d573d788331` closed on the read side, through the real publish route.
///
/// `nuspec_dependencies` skipped a `<dependency>` with no readable `id`, so the
/// group published *shorter* than the nuspec wrote it while still looking
/// complete. `dotnet restore` builds its graph out of the registration index
/// rather than the `.nupkg`, so the missing dependency reads as one the package
/// never declared: restore goes green and the build fails on an absent assembly
/// arbitrarily far from the push that caused it.
#[tokio::test]
async fn a_nuspec_dependency_the_registration_index_cannot_carry_is_refused_and_stores_nothing() {
    let fixture = Fixture::new().await;
    let before = fixture.stored_versions().await;

    for (label, dependencies, expected) in [
        (
            "grouped, one element short of an id",
            r#"<dependencies>
                 <group targetFramework="net8.0">
                   <dependency id="Kept.Dep" version="[1.0.0]" />
                   <dependency version="[2.0.0]" />
                 </group>
               </dependencies>"#,
            "at position 1",
        ),
        (
            "grouped, an id spelled empty",
            r#"<dependencies>
                 <group targetFramework="net8.0">
                   <dependency id="" version="[2.0.0]" />
                 </group>
               </dependencies>"#,
            "at position 0",
        ),
        (
            "the pre-2.0 flat layout",
            r#"<dependencies>
                 <dependency id="Kept.Dep" version="[1.0.0]" />
                 <dependency version="[2.0.0]" />
               </dependencies>"#,
            "at position 1",
        ),
    ] {
        let nuspec = format!(
            r#"<?xml version="1.0"?><package><metadata>
                 <id>lossy</id><version>1.0.0</version>
                 <description>d</description><authors>a</authors>
                 {dependencies}
               </metadata></package>"#
        );
        let response = fixture
            .publish(
                "nuget",
                "lossy.1.0.0.nupkg",
                zip_archive(&[("lossy.nuspec", nuspec.as_bytes())]),
                // Query coordinates are what makes `extract_metadata` failing
                // survivable, so this is the combination that used to store the
                // package with its dependency group quietly shortened.
                Some(("lossy", "1.0.0")),
            )
            .await;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{label}: a dependency the registration index cannot carry is not publishable: {body}"
        );
        assert!(
            body.contains("<dependency>") && body.contains(expected),
            "{label}: the refusal must name the element it refused, got: {body}"
        );
        assert_eq!(
            fixture.stored_versions().await,
            before,
            "{label}: a refused publish must not leave a version row behind"
        );
    }

    // And the discrimination. Every legal shape still publishes: both layouts,
    // a group that declares a framework and needs nothing, and a dependency
    // with no `version` — which is the nuspec's shorthand for "any version",
    // not a missing field.
    let good = fixture
        .publish(
            "nuget",
            "well-formed.1.0.0.nupkg",
            zip_archive(&[(
                "well-formed.nuspec",
                br#"<?xml version="1.0"?><package><metadata>
                      <id>well-formed</id><version>1.0.0</version>
                      <description>d</description><authors>a</authors>
                      <dependencies>
                        <group targetFramework="net8.0">
                          <dependency id="Newtonsoft.Json" version="[13.0.1, 14.0.0)" />
                          <dependency id="Serilog" />
                        </group>
                        <group targetFramework="netstandard2.0" />
                      </dependencies>
                    </metadata></package>"#,
            )]),
            None,
        )
        .await;
    assert_eq!(
        good.status(),
        StatusCode::CREATED,
        "a well-formed dependency group must still publish"
    );
}
