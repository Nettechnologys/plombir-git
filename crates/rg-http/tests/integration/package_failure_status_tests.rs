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
            None,
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

fn assert_sanitized_server_error(label: &str, status: StatusCode, body: &str) {
    assert!(
        status.is_server_error(),
        "{}: broken storage must be a 5xx, got {status}: {body}",
        label
    );
    let json: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|error| {
        panic!(
            "{}: server error must use the sanitized JSON envelope ({error}): {body}",
            label
        )
    });
    assert_eq!(
        json["error"]["message"], "Internal server error",
        "{}: internal database detail reached the client: {body}",
        label
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
        assert_sanitized_server_error(shape.label(), failed_status, &failed_body);
    }
}

#[derive(Clone, Copy)]
struct ResolverCase {
    package_type: &'static str,
    package_name: &'static str,
    read_path: &'static str,
}

const RESOLVER_CASES: [ResolverCase; 2] = [
    ResolverCase {
        package_type: "cargo",
        package_name: "metadata-cargo",
        read_path: "packages/cargo/index/metadata-cargo",
    },
    ResolverCase {
        package_type: "npm",
        package_name: "metadata-npm",
        read_path: "packages/npm/metadata-npm",
    },
];

async fn seed_version_without_metadata(fixture: &Fixture, case: ResolverCase) -> i64 {
    let registry = rg_db::ops::package_registry_ops::find_or_create(
        &fixture.db,
        fixture.repo_id,
        case.package_type,
    )
    .await
    .unwrap_or_else(|error| panic!("{} registry: {error}", case.package_type));
    let package = rg_db::ops::package_ops::create(
        &fixture.db,
        registry.id,
        fixture.owner_id,
        case.package_name,
        None,
        None,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("{} package: {error}", case.package_type));
    rg_db::ops::package_version_ops::create(
        &fixture.db,
        package.id,
        "1.0.0",
        None,
        Some("1.0.0"),
        None,
        7,
        None,
        Some(fixture.owner_id),
    )
    .await
    .unwrap_or_else(|error| panic!("{} version: {error}", case.package_type))
    .id
}

async fn resolver_response(fixture: &Fixture, case: ResolverCase) -> (StatusCode, String) {
    let response = fixture
        .client
        .get(format!(
            "{}/api/v1/repos/{OWNER}/{REPO}/{}",
            fixture.base, case.read_path
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("{} metadata request: {error}", case.package_type));
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| panic!("{} metadata body: {error}", case.package_type));
    (status, body)
}

fn assert_legacy_empty_graph(case: ResolverCase, body: &str) {
    let document: serde_json::Value = serde_json::from_str(body).unwrap_or_else(|error| {
        panic!(
            "{} metadata is not JSON ({error}): {body}",
            case.package_type
        )
    });
    match case.package_type {
        "cargo" => {
            assert_eq!(document["deps"], serde_json::json!([]), "{body}");
            assert_eq!(document["features"], serde_json::json!({}), "{body}");
        }
        "npm" => assert_eq!(
            document["versions"]["1.0.0"]["dependencies"],
            serde_json::json!({}),
            "{body}"
        ),
        other => panic!("unhandled resolver protocol {other}"),
    }
}

/// Acceptance for card_230f5e4a8cdd: the same stored version is first read as
/// a documented legacy row, then made unreadable behind the HTTP route. The
/// second request must fail closed instead of preserving the plausible empty
/// dependency graph from the first response.
#[tokio::test]
async fn corrupt_cargo_and_npm_metadata_is_not_served_as_an_empty_dependency_graph() {
    for case in RESOLVER_CASES {
        let fixture = fixture(FailureShape::Raw404).await;
        let version_id = seed_version_without_metadata(&fixture, case).await;

        let (legacy_status, legacy_body) = resolver_response(&fixture, case).await;
        assert_eq!(
            legacy_status,
            StatusCode::OK,
            "{}: absent stored metadata keeps the selected legacy response: {legacy_body}",
            case.package_type
        );
        assert_legacy_empty_graph(case, &legacy_body);

        let updated = fixture
            .db
            .execute_unprepared(&format!(
                "UPDATE package_versions SET metadata = '{{broken-json' WHERE id = {version_id}"
            ))
            .await
            .unwrap_or_else(|error| {
                panic!("{}: corrupt stored metadata: {error}", case.package_type)
            });
        assert_eq!(updated.rows_affected(), 1, "{} fixture", case.package_type);

        let (failed_status, failed_body) = resolver_response(&fixture, case).await;
        assert_sanitized_server_error(case.package_type, failed_status, &failed_body);
        assert!(
            !failed_body.contains("broken-json"),
            "{}: the unreadable stored blob leaked to the client: {failed_body}",
            case.package_type
        );
    }
}

#[derive(Clone, Copy)]
struct ProtocolMetadataCase {
    package_type: &'static str,
    package_name: &'static str,
    read_path: &'static str,
    valid_metadata: &'static str,
    valid_marker: &'static str,
}

const PROTOCOL_METADATA_CASES: [ProtocolMetadataCase; 7] = [
    ProtocolMetadataCase {
        package_type: "helm",
        package_name: "metadata-helm",
        read_path: "packages/helm/index.yaml",
        valid_metadata: r#"{"apiVersion":"v2","dependencies":[{"name":"metadata-chart-dep","version":"1.0.0"}]}"#,
        valid_marker: "metadata-chart-dep",
    },
    ProtocolMetadataCase {
        package_type: "nuget",
        package_name: "metadata-nuget",
        read_path: "packages/nuget/registration/metadata-nuget/index.json",
        valid_metadata: r#"{"dependencyGroups":[{"targetFramework":"net8.0","dependencies":[{"id":"Metadata.NuGet.Dep","range":"[1.0.0]"}]}]}"#,
        valid_marker: "Metadata.NuGet.Dep",
    },
    ProtocolMetadataCase {
        package_type: "pypi",
        package_name: "metadata-pypi",
        read_path: "packages/pypi/simple/metadata-pypi/",
        valid_metadata: r#"{"requires_python":">=3.10"}"#,
        valid_marker: "data-requires-python",
    },
    ProtocolMetadataCase {
        package_type: "rubygems",
        package_name: "metadata-rubygems-deps",
        read_path: "packages/rubygems/api/v1/dependencies.json?gems=metadata-rubygems-deps",
        valid_metadata: r#"{"dependencies":[{"name":"metadata-ruby-dep","requirements":">= 2"}]}"#,
        valid_marker: "metadata-ruby-dep",
    },
    ProtocolMetadataCase {
        package_type: "rubygems",
        package_name: "metadata-rubygems-info",
        read_path: "packages/rubygems/api/v1/gems/metadata-rubygems-info.json",
        valid_metadata: r#"{"summary":"metadata ruby summary"}"#,
        valid_marker: "metadata ruby summary",
    },
    ProtocolMetadataCase {
        package_type: "rubygems",
        package_name: "metadata-rubygems-versions",
        read_path: "packages/rubygems/versions",
        valid_metadata: r#"{"platform":"x86_64-linux","dependencies":[]}"#,
        valid_marker: "1.0.0-x86_64-linux",
    },
    ProtocolMetadataCase {
        package_type: "rubygems",
        package_name: "metadata-rubygems-compact",
        read_path: "packages/rubygems/info/metadata-rubygems-compact",
        valid_metadata: r#"{"dependencies":[{"name":"metadata-compact-dep","requirements":">= 1"}],"required_ruby_version":">= 3.1"}"#,
        valid_marker: "metadata-compact-dep",
    },
];

async fn seed_protocol_version_without_metadata(
    fixture: &Fixture,
    case: ProtocolMetadataCase,
) -> i64 {
    let registry = rg_db::ops::package_registry_ops::find_or_create(
        &fixture.db,
        fixture.repo_id,
        case.package_type,
    )
    .await
    .unwrap_or_else(|error| panic!("{} registry: {error}", case.package_type));
    let package = rg_db::ops::package_ops::create(
        &fixture.db,
        registry.id,
        fixture.owner_id,
        case.package_name,
        None,
        None,
        None,
    )
    .await
    .unwrap_or_else(|error| panic!("{} package: {error}", case.package_type));
    rg_db::ops::package_version_ops::create(
        &fixture.db,
        package.id,
        "1.0.0",
        None,
        Some("1.0.0"),
        None,
        7,
        None,
        Some(fixture.owner_id),
    )
    .await
    .unwrap_or_else(|error| panic!("{} version: {error}", case.package_type))
    .id
}

async fn protocol_metadata_response(
    fixture: &Fixture,
    case: ProtocolMetadataCase,
) -> (StatusCode, String) {
    let response = fixture
        .client
        .get(format!(
            "{}/api/v1/repos/{OWNER}/{REPO}/{}",
            fixture.base, case.read_path
        ))
        .send()
        .await
        .unwrap_or_else(|error| panic!("{} metadata request: {error}", case.package_type));
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|error| panic!("{} metadata body: {error}", case.package_type));
    (status, body)
}

async fn replace_protocol_metadata(fixture: &Fixture, version_id: i64, metadata: &str) {
    assert!(
        !metadata.contains('\''),
        "test metadata must remain safe for the literal fixture update"
    );
    let updated = fixture
        .db
        .execute_unprepared(&format!(
            "UPDATE package_versions SET metadata = '{metadata}' WHERE id = {version_id}"
        ))
        .await
        .expect("replace stored protocol metadata");
    assert_eq!(updated.rows_affected(), 1, "metadata fixture row");
}

/// Acceptance for card_a4be92713930: every affected real route first proves
/// that a legacy `NULL` and a healthy JSON object are readable, then rejects
/// malformed JSON and a valid JSON value of the wrong top-level shape.
#[tokio::test]
async fn corrupt_protocol_metadata_never_becomes_a_plausible_partial_index() {
    for case in PROTOCOL_METADATA_CASES {
        let fixture = fixture(FailureShape::Raw404).await;
        let version_id = seed_protocol_version_without_metadata(&fixture, case).await;

        let (legacy_status, legacy_body) = protocol_metadata_response(&fixture, case).await;
        assert_eq!(
            legacy_status,
            StatusCode::OK,
            "{}: NULL metadata must keep the selected legacy response: {legacy_body}",
            case.read_path
        );
        assert!(
            legacy_body.contains("1.0.0"),
            "{}: legacy response did not include the seeded version: {legacy_body}",
            case.read_path
        );

        replace_protocol_metadata(&fixture, version_id, case.valid_metadata).await;
        let (valid_status, valid_body) = protocol_metadata_response(&fixture, case).await;
        assert_eq!(
            valid_status,
            StatusCode::OK,
            "{}: valid metadata was rejected: {valid_body}",
            case.read_path
        );
        assert!(
            valid_body.contains(case.valid_marker),
            "{}: healthy metadata field was not served: {valid_body}",
            case.read_path
        );

        for damaged_metadata in ["{broken-json", "[]"] {
            replace_protocol_metadata(&fixture, version_id, damaged_metadata).await;
            let (failed_status, failed_body) = protocol_metadata_response(&fixture, case).await;
            assert_sanitized_server_error(case.read_path, failed_status, &failed_body);
            assert!(
                !failed_body.contains(damaged_metadata),
                "{}: stored metadata leaked to the client: {failed_body}",
                case.read_path
            );
        }
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
        None,
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
