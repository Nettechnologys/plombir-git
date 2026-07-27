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

struct Fixture {
    base: String,
    client: reqwest::Client,
    _db: rg_db::DatabaseConnection,
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

    Fixture {
        base,
        client,
        _db: db,
    }
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
        _db: db,
    };

    let (status, body) = fx.get(&fx.simple("/")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body.contains("<title>Simple index</title>"), "{body}");
}
