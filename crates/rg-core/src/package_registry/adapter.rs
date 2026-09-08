//! Package protocol adapter trait.
//!
//! Each package type (cargo, npm, docker, etc.) has its own protocol-specific
//! logic for metadata extraction, validation, content types, and protocol
//! endpoints.  Adapters implement this trait so the registry can serve
//! protocol-native clients (e.g. `cargo publish`, `npm install`).

use super::artifact::PackageArtifact;

/// Metadata extracted from a package file during publishing.
#[derive(Debug, Clone)]
pub struct ExtractedMetadata {
    /// Package name as declared in the package manifest.
    pub name: String,
    /// Version as declared in the package manifest.
    pub version: String,
    /// Whether [`name`](Self::name) and [`version`](Self::version) were *read
    /// from a manifest inside the artifact*, as opposed to guessed from its
    /// filename or left empty.
    ///
    /// [`PackageAdapter::manifest_is_authoritative`] answers this for a whole
    /// format, which is the wrong grain wherever one format has both kinds of
    /// artifact. Maven is exactly that: a `.pom` states its coordinates, while
    /// `matrix-1.0.0-sources.jar` carries no manifest at all and has to take
    /// them from the request. Asking the adapter forces one answer for both, so
    /// the whole format was left permissive and a `.pom` uploaded to somebody
    /// else's path published under that path's coordinates (card_13cadc8a9d7a).
    ///
    /// This field is the per-extraction half of the same question. There is no
    /// default for it on purpose: the struct has no `Default`, so a new adapter
    /// does not compile until it says which kind of extraction it performed.
    pub coordinates_from_manifest: bool,
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

    /// Extract metadata from the uploaded artifact.
    ///
    /// `filename` is the original file name (e.g. `mycrate-0.1.0.crate`).
    /// `artifact` is the complete file content — read it through
    /// [`PackageArtifact::reader`] rather than materialising it, because on a
    /// publish route it is a spooled upload the size of the configured
    /// artifact ceiling.
    fn extract_metadata(
        &self,
        filename: &str,
        artifact: &PackageArtifact,
    ) -> anyhow::Result<ExtractedMetadata>;

    /// Validate that the file is a well-formed package of this type.
    /// Returns `Ok(())` if valid, or an error describing the problem.
    fn validate(&self, artifact: &PackageArtifact) -> anyhow::Result<()>;

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

    /// Whether an extraction that read this format's manifest may be trusted
    /// over the coordinates the request names.
    ///
    /// A publish request carries the coordinates twice: the caller names them
    /// in `?name=&version=` and the artifact declares them in its manifest.
    /// Where the manifest is authoritative the two must agree, and a
    /// disagreement is refused instead of being resolved in the caller's
    /// favour — otherwise a real `serde.nupkg` publishes as whatever the query
    /// string says, carrying the nuspec of a package it is not.
    ///
    /// This is the *format-level* half of the question, and it is only half.
    /// Whether a particular upload actually stated its identity is answered by
    /// [`ExtractedMetadata::coordinates_from_manifest`], because a single
    /// format can have both kinds of artifact — Maven answers `true` here and
    /// still lets `matrix-1.0.0-sources.jar` take its coordinates from the
    /// request, since that file carries no manifest to contradict them. The
    /// two are read together at the one place that decides
    /// (`resolve_publish_info`).
    ///
    /// It stays `false` for the formats where no upload can ever state its
    /// identity: `GenericAdapter` returns empty coordinates by design, and
    /// `DockerAdapter` refuses this route outright.
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
            ("maven", true),
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
