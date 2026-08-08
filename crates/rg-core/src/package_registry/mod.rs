//! Package registry — OCI, npm, PyPI, Maven, Cargo, NuGet, Helm, RubyGems, Go, Composer, Generic.
pub mod adapter;
pub mod adapters;
pub mod oci;
pub mod service;
pub mod storage;
pub mod url_path;

pub use adapter::{get_adapter, ExtractedMetadata, PackageAdapter};
pub use adapters::cargo::{
    build_cargo_index_config, build_sparse_index, build_sparse_index_entry, cargo_index_prefix,
    CargoIndexVersion,
};
pub use adapters::helm::{build_helm_index, HelmIndexEntry};
pub use adapters::maven::{build_maven_metadata_xml, MavenVersionEntry};
pub use adapters::npm::{build_npm_metadata, NpmVersionInfo};
pub use adapters::nuget::{
    build_autocomplete_results, build_flat_container_index, build_registration_index,
    build_search_results, build_service_index, normalize_package_id, NuGetRegistrationEntry,
    NuGetSearchResult,
};
pub use adapters::pypi::{
    build_simple_repository_html, build_simple_root_html, normalize_project_name, PyPIProjectEntry,
    PyPIVersionEntry,
};
pub use adapters::rubygems::{
    build_compact_index_info, build_compact_index_names, build_compact_index_versions,
    build_dependencies_json, build_gem_info_json, compact_index_info_checksum, CompactIndexGem,
    CompactIndexVersion, RubyGemsDep, RubyGemsDependencyEntry, RubyGemsVersionEntry,
};
pub use service::{
    package_types, FileDetail, PackageDetail, PackageSummary, PublishInfo, PublishResult,
    VersionDetail,
};
pub use storage::PackageStorage;
pub use url_path::encode_path_segment;
