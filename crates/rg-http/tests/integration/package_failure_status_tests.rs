//! Package protocol indexes must distinguish absence from a failed lookup.
//!
//! The handlers in `api::packages` used every one of the failure shapes this
//! phase is meant to retire: a raw 404 carrying the database error, a valid
//! empty 200, and a partial aggregate that silently skipped one package. These
//! tests break the table *behind* the repository read gate so a green response
//! proves the handler ran and classified its own failure.

use crate::common::source_scan;
use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use reqwest::StatusCode;
use sea_orm::ConnectionTrait;

const OWNER: &str = "package-failure-owner";
const REPO: &str = "package-failure-repo";

#[derive(Clone, Copy)]
enum FailureShape {
    Raw404,
    Empty200,
    Partial200,
}

impl FailureShape {
    fn label(self) -> &'static str {
        match self {
            Self::Raw404 => "raw 404",
            Self::Empty200 => "empty 200",
            Self::Partial200 => "partial 200",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Raw404 => "packages/cargo/index/missing",
            Self::Empty200 => "packages/pypi/simple/",
            Self::Partial200 => "packages/helm/index.yaml",
        }
    }

    fn broken_table(self) -> &'static str {
        match self {
            Self::Raw404 | Self::Empty200 => "package_registry",
            // `list_packages` does not read this table, but the per-package
            // `list_versions` inside the aggregate does. That reaches the old
            // `continue` branch instead of failing the aggregate up front.
            Self::Partial200 => "package_files",
        }
    }
}

struct Fixture {
    base: String,
    client: reqwest::Client,
    db: rg_db::DatabaseConnection,
    repo_id: i64,
    owner_id: i64,
}

impl Fixture {
    fn url(&self, shape: FailureShape) -> String {
        format!("{}/api/v1/repos/{OWNER}/{REPO}/{}", self.base, shape.path())
    }

    async fn get(&self, shape: FailureShape) -> (StatusCode, String) {
        let response = self.client.get(self.url(shape)).send().await.unwrap();
        let status = response.status();
        (status, response.text().await.unwrap())
    }
}

async fn fixture(shape: FailureShape) -> Fixture {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, owner_id) = register_full(&base, OWNER, "package-failure-owner@example.test").await;
    let repo_id = create_repo(&base, &token, REPO).await;

    if matches!(shape, FailureShape::Partial200) {
        let registry = rg_db::ops::package_registry_ops::find_or_create(&db, repo_id, "helm")
            .await
            .expect("create Helm registry fixture");
        let package = rg_db::ops::package_ops::create(
            &db,
            registry.id,
            owner_id,
            "matrix-chart",
            Some("chart kept visible by the healthy aggregate"),
            None,
            None,
        )
        .await
        .expect("create Helm package fixture");
        rg_db::ops::package_version_ops::create(
            &db,
            package.id,
            "1.0.0",
            Some("1.0.0"),
            None,
            7,
            None,
            Some(owner_id),
        )
        .await
        .expect("create Helm version fixture");
    }

    Fixture {
        base,
        client: reqwest::Client::new(),
        db,
        repo_id,
        owner_id,
    }
}

fn assert_sanitized_server_error(shape: FailureShape, status: StatusCode, body: &str) {
    assert!(
        status.is_server_error(),
        "{}: broken storage must be a 5xx, got {status}: {body}",
        shape.label()
    );
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|error| {
        panic!(
            "{}: server error must use the sanitized JSON envelope ({error}): {body}",
            shape.label()
        )
    });
    assert_eq!(
        json["error"]["message"],
        "Internal server error",
        "{}: internal database detail reached the client: {body}",
        shape.label()
    );
}

/// Acceptance for card_e916d756af35: exercise one route from every response
/// shape in one table, with a healthy absence/data baseline before the fault.
#[tokio::test]
async fn package_read_failures_are_never_404_or_empty_partial_success() {
    for shape in [
        FailureShape::Raw404,
        FailureShape::Empty200,
        FailureShape::Partial200,
    ] {
        let fixture = fixture(shape).await;
        let (healthy_status, healthy_body) = fixture.get(shape).await;
        match shape {
            FailureShape::Raw404 => assert_eq!(
                healthy_status,
                StatusCode::NOT_FOUND,
                "genuine package absence stays a 404: {healthy_body}"
            ),
            FailureShape::Empty200 => {
                assert_eq!(healthy_status, StatusCode::OK, "{healthy_body}");
                assert!(
                    healthy_body.contains("<title>Simple index</title>"),
                    "an empty PyPI registry must still be a valid index: {healthy_body}"
                );
            }
            FailureShape::Partial200 => {
                assert_eq!(healthy_status, StatusCode::OK, "{healthy_body}");
                assert!(
                    healthy_body.contains("matrix-chart"),
                    "healthy aggregate must include its package: {healthy_body}"
                );
            }
        }

        fixture
            .db
            .execute_unprepared(&format!("DROP TABLE {}", shape.broken_table()))
            .await
            .unwrap_or_else(|error| panic!("{}: inject table failure: {error}", shape.label()));

        let (failed_status, failed_body) = fixture.get(shape).await;
        assert_sanitized_server_error(shape, failed_status, &failed_body);
    }
}

/// A database row may survive a manually removed object or a failed restore.
/// That is genuine file absence (404), not an internal error; other blob
/// failures still pass through the shared 5xx classifier.
#[tokio::test]
async fn a_missing_package_blob_is_a_sanitized_404() {
    let fixture = fixture(FailureShape::Raw404).await;
    let registry =
        rg_db::ops::package_registry_ops::find_or_create(&fixture.db, fixture.repo_id, "generic")
            .await
            .expect("create generic registry");
    let package = rg_db::ops::package_ops::create(
        &fixture.db,
        registry.id,
        fixture.owner_id,
        "sample",
        None,
        None,
        None,
    )
    .await
    .expect("create package");
    let version = rg_db::ops::package_version_ops::create(
        &fixture.db,
        package.id,
        "1.0.0",
        Some("1.0.0"),
        None,
        7,
        None,
        Some(fixture.owner_id),
    )
    .await
    .expect("create version");
    rg_db::ops::package_file_ops::create(
        &fixture.db,
        version.id,
        "sample.bin",
        7,
        rg_db::ops::package_file_ops::FileDigests::default(),
        "packages/missing/sample.bin",
    )
    .await
    .expect("create package file row");

    let response = fixture
        .client
        .get(format!(
            "{}/api/v1/repos/{OWNER}/{REPO}/packages/generic/sample/1.0.0/sample.bin",
            fixture.base
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let body: serde_json::Value = response.json().await.expect("404 JSON envelope");
    assert_eq!(body["error"]["message"], "package file not found");
}

/// A guard over every site found by the indexed sweep. The three dynamic cases
/// above prove behavior; this census makes a newly reintroduced local 404,
/// default or `continue` fail even when its route has no publish fixture here.
#[test]
fn every_package_read_failure_reaches_the_shared_classifier() {
    let source = include_str!("../../src/api/packages.rs");
    for handler in [
        "get_package",
        "list_versions",
        "get_version",
        "serve_package_file",
        "cargo_sparse_index",
        "npm_registry_metadata",
        "pypi_simple_root_index",
        "maven_metadata",
        "nuget_registration_index",
        "rubygems_dependencies",
        "rubygems_gem_info",
        "rubygems_compact_versions",
        "rubygems_compact_info",
        "rubygems_compact_names",
        "rubygems_gem_download",
        "helm_index",
        "composer_packages_json",
    ] {
        assert_eq!(
            source_scan::reaches_any(
                source,
                handler,
                &["package_error_response", "package_file_error_response"]
            ),
            Some(true),
            "{handler} can consume a storage result without reaching the shared error classifier"
        );
    }
}
