//! End-to-end publish, metadata and download coverage for native package formats.

use std::io::{Cursor, Write};

use crate::common::{
    create_repo, register_full, spawn_test_app_with_db, spawn_test_app_with_overrides,
    StateOverrides,
};
use base64::Engine as _;
use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::StatusCode;
use sea_orm::ConnectionTrait as _;
use sha2::Digest as _;

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(data).unwrap();
    encoder.finish().unwrap()
}

fn tar_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut output = Vec::new();
    {
        let mut archive = tar::Builder::new(&mut output);
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
    output
}

fn tar_gz(files: &[(&str, &[u8])]) -> Vec<u8> {
    gzip(&tar_archive(files))
}

fn tar_gz_uncompressed(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::none());
    encoder.write_all(&tar_archive(files)).unwrap();
    encoder.finish().unwrap()
}

fn npm_provenance_bundle(name: &str, version: &str, sha512: &str) -> serde_json::Value {
    let statement = serde_json::json!({
        "_type": "https://in-toto.io/Statement/v1",
        "subject": [{
            "name": format!("pkg:npm/{name}@{version}"),
            "digest": { "sha512": sha512 }
        }],
        "predicateType": "https://slsa.dev/provenance/v1",
        "predicate": { "buildDefinition": {}, "runDetails": {} }
    });
    serde_json::json!({
        "mediaType": "application/vnd.dev.sigstore.bundle.v0.3+json",
        "verificationMaterial": {},
        "dsseEnvelope": {
            "payloadType": "application/vnd.in-toto+json",
            "payload": base64::engine::general_purpose::STANDARD
                .encode(serde_json::to_vec(&statement).unwrap()),
            "signatures": [{ "sig": "integration-structure-signature" }]
        }
    })
}

fn zip_archive(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut output = Cursor::new(Vec::new());
    {
        let mut archive = zip::ZipWriter::new(&mut output);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        for (path, content) in files {
            archive.start_file(path, options).unwrap();
            archive.write_all(content).unwrap();
        }
        archive.finish().unwrap();
    }
    output.into_inner()
}

struct PackageCase {
    package_type: &'static str,
    name: &'static str,
    version: &'static str,
    filename: &'static str,
    body: Vec<u8>,
}

enum YankedIndexEncoding {
    /// The protocol keeps an exact pin addressable, but marks it so a fresh
    /// resolution cannot select it.
    Marked { required: &'static [&'static str] },
    /// The protocol has no version-level yank marker, so the index must omit
    /// the withdrawn version altogether.
    Omitted {
        status: StatusCode,
        forbidden: &'static [&'static str],
    },
}

struct ProtocolIndexCase {
    package_type: &'static str,
    segments: &'static [&'static str],
    live_marker: &'static str,
    yanked: YankedIndexEncoding,
}

/// The candidate surface of every file-published package protocol.
///
/// Both the live-version smoke and the yank contract consume this registry.
/// The census below derives the expected package types independently from the
/// production adapters, so adding a protocol endpoint without choosing one of
/// these two yank encodings makes the test fail.
fn protocol_index_cases() -> [ProtocolIndexCase; 8] {
    [
        ProtocolIndexCase {
            package_type: "cargo",
            segments: &["cargo", "index", "matrix-cargo"],
            live_marker: "\"vers\":\"1.0.0\"",
            yanked: YankedIndexEncoding::Marked {
                required: &["\"yanked\":true"],
            },
        },
        ProtocolIndexCase {
            package_type: "npm",
            segments: &["npm", "matrix-npm"],
            live_marker: "\"1.0.0\":",
            yanked: YankedIndexEncoding::Omitted {
                status: StatusCode::OK,
                forbidden: &["\"1.0.0\":"],
            },
        },
        ProtocolIndexCase {
            package_type: "pypi",
            segments: &["pypi", "simple", "matrix-pypi"],
            live_marker: "matrix_pypi-1.0.0",
            yanked: YankedIndexEncoding::Marked {
                required: &["data-yanked"],
            },
        },
        ProtocolIndexCase {
            package_type: "maven",
            segments: &["maven", "com.example", "matrix-maven", "maven-metadata.xml"],
            live_marker: "<version>1.0.0</version>",
            yanked: YankedIndexEncoding::Omitted {
                status: StatusCode::OK,
                forbidden: &["<version>1.0.0</version>", "<release>1.0.0</release>"],
            },
        },
        ProtocolIndexCase {
            package_type: "nuget",
            segments: &["nuget", "registration", "Matrix.NuGet", "index.json"],
            live_marker: "\"version\":\"1.0.0\"",
            yanked: YankedIndexEncoding::Marked {
                required: &["\"listed\":false"],
            },
        },
        ProtocolIndexCase {
            package_type: "rubygems",
            segments: &["rubygems", "info", "matrix-gem"],
            live_marker: "1.0.0",
            yanked: YankedIndexEncoding::Omitted {
                status: StatusCode::NOT_FOUND,
                forbidden: &["1.0.0"],
            },
        },
        ProtocolIndexCase {
            package_type: "helm",
            segments: &["helm", "index.yaml"],
            live_marker: "matrix-helm",
            yanked: YankedIndexEncoding::Omitted {
                status: StatusCode::OK,
                forbidden: &["matrix-helm", "1.0.0"],
            },
        },
        ProtocolIndexCase {
            package_type: "composer",
            segments: &["composer", "packages.json"],
            live_marker: "\"1.0.0\":",
            yanked: YankedIndexEncoding::Omitted {
                status: StatusCode::OK,
                forbidden: &["\"1.0.0\":"],
            },
        },
    ]
}

fn package_cases() -> Vec<PackageCase> {
    let cargo_toml = br#"[package]
name = "matrix-cargo"
version = "1.0.0"
description = "Cargo matrix package"
"#;
    let npm_json = br#"{
  "name": "matrix-npm",
  "version": "1.0.0",
  "description": "npm matrix package"
}"#;
    let maven_pom = br#"<?xml version="1.0"?>
<project><groupId>com.example</groupId><artifactId>matrix-maven</artifactId><version>1.0.0</version><description>Maven matrix package</description></project>"#;
    let pypi_metadata =
        b"Metadata-Version: 2.1\nName: matrix-pypi\nVersion: 1.0.0\nSummary: PyPI matrix package\n";
    let nuspec = br#"<?xml version="1.0"?>
<package><metadata><id>Matrix.NuGet</id><version>1.0.0</version><description>NuGet matrix package</description></metadata></package>"#;
    let gem_metadata = b"name: matrix-gem\nversion: 1.0.0\nsummary: RubyGems matrix package\n";
    let chart_yaml =
        b"apiVersion: v2\nname: matrix-helm\nversion: 1.0.0\ndescription: Helm matrix package\n";
    let composer_json = br#"{
  "name": "vendor/matrix-composer",
  "version": "1.0.0",
  "description": "Composer matrix package"
}"#;

    vec![
        PackageCase {
            package_type: "cargo",
            name: "matrix-cargo",
            version: "1.0.0",
            filename: "matrix-cargo-1.0.0.crate",
            body: tar_gz(&[("matrix-cargo-1.0.0/Cargo.toml", cargo_toml)]),
        },
        PackageCase {
            package_type: "npm",
            name: "matrix-npm",
            version: "1.0.0",
            filename: "matrix-npm-1.0.0.tgz",
            body: tar_gz(&[("package/package.json", npm_json)]),
        },
        PackageCase {
            package_type: "maven",
            name: "com.example:matrix-maven",
            version: "1.0.0",
            filename: "matrix-maven-1.0.0.pom",
            body: maven_pom.to_vec(),
        },
        PackageCase {
            package_type: "pypi",
            name: "matrix-pypi",
            version: "1.0.0",
            filename: "matrix_pypi-1.0.0-py3-none-any.whl",
            body: zip_archive(&[("matrix_pypi-1.0.0.dist-info/METADATA", pypi_metadata)]),
        },
        PackageCase {
            package_type: "nuget",
            name: "Matrix.NuGet",
            version: "1.0.0",
            filename: "Matrix.NuGet.1.0.0.nupkg",
            body: zip_archive(&[("Matrix.NuGet.nuspec", nuspec)]),
        },
        PackageCase {
            package_type: "rubygems",
            name: "matrix-gem",
            version: "1.0.0",
            filename: "matrix-gem-1.0.0.gem",
            body: tar_archive(&[("metadata.gz", &gzip(gem_metadata))]),
        },
        PackageCase {
            package_type: "helm",
            name: "matrix-helm",
            version: "1.0.0",
            filename: "matrix-helm-1.0.0.tgz",
            body: tar_gz(&[("matrix-helm/Chart.yaml", chart_yaml)]),
        },
        PackageCase {
            package_type: "composer",
            name: "vendor/matrix-composer",
            version: "1.0.0",
            filename: "matrix-composer-1.0.0.zip",
            body: zip_archive(&[("composer.json", composer_json)]),
        },
        PackageCase {
            package_type: "generic",
            name: "matrix-generic",
            version: "1.0.0",
            filename: "matrix-generic-1.0.0.bin",
            body: b"generic matrix package".to_vec(),
        },
    ]
}

fn package_url(base: &str, segments: &[&str]) -> reqwest::Url {
    let mut url = reqwest::Url::parse(base).unwrap();
    url.path_segments_mut()
        .unwrap()
        .extend([
            "api",
            "v1",
            "repos",
            "matrix-owner",
            "matrix-repo",
            "packages",
        ])
        .extend(segments.iter().copied());
    url
}

async fn publish_nuget_version(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    package: &str,
    version: &str,
) {
    let response = send_nuget_version(client, base, token, package, version).await;
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "{package} {version}"
    );
}

async fn send_nuget_version(
    client: &reqwest::Client,
    base: &str,
    token: &str,
    package: &str,
    version: &str,
) -> reqwest::Response {
    let nuspec = format!(
        "<package><metadata><id>{package}</id><version>{version}</version></metadata></package>"
    );
    client
        .post(package_url(base, &["nuget", "publish"]))
        .bearer_auth(token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{package}.{version}.nupkg\""),
        )
        .body(zip_archive(&[("package.nuspec", nuspec.as_bytes())]))
        .send()
        .await
        .unwrap()
}

async fn search_nuget(
    client: &reqwest::Client,
    base: &str,
    pairs: &[(&str, &str)],
) -> serde_json::Value {
    let mut query = package_url(base, &["nuget", "query"]);
    query.query_pairs_mut().extend_pairs(pairs.iter().copied());
    client
        .get(query)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

async fn autocomplete_nuget(
    client: &reqwest::Client,
    base: &str,
    pairs: &[(&str, &str)],
) -> serde_json::Value {
    let mut query = package_url(base, &["nuget", "autocomplete"]);
    query.query_pairs_mut().extend_pairs(pairs.iter().copied());
    client
        .get(query)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn nuget_search_result<'a>(
    search: &'a serde_json::Value,
    package: &str,
) -> Option<&'a serde_json::Value> {
    search["data"]
        .as_array()?
        .iter()
        .find(|result| result["id"] == package)
}

fn nuget_search_version<'a>(search: &'a serde_json::Value, package: &str) -> Option<&'a str> {
    nuget_search_result(search, package)?["version"].as_str()
}

fn nuget_autocomplete_names(autocomplete: &serde_json::Value) -> Vec<&str> {
    autocomplete["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect()
}

#[test]
fn every_file_package_protocol_declares_how_yank_is_encoded() {
    let mut production: Vec<&str> = rg_core::package_registry::adapter::REGISTERED_ADAPTER_TYPES
        .iter()
        .copied()
        // OCI stores manifests and tags outside `package_versions`; its
        // deletion semantics are a separate protocol contract.
        .filter(|package_type| *package_type != "docker")
        .filter(|package_type| {
            rg_core::package_registry::adapter::get_adapter(package_type)
                .is_some_and(|adapter| adapter.has_protocol_endpoint())
        })
        .collect();
    production.sort_unstable();

    let mut declared: Vec<&str> = protocol_index_cases()
        .iter()
        .map(|case| case.package_type)
        .collect();
    declared.sort_unstable();

    assert_eq!(
        declared, production,
        "every file-published protocol index needs an explicit yank encoding; \
         Docker/OCI is intentionally outside this matrix because it does not use package_versions"
    );
}

#[tokio::test]
async fn nine_native_package_formats_publish_index_and_download() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();
    let cases = package_cases();

    for case in &cases {
        let mut publish_url = package_url(&base, &[case.package_type, "publish"]);
        if case.package_type == "generic" {
            publish_url
                .query_pairs_mut()
                .append_pair("name", case.name)
                .append_pair("version", case.version);
        }
        let published = client
            .post(publish_url)
            .bearer_auth(&token)
            .header(
                reqwest::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", case.filename),
            )
            .body(case.body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(
            published.status(),
            StatusCode::CREATED,
            "{} publish failed: {}",
            case.package_type,
            published.text().await.unwrap()
        );

        let listed = client
            .get(package_url(&base, &[case.package_type, "list"]))
            .send()
            .await
            .unwrap();
        assert_eq!(
            listed.status(),
            StatusCode::OK,
            "{} list",
            case.package_type
        );
        let listed = listed.json::<serde_json::Value>().await.unwrap();
        assert!(listed["packages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|package| package["name"] == case.name));

        let version = client
            .get(package_url(
                &base,
                &[case.package_type, case.name, case.version],
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(version.status(), StatusCode::OK, "{}", case.package_type);

        let downloaded = client
            .get(package_url(
                &base,
                &[case.package_type, case.name, case.version, case.filename],
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(downloaded.status(), StatusCode::OK, "{}", case.package_type);
        assert_eq!(
            downloaded.bytes().await.unwrap().as_ref(),
            case.body.as_slice()
        );
    }

    for check in protocol_index_cases() {
        let response = client
            .get(package_url(&base, check.segments))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{:?}", check.segments);
        let body = response.text().await.unwrap();
        assert!(
            body.contains(check.live_marker),
            "{:?}: {body}",
            check.segments
        );
    }

    let composer = client
        .get(package_url(&base, &["composer", "packages.json"]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let dist_url = composer["packages"]["vendor/matrix-composer"]["1.0.0"]["dist"]["url"]
        .as_str()
        .unwrap();
    let composer_download = client.get(dist_url).send().await.unwrap();
    assert_eq!(composer_download.status(), StatusCode::OK);

    let registries = client
        .get(package_url(&base, &[]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(registries["registries"].as_array().unwrap().len(), 9);
}

/// The configured value is the decoded artifact ceiling, not Axum's extractor
/// ceiling and not npm's larger base64/JSON request size.
#[tokio::test]
async fn large_generic_and_npm_artifacts_cross_two_mib_but_not_the_configured_ceiling() {
    const MIB: usize = 1024 * 1024;
    const ARTIFACT_LIMIT: usize = 3 * MIB;

    let (base, _db) = spawn_test_app_with_overrides(StateOverrides {
        package_upload_max_bytes: Some(ARTIFACT_LIMIT),
        ..StateOverrides::default()
    })
    .await;
    let (token, _) = register_full(&base, "matrix-owner", "large-package@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let generic = vec![0x5a; 2 * MIB + 64 * 1024];
    let mut generic_publish = package_url(&base, &["generic", "publish"]);
    generic_publish
        .query_pairs_mut()
        .append_pair("name", "large-generic")
        .append_pair("version", "1.0.0");
    let response = client
        .post(generic_publish)
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"large-generic-1.0.0.bin\"",
        )
        .body(generic.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "large generic publish: {}",
        response.text().await.unwrap()
    );
    let downloaded = client
        .get(package_url(
            &base,
            &[
                "generic",
                "large-generic",
                "1.0.0",
                "large-generic-1.0.0.bin",
            ],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), generic);

    let package_json = br#"{ "name": "large-npm", "version": "1.0.0" }"#;
    let padding = vec![0xa5; 2 * MIB + 64 * 1024];
    let tarball = tar_gz_uncompressed(&[
        ("package/package.json", package_json.as_slice()),
        ("package/padding.bin", padding.as_slice()),
    ]);
    assert!(tarball.len() > 2 * MIB, "fixture did not cross 2 MiB");
    assert!(
        tarball.len() < ARTIFACT_LIMIT,
        "fixture crossed test ceiling"
    );
    let attachment = "large-npm-1.0.0.tgz";
    let packument = serde_json::json!({
        "_id": "large-npm",
        "name": "large-npm",
        "dist-tags": { "latest": "1.0.0" },
        "versions": {
            "1.0.0": { "name": "large-npm", "version": "1.0.0" }
        },
        "_attachments": {
            attachment: {
                "data": base64::engine::general_purpose::STANDARD.encode(&tarball),
                "length": tarball.len()
            }
        }
    });
    let npm_url = package_url(&base, &["npm", "large-npm"]);
    let response = client
        .put(npm_url.clone())
        .bearer_auth(&token)
        .json(&packument)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "large npm publish: {}",
        response.text().await.unwrap()
    );
    let document: serde_json::Value = client
        .get(npm_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let tarball_url = document["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .unwrap();
    let downloaded = client.get(tarball_url).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), tarball);

    let mut too_large_url = package_url(&base, &["generic", "publish"]);
    too_large_url
        .query_pairs_mut()
        .append_pair("name", "too-large")
        .append_pair("version", "1.0.0");
    let response = client
        .post(too_large_url)
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"too-large.bin\"",
        )
        .body(vec![0; ARTIFACT_LIMIT + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = response.text().await.unwrap();
    assert!(
        body.contains(&format!("configured {ARTIFACT_LIMIT}-byte request limit")),
        "{body}"
    );

    let oversized_npm_manifest = br#"{ "name": "too-large-npm", "version": "1.0.0" }"#;
    let oversized_padding = vec![0x3c; ARTIFACT_LIMIT];
    let oversized_tarball = tar_gz_uncompressed(&[
        ("package/package.json", oversized_npm_manifest.as_slice()),
        ("package/padding.bin", oversized_padding.as_slice()),
    ]);
    assert!(oversized_tarball.len() > ARTIFACT_LIMIT);
    let oversized_packument = serde_json::json!({
        "_id": "too-large-npm",
        "name": "too-large-npm",
        "dist-tags": { "latest": "1.0.0" },
        "versions": {
            "1.0.0": { "name": "too-large-npm", "version": "1.0.0" }
        },
        "_attachments": {
            "too-large-npm-1.0.0.tgz": {
                "data": base64::engine::general_purpose::STANDARD.encode(&oversized_tarball),
                "length": oversized_tarball.len()
            }
        }
    });
    let response = client
        .put(package_url(&base, &["npm", "too-large-npm"]))
        .bearer_auth(&token)
        .json(&oversized_packument)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = response.text().await.unwrap();
    assert!(
        body.contains(&format!("configured {ARTIFACT_LIMIT}-byte artifact limit")),
        "{body}"
    );
}

#[tokio::test]
async fn new_versions_refresh_present_package_metadata_without_rolling_it_back() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let publish = |version: &'static str, suffix: &'static str, manifest: &'static [u8]| {
        let base = base.clone();
        let client = client.clone();
        let token = token.clone();
        async move {
            let archive_path = format!("matrix-metadata-{version}/Cargo.toml");
            client
                .post(package_url(&base, &["cargo", "publish"]))
                .bearer_auth(token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"matrix-metadata-{version}-{suffix}.crate\""),
                )
                .body(tar_gz(&[(&archive_path, manifest)]))
                .send()
                .await
                .unwrap()
        }
    };

    let first = br#"[package]
name = "matrix-metadata"
version = "1.0.0"
description = "first description"
homepage = "https://example.test/first"
repository = "https://git.example.test/first"
"#;
    let second = br#"[package]
name = "matrix-metadata"
version = "2.0.0"
description = "second description"
homepage = "https://example.test/second"
repository = "https://git.example.test/second"
"#;
    let without_optional_metadata = br#"[package]
name = "matrix-metadata"
version = "3.0.0"
"#;

    assert_eq!(
        publish("1.0.0", "main", first).await.status(),
        StatusCode::CREATED
    );
    assert_eq!(
        publish("2.0.0", "main", second).await.status(),
        StatusCode::CREATED
    );

    let current = client
        .get(package_url(&base, &["cargo", "matrix-metadata"]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(current["description"], "second description");
    assert_eq!(current["homepage"], "https://example.test/second");
    assert_eq!(current["repository_url"], "https://git.example.test/second");

    // A later file for an older version can carry that version's old manifest,
    // but it must not roll package-level metadata back.
    assert_eq!(
        publish("1.0.0", "extra", first).await.status(),
        StatusCode::OK
    );
    // A genuinely new version without optional fields must not erase them.
    assert_eq!(
        publish("3.0.0", "main", without_optional_metadata)
            .await
            .status(),
        StatusCode::CREATED
    );

    let after_sparse_and_old_publishes = client
        .get(package_url(&base, &["cargo", "matrix-metadata"]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        after_sparse_and_old_publishes["description"],
        "second description"
    );
    assert_eq!(
        after_sparse_and_old_publishes["homepage"],
        "https://example.test/second"
    );
    assert_eq!(
        after_sparse_and_old_publishes["repository_url"],
        "https://git.example.test/second"
    );
}

/// npm resolves a bare package name through `dist-tags.latest`. Publication
/// time is not version precedence: a maintained 1.x branch can receive a
/// backport after 2.x without becoming the default for new installs.
#[tokio::test]
async fn npm_latest_and_package_summary_use_the_highest_live_semver() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let publish = |version: &'static str| {
        let base = base.clone();
        let client = client.clone();
        let token = token.clone();
        async move {
            let manifest = format!(r#"{{"name":"matrix-version-order","version":"{version}"}}"#);
            client
                .post(package_url(&base, &["npm", "publish"]))
                .bearer_auth(token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"matrix-version-order-{version}.tgz\""),
                )
                .body(tar_gz(&[("package/package.json", manifest.as_bytes())]))
                .send()
                .await
                .unwrap()
        }
    };

    // 1.2.4 is the later backport; 3.0.0 is higher but withdrawn.
    for version in ["2.0.0", "1.2.4", "3.0.0"] {
        assert_eq!(publish(version).await.status(), StatusCode::CREATED);
    }
    let yanked = client
        .patch(package_url(
            &base,
            &["npm", "matrix-version-order", "3.0.0", "yank"],
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "yank": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(yanked.status(), StatusCode::OK);

    for _ in 0..3 {
        let packument = client
            .get(package_url(&base, &["npm", "matrix-version-order"]))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        assert_eq!(packument["dist-tags"]["latest"], "2.0.0", "{packument}");

        let listed = client
            .get(package_url(&base, &["npm", "list"]))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        let summary = listed["packages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|package| package["name"] == "matrix-version-order")
            .unwrap_or_else(|| panic!("package summary missing: {listed}"));
        assert_eq!(summary["latest_version"], "2.0.0", "{listed}");
    }
}

/// NuGet search services negotiate prerelease and SemVer 2 independently. The
/// selected `version` is the highest live NuGetVersion the requesting client
/// can understand, and a package with no such version disappears altogether.
#[tokio::test]
async fn nuget_search_and_autocomplete_filter_prerelease_semver2_and_yanked_versions() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    for (package, version) in [
        ("Matrix.SearchModes", "1.0.0"),
        ("Matrix.SearchModes", "4.0.0-beta"),
        ("Matrix.SearchModes", "5.0.0-beta.1"),
        ("Matrix.SearchModes", "6.0.0+build.7"),
        ("Matrix.SearchModes", "9.0.0"),
        ("Matrix.SearchModes", "10.0.0-beta"),
        ("Matrix.PrereleaseOnly", "3.0.0-beta"),
        ("Matrix.PrereleaseOnly", "7.0.0-alpha.1"),
        ("Matrix.SemVer2Only", "2.0.0+build.5"),
        ("Matrix.YankedOnly", "11.0.0-alpha.1"),
    ] {
        publish_nuget_version(&client, &base, &token, package, version).await;
    }

    for (package, version) in [
        ("Matrix.SearchModes", "9.0.0"),
        ("Matrix.SearchModes", "10.0.0-beta"),
        ("Matrix.YankedOnly", "11.0.0-alpha.1"),
    ] {
        let yanked = client
            .patch(package_url(&base, &["nuget", package, version, "yank"]))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "yank": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(yanked.status(), StatusCode::OK, "{package} {version}");
    }

    // The generic package list is not a NuGet client capability surface. The
    // refactor must preserve its existing stable-only summary while still
    // allowing a stable SemVer 2 spelling.
    let listed = client
        .get(package_url(&base, &["nuget", "list"]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let summary = listed["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "Matrix.SearchModes")
        .unwrap_or_else(|| panic!("NuGet package summary missing: {listed}"));
    assert_eq!(summary["latest_version"], "6.0.0+build.7", "{listed}");

    let stable_semver1 = search_nuget(&client, &base, &[("q", "Matrix.")]).await;
    assert_eq!(stable_semver1["totalHits"], 1, "{stable_semver1}");
    assert_eq!(
        nuget_search_version(&stable_semver1, "Matrix.SearchModes"),
        Some("1.0.0")
    );

    let prerelease_semver1 =
        search_nuget(&client, &base, &[("q", "Matrix."), ("prerelease", "true")]).await;
    assert_eq!(prerelease_semver1["totalHits"], 2, "{prerelease_semver1}");
    assert_eq!(
        nuget_search_version(&prerelease_semver1, "Matrix.SearchModes"),
        Some("4.0.0-beta")
    );
    assert_eq!(
        nuget_search_version(&prerelease_semver1, "Matrix.PrereleaseOnly"),
        Some("3.0.0-beta")
    );

    let stable_semver2 = search_nuget(
        &client,
        &base,
        &[("q", "Matrix."), ("semVerLevel", "2.0.0")],
    )
    .await;
    assert_eq!(stable_semver2["totalHits"], 2, "{stable_semver2}");
    assert_eq!(
        nuget_search_version(&stable_semver2, "Matrix.SearchModes"),
        Some("6.0.0+build.7")
    );
    assert_eq!(
        nuget_search_version(&stable_semver2, "Matrix.SemVer2Only"),
        Some("2.0.0+build.5")
    );

    let all_versions = search_nuget(
        &client,
        &base,
        &[
            ("q", "Matrix."),
            ("prerelease", "true"),
            ("semVerLevel", "2.1.0"),
        ],
    )
    .await;
    assert_eq!(all_versions["totalHits"], 3, "{all_versions}");
    assert_eq!(
        nuget_search_version(&all_versions, "Matrix.SearchModes"),
        Some("6.0.0+build.7")
    );
    assert_eq!(
        nuget_search_version(&all_versions, "Matrix.PrereleaseOnly"),
        Some("7.0.0-alpha.1")
    );

    // SearchQueryService groups every live version allowed by the same
    // capability flags that selected the top-level `version`. It must not
    // collapse that group back to the latest row or leak a prerelease,
    // SemVer 2, or yanked row through a differently-filtered path.
    for (search, expected) in [
        (&stable_semver1, vec!["1.0.0"]),
        (&prerelease_semver1, vec!["1.0.0", "4.0.0-beta"]),
        (&stable_semver2, vec!["1.0.0", "6.0.0+build.7"]),
        (
            &all_versions,
            vec!["1.0.0", "4.0.0-beta", "5.0.0-beta.1", "6.0.0+build.7"],
        ),
    ] {
        let result = nuget_search_result(search, "Matrix.SearchModes")
            .unwrap_or_else(|| panic!("search result missing: {search}"));
        let actual = result["versions"]
            .as_array()
            .unwrap_or_else(|| panic!("versions missing: {result}"))
            .iter()
            .map(|version| version["version"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "wrong capability-filtered set: {result}");
        assert_eq!(
            result["version"],
            expected.last().copied().unwrap(),
            "latest must be the maximum advertised version: {result}"
        );
    }

    let search_modes = nuget_search_result(&all_versions, "Matrix.SearchModes").unwrap();
    let version_rows = search_modes["versions"].as_array().unwrap();
    let leaf_ids = version_rows
        .iter()
        .map(|version| {
            assert_eq!(version["downloads"], 0, "{version}");
            version["@id"]
                .as_str()
                .unwrap_or_else(|| panic!("registration leaf @id missing: {version}"))
        })
        .collect::<Vec<_>>();
    let unique_leaf_ids = leaf_ids
        .iter()
        .copied()
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(unique_leaf_ids.len(), leaf_ids.len(), "{search_modes}");

    for (version, leaf_id) in version_rows.iter().zip(leaf_ids) {
        let leaf_response = client.get(leaf_id).send().await.unwrap();
        assert_eq!(leaf_response.status(), StatusCode::OK, "{version}");
        let leaf = leaf_response.json::<serde_json::Value>().await.unwrap();
        assert_eq!(leaf["@id"], leaf_id, "{leaf}");

        let head = client.head(leaf_id).send().await.unwrap();
        assert_eq!(head.status(), StatusCode::OK, "{version}");
    }

    let invalid_semver_level = search_nuget(
        &client,
        &base,
        &[
            ("q", "Matrix."),
            ("prerelease", "true"),
            ("semVerLevel", "not-a-version"),
        ],
    )
    .await;
    assert_eq!(
        nuget_search_version(&invalid_semver_level, "Matrix.PrereleaseOnly"),
        Some("3.0.0-beta")
    );
    assert_eq!(
        nuget_search_version(&invalid_semver_level, "Matrix.SemVer2Only"),
        None
    );

    for search in [
        &stable_semver1,
        &prerelease_semver1,
        &stable_semver2,
        &all_versions,
        &invalid_semver_level,
    ] {
        assert_eq!(nuget_search_version(search, "Matrix.YankedOnly"), None);
    }

    // SearchAutocompleteService is the same capability surface as SearchQueryService:
    // each flag must make a package id newly discoverable, while unlisted-only ids
    // remain absent in every mode.
    let stable_autocomplete = autocomplete_nuget(&client, &base, &[("q", "Matrix.")]).await;
    assert_eq!(stable_autocomplete["totalHits"], 1, "{stable_autocomplete}");
    assert_eq!(
        nuget_autocomplete_names(&stable_autocomplete),
        ["Matrix.SearchModes"]
    );

    let prerelease_autocomplete =
        autocomplete_nuget(&client, &base, &[("q", "Matrix."), ("prerelease", "true")]).await;
    let prerelease_names = nuget_autocomplete_names(&prerelease_autocomplete);
    assert_eq!(
        prerelease_autocomplete["totalHits"], 2,
        "{prerelease_autocomplete}"
    );
    assert!(prerelease_names.contains(&"Matrix.SearchModes"));
    assert!(prerelease_names.contains(&"Matrix.PrereleaseOnly"));

    let semver2_autocomplete = autocomplete_nuget(
        &client,
        &base,
        &[("q", "Matrix."), ("semVerLevel", "2.0.0")],
    )
    .await;
    let semver2_names = nuget_autocomplete_names(&semver2_autocomplete);
    assert_eq!(
        semver2_autocomplete["totalHits"], 2,
        "{semver2_autocomplete}"
    );
    assert!(semver2_names.contains(&"Matrix.SearchModes"));
    assert!(semver2_names.contains(&"Matrix.SemVer2Only"));

    let all_autocomplete = autocomplete_nuget(
        &client,
        &base,
        &[
            ("q", "Matrix."),
            ("prerelease", "true"),
            ("semVerLevel", "2.0.0"),
        ],
    )
    .await;
    let all_names = nuget_autocomplete_names(&all_autocomplete);
    assert_eq!(all_autocomplete["totalHits"], 3, "{all_autocomplete}");
    for package in [
        "Matrix.SearchModes",
        "Matrix.PrereleaseOnly",
        "Matrix.SemVer2Only",
    ] {
        assert!(all_names.contains(&package), "{all_autocomplete}");
    }
    assert!(
        !all_names.contains(&"Matrix.YankedOnly"),
        "{all_autocomplete}"
    );
}

/// A NuGet client's SemVer level is negotiated over the dependency graph too,
/// not just over the version's own spelling. NuGet calls a package version
/// SemVer 2-specific when the minimum or maximum of any dependency range is —
/// so a perfectly plain `1.0.0` that depends on `[2.0.0-alpha.1, )` must stay
/// invisible to a client that did not ask for SemVer 2. Advertising it hands
/// that client a graph it cannot parse, and the failure surfaces at restore
/// time as an unresolvable version rather than as a package never offered.
#[tokio::test]
async fn nuget_search_reads_the_semver_level_out_of_dependency_ranges_too() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    // Every package here is version `1.0.0`: nothing but the dependency ranges
    // may decide whether a SemVer 1 client gets to see it.
    for (package, dependencies) in [
        (
            "Matrix.Range.DottedPrerelease",
            r#"<group targetFramework="net8.0">
                 <dependency id="Matrix.Stable" version="[1.0.0, 2.0.0)" />
                 <dependency id="Matrix.Preview" version="[2.0.0-alpha.1, )" />
               </group>"#,
        ),
        (
            "Matrix.Range.BuildMetadata",
            r#"<group targetFramework="net8.0">
                 <dependency id="Matrix.Pinned" version="[1.0.0+build.7]" />
               </group>"#,
        ),
        (
            "Matrix.Range.SemVer1",
            r#"<group targetFramework="net8.0">
                 <dependency id="Matrix.Stable" version="13.0.1" />
                 <dependency id="Matrix.Bounded" version="[3.0.0-beta, 4.0.0)" />
               </group>"#,
        ),
        ("Matrix.Range.None", ""),
    ] {
        let nuspec = format!(
            "<package><metadata><id>{package}</id><version>1.0.0</version>\
             <dependencies>{dependencies}</dependencies></metadata></package>"
        );
        let published = client
            .post(package_url(&base, &["nuget", "publish"]))
            .bearer_auth(&token)
            .header(
                reqwest::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{package}.1.0.0.nupkg\""),
            )
            .body(zip_archive(&[("package.nuspec", nuspec.as_bytes())]))
            .send()
            .await
            .unwrap();
        assert_eq!(published.status(), StatusCode::CREATED, "{package}");
    }

    let semver1 = search_nuget(&client, &base, &[("q", "Matrix.Range.")]).await;
    for hidden in [
        "Matrix.Range.DottedPrerelease",
        "Matrix.Range.BuildMetadata",
    ] {
        assert_eq!(
            nuget_search_version(&semver1, hidden),
            None,
            "a SemVer 2 dependency bound must hide {hidden}: {semver1}"
        );
    }
    for visible in ["Matrix.Range.SemVer1", "Matrix.Range.None"] {
        assert_eq!(
            nuget_search_version(&semver1, visible),
            Some("1.0.0"),
            "an ordinary dependency graph must not hide {visible}: {semver1}"
        );
    }
    assert_eq!(semver1["totalHits"], 2, "{semver1}");

    let semver2 = search_nuget(
        &client,
        &base,
        &[("q", "Matrix.Range."), ("semVerLevel", "2.0.0")],
    )
    .await;
    for package in [
        "Matrix.Range.DottedPrerelease",
        "Matrix.Range.BuildMetadata",
        "Matrix.Range.SemVer1",
        "Matrix.Range.None",
    ] {
        assert_eq!(
            nuget_search_version(&semver2, package),
            Some("1.0.0"),
            "a SemVer 2 client sees every version: {semver2}"
        );
    }
    assert_eq!(semver2["totalHits"], 4, "{semver2}");

    // The autocomplete service negotiates the same capability, so an id hidden
    // from one surface cannot stay discoverable through the other.
    let autocomplete_semver1 = autocomplete_nuget(&client, &base, &[("q", "Matrix.Range.")]).await;
    let semver1_names = nuget_autocomplete_names(&autocomplete_semver1);
    assert!(
        !semver1_names.contains(&"Matrix.Range.DottedPrerelease"),
        "{autocomplete_semver1}"
    );
    assert!(
        semver1_names.contains(&"Matrix.Range.SemVer1"),
        "{autocomplete_semver1}"
    );
    let autocomplete_semver2 = autocomplete_nuget(
        &client,
        &base,
        &[("q", "Matrix.Range."), ("semVerLevel", "2.0.0")],
    )
    .await;
    assert!(
        nuget_autocomplete_names(&autocomplete_semver2).contains(&"Matrix.Range.DottedPrerelease"),
        "{autocomplete_semver2}"
    );

    // The registration index is not a capability-negotiation surface: hiding
    // the version from search must not have hidden the graph search read it
    // out of.
    let registration = client
        .get(package_url(
            &base,
            &[
                "nuget",
                "registration",
                "matrix.range.dottedprerelease",
                "index.json",
            ],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(registration.status(), StatusCode::OK);
    let registration = registration.json::<serde_json::Value>().await.unwrap();
    let ranges = registration["items"][0]["items"][0]["catalogEntry"]["dependencyGroups"][0]
        ["dependencies"]
        .as_array()
        .unwrap_or_else(|| panic!("registration dependency graph missing: {registration}"))
        .iter()
        .map(|dependency| dependency["range"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert!(ranges.contains(&"[2.0.0-alpha.1, )"), "{registration}");

    // A row whose stored metadata cannot be read has no known SemVer level.
    // Answering "SemVer 1" for it is the one answer that advertises it to the
    // client least able to cope, so the request fails loudly instead — and only
    // the request whose answer that graph could have changed.
    let corrupted = db
        .execute_unprepared(
            "UPDATE package_versions SET metadata = '{broken-json' WHERE id IN (\
                 SELECT pv.id FROM package_versions pv JOIN packages p ON p.id = pv.package_id \
                 WHERE p.name = 'Matrix.Range.SemVer1')",
        )
        .await
        .unwrap();
    assert_eq!(corrupted.rows_affected(), 1);

    let mut damaged_query = package_url(&base, &["nuget", "query"]);
    damaged_query
        .query_pairs_mut()
        .append_pair("q", "Matrix.Range.");
    let damaged = client.get(damaged_query).send().await.unwrap();
    assert_eq!(
        damaged.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "an unreadable dependency graph must not be classified as SemVer 1"
    );
    let damaged_body = damaged.text().await.unwrap();
    assert!(
        !damaged_body.contains("broken-json"),
        "the unreadable stored blob leaked to the client: {damaged_body}"
    );

    // The same damaged row cannot fail a SemVer 2 request: that client sees
    // every version regardless, so its answer never depended on the graph.
    let still_served = search_nuget(
        &client,
        &base,
        &[("q", "Matrix.Range."), ("semVerLevel", "2.0.0")],
    )
    .await;
    assert_eq!(still_served["totalHits"], 4, "{still_served}");
}

/// Maven's metadata model calls the last publication `latest`, while `release`
/// is the last non-snapshot publication. It also requires a compact UTC update
/// timestamp, not the service's RFC 3339 representation.
#[tokio::test]
async fn maven_metadata_distinguishes_latest_release_and_formats_last_updated() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    for version in ["2.0.0", "1.2.4", "3.0-SNAPSHOT"] {
        let pom = format!(
            "<project><groupId>com.example</groupId><artifactId>matrix-order</artifactId><version>{version}</version></project>"
        );
        let response = client
            .post(package_url(&base, &["maven", "publish"]))
            .bearer_auth(&token)
            .header(
                reqwest::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"matrix-order-{version}.pom\""),
            )
            .body(pom)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED, "{version}");
    }

    let xml = client
        .get(package_url(
            &base,
            &["maven", "com.example", "matrix-order", "maven-metadata.xml"],
        ))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(xml.contains("<latest>3.0-SNAPSHOT</latest>"), "{xml}");
    assert!(xml.contains("<release>1.2.4</release>"), "{xml}");
    let updated = xml
        .split_once("<lastUpdated>")
        .and_then(|(_, tail)| tail.split_once("</lastUpdated>"))
        .map(|(value, _)| value)
        .unwrap_or_else(|| panic!("lastUpdated missing: {xml}"));
    assert_eq!(updated.len(), 14, "{xml}");
    assert!(updated.bytes().all(|byte| byte.is_ascii_digit()), "{xml}");
}

#[tokio::test]
async fn yanked_only_packages_do_not_advertise_a_fake_latest_version() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let indexed_types: std::collections::HashSet<&str> = protocol_index_cases()
        .iter()
        .map(|case| case.package_type)
        .collect();
    let cases = package_cases();

    for case in cases
        .iter()
        .filter(|case| indexed_types.contains(case.package_type))
    {
        let published = client
            .post(package_url(&base, &[case.package_type, "publish"]))
            .bearer_auth(&token)
            .header(
                reqwest::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", case.filename),
            )
            .body(case.body.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(
            published.status(),
            StatusCode::CREATED,
            "{} publish: {}",
            case.package_type,
            published.text().await.unwrap()
        );
    }

    for case in cases
        .iter()
        .filter(|case| indexed_types.contains(case.package_type))
    {
        let yanked = client
            .patch(package_url(
                &base,
                &[case.package_type, case.name, case.version, "yank"],
            ))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "yank": true }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            yanked.status(),
            StatusCode::OK,
            "{} yank: {}",
            case.package_type,
            yanked.text().await.unwrap()
        );
    }

    for check in protocol_index_cases() {
        let response = client
            .get(package_url(&base, check.segments))
            .send()
            .await
            .unwrap();
        let expected_status = match &check.yanked {
            YankedIndexEncoding::Marked { .. } => StatusCode::OK,
            YankedIndexEncoding::Omitted { status, .. } => *status,
        };
        assert_eq!(response.status(), expected_status, "{:?}", check.segments);
        let body = response.text().await.unwrap();
        match check.yanked {
            YankedIndexEncoding::Marked { required } => {
                for marker in required {
                    assert!(
                        body.contains(marker),
                        "yanked {} is not marked by {marker:?}: {body}",
                        check.package_type
                    );
                }
            }
            YankedIndexEncoding::Omitted { forbidden, .. } => {
                for marker in forbidden {
                    assert!(
                        !body.contains(marker),
                        "yanked {} is still a candidate via {marker:?}: {body}",
                        check.package_type
                    );
                }
            }
        }
    }

    let npm = client
        .get(package_url(&base, &["npm", "matrix-npm"]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert!(
        npm.get("dist-tags").is_none(),
        "all-yanked npm metadata must not publish dist-tags: {npm}"
    );
    assert!(npm["versions"].get("1.0.0").is_none(), "{npm}");
    assert!(
        !npm.to_string().contains("0.0.0"),
        "npm metadata must not invent a version: {npm}"
    );

    let listed = client
        .get(package_url(&base, &["nuget", "list"]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let package = listed["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|package| package["name"] == "Matrix.NuGet")
        .unwrap_or_else(|| panic!("NuGet package missing from list: {listed}"));
    assert!(
        package["latest_version"].is_null(),
        "a package with no live versions has no latest_version: {listed}"
    );

    let mut query = package_url(&base, &["nuget", "query"]);
    query.query_pairs_mut().append_pair("q", "Matrix.NuGet");
    let search = client
        .get(query)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(search["totalHits"], 0, "{search}");
    assert_eq!(search["data"].as_array().unwrap().len(), 0, "{search}");
    assert!(
        !search.to_string().contains("0.0.0"),
        "NuGet search must not invent a version: {search}"
    );

    let helm_index = client
        .get(package_url(&base, &["helm", "index.yaml"]))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        !helm_index.contains("matrix-helm"),
        "all-yanked Helm chart must not be advertised in index.yaml: {helm_index}"
    );
    assert!(
        !helm_index.contains("1.0.0"),
        "all-yanked Helm chart version must not be advertised in index.yaml: {helm_index}"
    );
}

/// Maven asks for an artifact at the path it builds from the coordinate, and a
/// `groupId` becomes one directory per dot: `com.example.tools:matrix-deep` is
/// fetched from `com/example/tools/matrix-deep/…`, never from
/// `com.example.tools/matrix-deep/…`. This walks exactly the URLs `mvn` and
/// Gradle send — the flat spelling the rest of the suite uses is our own, and a
/// registry that only answers that one is unreachable from a build tool.
#[tokio::test]
async fn maven_repository_layout_serves_metadata_and_artifacts() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    // A three-segment project groupId, so the test fails both when the layout
    // treats it as one path segment and when the POM parser takes the earlier
    // parent coordinate instead of the project's direct children.
    let pom = br#"<?xml version="1.0"?>
<project>
  <parent>
    <groupId>org.parent</groupId>
    <artifactId>parent-bom</artifactId>
    <version>9.8.7</version>
  </parent>
  <groupId>com.example.tools</groupId>
  <artifactId>matrix-deep</artifactId>
  <version>1.0.0</version>
</project>"#
        .to_vec();
    let jar = zip_archive(&[("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n")]);

    let publish = |filename: &'static str,
                   body: Vec<u8>,
                   coordinates: Option<(&'static str, &'static str)>| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        async move {
            let mut url = package_url(&base, &["maven", "publish"]);
            if let Some((name, version)) = coordinates {
                url.query_pairs_mut()
                    .append_pair("name", name)
                    .append_pair("version", version);
            }
            client
                .post(url)
                .bearer_auth(&token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{filename}\""),
                )
                .body(body)
                .send()
                .await
                .unwrap()
        }
    };

    // The POM carries its own coordinate; the JAR is deployed under the same
    // one as a second version, the way a build tool publishes a release.
    let published = publish("matrix-deep-1.0.0.pom", pom.clone(), None).await;
    assert_eq!(published.status(), StatusCode::CREATED);
    let published = publish(
        "matrix-deep-2.0.0.jar",
        jar.clone(),
        Some(("com.example.tools:matrix-deep", "2.0.0")),
    )
    .await;
    assert_eq!(published.status(), StatusCode::CREATED);

    // ── The two URLs a Maven client actually sends ──────────────────────────
    let metadata = client
        .get(package_url(
            &base,
            &[
                "maven",
                "com",
                "example",
                "tools",
                "matrix-deep",
                "maven-metadata.xml",
            ],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK, "layout metadata");
    let metadata = metadata.text().await.unwrap();
    assert!(
        metadata.contains("<groupId>com.example.tools</groupId>")
            && metadata.contains("<artifactId>matrix-deep</artifactId>")
            && metadata.contains("<version>1.0.0</version>")
            && metadata.contains("<version>2.0.0</version>"),
        "layout metadata lost the coordinate or the versions: {metadata}"
    );

    for (version, filename, expected) in [
        ("1.0.0", "matrix-deep-1.0.0.pom", &pom),
        ("2.0.0", "matrix-deep-2.0.0.jar", &jar),
    ] {
        let response = client
            .get(package_url(
                &base,
                &[
                    "maven",
                    "com",
                    "example",
                    "tools",
                    "matrix-deep",
                    version,
                    filename,
                ],
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "layout download {filename}"
        );
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            expected.as_slice()
        );
    }

    // A coordinate nobody published is a miss, not a stray 200.
    let missing = client
        .get(package_url(
            &base,
            &[
                "maven",
                "com",
                "example",
                "tools",
                "matrix-deep",
                "9.9.9",
                "matrix-deep-9.9.9.jar",
            ],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // ── The routes the layout ones sit on top of still answer ───────────────
    // The flat spelling ForgeKeep's own API and UI use: one segment that
    // already carries the dots resolves to the same package.
    let flat = client
        .get(package_url(
            &base,
            &[
                "maven",
                "com.example.tools",
                "matrix-deep",
                "maven-metadata.xml",
            ],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(flat.status(), StatusCode::OK, "flat metadata");
    assert!(flat
        .text()
        .await
        .unwrap()
        .contains("<version>1.0.0</version>"));

    // The generic package API lives one level up under `{pkg_type}`; a Maven
    // route that swallowed it would leave these `404`/`405`.
    let listed = client
        .get(package_url(&base, &["maven", "list"]))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK, "generic list");
    let listed = listed.json::<serde_json::Value>().await.unwrap();
    assert!(listed["packages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|package| package["name"] == "com.example.tools:matrix-deep"));

    let canonical = client
        .get(package_url(
            &base,
            &[
                "maven",
                "com.example.tools:matrix-deep",
                "1.0.0",
                "matrix-deep-1.0.0.pom",
            ],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(canonical.status(), StatusCode::OK, "generic download");
    assert_eq!(canonical.bytes().await.unwrap().as_ref(), pom.as_slice());
}

/// A version is not uploaded in one request. `mvn deploy` sends the POM, then
/// the JAR, then the sources, each as its own `PUT`; PyPI puts an sdist beside
/// a wheel. Every request after the first used to be answered `200 OK` with its
/// payload dropped on the floor — the file reached neither storage nor the
/// database, and the client was told nothing.
#[tokio::test]
async fn a_second_file_published_into_one_version_is_kept_and_a_repeat_is_refused() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let pom = br#"<?xml version="1.0"?>
<project><groupId>com.example</groupId><artifactId>matrix-multi</artifactId><version>1.0.0</version></project>"#
        .to_vec();
    let jar = zip_archive(&[("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n")]);
    let sources = zip_archive(&[("Main.java", b"class Main {}\n")]);

    let publish = |filename: &'static str, body: Vec<u8>| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        async move {
            let mut url = package_url(&base, &["maven", "publish"]);
            url.query_pairs_mut()
                .append_pair("name", "com.example:matrix-multi")
                .append_pair("version", "1.0.0");
            client
                .post(url)
                .bearer_auth(&token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{filename}\""),
                )
                .body(body)
                .send()
                .await
                .unwrap()
        }
    };

    // The first request creates the version; the next two add to it.
    let created = publish("matrix-multi-1.0.0.pom", pom.clone()).await;
    assert_eq!(created.status(), StatusCode::CREATED);

    for (filename, body) in [
        ("matrix-multi-1.0.0.jar", &jar),
        ("matrix-multi-1.0.0-sources.jar", &sources),
    ] {
        let added = publish(filename, body.clone()).await;
        assert_eq!(added.status(), StatusCode::OK, "adding {filename}");
        let added = added.json::<serde_json::Value>().await.unwrap();
        assert_eq!(added["existing"], true, "adding {filename}");
    }

    // All three are in the version, and all three come back byte for byte.
    let version = client
        .get(package_url(
            &base,
            &["maven", "com.example:matrix-multi", "1.0.0"],
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let files = version["files"].as_array().unwrap();
    assert_eq!(files.len(), 3, "version files: {version}");

    for (filename, expected) in [
        ("matrix-multi-1.0.0.pom", &pom),
        ("matrix-multi-1.0.0.jar", &jar),
        ("matrix-multi-1.0.0-sources.jar", &sources),
    ] {
        let downloaded = client
            .get(package_url(
                &base,
                &["maven", "com", "example", "matrix-multi", "1.0.0", filename],
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(downloaded.status(), StatusCode::OK, "download {filename}");
        assert_eq!(
            downloaded.bytes().await.unwrap().as_ref(),
            expected.as_slice(),
            "download {filename}"
        );
    }

    // The size the registry reports is the total of what the version holds, not
    // of the request that happened to create it.
    let total = (pom.len() + jar.len() + sources.len()) as u64;
    assert_eq!(version["size"].as_u64(), Some(total), "version size");

    // Re-sending a filename the version already carries is a conflict, not a
    // silent overwrite of an artifact others have already resolved.
    let repeat = publish("matrix-multi-1.0.0.jar", jar.clone()).await;
    assert_eq!(repeat.status(), StatusCode::CONFLICT);
}

/// Cargo reads a sparse registry the way RFC 2789 spells it: `config.json`
/// first, and then a crate at the path its name expands to — `matrix-cargo`
/// lives at `ma/tr/matrix-cargo`, never at the bare name. This walks those
/// URLs; the flat spelling the rest of the suite uses is ForgeKeep's own, and a
/// registry that answers only that one is unreachable from `cargo`.
#[tokio::test]
async fn cargo_sparse_index_serves_the_layout_cargo_requests() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let cargo_toml = br#"[package]
name = "matrix-cargo"
version = "1.0.0"
"#;
    let crate_file = tar_gz(&[("matrix-cargo-1.0.0/Cargo.toml", cargo_toml)]);

    let published = client
        .post(package_url(&base, &["cargo", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-cargo-1.0.0.crate\"",
        )
        .body(crate_file.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    // ── `config.json`, the request cargo makes before any crate ─────────────
    let config = client
        .get(package_url(&base, &["cargo", "index", "config.json"]))
        .send()
        .await
        .unwrap();
    assert_eq!(config.status(), StatusCode::OK, "sparse index config.json");
    let config = config.json::<serde_json::Value>().await.unwrap();
    let dl = config["dl"]
        .as_str()
        .unwrap_or_else(|| panic!("config.json carries no `dl`: {config}"));

    // Cargo substitutes the markers itself; the URL that comes out has to be a
    // route this server serves, or every `cargo build` ends in a 404.
    let download_url = dl
        .replace("{crate}", "matrix-cargo")
        .replace("{version}", "1.0.0");
    let downloaded = client.get(&download_url).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK, "dl url {download_url}");
    assert_eq!(
        downloaded.bytes().await.unwrap().as_ref(),
        crate_file.as_slice()
    );

    // ── The crate entry, at the prefix the name expands to ──────────────────
    let index = client
        .get(package_url(
            &base,
            &["cargo", "index", "ma", "tr", "matrix-cargo"],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK, "prefixed index");
    let index = index.text().await.unwrap();
    assert!(
        index.contains("\"name\":\"matrix-cargo\"") && index.contains("\"vers\":\"1.0.0\""),
        "prefixed index lost the crate or the version: {index}"
    );

    // A prefix that does not spell the name out is a miss. Without the check
    // the index would answer any crate under any path and stop being an index.
    let wrong = client
        .get(package_url(
            &base,
            &["cargo", "index", "zz", "zz", "matrix-cargo"],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::NOT_FOUND, "unrelated prefix");

    // The other shapes of the layout are routed too — an unpublished crate is a
    // 404 from the handler, whereas an unregistered shape falls through to the
    // SPA fallback and answers `200` with HTML.
    for segments in [
        vec!["cargo", "index", "1", "a"],
        vec!["cargo", "index", "2", "ab"],
        vec!["cargo", "index", "3", "a", "abc"],
    ] {
        let response = client
            .get(package_url(&base, &segments))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{segments:?}");
        let body = response.text().await.unwrap();
        assert!(
            !body.contains("<html"),
            "{segments:?} reached the SPA fallback: {body}"
        );
    }

    // ── The routes the layout ones sit on top of still answer ───────────────
    let flat = client
        .get(package_url(&base, &["cargo", "index", "matrix-cargo"]))
        .send()
        .await
        .unwrap();
    assert_eq!(flat.status(), StatusCode::OK, "flat index");
    assert!(flat.text().await.unwrap().contains("\"vers\":\"1.0.0\""));

    let listed = client
        .get(package_url(&base, &["cargo", "list"]))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK, "generic list");
    let listed = listed.json::<serde_json::Value>().await.unwrap();
    assert!(listed["packages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|package| package["name"] == "matrix-cargo"));
}

/// Cargo resolves against the index, never against the `.crate` file, so the
/// dependencies and features of a published crate have to survive the trip out
/// of its manifest and into the index line. Served without them, the entry does
/// not say "unknown" — it says the crate needs nothing, which resolves cleanly
/// and only fails much later at `unresolved import`.
#[tokio::test]
async fn cargo_index_carries_the_manifest_dependencies_and_features() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let cargo_toml = br#"[package]
name = "matrix-deps-crate"
version = "1.0.0"
links = "openssl"
rust-version = "1.70"

[dependencies]
serde = { version = "1.0", features = ["derive"], default-features = false }
rand = { version = "0.8", optional = true }

[dev-dependencies]
tempfile = "3"

[target.'cfg(unix)'.dependencies]
nix = "0.27"

[features]
default = ["std"]
std = []
fast = ["dep:rand"]
"#;
    let crate_file = tar_gz(&[("matrix-deps-crate-1.0.0/Cargo.toml", cargo_toml)]);

    let published = client
        .post(package_url(&base, &["cargo", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-deps-crate-1.0.0.crate\"",
        )
        .body(crate_file)
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let index = client
        .get(package_url(
            &base,
            &["cargo", "index", "ma", "tr", "matrix-deps-crate"],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    let body = index.text().await.unwrap();
    let line = body
        .lines()
        .find(|line| line.contains("\"vers\":\"1.0.0\""))
        .unwrap_or_else(|| panic!("index lost the version: {body}"));
    let entry: serde_json::Value = serde_json::from_str(line).unwrap();

    let deps = entry["deps"].as_array().unwrap();
    let dep = |name: &str| {
        deps.iter()
            .find(|d| d["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing from the index entry: {entry}"))
    };

    // The whole RFC 2789 dependency shape, not just the name.
    assert_eq!(
        *dep("serde"),
        serde_json::json!({
            "name": "serde",
            "req": "1.0",
            "features": ["derive"],
            "optional": false,
            "default_features": false,
            "target": null,
            "kind": "normal",
            "registry": null,
            "package": null,
        }),
        "index entry: {entry}"
    );
    assert_eq!(dep("rand")["optional"], true);
    assert_eq!(dep("tempfile")["kind"], "dev");
    assert_eq!(dep("nix")["target"], "cfg(unix)");

    // `--features` is checked against this map; without it every feature the
    // crate really has comes back as "unknown feature".
    assert_eq!(
        entry["features"],
        serde_json::json!({ "default": ["std"], "std": [] }),
        "index entry: {entry}"
    );
    // `dep:` syntax is legible only under schema 2, so it travels separately.
    assert_eq!(
        entry["features2"],
        serde_json::json!({ "fast": ["dep:rand"] })
    );
    assert_eq!(entry["v"], 2);

    assert_eq!(entry["links"], "openssl");
    assert_eq!(entry["rust_version"], "1.70");
}

/// npm's resolver reads the abbreviated document and nothing else — it never
/// opens a tarball to find out what a package needs. A version object without
/// `dependencies` therefore does not say "unknown", it says the package depends
/// on nothing: `npm install` succeeds and leaves a package in `node_modules`
/// that cannot run.
#[tokio::test]
async fn npm_metadata_carries_the_manifest_dependencies() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let package_json = br#"{
  "name": "matrix-deps-npm",
  "version": "1.0.0",
  "description": "npm matrix package with dependencies",
  "dependencies": { "left-pad": "^1.3.0" },
  "devDependencies": { "jest": "^29.0.0" },
  "peerDependencies": { "react": ">=17" },
  "peerDependenciesMeta": { "react": { "optional": true } },
  "optionalDependencies": { "fsevents": "^2.3.0" },
  "bin": { "matrix": "./cli.js" },
  "engines": { "node": ">=18" },
  "scripts": { "postinstall": "node build.js" }
}"#;
    let tarball = tar_gz(&[("package/package.json", package_json)]);

    let published = client
        .post(package_url(&base, &["npm", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-deps-npm-1.0.0.tgz\"",
        )
        .body(tarball)
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let metadata = client
        .get(package_url(&base, &["npm", "matrix-deps-npm"]))
        .send()
        .await
        .unwrap();
    assert_eq!(metadata.status(), StatusCode::OK);
    let document: serde_json::Value = metadata.json().await.unwrap();
    let version = &document["versions"]["1.0.0"];

    assert_eq!(
        version["dependencies"],
        serde_json::json!({ "left-pad": "^1.3.0" }),
        "version object: {version}"
    );
    assert_eq!(
        version["devDependencies"],
        serde_json::json!({ "jest": "^29.0.0" })
    );
    // Peers decide whether an install warns or errors; the meta table is what
    // marks one as optional rather than missing.
    assert_eq!(
        version["peerDependencies"],
        serde_json::json!({ "react": ">=17" })
    );
    assert_eq!(
        version["peerDependenciesMeta"],
        serde_json::json!({ "react": { "optional": true } })
    );
    assert_eq!(
        version["optionalDependencies"],
        serde_json::json!({ "fsevents": "^2.3.0" })
    );
    // `bin` is what puts a command on PATH, `engines` what lets a client refuse
    // a runtime it cannot satisfy, `hasInstallScript` what tells npm the
    // package has a build step at all.
    assert_eq!(version["bin"], serde_json::json!({ "matrix": "./cli.js" }));
    assert_eq!(version["engines"], serde_json::json!({ "node": ">=18" }));
    assert_eq!(version["hasInstallScript"], true);

    // The registry's own answer about its own storage is unchanged by any of it.
    assert_eq!(document["dist-tags"]["latest"], "1.0.0");
    assert!(
        version["dist"]["tarball"]
            .as_str()
            .unwrap()
            .ends_with("/packages/npm/matrix-deps-npm/1.0.0/matrix-deps-npm-1.0.0.tgz"),
        "version object: {version}"
    );
}

/// `npm publish` does not POST a tarball to ForgeKeep's generic upload route.
/// It PUTs a CouchDB-shaped packument to the package URL, with the tarball in a
/// base64 `_attachments` entry. The scoped spelling matters twice: npm escapes
/// the slash in the request URL but retains it in the attachment key.
#[tokio::test]
async fn npm_put_packument_publishes_normal_and_scoped_tarballs() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "npm-put-owner", "npm-put-owner@example.com").await;
    create_repo(&base, &token, "npm-put-repo").await;
    let client = reqwest::Client::new();

    for (name, encoded_name) in [
        ("matrix-put-npm", "matrix-put-npm"),
        (
            "@matrix-scope/scoped-put-npm",
            "@matrix-scope%2Fscoped-put-npm",
        ),
    ] {
        let version = "1.0.0";
        let package_json = format!(
            r#"{{"name":"{name}","version":"{version}","description":"published by npm PUT"}}"#
        );
        let tarball = tar_gz(&[("package/package.json", package_json.as_bytes())]);
        let attachment_name = format!("{name}-{version}.tgz");
        let publish_url = format!(
            "{}/api/v1/repos/npm-put-owner/npm-put-repo/packages/npm/{encoded_name}",
            base.trim_end_matches('/')
        );
        let packument = serde_json::json!({
            "_id": name,
            "name": name,
            "dist-tags": { "latest": version },
            "versions": {
                version: {
                    "name": name,
                    "version": version,
                    "description": "published by npm PUT"
                }
            },
            "_attachments": {
                attachment_name: {
                    "content_type": "application/octet-stream",
                    "data": base64::engine::general_purpose::STANDARD.encode(&tarball),
                    "length": tarball.len()
                }
            }
        });

        let published = client
            .put(&publish_url)
            .bearer_auth(&token)
            .json(&packument)
            .send()
            .await
            .unwrap();
        assert_eq!(
            published.status(),
            StatusCode::CREATED,
            "npm PUT failed for {name}: {}",
            published.text().await.unwrap()
        );

        let document: serde_json::Value = client
            .get(&publish_url)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let tarball_url = document["versions"][version]["dist"]["tarball"]
            .as_str()
            .unwrap_or_else(|| panic!("no dist.tarball for {name}: {document}"));
        let downloaded = client.get(tarball_url).send().await.unwrap();
        assert_eq!(downloaded.status(), StatusCode::OK, "{tarball_url}");
        assert_eq!(downloaded.bytes().await.unwrap().to_vec(), tarball);

        // npm versions are immutable. The deterministic attachment name makes
        // this the package-file conflict path, not Maven's add-another-file
        // path for an existing version.
        let repeated = client
            .put(&publish_url)
            .bearer_auth(&token)
            .json(&packument)
            .send()
            .await
            .unwrap();
        assert_eq!(repeated.status(), StatusCode::CONFLICT, "{name}");
    }
}

/// npm publishes provenance as a second raw-JSON attachment. The registry must
/// keep it atomically with the tarball, advertise npm's standard discovery URL,
/// and reject a DSSE statement whose SHA-512 names different bytes.
#[tokio::test]
async fn npm_provenance_attachment_round_trips_and_foreign_digest_is_atomic() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(
        &base,
        "npm-provenance-owner",
        "npm-provenance-owner@example.com",
    )
    .await;
    create_repo(&base, &token, "npm-provenance-repo").await;
    let client = reqwest::Client::new();
    let name = "matrix-provenance-npm";
    let publish_url = format!(
        "{}/api/v1/repos/npm-provenance-owner/npm-provenance-repo/packages/npm/{name}",
        base.trim_end_matches('/')
    );

    let publish_document = |version: &str, tarball: &[u8], bundle: &serde_json::Value| {
        let bundle_data = bundle.to_string();
        let bundle_len = bundle_data.encode_utf16().count();
        let tarball_filename = format!("{name}-{version}.tgz");
        let provenance_filename = format!("{name}-{version}.sigstore");
        serde_json::json!({
            "_id": name,
            "name": name,
            "dist-tags": { "latest": version },
            "versions": { version: { "name": name, "version": version } },
            "_attachments": {
                tarball_filename: {
                    "content_type": "application/octet-stream",
                    "data": base64::engine::general_purpose::STANDARD.encode(tarball),
                    "length": tarball.len()
                },
                provenance_filename: {
                    "content_type": "application/vnd.dev.sigstore.bundle.v0.3+json",
                    "data": bundle_data,
                    "length": bundle_len
                }
            }
        })
    };

    let version = "1.0.0";
    let manifest = format!(r#"{{"name":"{name}","version":"{version}"}}"#);
    let tarball = tar_gz(&[("package/package.json", manifest.as_bytes())]);
    let tarball_sha512 = hex::encode(sha2::Sha512::digest(&tarball));
    let bundle = npm_provenance_bundle(name, version, &tarball_sha512);
    let published = client
        .put(&publish_url)
        .bearer_auth(&token)
        .json(&publish_document(version, &tarball, &bundle))
        .send()
        .await
        .unwrap();
    assert_eq!(
        published.status(),
        StatusCode::CREATED,
        "{}",
        published.text().await.unwrap()
    );

    let packument: serde_json::Value = client
        .get(&publish_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let dist = &packument["versions"][version]["dist"];
    let attestation_url = dist["attestations"]["url"]
        .as_str()
        .unwrap_or_else(|| panic!("no dist.attestations URL: {packument}"));
    assert_eq!(
        dist["attestations"]["provenance"]["predicateType"],
        "https://slsa.dev/provenance/v1"
    );
    assert!(
        attestation_url.contains("/-/npm/v1/attestations/"),
        "non-standard npm attestation URL: {attestation_url}"
    );
    let served: serde_json::Value = client
        .get(attestation_url)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(served["attestations"][0]["bundle"], bundle);
    assert_eq!(
        served["attestations"][0]["predicateType"],
        "https://slsa.dev/provenance/v1"
    );

    let foreign_version = "2.0.0";
    let foreign_manifest = format!(r#"{{"name":"{name}","version":"{foreign_version}"}}"#);
    let foreign_tarball = tar_gz(&[("package/package.json", foreign_manifest.as_bytes())]);
    let foreign_bundle = npm_provenance_bundle(
        name,
        foreign_version,
        &hex::encode(sha2::Sha512::digest(b"different tarball")),
    );
    let refused = client
        .put(&publish_url)
        .bearer_auth(&token)
        .json(&publish_document(
            foreign_version,
            &foreign_tarball,
            &foreign_bundle,
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(
        refused
            .text()
            .await
            .unwrap()
            .contains("SHA-512 does not match"),
        "foreign provenance was not diagnosed"
    );

    let after_refusal: serde_json::Value = client
        .get(&publish_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        after_refusal["versions"].get(foreign_version).is_none(),
        "refused provenance left a partial package version: {after_refusal}"
    );
}

/// Dist-tags are mutable selectors, while the version rows they point at are
/// immutable. Publishing a prerelease under `beta` must preserve the prior
/// `latest`, and the standalone `npm dist-tag` protocol must mutate the same
/// canonical map the packument exposes.
#[tokio::test]
async fn npm_named_dist_tags_survive_publish_and_protocol_mutation() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "npm-tag-owner", "npm-tag-owner@example.com").await;
    create_repo(&base, &token, "npm-tag-repo").await;
    let client = reqwest::Client::new();
    let name = "matrix-tag-npm";
    let publish_url = format!(
        "{}/api/v1/repos/npm-tag-owner/npm-tag-repo/packages/npm/{name}",
        base.trim_end_matches('/')
    );

    let mut beta_tarball = Vec::new();
    for (version, tag) in [("1.0.0", "latest"), ("2.0.0-beta.1", "beta")] {
        let package_json =
            format!(r#"{{"name":"{name}","version":"{version}","channel":"{tag}"}}"#);
        let tarball = tar_gz(&[("package/package.json", package_json.as_bytes())]);
        if tag == "beta" {
            beta_tarball = tarball.clone();
        }
        let attachment_name = format!("{name}-{version}.tgz");
        let packument = serde_json::json!({
            "_id": name,
            "name": name,
            "dist-tags": { tag: version },
            "versions": { version: { "name": name, "version": version } },
            "_attachments": {
                attachment_name: {
                    "data": base64::engine::general_purpose::STANDARD.encode(&tarball),
                    "length": tarball.len()
                }
            }
        });
        let response = client
            .put(&publish_url)
            .bearer_auth(&token)
            .json(&packument)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::CREATED,
            "publish {version} under {tag}: {}",
            response.text().await.unwrap()
        );
    }

    let document: serde_json::Value = client
        .get(&publish_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(document["dist-tags"]["latest"], "1.0.0", "{document}");
    assert_eq!(document["dist-tags"]["beta"], "2.0.0-beta.1", "{document}");
    let beta_url = document["versions"]["2.0.0-beta.1"]["dist"]["tarball"]
        .as_str()
        .expect("beta tarball URL");
    let downloaded = client.get(beta_url).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), beta_tarball);

    let tags_url = format!(
        "{}/api/v1/repos/npm-tag-owner/npm-tag-repo/packages/npm/-/package/{name}/dist-tags",
        base.trim_end_matches('/')
    );
    let stable_url = format!("{tags_url}/stable");
    let set = client
        .put(&stable_url)
        .bearer_auth(&token)
        .json("1.0.0")
        .send()
        .await
        .unwrap();
    assert_eq!(set.status(), StatusCode::OK);
    let tags: serde_json::Value = client
        .get(&tags_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tags["stable"], "1.0.0", "{tags}");
    assert_eq!(tags["beta"], "2.0.0-beta.1", "{tags}");

    let removed = client
        .delete(&stable_url)
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::OK);
    let tags: serde_json::Value = client
        .get(&tags_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(tags.get("stable").is_none(), "{tags}");
    assert_eq!(tags["latest"], "1.0.0", "{tags}");
}

/// A scoped name (`@scope/name`) carries a literal slash, and `dist.tarball` is
/// the *only* address npm ever downloads from — it never rebuilds the path from
/// the package name itself. Pasted raw into the URL, that slash turns the
/// four-segment download route into six: the router reads `pkg_name=@scope`,
/// `version=name`, and answers the registry's own published link with a 404,
/// while the metadata request right before it succeeded (npm sends the scoped
/// name percent-encoded, and axum hands the handler the stored spelling back).
///
/// So the assertion has to *follow* the published URL rather than assert a
/// package exists: the failure only shows up between the two.
#[tokio::test]
async fn a_scoped_npm_package_downloads_from_the_url_it_publishes() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let package_json = br#"{
  "name": "@matrix-scope/scoped-npm",
  "version": "1.0.0",
  "description": "scoped npm matrix package"
}"#;
    let tarball = tar_gz(&[("package/package.json", package_json)]);

    let published = client
        .post(package_url(&base, &["npm", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-scope-scoped-npm-1.0.0.tgz\"",
        )
        .body(tarball.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    // Spelled the way npm itself spells a scoped packument request: the slash
    // percent-encoded so the name stays one route segment.
    let metadata_url = format!(
        "{}/api/v1/repos/matrix-owner/matrix-repo/packages/npm/@matrix-scope%2Fscoped-npm",
        base.trim_end_matches('/')
    );
    let metadata = client.get(&metadata_url).send().await.unwrap();
    assert_eq!(
        metadata.status(),
        StatusCode::OK,
        "scoped packument must resolve at {metadata_url}"
    );
    let document: serde_json::Value = metadata.json().await.unwrap();
    assert_eq!(document["name"], "@matrix-scope/scoped-npm", "{document}");

    let tarball_url = document["versions"]["1.0.0"]["dist"]["tarball"]
        .as_str()
        .unwrap_or_else(|| panic!("no dist.tarball in {document}"));
    assert!(
        tarball_url.ends_with(
            "/packages/npm/%40matrix-scope%2Fscoped-npm/1.0.0/matrix-scope-scoped-npm-1.0.0.tgz"
        ),
        "the scope separator must not become a path separator: {tarball_url}"
    );

    // The bytes, followed from the link the registry published.
    let downloaded = client.get(tarball_url).send().await.unwrap();
    assert_eq!(
        downloaded.status(),
        StatusCode::OK,
        "dist.tarball must be a path this server routes: {tarball_url}"
    );
    assert_eq!(
        downloaded.bytes().await.unwrap().to_vec(),
        tarball,
        "the tarball served is the one published"
    );
}

/// The `dist` block is a promise about bytes, and both of its checksum fields
/// name the algorithm they are in: `shasum` is SHA-1 (pacote feeds it to ssri
/// as `sha1-<base64>`, Composer runs `hash_file('sha1')` on the archive) and
/// `integrity` is an SRI string over the raw digest bytes. A SHA-256 published
/// under either name is not a stronger answer but a failing one — `npm install`
/// aborts with `EINTEGRITY` after resolving the whole tree, and `composer
/// install` throws "The checksum verification of the file failed".
///
/// So the assertion is not "some digest is present" but "every digest the
/// metadata publishes is that algorithm, over exactly the bytes the download
/// route serves".
#[tokio::test]
async fn npm_and_composer_publish_each_checksum_under_its_own_algorithm() {
    use base64::Engine as _;
    use sha1::Digest as _;

    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    // ── npm ────────────────────────────────────────────────────────────────
    let tarball = tar_gz(&[(
        "package/package.json",
        br#"{ "name": "matrix-integrity-npm", "version": "1.0.0" }"#.as_slice(),
    )]);
    let published = client
        .post(package_url(&base, &["npm", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-integrity-npm-1.0.0.tgz\"",
        )
        .body(tarball.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let document = client
        .get(package_url(&base, &["npm", "matrix-integrity-npm"]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let dist = &document["versions"]["1.0.0"]["dist"];

    // The bytes the client actually gets, followed from the published URL.
    let tarball_url = dist["tarball"].as_str().expect("no tarball url");
    let downloaded = client.get(tarball_url).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    let downloaded = downloaded.bytes().await.unwrap().to_vec();
    assert_eq!(
        downloaded, tarball,
        "the tarball served is the one published"
    );

    assert_eq!(
        dist["shasum"].as_str(),
        Some(hex::encode(sha1::Sha1::digest(&downloaded)).as_str()),
        "dist.shasum must be the SHA-1 of the tarball: {dist}"
    );
    assert_eq!(
        dist["integrity"].as_str(),
        Some(
            format!(
                "sha512-{}",
                base64::engine::general_purpose::STANDARD.encode(sha2::Sha512::digest(&downloaded))
            )
            .as_str()
        ),
        "dist.integrity must be an SRI SHA-512 of the tarball: {dist}"
    );

    // ── Composer ───────────────────────────────────────────────────────────
    let archive = zip_archive(&[(
        "composer.json",
        br#"{ "name": "vendor/matrix-integrity", "version": "1.0.0" }"#.as_slice(),
    )]);
    let published = client
        .post(package_url(&base, &["composer", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-integrity-1.0.0.zip\"",
        )
        .body(archive.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let packages = client
        .get(package_url(&base, &["composer", "packages.json"]))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let dist = &packages["packages"]["vendor/matrix-integrity"]["1.0.0"]["dist"];

    let archive_url = dist["url"].as_str().expect("no dist url");
    let downloaded = client.get(archive_url).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    let downloaded = downloaded.bytes().await.unwrap().to_vec();
    assert_eq!(
        downloaded, archive,
        "the archive served is the one published"
    );

    assert_eq!(
        dist["shasum"].as_str(),
        Some(hex::encode(sha1::Sha1::digest(&downloaded)).as_str()),
        "dist.shasum must be the SHA-1 of the zip Composer downloads: {dist}"
    );
}

/// card_343d636b1157: an index that puts a checksum next to a download link is
/// answering "the digest of *what*", and `package_versions.sha256` is the
/// digest of the first file of the first publish request — a different file as
/// soon as the version carries two, which is the normal case (`twine upload
/// dist/*` sends a wheel and an sdist). The assert is deliberately not "a
/// digest is present": every one is checked against the bytes the published URL
/// actually serves.
#[tokio::test]
async fn every_index_publishes_the_digest_of_the_file_its_link_points_at() {
    use sha2::Digest as _;

    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let publish = |pkg_type: &'static str, filename: String, body: Vec<u8>, expect: StatusCode| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(package_url(&base, &[pkg_type, "publish"]))
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
                response.status(),
                expect,
                "{pkg_type} publish of {filename}"
            );
        }
    };

    let sha256_of = |bytes: &[u8]| hex::encode(sha2::Sha256::digest(bytes));

    // ── PyPI: a wheel and an sdist in one version ──────────────────────────
    let wheel = zip_archive(&[(
        "matrix_hash-1.0.0.dist-info/METADATA",
        b"Metadata-Version: 2.1\nName: matrix-hash\nVersion: 1.0.0\nSummary: hashes\n".as_slice(),
    )]);
    let sdist = tar_gz(&[(
        "matrix_hash-1.0.0/PKG-INFO",
        b"Metadata-Version: 2.1\nName: matrix-hash\nVersion: 1.0.0\nSummary: hashes\n".as_slice(),
    )]);
    publish(
        "pypi",
        "matrix_hash-1.0.0-py3-none-any.whl".into(),
        wheel.clone(),
        StatusCode::CREATED,
    )
    .await;
    // The second file lands in the version that already exists.
    publish(
        "pypi",
        "matrix_hash-1.0.0.tar.gz".into(),
        sdist.clone(),
        StatusCode::OK,
    )
    .await;

    let simple = client
        .get(package_url(&base, &["pypi", "simple", "matrix-hash", ""]))
        .send()
        .await
        .unwrap();
    assert_eq!(simple.status(), StatusCode::OK);
    let simple = simple.text().await.unwrap();

    // Both artifacts are on the page: a release is not one file, and pip picks
    // between them by name.
    for (filename, body) in [
        ("matrix_hash-1.0.0-py3-none-any.whl", &wheel),
        ("matrix_hash-1.0.0.tar.gz", &sdist),
    ] {
        let href = simple
            .lines()
            .find(|line| line.contains(filename))
            .unwrap_or_else(|| panic!("{filename} is missing from the Simple page:\n{simple}"));
        let url = href
            .split("href=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("a link with an href");
        let (url, fragment) = url.split_once("#sha256=").unwrap_or_else(|| {
            panic!("{filename} carries no #sha256= fragment: {href}");
        });

        let downloaded = client.get(url).send().await.unwrap();
        assert_eq!(downloaded.status(), StatusCode::OK, "download {filename}");
        let downloaded = downloaded.bytes().await.unwrap().to_vec();
        assert_eq!(&downloaded, body, "the file served is the one published");
        assert_eq!(
            fragment,
            sha256_of(&downloaded),
            "the Simple page's #sha256 for {filename} is not that file's digest"
        );
    }

    // ── RubyGems: the JSON info endpoint ───────────────────────────────────
    let gem = tar_archive(&[(
        "metadata.gz",
        &gzip(b"name: matrix-hash-gem\nversion: 1.0.0\nsummary: hashes\n"),
    )]);
    publish(
        "rubygems",
        "matrix-hash-gem-1.0.0.gem".into(),
        gem.clone(),
        StatusCode::CREATED,
    )
    .await;
    // The gem and chart adapters accept only their own archive, so those
    // versions hold one file each and the version digest happens to coincide
    // with it. These two halves are therefore a regression guard rather than a
    // reproduction: they pin "the digest belongs to the file the link points
    // at" against the downloaded bytes, so the day a second file can land in
    // one of these versions — a platform gem, a `.prov` beside a chart — the
    // answer does not quietly become the wrong file's.
    let info = client
        .get(package_url(
            &base,
            &["rubygems", "api", "v1", "gems", "matrix-hash-gem.json"],
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let entry = &info["versions"]["1.0.0"];
    let gem_url = entry["gem_uri"].as_str().expect("no gem_uri");
    let downloaded = client.get(gem_url).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK, "download {gem_url}");
    let downloaded = downloaded.bytes().await.unwrap().to_vec();
    assert_eq!(downloaded, gem, "the gem served is the one published");
    assert_eq!(
        entry["sha"].as_str(),
        Some(sha256_of(&downloaded).as_str()),
        "gem info sha must be the digest of the .gem it links to: {entry}"
    );

    // ── Helm: index.yaml ───────────────────────────────────────────────────
    let chart = tar_gz(&[(
        "matrix-hash-chart/Chart.yaml",
        b"apiVersion: v2\nname: matrix-hash-chart\nversion: 1.0.0\n".as_slice(),
    )]);
    publish(
        "helm",
        "matrix-hash-chart-1.0.0.tgz".into(),
        chart.clone(),
        StatusCode::CREATED,
    )
    .await;
    let index = client
        .get(package_url(&base, &["helm", "index.yaml"]))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let digest = index
        .lines()
        .find_map(|line| line.trim().strip_prefix("digest: "))
        .unwrap_or_else(|| panic!("no digest in the Helm index:\n{index}"));
    let url = index
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("- ")
                .filter(|u| u.contains("http"))
        })
        .unwrap_or_else(|| panic!("no chart url in the Helm index:\n{index}"));

    let downloaded = client.get(url).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK, "download {url}");
    let downloaded = downloaded.bytes().await.unwrap().to_vec();
    assert_eq!(downloaded, chart, "the chart served is the one published");
    assert_eq!(
        digest,
        sha256_of(&downloaded),
        "the Helm index digest must be the SHA-256 of the .tgz it links to"
    );
}

/// RubyGems resolves through the compact index: `versions` first — its presence
/// is what keeps the client off the legacy Marshal index we do not serve — then
/// `info/<gem>`, then a `.gem` at a path the client builds itself by appending
/// `gems/<file>` to the source URL. This walks those three, in that order, and
/// then follows the published `gem_uri`; the `api/v1/gems/<gem>.json` spelling
/// the rest of the suite uses is ForgeKeep's own and no client asks for it.
#[tokio::test]
async fn rubygems_compact_index_serves_the_layout_gem_requests() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let gem_metadata = b"name: matrix-gem\nversion: 1.0.0\nsummary: RubyGems matrix package\n";
    let gem_file = tar_archive(&[("metadata.gz", &gzip(gem_metadata))]);

    let published = client
        .post(package_url(&base, &["rubygems", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-gem-1.0.0.gem\"",
        )
        .body(gem_file.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    // ── `versions`, the request that picks the protocol ─────────────────────
    let versions = client
        .get(package_url(&base, &["rubygems", "versions"]))
        .send()
        .await
        .unwrap();
    assert_eq!(versions.status(), StatusCode::OK, "compact index versions");
    let versions = versions.text().await.unwrap();
    let entry = versions
        .lines()
        .find(|line| line.starts_with("matrix-gem "))
        .unwrap_or_else(|| panic!("versions index lost the gem: {versions}"));
    let mut columns = entry.split(' ');
    assert_eq!(columns.next(), Some("matrix-gem"));
    assert_eq!(columns.next(), Some("1.0.0"));
    let info_checksum = columns.next().expect("versions line carries no checksum");

    // ── `info/<gem>`, what the resolver actually reads ──────────────────────
    let info = client
        .get(package_url(&base, &["rubygems", "info", "matrix-gem"]))
        .send()
        .await
        .unwrap();
    assert_eq!(info.status(), StatusCode::OK, "compact index info");
    let info = info.text().await.unwrap();
    let line = info
        .lines()
        .find(|line| line.starts_with("1.0.0"))
        .unwrap_or_else(|| panic!("info file lost the version: {info}"));
    // The pipe is what the client's parser splits on before it reads either
    // side, and the checksum after it is what it verifies the download against.
    let (_, requirements) = line.split_once('|').expect("info line carries no pipe");
    let checksum = requirements
        .strip_prefix("checksum:")
        .unwrap_or_else(|| panic!("info line carries no checksum: {line}"));
    assert_eq!(checksum.len(), 64, "checksum is not a SHA-256: {checksum}");

    // The column in `versions` is the client's cache key for this exact body —
    // if it is the digest of anything else, every resolve refetches forever.
    use md5::{Digest, Md5};
    assert_eq!(
        info_checksum,
        format!("{:x}", Md5::digest(info.as_bytes())),
        "versions index publishes a checksum of a different info body"
    );

    // ── The download, at the path the client builds on its own ──────────────
    let derived = package_url(&base, &["rubygems", "gems", "matrix-gem-1.0.0.gem"]);
    let downloaded = client.get(derived.clone()).send().await.unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK, "derived {derived}");
    assert_eq!(
        downloaded.bytes().await.unwrap().as_ref(),
        gem_file.as_slice()
    );

    // ── And the URL we publish has to be that same one ──────────────────────
    let gem_info = client
        .get(package_url(
            &base,
            &["rubygems", "api", "v1", "gems", "matrix-gem.json"],
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let gem_uri = gem_info["versions"]["1.0.0"]["gem_uri"]
        .as_str()
        .unwrap_or_else(|| panic!("no gem_uri: {gem_info}"));
    let advertised = client.get(gem_uri).send().await.unwrap();
    assert_eq!(advertised.status(), StatusCode::OK, "gem_uri {gem_uri}");
    assert_eq!(
        advertised.bytes().await.unwrap().as_ref(),
        gem_file.as_slice()
    );

    // ── Misses are misses, not the SPA ──────────────────────────────────────
    for segments in [
        vec!["rubygems", "info", "no-such-gem"],
        vec!["rubygems", "gems", "no-such-gem-1.0.0.gem"],
    ] {
        let response = client
            .get(package_url(&base, &segments))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{segments:?}");
        let body = response.text().await.unwrap();
        assert!(
            !body.contains("<html"),
            "{segments:?} reached the SPA fallback: {body}"
        );
    }

    // ── The routes these sit on top of still answer ─────────────────────────
    let names = client
        .get(package_url(&base, &["rubygems", "names"]))
        .send()
        .await
        .unwrap();
    assert_eq!(names.status(), StatusCode::OK, "names");
    assert!(names.text().await.unwrap().contains("matrix-gem"));

    let listed = client
        .get(package_url(&base, &["rubygems", "list"]))
        .send()
        .await
        .unwrap();
    assert_eq!(listed.status(), StatusCode::OK, "generic list");
    let listed = listed.json::<serde_json::Value>().await.unwrap();
    assert!(listed["packages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|package| package["name"] == "matrix-gem"));
}

/// A gemspec's dependencies live nowhere but the gemspec, and both endpoints a
/// client resolves through read them back out of the stored version metadata.
/// Published without them, the registry answers `200` with an empty dependency
/// list — which Bundler reads as "this gem needs nothing" and resolves cleanly,
/// so the failure only shows up as a missing transitive gem at runtime.
#[tokio::test]
async fn rubygems_publishes_gemspec_dependencies_into_both_resolvers() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    // The shape `gem build` writes: tagged objects, `[operator, Gem::Version]`
    // constraint pairs, and both dependency kinds in one list.
    let gem_metadata = br#"--- !ruby/object:Gem::Specification
name: matrix-deps-gem
version: !ruby/object:Gem::Version
  version: '1.0.0'
summary: A gem with dependencies
description: The long form of the summary
homepage: https://example.com/matrix-deps-gem
licenses:
- MIT
dependencies:
- !ruby/object:Gem::Dependency
  name: rack
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - ">="
      - !ruby/object:Gem::Version
        version: '2.0'
    - - "<"
      - !ruby/object:Gem::Version
        version: '4.0'
  type: :runtime
- !ruby/object:Gem::Dependency
  name: rspec
  requirement: !ruby/object:Gem::Requirement
    requirements:
    - - "~>"
      - !ruby/object:Gem::Version
        version: '3.0'
  type: :development
"#;
    let gem_file = tar_archive(&[("metadata.gz", &gzip(gem_metadata))]);

    let published = client
        .post(package_url(&base, &["rubygems", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-deps-gem-1.0.0.gem\"",
        )
        .body(gem_file)
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    // ── The dependencies API ────────────────────────────────────────────────
    let mut deps_url = package_url(&base, &["rubygems", "api", "v1", "dependencies"]);
    deps_url
        .query_pairs_mut()
        .append_pair("gems", "matrix-deps-gem");
    let deps = client
        .get(deps_url)
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();

    let entry = deps
        .as_array()
        .and_then(|entries| entries.first())
        .unwrap_or_else(|| panic!("dependencies API returned nothing: {deps}"));
    assert_eq!(entry["number"], "1.0.0");
    assert_eq!(
        entry["dependencies"],
        serde_json::json!([["rack", ">= 2.0, < 4.0"]]),
        "the runtime dependency is missing, or the development one leaked in"
    );

    // ── The compact index, which is what a modern client actually reads ─────
    let info = client
        .get(package_url(&base, &["rubygems", "info", "matrix-deps-gem"]))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let line = info
        .lines()
        .find(|line| line.starts_with("1.0.0"))
        .unwrap_or_else(|| panic!("info file lost the version: {info}"));
    let (versioned_deps, _) = line.split_once('|').expect("info line carries no pipe");
    // Two constraints on one gem join with `&`; the comma is what separates
    // dependencies in this format.
    assert_eq!(
        versioned_deps.trim(),
        "1.0.0 rack:>= 2.0&< 4.0",
        "compact index line: {line}"
    );

    // ── And the gem info endpoint, which reads the same stored blob ─────────
    let gem_info = client
        .get(package_url(
            &base,
            &["rubygems", "api", "v1", "gems", "matrix-deps-gem.json"],
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let version = &gem_info["versions"]["1.0.0"];
    assert_eq!(version["summary"], "A gem with dependencies");
    assert_eq!(version["description"], "The long form of the summary");
    assert_eq!(
        version["homepage_uri"],
        "https://example.com/matrix-deps-gem"
    );
    assert_eq!(version["licenses"], serde_json::json!(["MIT"]));
}

/// NuGet's registration index and Helm's `index.yaml` describe one *version*
/// each, and the fields they publish have no package column — they survive the
/// publish only through the stored version metadata.
#[tokio::test]
async fn nuget_and_helm_indexes_carry_per_version_metadata() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let publish = |pkg_type: &'static str, filename: &'static str, body: Vec<u8>| {
        let client = client.clone();
        let token = token.clone();
        let base = base.clone();
        async move {
            let response = client
                .post(package_url(&base, &[pkg_type, "publish"]))
                .bearer_auth(&token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{filename}\""),
                )
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::CREATED, "{pkg_type} publish");
        }
    };

    let nuspec = br#"<?xml version="1.0"?>
<package><metadata>
  <id>Matrix.Meta</id>
  <version>1.0.0</version>
  <description>NuGet metadata package</description>
  <projectUrl>https://example.com/matrix-meta</projectUrl>
  <license type="expression">MIT</license>
  <tags>matrix testing</tags>
</metadata></package>"#;
    publish(
        "nuget",
        "Matrix.Meta.1.0.0.nupkg",
        zip_archive(&[("Matrix.Meta.nuspec", nuspec)]),
    )
    .await;

    let chart_yaml = br#"apiVersion: v2
name: matrix-chart
version: 1.0.0
appVersion: 1.19
keywords:
  - web
  - proxy
sources:
  - https://example.com/matrix-chart-source
"#;
    publish(
        "helm",
        "matrix-chart-1.0.0.tgz",
        tar_gz(&[("matrix-chart/Chart.yaml", chart_yaml)]),
    )
    .await;

    let registration = client
        .get(package_url(
            &base,
            &["nuget", "registration", "Matrix.Meta", "index.json"],
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let catalog = &registration["items"][0]["items"][0]["catalogEntry"];
    assert_eq!(catalog["description"], "NuGet metadata package");
    assert_eq!(catalog["projectUrl"], "https://example.com/matrix-meta");
    assert_eq!(catalog["licenseUrl"], "MIT");
    assert_eq!(catalog["tags"], serde_json::json!(["matrix", "testing"]));

    let index = client
        .get(package_url(&base, &["helm", "index.yaml"]))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let index: serde_json::Value = serde_yaml::from_str(&index).unwrap();
    let chart = &index["entries"]["matrix-chart"][0];
    assert_eq!(chart["apiVersion"], "v2");
    assert_eq!(chart["appVersion"], "1.19");
    assert_eq!(chart["keywords"], serde_json::json!(["web", "proxy"]));
    assert_eq!(
        chart["sources"],
        serde_json::json!(["https://example.com/matrix-chart-source"])
    );
}

/// Composer 2.x resolves against the repository's own `packages.json` and never
/// opens the archive. An entry without `require` therefore does not say
/// "unknown", it says the package needs nothing: `composer require vendor/pkg`
/// succeeds and installs none of its dependencies. `type` is the other half —
/// it dispatches the installer, so a `composer-plugin` announced as a `library`
/// is unpacked into `vendor/` where nothing picks it up, and `autoload` is what
/// makes the installed files reachable at all.
#[tokio::test]
async fn composer_metadata_carries_the_manifest_requirements_type_and_license() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let composer_json = br#"{
  "name": "vendor/matrix-plugin",
  "version": "1.0.0",
  "description": "composer matrix plugin",
  "type": "composer-plugin",
  "license": ["MIT", "Apache-2.0"],
  "require": { "php": ">=8.1", "composer-plugin-api": "^2.0" },
  "require-dev": { "phpunit/phpunit": "^10.0" },
  "conflict": { "vendor/old-plugin": "*" },
  "autoload": { "psr-4": { "Vendor\\Matrix\\": "src/" } },
  "extra": { "class": "Vendor\\Matrix\\Plugin" }
}"#;

    let published = client
        .post(package_url(&base, &["composer", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-plugin-1.0.0.zip\"",
        )
        .body(zip_archive(&[("composer.json", composer_json)]))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let document: serde_json::Value = client
        .get(package_url(&base, &["composer", "packages.json"]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entry = &document["packages"]["vendor/matrix-plugin"]["1.0.0"];

    // The solver's input.
    assert_eq!(
        entry["require"],
        serde_json::json!({ "php": ">=8.1", "composer-plugin-api": "^2.0" }),
        "entry: {entry}"
    );
    assert_eq!(
        entry["require-dev"],
        serde_json::json!({ "phpunit/phpunit": "^10.0" })
    );
    assert_eq!(
        entry["conflict"],
        serde_json::json!({ "vendor/old-plugin": "*" })
    );

    // The installer's input: `library` here would put the plugin in `vendor/`.
    assert_eq!(
        entry["type"], "composer-plugin",
        "the manifest's own type must win over the fallback: {entry}"
    );
    assert_eq!(
        entry["extra"],
        serde_json::json!({ "class": "Vendor\\Matrix\\Plugin" })
    );
    // Composer builds the autoloader from the repository metadata, not from the
    // archive: without this the files land on disk unreachable.
    assert_eq!(
        entry["autoload"],
        serde_json::json!({ "psr-4": { "Vendor\\Matrix\\": "src/" } })
    );
    assert_eq!(entry["license"], "MIT, Apache-2.0", "entry: {entry}");

    // The registry's own answers about its own storage are unchanged by any of it.
    assert_eq!(entry["name"], "vendor/matrix-plugin");
    assert_eq!(entry["version"], "1.0.0");
    assert!(
        entry["dist"]["url"]
            .as_str()
            .unwrap()
            .ends_with("/packages/composer/vendor%2Fmatrix-plugin/1.0.0/matrix-plugin-1.0.0.zip"),
        "entry: {entry}"
    );
}

/// A package published before the adapter recorded any manifest sections — and
/// one whose format never carries them — must still resolve. To Composer a
/// missing `require` reads as "needs nothing", so the key is written whatever
/// the row held.
#[tokio::test]
async fn a_composer_entry_always_carries_a_require_table() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let composer_json = br#"{ "name": "vendor/matrix-bare", "version": "2.0.0" }"#;
    let published = client
        .post(package_url(&base, &["composer", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"matrix-bare-2.0.0.zip\"",
        )
        .body(zip_archive(&[("composer.json", composer_json)]))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let document: serde_json::Value = client
        .get(package_url(&base, &["composer", "packages.json"]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let entry = &document["packages"]["vendor/matrix-bare"]["2.0.0"];

    assert_eq!(entry["require"], serde_json::json!({}), "entry: {entry}");
    assert_eq!(
        entry["type"], "library",
        "a manifest that declares no type falls back to the Composer default: {entry}"
    );
}

/// Every resource the NuGet service index advertises has to be a path this
/// server answers.
///
/// A NuGet client does not guess its endpoints: it reads `index.json` and uses
/// the `@id` of each `@type` it needs. Two of the five were fiction — the flat
/// container (`PackageBaseAddress`, which is where `dotnet restore` downloads
/// from) had no routes at all, and autocomplete pointed at the bare `nuget/`
/// root — so the document described a server nobody was running
/// (card_dba77cceec56).
///
/// The `@type` list is asserted exhaustively on purpose. A resource added to
/// the index later, with no probe here, fails this test rather than being
/// silently exempt from it — which is exactly how the two dead resources
/// survived.
#[tokio::test]
async fn every_advertised_nuget_resource_is_a_path_the_registry_serves() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let nuspec = br#"<?xml version="1.0"?>
<package><metadata><id>Matrix.NuGet</id><version>1.0.0</version><description>NuGet matrix package</description></metadata></package>"#;
    let nupkg = zip_archive(&[("Matrix.NuGet.nuspec", nuspec)]);

    let published = client
        .post(package_url(&base, &["nuget", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"Matrix.NuGet.1.0.0.nupkg\"",
        )
        .body(nupkg.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let index: serde_json::Value = client
        .get(package_url(&base, &["nuget", "index.json"]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let mut advertised: std::collections::BTreeMap<String, String> = Default::default();
    for resource in index["resources"].as_array().expect("resources array") {
        advertised.insert(
            resource["@type"].as_str().expect("@type").to_string(),
            resource["@id"].as_str().expect("@id").to_string(),
        );
    }
    let types: Vec<&str> = advertised.keys().map(String::as_str).collect();
    assert_eq!(
        types,
        vec![
            "PackageBaseAddress/3.0.0",
            "PackagePublish/2.0.0",
            "RegistrationsBaseUrl/3.6.0",
            "SearchAutocompleteService/3.5.0",
            "SearchQueryService/3.5.0",
        ],
        "an advertised resource with no probe below is a resource nobody proved is served"
    );

    // Base addresses are prefixes, not endpoints: the client appends the layout
    // the protocol defines, so that layout is what gets probed. The lowercase
    // ids are deliberate — that is the only spelling a NuGet client ever sends.
    let base_address = &advertised["PackageBaseAddress/3.0.0"];
    let versions: serde_json::Value = {
        let response = client
            .get(format!("{base_address}matrix.nuget/index.json"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the advertised flat container does not serve {base_address}matrix.nuget/index.json"
        );
        response.json().await.unwrap()
    };
    assert_eq!(versions["versions"], serde_json::json!(["1.0.0"]));

    let content = client
        .get(format!(
            "{base_address}matrix.nuget/1.0.0/matrix.nuget.1.0.0.nupkg"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        content.status(),
        StatusCode::OK,
        "the flat container must serve the package content itself, not only its version list"
    );
    assert_eq!(
        content.bytes().await.unwrap().as_ref(),
        nupkg.as_slice(),
        "restore downloads the bytes that were published"
    );

    let registration_url = format!(
        "{}matrix.nuget/index.json",
        advertised["RegistrationsBaseUrl/3.6.0"]
    );
    let registration = client.get(&registration_url).send().await.unwrap();
    assert_eq!(
        registration.status(),
        StatusCode::OK,
        "the registration index must answer the lowercase id a client sends, \
         not only the published spelling"
    );
    let registration: serde_json::Value = registration.json().await.unwrap();
    assert_eq!(registration["count"], 1, "registration: {registration}");

    // Walk every same-origin URL the registration document publishes. This is
    // recursive so a later registry URL cannot appear without being driven by
    // the test — the exact blind spot that let the dead `.nuspec` link survive.
    // Publisher-supplied project/license URLs may legitimately be external.
    fn collect_registry_urls(
        value: &serde_json::Value,
        registry_origin: &str,
        urls: &mut std::collections::BTreeSet<String>,
    ) {
        match value {
            serde_json::Value::String(url) if url.starts_with(registry_origin) => {
                urls.insert(url.clone());
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    collect_registry_urls(item, registry_origin, urls);
                }
            }
            serde_json::Value::Object(fields) => {
                for value in fields.values() {
                    collect_registry_urls(value, registry_origin, urls);
                }
            }
            _ => {}
        }
    }

    let mut registration_urls = std::collections::BTreeSet::new();
    collect_registry_urls(&registration, &base, &mut registration_urls);
    for url in &registration_urls {
        let response = client.get(url).send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the registration index advertises an URL this registry does not serve: {url}"
        );
    }

    let page = &registration["items"][0];
    let leaf = &page["items"][0];
    assert_eq!(leaf["registration"], registration_url);
    assert_eq!(page["@id"], format!("{registration_url}#page/1.0.0/1.0.0"));
    assert_eq!(
        leaf["catalogEntry"]["@id"],
        format!("{}#catalogEntry", leaf["@id"].as_str().unwrap())
    );

    for (resource, label) in [
        ("SearchQueryService/3.5.0", "search"),
        ("SearchAutocompleteService/3.5.0", "autocomplete"),
    ] {
        let url = &advertised[resource];
        let response = client
            .get(url.clone())
            .query(&[("q", "matrix")])
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "the advertised {label} resource {url} is not a route this registry serves"
        );
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(body["totalHits"], 1, "{label}: {body}");
    }

    // `dotnet nuget push` sends PUT with one multipart field named `package`.
    // Its filename is deliberately the generic `package.nupkg`: the package's
    // real coordinates live in the nuspec, not in the form envelope.
    let pushed_nupkg = zip_archive(&[(
        "Matrix.NuGet.nuspec",
        br#"<?xml version="1.0"?>
<package><metadata><id>Matrix.NuGet</id><version>2.0.0</version><description>NuGet matrix package</description></metadata></package>"# as &[u8],
    )]);
    let dotnet_push = reqwest::multipart::Form::new().part(
        "package",
        reqwest::multipart::Part::bytes(pushed_nupkg.clone())
            .file_name("package.nupkg")
            .mime_str("application/octet-stream")
            .expect("literal MIME type"),
    );
    let pushed = client
        .put(advertised["PackagePublish/2.0.0"].clone())
        .bearer_auth(&token)
        .multipart(dotnet_push)
        .send()
        .await
        .unwrap();
    assert_eq!(
        pushed.status(),
        StatusCode::CREATED,
        "the advertised publish resource must accept the verb `dotnet nuget push` sends"
    );

    let versions: serde_json::Value = client
        .get(format!("{base_address}matrix.nuget/index.json"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        versions["versions"],
        serde_json::json!(["1.0.0", "2.0.0"]),
        "the pushed version has to show up where restore looks for it"
    );

    let pushed_content = client
        .get(format!(
            "{base_address}matrix.nuget/2.0.0/matrix.nuget.2.0.0.nupkg"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(pushed_content.status(), StatusCode::OK);
    assert_eq!(
        pushed_content.bytes().await.unwrap().as_ref(),
        pushed_nupkg.as_slice(),
        "restore must receive the nupkg part, not the surrounding multipart envelope"
    );

    let missing_package = client
        .put(advertised["PackagePublish/2.0.0"].clone())
        .bearer_auth(&token)
        .multipart(reqwest::multipart::Form::new().text("metadata", "not a package"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_package.status(), StatusCode::BAD_REQUEST);
    assert!(missing_package
        .text()
        .await
        .unwrap()
        .contains("missing `package`"));

    let duplicate_package = reqwest::multipart::Form::new()
        .part(
            "package",
            reqwest::multipart::Part::bytes(pushed_nupkg.clone()).file_name("package.nupkg"),
        )
        .part(
            "package",
            reqwest::multipart::Part::bytes(pushed_nupkg).file_name("package.nupkg"),
        );
    let duplicate_package = client
        .put(advertised["PackagePublish/2.0.0"].clone())
        .bearer_auth(&token)
        .multipart(duplicate_package)
        .send()
        .await
        .unwrap();
    assert_eq!(duplicate_package.status(), StatusCode::BAD_REQUEST);
    assert!(duplicate_package
        .text()
        .await
        .unwrap()
        .contains("repeats the `package` field"));
}

/// A second raw spelling of the same NuGet identity is not an additional-file
/// upload into the first spelling: it is a conflicting immutable publication.
#[tokio::test]
async fn equivalent_nuget_version_spelling_is_a_conflict_without_a_second_row() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let first = send_nuget_version(&client, &base, &token, "Matrix.Identity", "1").await;
    let first_status = first.status();
    let first_body = first.text().await.unwrap();
    assert_eq!(first_status, StatusCode::CREATED, "{first_body}");

    let equivalent = send_nuget_version(&client, &base, &token, "Matrix.Identity", "1.0.0").await;
    assert_eq!(equivalent.status(), StatusCode::CONFLICT);

    let repo = rg_core::repo::service::find_repo_by_owner_name(&db, "matrix-owner", "matrix-repo")
        .await
        .unwrap()
        .unwrap();
    let registry = rg_db::ops::package_registry_ops::find_by_repo_and_type(&db, repo.id, "nuget")
        .await
        .unwrap()
        .unwrap();
    let package =
        rg_db::ops::package_ops::find_by_registry_and_name(&db, registry.id, "Matrix.Identity")
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
        versions[0].version, "1",
        "the first spelling stays canonical storage"
    );
    assert_eq!(versions[0].protocol_version_key.as_deref(), Some("1.0.0"));
    let files = rg_db::ops::package_file_ops::list_by_version(&db, versions[0].id)
        .await
        .unwrap();
    assert_eq!(files.len(), 1, "the refused publish created a file row");
}

/// Registration page bounds and flat-container paths use NuGetVersion
/// semantics, not publication order or the spelling stored in the database.
#[tokio::test]
async fn nuget_normalized_versions_round_trip_from_indexes_to_package_content() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let versions = [
        ("2.0.0.1+Build.7", Some("2.0.0.1")),
        ("1.2", Some("1.2.0")),
        ("1.5.0-RC.2+metadata", Some("1.5.0-rc.2")),
        ("legacy-row", None),
    ];
    let mut downloadable = Vec::new();

    for (stored, normalized) in versions {
        let nuspec = format!(
            "<?xml version=\"1.0\"?><package><metadata><id>Matrix.Bounds</id>\
             <version>{stored}</version><description>NuGet bounds</description>\
             </metadata></package>"
        );
        let nupkg = zip_archive(&[("Matrix.Bounds.nuspec", nuspec.as_bytes())]);
        let published = client
            .post(package_url(&base, &["nuget", "publish"]))
            .bearer_auth(&token)
            .header(
                reqwest::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"Matrix.Bounds.{stored}.nupkg\""),
            )
            .body(nupkg.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(published.status(), StatusCode::CREATED, "version {stored}");
        if let Some(normalized) = normalized {
            downloadable.push((normalized, nupkg));
        }
    }

    let registration: serde_json::Value = client
        .get(package_url(
            &base,
            &["nuget", "registration", "matrix.bounds", "index.json"],
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let page = &registration["items"][0];
    assert_eq!(page["lower"], "1.2.0", "page: {page}");
    assert_eq!(page["upper"], "2.0.0.1", "page: {page}");
    assert_eq!(page["count"], 4, "the unreadable legacy row stays visible");

    let leaves = page["items"]
        .as_array()
        .unwrap_or_else(|| panic!("registration page must inline its leaves: {page}"));
    let mut leaf_ids = std::collections::HashSet::new();
    for inline in leaves {
        let leaf_id = inline["@id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .unwrap_or_else(|| panic!("every inline leaf needs a non-empty @id: {inline}"));
        assert!(
            leaf_ids.insert(leaf_id.to_string()),
            "registration leaf @id must be unique: {leaf_id}"
        );

        let response = client.get(leaf_id).send().await.unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "advertised registration leaf must be served: {leaf_id}"
        );
        let leaf: serde_json::Value = response.json().await.unwrap();
        assert_eq!(leaf["@id"], inline["@id"], "leaf identity: {leaf_id}");
        assert_eq!(
            leaf["packageContent"], inline["packageContent"],
            "leaf package content: {leaf_id}"
        );
        assert_eq!(
            leaf["registration"], inline["registration"],
            "leaf registration index: {leaf_id}"
        );
        assert_eq!(
            leaf["catalogEntry"], inline["catalogEntry"]["@id"],
            "standalone leaf must point at the catalog entry embedded inline: {leaf_id}"
        );
        assert_eq!(
            leaf["listed"], inline["catalogEntry"]["listed"],
            "standalone leaf availability: {leaf_id}"
        );

        let head = client.head(leaf_id).send().await.unwrap();
        assert_eq!(
            head.status(),
            StatusCode::OK,
            "every advertised registration URL must support HEAD: {leaf_id}"
        );
        assert!(head.bytes().await.unwrap().is_empty());
    }

    let flat: serde_json::Value = client
        .get(package_url(
            &base,
            &["nuget", "package", "matrix.bounds", "index.json"],
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        flat["versions"],
        serde_json::json!(["1.2.0", "1.5.0-rc.2", "2.0.0.1"]),
        "flat-container versions must be normalized and ordered by NuGetVersion: {flat}"
    );

    for (normalized, nupkg) in downloadable {
        let downloaded = client
            .get(package_url(
                &base,
                &[
                    "nuget",
                    "package",
                    "matrix.bounds",
                    normalized,
                    &format!("matrix.bounds.{normalized}.nupkg"),
                ],
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(
            downloaded.status(),
            StatusCode::OK,
            "normalized URL for stored version must resolve: {normalized}"
        );
        assert_eq!(downloaded.bytes().await.unwrap().as_ref(), nupkg.as_slice());
    }
}

/// `mvn deploy` publishes by PUT-ing each file to the layout its resolver reads.
///
/// The registry only had `POST .../packages/maven/publish`, a spelling no Maven
/// client knows, so the deploy half of the round trip could not be driven by the
/// real tool at all — reading the layout was fixed long before writing it
/// (card_11d8655a9cd8). This drives the sequence Wagon actually sends: the
/// artifact, then its checksums, then `maven-metadata.xml`.
#[tokio::test]
async fn maven_deploys_by_layout_and_resolves_the_same_paths_back() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let pom = br#"<?xml version="1.0"?>
<project><groupId>com.example</groupId><artifactId>matrix-maven</artifactId><version>1.0.0</version></project>"#;
    let jar = zip_archive(&[("META-INF/MANIFEST.MF", b"Manifest-Version: 1.0\n")]);

    let layout = |file: &str| {
        format!(
            "{base}/api/v1/repos/matrix-owner/matrix-repo/packages/maven/\
             com/example/matrix-maven/1.0.0/{file}"
        )
        .replace(' ', "")
    };

    for (file, body) in [
        ("matrix-maven-1.0.0.pom", pom.to_vec()),
        ("matrix-maven-1.0.0.jar", jar.clone()),
    ] {
        let deployed = client
            .put(layout(file))
            .bearer_auth(&token)
            .body(body)
            .send()
            .await
            .unwrap();
        assert!(
            deployed.status().is_success(),
            "PUT {file} answered {} — `mvn deploy` cannot publish here",
            deployed.status()
        );
    }

    // The resolver reads back from the very URLs the deploy wrote to.
    for (file, expected) in [
        ("matrix-maven-1.0.0.pom", pom.to_vec()),
        ("matrix-maven-1.0.0.jar", jar.clone()),
    ] {
        let resolved = client.get(layout(file)).send().await.unwrap();
        assert_eq!(resolved.status(), StatusCode::OK, "GET {file}");
        assert_eq!(
            resolved.bytes().await.unwrap().as_ref(),
            expected.as_slice()
        );
    }

    // ...and it fetches the checksums to verify what it downloaded. They are not
    // stored, so a 404 here would make every build warn about intact artifacts.
    let sha1 = client
        .get(layout("matrix-maven-1.0.0.jar.sha1"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        sha1.status(),
        StatusCode::OK,
        "the resolver's checksum fetch"
    );
    let sha1 = sha1.text().await.unwrap();
    assert_eq!(
        sha1,
        rg_core::package_registry::MavenChecksum::Sha1.hex(&jar),
        "the served checksum has to describe the bytes actually stored"
    );

    // Wagon uploads the same checksum right after the artifact. A correct one is
    // accepted; a wrong one has to fail the deploy, which is the only reason to
    // send a checksum at all.
    let accepted = client
        .put(layout("matrix-maven-1.0.0.jar.sha1"))
        .bearer_auth(&token)
        .body(sha1)
        .send()
        .await
        .unwrap();
    assert!(
        accepted.status().is_success(),
        "a matching checksum: {}",
        accepted.status()
    );

    let refused = client
        .put(layout("matrix-maven-1.0.0.jar.sha1"))
        .bearer_auth(&token)
        .body("0000000000000000000000000000000000000000")
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        StatusCode::BAD_REQUEST,
        "a checksum that does not describe the stored bytes must fail the deploy"
    );

    // Maven finishes by uploading its own metadata document. The registry
    // derives that answer from its rows, so it is accepted and not stored —
    // refusing it would fail the deploy over a document nobody reads back.
    let metadata = client
        .put(format!(
            "{base}/api/v1/repos/matrix-owner/matrix-repo/packages/maven/com/example/matrix-maven/maven-metadata.xml"
        ))
        .bearer_auth(&token)
        .body("<metadata/>")
        .send()
        .await
        .unwrap();
    assert!(
        metadata.status().is_success(),
        "maven-metadata.xml upload answered {}",
        metadata.status()
    );

    let derived = client
        .get(format!(
            "{base}/api/v1/repos/matrix-owner/matrix-repo/packages/maven/com/example/matrix-maven/maven-metadata.xml"
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(derived.status(), StatusCode::OK);
    let derived = derived.text().await.unwrap();
    assert!(
        derived.contains("<version>1.0.0</version>") && derived.contains("matrix-maven"),
        "the served metadata is derived from the rows, not from the uploaded copy: {derived}"
    );
}

/// `cargo publish` and `cargo yank` reach the registry the way cargo builds
/// their URLs: from the `api` key of the sparse index's `config.json`.
///
/// Reading the index was served long before any write route existed, so a crate
/// could be resolved out of ForgeKeep but never put there by the tool that
/// builds it — and `api` was deliberately withheld to keep cargo saying "this
/// registry does not support API commands" instead of walking into a 404
/// (card_5a790cc6ac35). Both halves are asserted together here: the key is only
/// honest while the routes behind it answer.
#[tokio::test]
async fn cargo_publishes_and_yanks_through_the_api_its_index_advertises() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let config: serde_json::Value = client
        .get(package_url(&base, &["cargo", "index", "config.json"]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let api = config["api"]
        .as_str()
        .expect("the sparse index must name its write API")
        .to_string();

    let manifest = b"[package]\nname = \"matrix-crate\"\nversion = \"1.0.0\"\n";
    let archive = tar_gz(&[("matrix-crate-1.0.0/Cargo.toml", manifest)]);

    // The publish body: `u32-LE len` + index metadata, then `u32-LE len` + the
    // `.crate` archive. Exactly what cargo puts on the wire.
    let metadata = serde_json::json!({
        "name": "matrix-crate",
        "vers": "1.0.0",
        "deps": [],
        "features": {},
        "authors": [],
        "links": serde_json::Value::Null,
    })
    .to_string();
    let mut frame = Vec::new();
    frame.extend_from_slice(&(metadata.len() as u32).to_le_bytes());
    frame.extend_from_slice(metadata.as_bytes());
    frame.extend_from_slice(&(archive.len() as u32).to_le_bytes());
    frame.extend_from_slice(&archive);

    // cargo sends the registry token with no scheme at all.
    let published = client
        .put(format!("{api}/api/v1/crates/new"))
        .header(reqwest::header::AUTHORIZATION, token.clone())
        .body(frame.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        published.status(),
        StatusCode::OK,
        "`cargo publish` could not reach the API its own index advertises"
    );
    let warnings: serde_json::Value = published.json().await.unwrap();
    assert!(
        warnings["warnings"]["other"].is_array(),
        "cargo reads the 2xx body as its warnings document: {warnings}"
    );

    // The crate has to show up where cargo looks for it: the prefixed index path.
    let index = client
        .get(package_url(
            &base,
            &["cargo", "index", "ma", "tr", "matrix-crate"],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(index.status(), StatusCode::OK);
    let index = index.text().await.unwrap();
    assert!(
        index.contains("\"vers\":\"1.0.0\"") && index.contains("\"yanked\":false"),
        "the published version is missing from the sparse index: {index}"
    );

    // ...and the archive downloads from the `dl` template of the same document.
    let downloaded = client
        .get(package_url(
            &base,
            &["cargo", "matrix-crate", "1.0.0", "matrix-crate-1.0.0.crate"],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), StatusCode::OK);
    assert_eq!(
        downloaded.bytes().await.unwrap().as_ref(),
        archive.as_slice()
    );

    // A body cargo would never send is a protocol mismatch, and says so.
    let malformed = client
        .put(format!("{api}/api/v1/crates/new"))
        .header(reqwest::header::AUTHORIZATION, token.clone())
        .body(vec![0u8, 1, 2])
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);

    // `cargo yank` / `cargo yank --undo`: separated by verb, answered `{"ok":true}`.
    for (yanked, request) in [
        (
            true,
            client.delete(format!("{api}/api/v1/crates/matrix-crate/1.0.0/yank")),
        ),
        (
            false,
            client.put(format!("{api}/api/v1/crates/matrix-crate/1.0.0/unyank")),
        ),
    ] {
        let response = request
            .header(reqwest::header::AUTHORIZATION, token.clone())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "yank={yanked}");
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(
            body["ok"], true,
            "cargo reads `ok`, not our own envelope: {body}"
        );

        let index = client
            .get(package_url(
                &base,
                &["cargo", "index", "ma", "tr", "matrix-crate"],
            ))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(
            index.contains(&format!("\"yanked\":{yanked}")),
            "the index must reflect the yank cargo just performed: {index}"
        );
    }
}

/// `gem push` reaches the registry at the URL it derives from `--host`, and
/// what it pushed is resolvable and downloadable afterwards.
///
/// The read side was finished first (card_01ee27252197), which left the gem the
/// odd one out: installable from ForgeKeep, never publishable to it, because
/// nothing under `/packages/rubygems/` answered a `POST` at all
/// (card_11a578ae1820). The download half is asserted here and not taken on
/// trust: `gem push` sends no filename, and the name the file is stored under is
/// the only thing `Gem::RemoteFetcher#download` can ask for.
#[tokio::test]
async fn rubygems_pushes_the_way_gem_push_sends_it() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let gem_metadata = b"name: matrix-push-gem\nversion: 2.1.0\nsummary: pushed by gem push\n";
    let gem_file = tar_archive(&[("metadata.gz", &gzip(gem_metadata))]);

    // The whole request: the `.gem` as the body, no `Content-Disposition`, and
    // the api key out of `~/.gem/credentials` with no `Bearer` in front of it.
    let pushed = client
        .post(package_url(&base, &["rubygems", "api", "v1", "gems"]))
        .header(reqwest::header::AUTHORIZATION, token.clone())
        .header(reqwest::header::CONTENT_TYPE, "application/octet-stream")
        .body(gem_file.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(
        pushed.status(),
        StatusCode::CREATED,
        "`gem push` could not reach the registry its --host names"
    );
    assert!(
        pushed
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/plain")),
        "`gem` prints the body of a 2xx verbatim, so it must not be JSON"
    );
    let said = pushed.text().await.unwrap();
    assert!(
        said.contains("matrix-push-gem") && said.contains("2.1.0"),
        "the line `gem push` prints back should name what was published: {said}"
    );

    // The version has to be resolvable through the file a modern client reads.
    let info = client
        .get(package_url(&base, &["rubygems", "info", "matrix-push-gem"]))
        .send()
        .await
        .unwrap();
    assert_eq!(info.status(), StatusCode::OK);
    let info = info.text().await.unwrap();
    assert!(
        info.lines().any(|line| line.starts_with("2.1.0")),
        "the pushed version never reached the compact index: {info}"
    );

    // ...and downloadable at the path the client builds on its own. This is
    // what the derived filename buys: stored under the `Content-Disposition`
    // fallback the gem would sit in the registry under the name `package`.
    let downloaded = client
        .get(package_url(
            &base,
            &["rubygems", "gems", "matrix-push-gem-2.1.0.gem"],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        downloaded.status(),
        StatusCode::OK,
        "the pushed gem is not at the only path `gem install` will ask for"
    );
    assert_eq!(
        downloaded.bytes().await.unwrap().as_ref(),
        gem_file.as_slice()
    );

    // A body that is not a gem is refused before anything is stored.
    let rejected = client
        .post(package_url(&base, &["rubygems", "api", "v1", "gems"]))
        .header(reqwest::header::AUTHORIZATION, token.clone())
        .body(vec![0u8; 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);

    // And an anonymous push is a push nobody made.
    let anonymous = client
        .post(package_url(&base, &["rubygems", "api", "v1", "gems"]))
        .body(gem_file)
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), StatusCode::UNAUTHORIZED);
}

/// `dotnet restore` builds its dependency graph out of the registration index
/// and never opens the `.nupkg` to look for one, so a leaf without
/// `dependencyGroups` does not read as "unknown" — it reads as a package that
/// genuinely depends on nothing. Restore then goes green and the build fails on
/// a missing assembly instead (card_b21fb6511a25). `listed` is the other half:
/// its default is `true`, so a yanked version inherits candidacy unless the
/// leaf says otherwise.
#[tokio::test]
async fn nuget_registration_publishes_the_graph_and_the_availability_restore_reads() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let nuspec = br#"<?xml version="1.0"?>
<package><metadata>
  <id>Matrix.Deps</id>
  <version>1.0.0</version>
  <description>NuGet package with a dependency graph</description>
  <dependencies>
    <group targetFramework="net8.0">
      <dependency id="Newtonsoft.Json" version="13.0.1" />
      <dependency id="Serilog" version="[3.0.0, 4.0.0)" exclude="Build,Analyzers" />
    </group>
    <group targetFramework="netstandard2.0" />
  </dependencies>
</metadata></package>"#;

    let published = client
        .post(package_url(&base, &["nuget", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"Matrix.Deps.1.0.0.nupkg\"",
        )
        .body(zip_archive(&[("Matrix.Deps.nuspec", nuspec)]))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let registration = |client: reqwest::Client, base: String| async move {
        client
            .get(package_url(
                &base,
                &["nuget", "registration", "Matrix.Deps", "index.json"],
            ))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    };

    let index = registration(client.clone(), base.clone()).await;
    let catalog = &index["items"][0]["items"][0]["catalogEntry"];

    let groups = catalog["dependencyGroups"]
        .as_array()
        .unwrap_or_else(|| panic!("restore resolves against this key: {catalog}"));
    assert_eq!(groups.len(), 2, "both groups of the nuspec: {catalog}");
    assert_eq!(groups[0]["@type"], "PackageDependencyGroup");
    assert_eq!(groups[0]["targetFramework"], "net8.0");
    assert_eq!(
        groups[0]["dependencies"],
        serde_json::json!([
            { "@type": "PackageDependency", "id": "Newtonsoft.Json", "range": "13.0.1" },
            { "@type": "PackageDependency", "id": "Serilog", "range": "[3.0.0, 4.0.0)" },
        ]),
        "the range is passed through as the nuspec spelled it"
    );
    // A framework that needs nothing is a declaration, not an absence.
    assert_eq!(groups[1]["targetFramework"], "netstandard2.0");
    assert_eq!(groups[1]["dependencies"], serde_json::json!([]));
    assert_eq!(catalog["listed"], true);

    // Yanking has to reach the resolver: the version stays in the registration
    // so an already-locked consumer keeps restoring, and stops being a candidate.
    let yanked = client
        .patch(package_url(
            &base,
            &["nuget", "Matrix.Deps", "1.0.0", "yank"],
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "yank": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(yanked.status(), StatusCode::OK);

    let index = registration(client.clone(), base.clone()).await;
    let catalog = &index["items"][0]["items"][0]["catalogEntry"];
    assert_eq!(
        catalog["listed"], false,
        "a yanked version inherits `listed: true` unless the leaf says otherwise: {catalog}"
    );
    assert_eq!(
        catalog["version"], "1.0.0",
        "the yanked version must still be listed for a consumer that already resolved it"
    );

    // A nuspec that declares no dependencies says nothing, rather than
    // publishing an empty graph it never read.
    let bare = br#"<?xml version="1.0"?>
<package><metadata>
  <id>Matrix.Bare</id>
  <version>1.0.0</version>
  <description>No dependency block at all</description>
</metadata></package>"#;
    let published = client
        .post(package_url(&base, &["nuget", "publish"]))
        .bearer_auth(&token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            "attachment; filename=\"Matrix.Bare.1.0.0.nupkg\"",
        )
        .body(zip_archive(&[("Matrix.Bare.nuspec", bare)]))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);

    let index = client
        .get(package_url(
            &base,
            &["nuget", "registration", "Matrix.Bare", "index.json"],
        ))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    let catalog = &index["items"][0]["items"][0]["catalogEntry"];
    assert!(catalog["dependencyGroups"].is_null(), "{catalog}");
    assert_eq!(catalog["listed"], true);
}

/// The Simple page is where pip / uv / poetry choose a candidate, and they
/// choose on two attributes before they look at anything else: PEP 503's
/// `data-requires-python` and PEP 592's `data-yanked`. Neither used to be
/// emitted, so a 3.12-only wheel was offered to a 3.8 interpreter and a
/// withdrawn release was offered to everyone (card_5d5a0e91d324).
#[tokio::test]
async fn pypi_simple_page_states_what_a_resolver_filters_on() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    let publish = |filename: &'static str, body: Vec<u8>, expect: StatusCode| {
        let (client, token, base) = (client.clone(), token.clone(), base.clone());
        async move {
            let response = client
                .post(package_url(&base, &["pypi", "publish"]))
                .bearer_auth(&token)
                .header(
                    reqwest::header::CONTENT_DISPOSITION,
                    format!("attachment; filename=\"{filename}\""),
                )
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), expect, "publish {filename}");
        }
    };

    let wheel = |version: &str, requires_python: Option<&str>| {
        let constraint = requires_python
            .map(|spec| format!("Requires-Python: {spec}\n"))
            .unwrap_or_default();
        zip_archive(&[(
            &format!("matrix_pin-{version}.dist-info/METADATA"),
            format!(
                "Metadata-Version: 2.1\nName: matrix-pin\nVersion: {version}\n\
                 Summary: resolver attributes\n{constraint}"
            )
            .as_bytes(),
        )])
    };

    publish(
        "matrix_pin-1.0.0-py3-none-any.whl",
        wheel("1.0.0", Some(">=3.10")),
        StatusCode::CREATED,
    )
    .await;
    publish(
        "matrix_pin-1.1.0-py3-none-any.whl",
        wheel("1.1.0", Some(">=3.12")),
        StatusCode::CREATED,
    )
    .await;
    // Published without the header at all — the page must say nothing about it
    // rather than claim it runs anywhere.
    publish(
        "matrix_pin-0.9.0-py3-none-any.whl",
        wheel("0.9.0", None),
        StatusCode::CREATED,
    )
    .await;

    let simple_page = |client: reqwest::Client, base: String| async move {
        let page = client
            .get(package_url(&base, &["pypi", "simple", "matrix-pin", ""]))
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        page.text().await.unwrap()
    };
    let line_for = |page: &str, filename: &str| {
        page.lines()
            .find(|line| line.contains(filename))
            .unwrap_or_else(|| panic!("{filename} is missing from the Simple page:\n{page}"))
            .to_string()
    };

    let page = simple_page(client.clone(), base.clone()).await;
    let pinned = line_for(&page, "matrix_pin-1.0.0-py3-none-any.whl");
    assert!(
        pinned.contains("data-requires-python=\"&gt;=3.10\""),
        "the first filter a resolver applies is missing: {pinned}"
    );
    assert!(
        line_for(&page, "matrix_pin-1.1.0-py3-none-any.whl")
            .contains("data-requires-python=\"&gt;=3.12\""),
        "{page}"
    );
    let undeclared = line_for(&page, "matrix_pin-0.9.0-py3-none-any.whl");
    assert!(
        !undeclared.contains("data-requires-python"),
        "a distribution that declared nothing must not be given a constraint: {undeclared}"
    );
    assert!(
        !page.contains("data-yanked"),
        "nothing is yanked yet: {page}"
    );

    // Yanking has to reach the resolver. The file stays on the page — that is
    // what keeps `matrix-pin==1.1.0` resolvable — and the attribute is what
    // takes it out of every resolution that is not that exact pin.
    let yanked = client
        .patch(package_url(&base, &["pypi", "matrix-pin", "1.1.0", "yank"]))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "yank": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(yanked.status(), StatusCode::OK);

    let page = simple_page(client.clone(), base.clone()).await;
    let withdrawn = line_for(&page, "matrix_pin-1.1.0-py3-none-any.whl");
    assert!(
        withdrawn.contains("data-yanked=\"\""),
        "the yanked release is still offered as an ordinary candidate: {withdrawn}"
    );
    assert!(
        !line_for(&page, "matrix_pin-1.0.0-py3-none-any.whl").contains("data-yanked"),
        "{page}"
    );
    assert_eq!(page.matches("data-yanked").count(), 1, "{page}");
}

/// Bundler keys a candidate on `(number, platform)` and picks between a
/// pure-ruby gem and a native build with it, so answering a flat `ruby` for
/// every gem locks a native build as platform-independent and sends the client
/// after a `VERSION-PLATFORM` file that does not exist. `required_ruby_version`
/// is the other constraint it filters on, and the compact index published none
/// of it (card_0c9e858230b6).
#[tokio::test]
async fn rubygems_publishes_the_platform_and_interpreter_constraints_bundler_resolves_on() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "matrix-owner", "matrix-owner@example.com").await;
    create_repo(&base, &token, "matrix-repo").await;
    let client = reqwest::Client::new();

    // A native gem: `platform` is declared, and so is the interpreter it needs.
    let gem_metadata = br#"name: matrix-native
version: 1.0.0
platform: x86_64-linux
summary: a native build
required_ruby_version: !ruby/object:Gem::Requirement
  requirements:
  - - ">="
    - !ruby/object:Gem::Version
      version: '3.1'
"#;
    let gem_file = tar_archive(&[("metadata.gz", &gzip(gem_metadata))]);

    // Pushed the way `gem push` sends it — no filename on the wire, so the one
    // the registry derives has to carry the platform too.
    let pushed = client
        .post(package_url(&base, &["rubygems", "api", "v1", "gems"]))
        .header(reqwest::header::AUTHORIZATION, token.clone())
        .body(gem_file.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(pushed.status(), StatusCode::CREATED);

    // ── The compact index, which is what a modern client reads ─────────────
    let info = client
        .get(package_url(&base, &["rubygems", "info", "matrix-native"]))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let line = info
        .lines()
        .find(|line| line.starts_with("1.0.0"))
        .unwrap_or_else(|| panic!("the version is missing from the info file: {info}"));
    assert!(
        line.starts_with("1.0.0-x86_64-linux "),
        "the chunk the client turns into a download URL must carry the platform: {line}"
    );
    assert!(
        line.contains("ruby:>= 3.1"),
        "a resolver with no interpreter constraint installs this on 2.7: {line}"
    );

    // ...and the file really is at the path that chunk builds.
    let downloaded = client
        .get(package_url(
            &base,
            &["rubygems", "gems", "matrix-native-1.0.0-x86_64-linux.gem"],
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(
        downloaded.status(),
        StatusCode::OK,
        "the platform chunk names a file the registry does not serve"
    );
    assert_eq!(
        downloaded.bytes().await.unwrap().as_ref(),
        gem_file.as_slice()
    );

    // ── The two JSON endpoints, which used to answer `ruby` for everything ──
    let mut deps_url = package_url(&base, &["rubygems", "api", "v1", "dependencies"]);
    deps_url
        .query_pairs_mut()
        .append_pair("gems", "matrix-native");
    let deps: serde_json::Value = client
        .get(deps_url.clone())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(deps[0]["platform"], "x86_64-linux", "{deps}");

    let gem_info: serde_json::Value = client
        .get(package_url(
            &base,
            &["rubygems", "api", "v1", "gems", "matrix-native.json"],
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        gem_info["versions"]["1.0.0"]["platform"], "x86_64-linux",
        "{gem_info}"
    );

    // ── A pure-ruby gem keeps the default, and no `-ruby` suffix ───────────
    let pure = tar_archive(&[(
        "metadata.gz",
        &gzip(b"name: matrix-pure\nversion: 2.0.0\nplatform: ruby\nsummary: pure\n"),
    )]);
    let pushed = client
        .post(package_url(&base, &["rubygems", "api", "v1", "gems"]))
        .header(reqwest::header::AUTHORIZATION, token.clone())
        .body(pure)
        .send()
        .await
        .unwrap();
    assert_eq!(pushed.status(), StatusCode::CREATED);

    let info = client
        .get(package_url(&base, &["rubygems", "info", "matrix-pure"]))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        info.lines().any(|line| line.starts_with("2.0.0 ")),
        "a pure-ruby gem must not grow a `-ruby` suffix: {info}"
    );
    assert!(!info.contains("ruby:"), "nothing was declared: {info}");

    // ── Yanking reaches both JSON endpoints too ────────────────────────────
    let yanked = client
        .patch(package_url(
            &base,
            &["rubygems", "matrix-native", "1.0.0", "yank"],
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "yank": true }))
        .send()
        .await
        .unwrap();
    assert_eq!(yanked.status(), StatusCode::OK);

    let deps: serde_json::Value = client
        .get(deps_url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        deps,
        serde_json::json!([]),
        "the dependency API still offers a withdrawn version as a candidate"
    );

    let gem_info: serde_json::Value = client
        .get(package_url(
            &base,
            &["rubygems", "api", "v1", "gems", "matrix-native.json"],
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        gem_info["versions"]["1.0.0"].is_null(),
        "the gem info API still offers a withdrawn version: {gem_info}"
    );
}
