//! End-to-end publish, metadata and download coverage for native package formats.

mod common;

use std::io::{Cursor, Write};

use common::{create_repo, register_full, spawn_test_app_with_db};
use flate2::write::GzEncoder;
use flate2::Compression;
use reqwest::StatusCode;

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

    let protocol_checks = [
        (vec!["cargo", "index", "matrix-cargo"], "\"vers\":\"1.0.0\""),
        (vec!["npm", "matrix-npm"], "\"matrix-npm\""),
        (vec!["pypi", "simple", "matrix-pypi"], "matrix_pypi-1.0.0"),
        (
            vec!["maven", "com.example", "matrix-maven", "maven-metadata.xml"],
            "<version>1.0.0</version>",
        ),
        (
            vec!["nuget", "registration", "Matrix.NuGet", "index.json"],
            "1.0.0",
        ),
        (
            vec!["rubygems", "api", "v1", "gems", "matrix-gem.json"],
            "1.0.0",
        ),
        (vec!["helm", "index.yaml"], "matrix-helm"),
        (vec!["composer", "packages.json"], "vendor/matrix-composer"),
    ];
    for (segments, expected) in protocol_checks {
        let response = client
            .get(package_url(&base, &segments))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{segments:?}");
        let body = response.text().await.unwrap();
        assert!(body.contains(expected), "{segments:?}: {body}");
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
