//! OCI common types — shared between storage, manifest, and HTTP API.

use serde::{Deserialize, Serialize};

/// Parsed OCI manifest reference — either a tag or a digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Tag(String),
    Digest(String), // "sha256:..."
}

/// The one digest algorithm this registry can compute, and therefore the only
/// one it can accept a push for.
pub const SUPPORTED_DIGEST_ALGORITHM: &str = "sha256";

/// Why a `{reference}` in a manifest URL is neither a tag this registry can
/// serve nor a digest it can verify.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReferenceError {
    /// Carries a `:` — so it is a digest claim — but does not match the spec's
    /// `algorithm ":" encoded` grammar.
    MalformedDigest { reference: String },
    /// A well-formed digest naming an algorithm the registry cannot compute,
    /// and therefore cannot check the body against.
    UnsupportedAlgorithm {
        reference: String,
        algorithm: String,
    },
    /// No `:`, so it can only be a tag — and it is not a legal one.
    InvalidTag { reference: String },
}

impl std::fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReferenceError::MalformedDigest { reference } => write!(
                f,
                "reference {reference} carries a ':' and so names a digest, but it is not a \
                 well-formed <algorithm>:<encoded> digest"
            ),
            ReferenceError::UnsupportedAlgorithm {
                reference,
                algorithm,
            } => write!(
                f,
                "reference {reference} names digest algorithm {algorithm}, which this registry \
                 cannot compute — it could only be stored without ever being verified"
            ),
            ReferenceError::InvalidTag { reference } => write!(
                f,
                "reference {reference} is not a legal tag: a tag is \
                 [a-zA-Z0-9_][a-zA-Z0-9._-]{{0,127}}"
            ),
        }
    }
}

impl std::error::Error for ReferenceError {}

impl Reference {
    /// Classify a `{reference}` path segment.
    ///
    /// The colon decides, not a prefix. The spec's tag grammar is
    /// `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}` and has no `:` in it, so a reference
    /// carrying one is *always* a digest claim and never a tag — while a
    /// reference without one can only be a tag.
    ///
    /// This used to be `starts_with("sha256:")` with an unconditional `Tag` in
    /// the `else`, which fused both halves of that statement into one bug
    /// (card_413991c11c56). `PUT .../manifests/sha512:<128 hex>` became a *tag*
    /// write: it skipped the digest verification that only runs on the `Digest`
    /// arm, so a manifest was published under an address nothing had checked
    /// the body against; and `tags/list` then advertised `sha512:0000…` as a
    /// tag, which no client can put back into a URL — `docker pull
    /// repo:sha512:0000…` does not even parse.
    ///
    /// Returning a `Result` is the point: the third case — neither a servable
    /// tag nor a verifiable digest — has to be answered, and a new call site
    /// cannot forget it the way an infallible `else` invited.
    pub fn parse(s: &str) -> Result<Self, ReferenceError> {
        let Some((algorithm, encoded)) = s.split_once(':') else {
            return if is_tag(s) {
                Ok(Reference::Tag(s.to_string()))
            } else {
                Err(ReferenceError::InvalidTag {
                    reference: s.to_string(),
                })
            };
        };

        if !is_digest_algorithm(algorithm) || !is_digest_encoded(encoded) {
            return Err(ReferenceError::MalformedDigest {
                reference: s.to_string(),
            });
        }
        if algorithm != SUPPORTED_DIGEST_ALGORITHM {
            return Err(ReferenceError::UnsupportedAlgorithm {
                reference: s.to_string(),
                algorithm: algorithm.to_string(),
            });
        }
        // The spec pins `sha256` to exactly 64 lowercase hex digits. Two
        // spellings of one hash would be two content addresses for one
        // manifest, so the loose form is refused rather than normalized.
        if encoded.len() != 64
            || !encoded
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(ReferenceError::MalformedDigest {
                reference: s.to_string(),
            });
        }
        Ok(Reference::Digest(s.to_string()))
    }

    /// Classify a `{digest}` path segment on a **blob** endpoint.
    ///
    /// A blob has no tags: `/v2/{name}/blobs/latest` is not a name this
    /// registry might one day resolve, it is a digest that was never written
    /// as one. So the tag arm of [`Reference::parse`] is not a legal answer
    /// here and collapses into `MalformedDigest`, while the two digest
    /// refusals keep their own codes — a `sha512` layer address is a legal
    /// reference this registry declines (`UNSUPPORTED`), and a mirroring
    /// client acts on that differently than on `DIGEST_INVALID`.
    ///
    /// Sharing the grammar with the manifest endpoints is the point. The blob
    /// handlers used to reach storage with whatever the URL held and let the
    /// key builder object, which answered one code for both refusals — and,
    /// because that objection travelled as an ordinary storage failure,
    /// answered `500` for it on two of the three endpoints.
    pub fn parse_blob_digest(s: &str) -> Result<Self, ReferenceError> {
        match Self::parse(s) {
            Ok(digest @ Reference::Digest(_)) => Ok(digest),
            Ok(Reference::Tag(_)) | Err(ReferenceError::InvalidTag { .. }) => {
                Err(ReferenceError::MalformedDigest {
                    reference: s.to_string(),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// The reference as it was written in the URL.
    pub fn as_str(&self) -> &str {
        match self {
            Reference::Tag(tag) => tag,
            Reference::Digest(digest) => digest,
        }
    }

    /// `true` if this is a tag reference.
    pub fn is_tag(&self) -> bool {
        matches!(self, Reference::Tag(_))
    }

    /// `true` if this is a digest reference.
    pub fn is_digest(&self) -> bool {
        matches!(self, Reference::Digest(_))
    }
}

/// The spec's tag grammar: `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}`.
fn is_tag(s: &str) -> bool {
    let mut bytes = s.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !(first.is_ascii_alphanumeric() || first == b'_') {
        return false;
    }
    if s.len() > 128 {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// `algorithm ::= component (separator component)*`, with
/// `component ::= [a-z0-9]+` and `separator ::= [+._-]`.
fn is_digest_algorithm(s: &str) -> bool {
    !s.is_empty()
        && s.split(['+', '.', '_', '-']).all(|component| {
            !component.is_empty()
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// `encoded ::= [a-zA-Z0-9=_-]+`.
fn is_digest_encoded(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'=' | b'_' | b'-'))
}

/// OCI tag listing response (RFC 7153).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TagListResponse {
    pub name: String,
    pub tags: Vec<String>,
}

/// Error response body (RFC 7807 — Problem Details).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub errors: Vec<ErrorDetail>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl ErrorResponse {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            errors: vec![ErrorDetail {
                code: code.to_string(),
                message: message.to_string(),
                detail: None,
            }],
        }
    }
}

/// OCI Distribution API version check.
pub const API_VERSION_HEADER: &str = "Docker-Distribution-API-Version";
pub const API_VERSION: &str = "registry/2.0";

/// OCI mediatype constants.
pub mod media_types {
    // Docker V2 Schema 2
    pub const MANIFEST_V2: &str = "application/vnd.docker.distribution.manifest.v2+json";
    pub const MANIFEST_LIST_V2: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
    pub const CONFIG_V1: &str = "application/vnd.docker.container.image.v1+json";
    pub const LAYER_TAR_GZ: &str = "application/vnd.docker.image.rootfs.diff.tar.gzip";

    // OCI Image Spec
    pub const OCI_MANIFEST_V1: &str = "application/vnd.oci.image.manifest.v1+json";
    pub const OCI_INDEX_V1: &str = "application/vnd.oci.image.index.v1+json";
    pub const OCI_CONFIG_V1: &str = "application/vnd.oci.image.config.v1+json";
    pub const OCI_LAYER_TAR_GZ: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

    /// Known manifest media types (for accept header validation).
    pub const MANIFEST_TYPES: &[&str] =
        &[MANIFEST_V2, MANIFEST_LIST_V2, OCI_MANIFEST_V1, OCI_INDEX_V1];
}

/// OCI error codes (per distribution spec).
pub mod error_codes {
    pub const BLOB_UNKNOWN: &str = "BLOB_UNKNOWN";
    pub const BLOB_UPLOAD_INVALID: &str = "BLOB_UPLOAD_INVALID";
    pub const BLOB_UPLOAD_UNKNOWN: &str = "BLOB_UPLOAD_UNKNOWN";
    pub const DIGEST_INVALID: &str = "DIGEST_INVALID";
    pub const MANIFEST_BLOB_UNKNOWN: &str = "MANIFEST_BLOB_UNKNOWN";
    pub const MANIFEST_INVALID: &str = "MANIFEST_INVALID";
    pub const MANIFEST_UNKNOWN: &str = "MANIFEST_UNKNOWN";
    pub const NAME_INVALID: &str = "NAME_INVALID";
    pub const NAME_UNKNOWN: &str = "NAME_UNKNOWN";
    pub const PAGINATION_NUMBER_INVALID: &str = "PAGINATION_NUMBER_INVALID";
    pub const SIZE_INVALID: &str = "SIZE_INVALID";
    /// Docker's registry v2 code for a tag the registry will not address.
    /// Kept because that is what the reference implementation emits and what
    /// clients recognize; the OCI spec's own list has no narrower code.
    pub const TAG_INVALID: &str = "TAG_INVALID";
    pub const UNAUTHORIZED: &str = "UNAUTHORIZED";
    pub const UNSUPPORTED: &str = "UNSUPPORTED";
}

#[cfg(test)]
mod reference_tests {
    use super::*;

    #[test]
    fn a_plain_name_is_a_tag() {
        for tag in ["latest", "v1.0.0", "_underscored", "a", &"t".repeat(128)] {
            assert_eq!(
                Reference::parse(tag),
                Ok(Reference::Tag(tag.to_string())),
                "{tag} is a legal tag"
            );
        }
    }

    #[test]
    fn a_sha256_digest_is_a_digest() {
        let digest = format!("sha256:{}", "a1".repeat(32));
        assert_eq!(
            Reference::parse(&digest),
            Ok(Reference::Digest(digest.clone()))
        );
    }

    /// The defect this parser was rewritten for: a colon can only ever be a
    /// digest, so `sha512:` must not slip past the verification the `Digest`
    /// arm performs and must never reach `tags/list` as a tag name.
    #[test]
    fn another_algorithm_is_an_unverifiable_digest_not_a_tag() {
        let digest = format!("sha512:{}", "0".repeat(128));
        assert_eq!(
            Reference::parse(&digest),
            Err(ReferenceError::UnsupportedAlgorithm {
                reference: digest,
                algorithm: "sha512".to_string(),
            })
        );
    }

    #[test]
    fn a_colon_is_never_a_tag_however_it_is_spelled() {
        for reference in ["not:a:digest", ":", "sha256:", ":deadbeef", "sha256:zz"] {
            let parsed = Reference::parse(reference);
            assert!(
                matches!(
                    parsed,
                    Err(ReferenceError::MalformedDigest { .. })
                        | Err(ReferenceError::UnsupportedAlgorithm { .. })
                ),
                "{reference} must not parse as a tag, got {parsed:?}"
            );
        }
    }

    #[test]
    fn a_sha256_digest_of_the_wrong_shape_is_malformed() {
        for encoded in [
            "a1".repeat(31),                       // too short
            "a1".repeat(33),                       // too long
            "A1".repeat(32),                       // uppercase is a second spelling of one hash
            format!("{}=", "a1".repeat(31) + "a"), // legal `encoded`, illegal for sha256
        ] {
            let reference = format!("sha256:{encoded}");
            assert_eq!(
                Reference::parse(&reference),
                Err(ReferenceError::MalformedDigest {
                    reference: reference.clone()
                }),
                "sha256:{encoded} is not a sha256 digest"
            );
        }
    }

    /// A blob address that is not a digest is a malformed digest, whatever it
    /// would have meant on a manifest URL — but an algorithm this registry
    /// declines keeps the code that says so.
    #[test]
    fn a_blob_is_addressed_by_digest_or_not_at_all() {
        let digest = format!("sha256:{}", "a1".repeat(32));
        assert_eq!(
            Reference::parse_blob_digest(&digest),
            Ok(Reference::Digest(digest.clone()))
        );

        for reference in ["latest", "not-a-digest", "", ".leading-dot", "sha256:zz"] {
            assert_eq!(
                Reference::parse_blob_digest(reference),
                Err(ReferenceError::MalformedDigest {
                    reference: reference.to_string()
                }),
                "{reference} is not a blob address"
            );
        }

        let sha512 = format!("sha512:{}", "0".repeat(128));
        assert_eq!(
            Reference::parse_blob_digest(&sha512),
            Err(ReferenceError::UnsupportedAlgorithm {
                reference: sha512,
                algorithm: "sha512".to_string(),
            })
        );
    }

    #[test]
    fn a_name_that_is_not_a_legal_tag_is_refused() {
        for reference in ["", "not a tag!!", ".leading-dot", "-leading-dash", "über"] {
            assert_eq!(
                Reference::parse(reference),
                Err(ReferenceError::InvalidTag {
                    reference: reference.to_string()
                }),
                "{reference} is not a legal tag"
            );
        }
        assert!(
            Reference::parse(&"t".repeat(129)).is_err(),
            "a tag is at most 128 characters"
        );
    }
}
