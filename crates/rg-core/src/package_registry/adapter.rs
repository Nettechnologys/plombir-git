//! Package protocol adapter trait.
//!
//! Each package type (cargo, npm, docker, etc.) has its own protocol-specific
//! logic for metadata extraction, validation, content types, and protocol
//! endpoints.  Adapters implement this trait so the registry can serve
//! protocol-native clients (e.g. `cargo publish`, `npm install`).

/// Metadata extracted from a package file during publishing.
#[derive(Debug, Clone)]
pub struct ExtractedMetadata {
    /// Package name as declared in the package manifest.
    pub name: String,
    /// Version as declared in the package manifest.
    pub version: String,
    /// Human-readable description.
    pub description: Option<String>,
    /// Homepage URL.
    pub homepage: Option<String>,
    /// Repository URL.
    pub repository_url: Option<String>,
    /// Keywords / tags (comma-separated or JSON array string).
    pub keywords: Option<String>,
    /// License identifier.
    pub license: Option<String>,
    /// semver-compatible version (if different from `version`).
    pub semver: Option<String>,
}

/// Trait implemented by every package-type adapter.
pub trait PackageAdapter: Send + Sync {
    /// The package type constant (e.g. `"cargo"`, `"npm"`).
    fn package_type() -> &'static str
    where
        Self: Sized;

    /// Extract metadata from the raw package file bytes.
    ///
    /// `filename` is the original file name (e.g. `mycrate-0.1.0.crate`).
    /// `data` is the complete file content.
    fn extract_metadata(&self, filename: &str, data: &[u8]) -> anyhow::Result<ExtractedMetadata>;

    /// Validate that the file is a well-formed package of this type.
    /// Returns `Ok(())` if valid, or an error describing the problem.
    fn validate(&self, data: &[u8]) -> anyhow::Result<()>;

    /// Content-Type to use when serving a file download to a generic client.
    fn content_type_for_file(&self, filename: &str) -> String;

    /// Default Content-Type for downloads of this package type.
    fn default_content_type(&self) -> &'static str {
        "application/octet-stream"
    }

    /// Whether this adapter requires a specific index/protocol endpoint
    /// (e.g. Cargo sparse index, npm registry JSON).
    fn has_protocol_endpoint(&self) -> bool {
        false
    }
}

/// Boxed adapter for type-erased storage.
pub type BoxedAdapter = Box<dyn PackageAdapter>;

/// Get the adapter for a given package type, if one is registered.
pub fn get_adapter(package_type: &str) -> Option<BoxedAdapter> {
    match package_type {
        "cargo" => Some(Box::new(super::adapters::CargoAdapter)),
        "npm" => Some(Box::new(super::adapters::NpmAdapter)),
        "nuget" => Some(Box::new(super::adapters::NuGetAdapter)),
        "pypi" => Some(Box::new(super::adapters::PyPIAdapter)),
        "rubygems" => Some(Box::new(super::adapters::RubyGemsAdapter)),
        "maven" => Some(Box::new(super::adapters::MavenAdapter)),
        "docker" => Some(Box::new(super::adapters::DockerAdapter)),
        "generic" => Some(Box::new(super::adapters::GenericAdapter)),
        "helm" => Some(Box::new(super::adapters::HelmAdapter)),
        "composer" => Some(Box::new(super::adapters::ComposerAdapter)),
        // Other types fall back to generic
        _ => {
            if package_type != "generic" {
                tracing::debug!(
                    "no specific adapter for '{}', falling back to generic",
                    package_type
                );
            }
            Some(Box::new(super::adapters::GenericAdapter))
        }
    }
}
