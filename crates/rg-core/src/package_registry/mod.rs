//! Package registry — OCI, npm, PyPI, Maven, Cargo, NuGet, Helm, RubyGems, Go, Composer, Generic.
pub mod adapter;
pub mod adapters;
pub mod oci;
pub mod service;
pub mod storage;

pub use adapter::{get_adapter, ExtractedMetadata, PackageAdapter};
pub use adapters::cargo::{build_sparse_index, build_sparse_index_entry};
pub use adapters::helm::{build_helm_index, HelmIndexEntry};
pub use adapters::maven::{build_maven_metadata_xml, MavenVersionEntry};
pub use adapters::npm::{build_npm_metadata, NpmVersionInfo};
pub use adapters::nuget::{
    build_registration_index, build_search_results, build_service_index, NuGetRegistrationEntry,
    NuGetSearchResult,
};
pub use adapters::pypi::{build_simple_repository_html, PyPIVersionEntry};
pub use adapters::rubygems::{
    build_dependencies_json, build_gem_info_json, RubyGemsDep, RubyGemsDependencyEntry,
    RubyGemsVersionEntry,
};
pub use service::{
    package_types, FileDetail, PackageDetail, PackageSummary, PublishInfo, PublishResult,
    VersionDetail,
};
pub use storage::PackageStorage;
