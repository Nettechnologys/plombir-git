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
    /// Everything the protocol needs that has no column of its own — stored
    /// verbatim in `package_version.metadata` and read back by the protocol
    /// handler for this package type.
    ///
    /// The fields above are the ones the *registry* understands (they end up on
    /// the package row and in the generic API); a gemspec's dependency list, a
    /// nuspec's tags or a chart's `appVersion` are meaningful only to the one
    /// client that asks for them, so they travel as a JSON object whose keys are
    /// whatever that client's endpoint parses. An adapter with nothing
    /// protocol-specific to carry leaves this `None`.
    pub protocol_metadata: Option<String>,
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

    /// Whether a successful [`extract_metadata`](PackageAdapter::extract_metadata)
    /// means the artifact stated its own identity.
    ///
    /// A publish request carries the coordinates twice: the caller names them
    /// in `?name=&version=` and the artifact declares them in its manifest.
    /// When the manifest is authoritative the two must agree, and a
    /// disagreement is refused instead of being resolved in the caller's
    /// favour — otherwise a real `serde.nupkg` publishes as whatever the query
    /// string says, carrying the nuspec of a package it is not.
    ///
    /// It is `false` for the formats whose successful extraction does *not*
    /// prove a manifest was read:
    /// - `GenericAdapter` returns empty coordinates by design;
    /// - `MavenAdapter` falls back to parsing the filename, and one Maven
    ///   version is several artifacts of which only the `.pom` carries
    ///   coordinates at all;
    /// - `DockerAdapter` refuses this route outright.
    ///
    /// Answer it explicitly in every adapter. The default is the permissive
    /// one because it is the only safe default for a format nobody has
    /// classified yet, which is exactly why leaving it implicit is a mistake —
    /// `every_registered_adapter_states_its_manifest_authority` pins the whole
    /// census so a new adapter cannot inherit it by accident.
    fn manifest_is_authoritative(&self) -> bool {
        false
    }
}

/// Boxed adapter for type-erased storage.
pub type BoxedAdapter = Box<dyn PackageAdapter>;

macro_rules! register_adapters {
    ($($package_type:literal => $adapter:path),+ $(,)?) => {
        /// Package types with a dedicated adapter rather than the generic fallback.
        ///
        /// Generated from the same declaration as [`get_adapter`], so contract
        /// tests can derive their census from production without maintaining a
        /// second registry that can silently omit a new adapter.
        pub const REGISTERED_ADAPTER_TYPES: &[&str] = &[$($package_type),+];

        /// Get the adapter for a given package type, if one is registered.
        pub fn get_adapter(package_type: &str) -> Option<BoxedAdapter> {
            match package_type {
                $($package_type => Some(Box::new($adapter)),)+
                // Other types fall back to generic.
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

    };
}

#[cfg(test)]
mod manifest_authority_tests {
    use super::{get_adapter, REGISTERED_ADAPTER_TYPES};

    /// The census, not a sample. `manifest_is_authoritative` has a default, so
    /// a new adapter that never answers it inherits the permissive one and its
    /// artifacts silently go back to being publishable under any coordinates
    /// the query string names. Pinning every registered type here means adding
    /// an adapter fails this test until somebody decides which side it is on.
    #[test]
    fn every_registered_adapter_states_its_manifest_authority() {
        let expected: &[(&str, bool)] = &[
            ("cargo", true),
            ("npm", true),
            ("nuget", true),
            ("pypi", true),
            ("rubygems", true),
            ("maven", false),
            ("docker", false),
            ("generic", false),
            ("helm", true),
            ("composer", true),
        ];

        let declared: Vec<&str> = expected.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            REGISTERED_ADAPTER_TYPES, declared,
            "a package adapter was registered or removed without stating whether its manifest \
             is authoritative"
        );

        for (package_type, authoritative) in expected {
            let adapter = get_adapter(package_type).expect("every registered type has an adapter");
            assert_eq!(
                adapter.manifest_is_authoritative(),
                *authoritative,
                "{package_type} changed its manifest authority"
            );
        }
    }
}

register_adapters! {
    "cargo" => super::adapters::CargoAdapter,
    "npm" => super::adapters::NpmAdapter,
    "nuget" => super::adapters::NuGetAdapter,
    "pypi" => super::adapters::PyPIAdapter,
    "rubygems" => super::adapters::RubyGemsAdapter,
    "maven" => super::adapters::MavenAdapter,
    "docker" => super::adapters::DockerAdapter,
    "generic" => super::adapters::GenericAdapter,
    "helm" => super::adapters::HelmAdapter,
    "composer" => super::adapters::ComposerAdapter,
}
