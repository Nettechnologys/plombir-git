//! PyPI (Python) package adapter.
//!
//! Handles two package formats:
//! - **Wheel (.whl)**: ZIP archive containing `{name}-{version}.dist-info/METADATA`
//! - **Source distribution (.tar.gz)**: tar.gz containing `{name}-{version}/PKG-INFO`
//!
//! Metadata is in RFC 822-style email header format (PEP 314 / PEP 566).
//!
//! ## Simple Repository API (PEP 503)
//!
//! PyPI clients (pip, poetry, uv) expect HTML at:
//!   `GET /simple/{package}/`
//!
//! Returns an HTML page with `<a>` links to each version's download URL:
//! ```html
//! <!DOCTYPE html>
//! <html><body>
//!   <a href="https://.../mypkg-1.0.0.tar.gz#sha256=...">mypkg-1.0.0.tar.gz</a>
//!   <a href="https://.../mypkg-1.1.0-py3-none-any.whl#sha256=...">mypkg-1.1.0-py3-none-any.whl</a>
//! </body></html>
//! ```
//!
//! ForgeKeep serves this at:
//!   `GET /api/v1/repos/{owner}/{repo}/packages/pypi/simple/{pkg_name}/`
//!
//! The trailing slash is part of the spec, not decoration: a client builds the
//! project URL as `<index-url>/<normalized name>/`, so both that spelling and
//! the root index `.../simple/` have to be served. The name in the URL is the
//! PEP 503 *normalized* one — see [`normalize_project_name`] — which is rarely
//! the spelling the project was published under.

use flate2::read::GzDecoder;
use std::io::{Cursor, Read};

use crate::package_registry::adapter::{ExtractedMetadata, PackageAdapter};

pub struct PyPIAdapter;

impl PackageAdapter for PyPIAdapter {
    fn package_type() -> &'static str {
        "pypi"
    }

    fn extract_metadata(
        &self,
        filename: &str,
        data: &[u8],
    ) -> Result<ExtractedMetadata, anyhow::Error> {
        let filename_lower = filename.to_lowercase();

        if filename_lower.ends_with(".whl") {
            extract_from_whl(data)
        } else if filename_lower.ends_with(".tar.gz") || filename_lower.ends_with(".tgz") {
            extract_from_sdist(data)
        } else {
            // Try wheel first (ZIP magic), then sdist
            if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
                extract_from_whl(data)
            } else if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
                extract_from_sdist(data)
            } else {
                anyhow::bail!(
                    "unrecognized PyPI package format: expected .whl (ZIP) or .tar.gz (gzip); got '{}'",
                    filename
                )
            }
        }
    }

    fn validate(&self, data: &[u8]) -> Result<(), anyhow::Error> {
        // Check for wheel (ZIP magic: PK\x03\x04)
        if data.len() >= 4 && &data[0..4] == b"PK\x03\x04" {
            validate_whl(data)
        } else if data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b {
            validate_sdist(data)
        } else {
            anyhow::bail!("invalid PyPI package: not a recognized format (expect .whl or .tar.gz)")
        }
    }

    /// A wheel carries one `dist-info/METADATA` and an sdist one `PKG-INFO`;
    /// `Name` and `Version` are required fields of both.
    fn manifest_is_authoritative(&self) -> bool {
        true
    }

    fn content_type_for_file(&self, filename: &str) -> String {
        let lower = filename.to_lowercase();
        if lower.ends_with(".whl") {
            "application/zip".into()
        } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            "application/gzip".into()
        } else if lower.ends_with(PYPI_PROVENANCE_SUFFIX) {
            // PEP 740 defines the provenance file as a JSON document, and the
            // link the Simple page advertises is followed by tooling that
            // parses it rather than by a browser that downloads it.
            "application/json".into()
        } else {
            self.default_content_type().into()
        }
    }

    fn default_content_type(&self) -> &'static str {
        "application/octet-stream"
    }

    fn has_protocol_endpoint(&self) -> bool {
        true
    }
}

/// Parse RFC 822-style metadata (e.g., METADATA / PKG-INFO).
/// Fields: Name, Version, Summary, Home-page, License, Keywords, etc.
fn parse_rfc822_meta(content: &str) -> Result<ExtractedMetadata, anyhow::Error> {
    let mut fields = Rfc822Fields::default();
    let mut current_field = String::new();
    let mut current_value = String::new();
    let mut in_continuation = false;

    for line in content.lines() {
        if line.starts_with(' ') || line.starts_with('\t') {
            // Continuation line (folded header per RFC 822)
            if in_continuation {
                current_value.push(' ');
                current_value.push_str(line.trim());
            }
            continue;
        }

        // Save previous field if any
        if !current_field.is_empty() {
            save_field(&current_field, &current_value, &mut fields);
            current_field.clear();
            current_value.clear();
        }

        // Parse new header line
        if let Some(colon_pos) = line.find(':') {
            current_field = line[..colon_pos].trim().to_lowercase();
            current_value = line[colon_pos + 1..].trim().to_string();
            in_continuation = true;
        } else if line.is_empty() {
            in_continuation = false;
        }
    }

    // Save last field
    if !current_field.is_empty() {
        save_field(&current_field, &current_value, &mut fields);
    }

    if fields.name.is_empty() {
        anyhow::bail!("metadata missing 'Name' field");
    }
    if fields.version.is_empty() {
        anyhow::bail!("metadata missing 'Version' field");
    }

    Ok(ExtractedMetadata {
        name: fields.name,
        version: fields.version.clone(),
        description: fields.description,
        homepage: fields.homepage,
        repository_url: None, // PyPI metadata has no standard repository field
        keywords: fields.keywords,
        license: fields.license,
        semver: Some(fields.version),
        protocol_metadata: pypi_protocol_metadata(fields.requires_python.as_deref()),
        coordinates_from_manifest: true,
    })
}

/// The `METADATA` / `PKG-INFO` headers this adapter reads.
#[derive(Default)]
struct Rfc822Fields {
    name: String,
    version: String,
    description: Option<String>,
    homepage: Option<String>,
    license: Option<String>,
    keywords: Option<String>,
    /// `Requires-Python`. No package column holds it, and it is the *first*
    /// filter a resolver applies — see [`pypi_protocol_metadata`].
    requires_python: Option<String>,
}

fn save_field(field: &str, value: &str, fields: &mut Rfc822Fields) {
    match field {
        "name" => fields.name = value.to_string(),
        "version" => fields.version = value.to_string(),
        "summary" => fields.description = Some(value.to_string()),
        "description" => {
            // If we already have a summary, keep it (summary is shorter/better)
            if fields.description.is_none() {
                // Truncate the long description by CHARACTERS, not bytes: a
                // byte-indexed cut lands inside a multi-byte character for any
                // description whose 500th byte is mid-sequence, and slicing a
                // `&str` there panics — on a value that arrives verbatim from
                // the uploaded `METADATA` file.
                let mut desc: String = value.chars().take(500).collect();
                if desc.len() < value.len() {
                    desc.push_str("...");
                }
                fields.description = Some(desc);
            }
        }
        "home-page" | "homepage" | "project-url" | "url" => {
            fields.homepage = Some(value.to_string());
        }
        "license" => fields.license = Some(value.to_string()),
        "keywords" => fields.keywords = Some(value.to_string()),
        "requires-python" => {
            fields.requires_python = Some(value.to_string()).filter(|v| !v.is_empty())
        }
        _ => {}
    }
}

/// The per-version PyPI fields no package column holds, keyed the way
/// `parse_pypi_metadata` (rg-http) reads them back.
///
/// `Requires-Python` is the reason this exists. It is the first filter pip / uv
/// / poetry apply when choosing a candidate, and a link published without
/// `data-requires-python` is a link they consider compatible with every
/// interpreter — so a 3.12-only wheel installs on 3.8 and fails at import time
/// instead of resolving honestly (card_5d5a0e91d324).
fn pypi_protocol_metadata(requires_python: Option<&str>) -> Option<String> {
    let requires_python = requires_python.filter(|v| !v.is_empty())?;
    Some(serde_json::json!({ "requires_python": requires_python }).to_string())
}

/// Extract metadata from a .whl (ZIP) file.
fn extract_from_whl(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let cursor = Cursor::new(data);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| anyhow::anyhow!("invalid .whl file (not a valid ZIP): {e}"))?;

    let mut metadata_content = None;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| anyhow::anyhow!("failed to read .whl entry {}: {e}", i))?;
        let path = entry.name().to_lowercase();

        // Look for *.dist-info/METADATA
        if path.ends_with(".dist-info/metadata") || path.ends_with(".dist-info\\metadata") {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            metadata_content = Some(content);
            break;
        }
    }

    let content = metadata_content
        .ok_or_else(|| anyhow::anyhow!("invalid .whl file: no .dist-info/METADATA found"))?;

    parse_rfc822_meta(&content)
}

/// Extract metadata from a source distribution (.tar.gz).
fn extract_from_sdist(data: &[u8]) -> Result<ExtractedMetadata, anyhow::Error> {
    let tar = GzDecoder::new(data);
    let mut archive = tar::Archive::new(tar);

    let mut pkg_info = None;

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_path_buf();

        // PKG-INFO is in the top-level directory: {name}-{version}/PKG-INFO
        if path.file_name().map(|n| n == "PKG-INFO").unwrap_or(false) {
            let mut content = String::new();
            entry.read_to_string(&mut content)?;
            pkg_info = Some(content);
            break;
        }
    }

    let content = pkg_info
        .ok_or_else(|| anyhow::anyhow!("invalid source distribution: no PKG-INFO found"))?;

    parse_rfc822_meta(&content)
}

/// Validate a .whl file (check ZIP structure and METADATA presence).
fn validate_whl(data: &[u8]) -> Result<(), anyhow::Error> {
    // Read METADATA, don't merely find it — see `CargoAdapter::validate`. A
    // `METADATA` with no `Name:` is a distribution pip cannot resolve, and
    // `validate` is the only gate publish runs unconditionally. The absence
    // check survives inside the parser (`no .dist-info/METADATA found`).
    extract_from_whl(data)?;
    Ok(())
}

/// Validate a source distribution (check tar.gz + PKG-INFO presence).
fn validate_sdist(data: &[u8]) -> Result<(), anyhow::Error> {
    // Check gzip
    let mut decoder = GzDecoder::new(data);
    let mut buf = Vec::new();
    decoder
        .read_to_end(&mut buf)
        .map_err(|e| anyhow::anyhow!("invalid source distribution (not valid gzip): {e}"))?;

    // Read PKG-INFO, don't merely find it — same reasoning as `validate_whl`.
    extract_from_sdist(data)?;
    Ok(())
}

// ── Simple Repository API helpers ─────────────────────────

/// Normalize a project name the way PEP 503 defines it: every run of `-`, `_`
/// or `.` collapses to a single `-`, and the result is lower-cased.
///
/// This is the spelling a client puts in the URL. pip, poetry and uv all
/// normalize before requesting, whatever case the project was published under,
/// so `Matrix_PyPI` is fetched as `matrix-pypi` — and a registry that only
/// answers to the stored spelling answers nothing at all.
pub fn normalize_project_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut in_separator = false;
    for ch in name.chars() {
        if matches!(ch, '-' | '_' | '.') {
            if !in_separator {
                out.push('-');
                in_separator = true;
            }
        } else {
            out.extend(ch.to_lowercase());
            in_separator = false;
        }
    }
    out
}

/// Escape text for inclusion in HTML, in element text and in a quoted
/// attribute alike.
///
/// Package names and filenames are whatever the publisher put in the metadata
/// or the `Content-Disposition` header. Interpolated raw, a `"` in a filename
/// silently truncates the `href` next to it — the link pip is supposed to
/// follow — long before anyone gets to the security argument.
fn escape_html(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Generate the Simple Repository API HTML page (PEP 503).
///
/// `versions` is a list of (version, filename, sha256, download_url).
pub fn build_simple_repository_html(package_name: &str, versions: &[PyPIVersionEntry]) -> String {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n");
    html.push_str(&format!(
        "<title>Simple index for {}</title>\n",
        escape_html(package_name)
    ));
    html.push_str("<meta name=\"api-version\" content=\"2\" />\n");
    html.push_str("</head>\n<body>\n");

    for entry in versions {
        let sha_frag = entry
            .sha256
            .as_ref()
            .map(|s| format!("#sha256={}", s))
            .unwrap_or_default();

        // PEP 503: the interpreter constraint the resolver filters on before it
        // looks at anything else. Absent, the file is a candidate for every
        // interpreter — which is a claim, not a silence.
        let requires_python = entry
            .requires_python
            .as_ref()
            .map(|spec| format!(" data-requires-python=\"{}\"", escape_html(spec)))
            .unwrap_or_default();

        // PEP 592: a yanked file stays in the index — that is what keeps an
        // exact pin resolvable — and the attribute is what takes it out of every
        // resolution that is not one. The attribute's value is the reason, and
        // the empty string is the spelling for "yanked, no reason given", which
        // is all this registry records: `set_yanked` stores a bool.
        let yanked = if entry.yanked {
            " data-yanked=\"\""
        } else {
            ""
        };

        // PEP 740: the attribute is how a client finds the provenance document
        // for this exact file. It is emitted only when the registry actually
        // holds that document and serves it at this URL — an attribute pointing
        // at a 404 is worse than none, because the tool that follows it reports
        // a broken index rather than an unattested file.
        let provenance = entry
            .provenance_url
            .as_ref()
            .map(|url| format!(" data-provenance=\"{}\"", escape_html(url)))
            .unwrap_or_default();

        html.push_str(&format!(
            "  <a href=\"{}{}\"{}{}{}>{}</a><br/>\n",
            escape_html(&entry.download_url),
            escape_html(&sha_frag),
            requires_python,
            yanked,
            provenance,
            escape_html(&entry.filename),
        ));
    }

    html.push_str("</body>\n</html>\n");
    html
}

/// Generate the root of the Simple Repository API (PEP 503) — the page that
/// lists every project in the index, each linking to its own project page.
///
/// The href carries the trailing slash the spec mandates, so a client that
/// follows the link lands on the project page rather than on the SPA fallback.
pub fn build_simple_root_html(projects: &[PyPIProjectEntry]) -> String {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n");
    html.push_str("<title>Simple index</title>\n");
    html.push_str("<meta name=\"api-version\" content=\"2\" />\n");
    html.push_str("</head>\n<body>\n");

    for project in projects {
        html.push_str(&format!(
            "  <a href=\"{}\">{}</a><br/>\n",
            escape_html(&project.url),
            escape_html(&project.name),
        ));
    }

    html.push_str("</body>\n</html>\n");
    html
}

/// Info for each version entry in the Simple Repository HTML.
pub struct PyPIVersionEntry {
    pub version: String,
    pub filename: String,
    pub sha256: Option<String>,
    pub download_url: String,
    /// The `Requires-Python` of the distribution, as its metadata spelled it.
    /// `None` means the distribution declared none, not that we did not look.
    pub requires_python: Option<String>,
    /// Whether the version was yanked (PEP 592).
    pub yanked: bool,
    /// Absolute URL of this file's PEP 740 provenance document, when the
    /// registry stores one. `None` means no attestation was ever published for
    /// this file — never "we have one but did not look it up".
    pub provenance_url: Option<String>,
}

// ── PEP 740 attestations ──────────────────────────────────

/// The suffix of the package file that stores a distribution's provenance.
///
/// PEP 740 does not name the file — the index is free to serve the provenance
/// document wherever it likes as long as the Simple API points at it. Storing
/// it as a package file named after the distribution buys two things: the
/// distribution and its evidence are written in one publish transaction, so
/// neither can exist without the other, and the URL the Simple page advertises
/// is the download route that already serves every other package file.
pub const PYPI_PROVENANCE_SUFFIX: &str = ".provenance";

/// The stored name of the provenance document belonging to `distribution`.
pub fn pypi_provenance_filename(distribution: &str) -> String {
    format!("{distribution}{PYPI_PROVENANCE_SUFFIX}")
}

/// The predicate types PEP 740 admits inside an attestation's statement.
const PEP_740_PREDICATE_TYPES: [&str; 2] = [
    "https://slsa.dev/provenance/v1",
    "https://docs.pypi.org/attestations/publish/v1",
];

/// How many attestations one distribution may carry.
///
/// PEP 740 sets no number, but the field is unauthenticated publisher input
/// that is stored verbatim, and one attestation per admissible predicate type
/// is all a consumer can act on. The bound is generous enough that it can only
/// be hit deliberately.
const PEP_740_MAX_ATTESTATIONS: usize = 8;

/// The publisher identity ForgeKeep can honestly state for a Twine upload.
///
/// PEP 740's provenance object describes *who* published, and on PyPI that is a
/// Trusted Publisher: an OIDC workload identity whose claims the index verified
/// itself. A ForgeKeep upload is authenticated by a repository write token,
/// which proves the caller may write here and nothing about the build that
/// produced the artifact. Emitting a `GitHub`-shaped publisher with invented
/// claims would turn that token into a verified workload identity on paper, so
/// the bundle names the registry as the publisher, carries no claims, and says
/// `trusted_publisher: false` outright. When ForgeKeep grows Trusted Publisher
/// support this is the one place that changes.
pub fn pypi_upload_token_publisher(owner: &str, repo: &str) -> serde_json::Value {
    serde_json::json!({
        "kind": "ForgeKeep",
        "claims": {},
        "repository": format!("{owner}/{repo}"),
        "trusted_publisher": false,
    })
}

fn decode_base64_field(value: &str, field: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|error| format!("PEP 740 attestation `{field}` is not valid base64: {error}"))
}

fn non_empty_string<'a>(
    parent: &'a serde_json::Value,
    key: &str,
    field: &str,
) -> Result<&'a str, String> {
    parent
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("PEP 740 attestation is missing `{field}`"))
}

/// Check one PEP 740 attestation against the distribution it was uploaded with.
///
/// Signature and certificate-chain verification is Sigstore's job and needs a
/// trust root ForgeKeep does not carry — the same boundary the npm provenance
/// path draws. What the registry *can* decide, and what nobody else can decide
/// for it, is whether this signed statement is about *this* upload: an
/// attestation whose subject names another file or hashes other bytes is
/// evidence for something else and must not be filed here.
///
/// Returns the statement's `predicateType` so the caller can reject duplicates.
fn inspect_pep_740_attestation(
    attestation: &serde_json::Value,
    filename: &str,
    sha256_hex: &str,
) -> Result<String, String> {
    if !attestation.is_object() {
        return Err("PEP 740 attestation must be a JSON object".into());
    }
    if attestation
        .get("version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return Err("PEP 740 attestation `version` must be 1".into());
    }

    let material = attestation
        .get("verification_material")
        .filter(|value| value.is_object())
        .ok_or_else(|| "PEP 740 attestation is missing `verification_material`".to_string())?;
    let certificate =
        non_empty_string(material, "certificate", "verification_material.certificate")?;
    decode_base64_field(certificate, "verification_material.certificate")?;
    // An attestation with no transparency log entry cannot be verified by
    // anyone downstream either, so accepting it would mean storing material
    // that only ever looks like evidence.
    let has_transparency_entry = material
        .get("transparency_entries")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|entries| !entries.is_empty());
    if !has_transparency_entry {
        return Err(
            "PEP 740 attestation `verification_material.transparency_entries` must not be empty"
                .into(),
        );
    }

    let envelope = attestation
        .get("envelope")
        .filter(|value| value.is_object())
        .ok_or_else(|| "PEP 740 attestation is missing `envelope`".to_string())?;
    let signature = non_empty_string(envelope, "signature", "envelope.signature")?;
    decode_base64_field(signature, "envelope.signature")?;
    let encoded_statement = non_empty_string(envelope, "statement", "envelope.statement")?;
    let statement_bytes = decode_base64_field(encoded_statement, "envelope.statement")?;
    let statement: serde_json::Value = serde_json::from_slice(&statement_bytes)
        .map_err(|error| format!("PEP 740 attestation statement is not valid JSON: {error}"))?;

    if statement.get("_type").and_then(serde_json::Value::as_str)
        != Some("https://in-toto.io/Statement/v1")
    {
        return Err("PEP 740 attestation statement is not an in-toto Statement v1".into());
    }
    let predicate_type = statement
        .get("predicateType")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "PEP 740 attestation statement has no `predicateType`".to_string())?;
    if !PEP_740_PREDICATE_TYPES.contains(&predicate_type) {
        return Err(format!(
            "PEP 740 attestation has unsupported predicateType '{predicate_type}'"
        ));
    }

    let subjects = statement
        .get("subject")
        .and_then(serde_json::Value::as_array)
        .filter(|subjects| subjects.len() == 1)
        .ok_or_else(|| {
            "PEP 740 attestation statement must contain exactly one subject".to_string()
        })?;
    let subject_name = subjects[0]
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| "PEP 740 attestation subject has no name".to_string())?;
    if subject_name != filename {
        return Err(format!(
            "PEP 740 attestation subject names '{subject_name}', uploaded file is '{filename}'"
        ));
    }
    let subject_digest = subjects[0]
        .pointer("/digest/sha256")
        .and_then(serde_json::Value::as_str)
        .filter(|digest| digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| "PEP 740 attestation subject has no valid SHA-256 digest".to_string())?;
    if !subject_digest.eq_ignore_ascii_case(sha256_hex) {
        return Err(
            "PEP 740 attestation subject SHA-256 does not match the uploaded distribution".into(),
        );
    }

    Ok(predicate_type.to_string())
}

/// Validate the `attestations` field of a Twine upload and build the provenance
/// document to store beside the distribution.
///
/// PEP 740 is explicit that a failure here is a failure of the *upload*: "if
/// the index fails to verify any attestation in `attestations`, it MUST reject
/// the upload". So this returns an error the caller answers with before a byte
/// of the distribution is written, rather than a partial bundle.
pub fn build_pypi_provenance(
    raw_attestations: &str,
    filename: &str,
    sha256_hex: &str,
    publisher: serde_json::Value,
) -> Result<Vec<u8>, String> {
    let attestations: Vec<serde_json::Value> = serde_json::from_str(raw_attestations)
        .map_err(|error| format!("Twine `attestations` is not a valid JSON array: {error}"))?;

    // Twine sends the field only when the publisher asked for attestations, so
    // an empty array is a client that meant to attach evidence and attached
    // none. Answering 200 to it is exactly the silent loss this endpoint is
    // supposed to have stopped doing.
    if attestations.is_empty() {
        return Err("Twine `attestations` must not be an empty array".into());
    }
    if attestations.len() > PEP_740_MAX_ATTESTATIONS {
        return Err(format!(
            "Twine `attestations` carries {} attestations, at most {PEP_740_MAX_ATTESTATIONS} are accepted",
            attestations.len()
        ));
    }

    let mut predicate_types = Vec::with_capacity(attestations.len());
    for attestation in &attestations {
        let predicate_type = inspect_pep_740_attestation(attestation, filename, sha256_hex)?;
        // Two attestations of the same predicate type make the pair that
        // describes this file ambiguous, and a consumer picking either one is
        // picking arbitrarily. PyPI refuses the same shape.
        if predicate_types.contains(&predicate_type) {
            return Err(format!(
                "Twine `attestations` repeats predicateType '{predicate_type}'"
            ));
        }
        predicate_types.push(predicate_type);
    }

    let provenance = serde_json::json!({
        "version": 1,
        "attestation_bundles": [{
            "publisher": publisher,
            "attestations": attestations,
        }],
    });
    serde_json::to_vec(&provenance)
        .map_err(|error| format!("cannot serialize the PEP 740 provenance document: {error}"))
}

/// One project row of the Simple Repository API root index.
pub struct PyPIProjectEntry {
    /// The project name as published.
    pub name: String,
    /// Absolute URL of the project page, trailing slash included.
    pub url: String,
}

#[cfg(test)]
mod simple_repository_tests {
    use super::*;

    /// The examples PEP 503 itself gives for the normalization rule.
    #[test]
    fn project_names_normalize_the_way_pep_503_spells_them() {
        for (raw, normalized) in [
            ("Matrix_PyPI", "matrix-pypi"),
            ("friendly-bard", "friendly-bard"),
            ("Friendly-Bard", "friendly-bard"),
            ("FRIENDLY-BARD", "friendly-bard"),
            ("friendly.bard", "friendly-bard"),
            ("friendly_bard", "friendly-bard"),
            ("friendly--bard", "friendly-bard"),
            ("FrIeNdLy-._.-bArD", "friendly-bard"),
        ] {
            assert_eq!(normalize_project_name(raw), normalized, "{raw}");
        }
    }

    /// A filename is publisher-controlled input; a bare `"` in it used to end
    /// the `href` early and hand pip a link to nowhere.
    #[test]
    fn publisher_controlled_text_cannot_break_out_of_the_markup() {
        let html = build_simple_repository_html(
            "evil",
            &[PyPIVersionEntry {
                version: "1.0.0".into(),
                filename: "x\"><script>alert(1)</script>.whl".into(),
                sha256: None,
                download_url: "https://example.test/a\"b".into(),
                // The resolver attributes are publisher-controlled text too.
                requires_python: Some(">=3.10,\"><script>alert(2)</script>".into()),
                yanked: false,
                provenance_url: Some("https://example.test/p\"><script>alert(3)</script>".into()),
            }],
        );

        assert!(!html.contains("<script>"), "{html}");
        assert!(html.contains("&quot;"), "{html}");
    }

    /// PEP 503's `data-requires-python` is the first filter pip / uv / poetry
    /// apply, and PEP 592's `data-yanked` is what keeps a withdrawn release out
    /// of every resolution that is not an exact pin. Neither used to be emitted,
    /// so every file was offered as compatible with everything and alive.
    #[test]
    fn the_project_page_states_the_two_facts_a_resolver_filters_on() {
        let entry = |version: &str, requires_python: Option<&str>, yanked: bool| PyPIVersionEntry {
            version: version.into(),
            filename: format!("matrix-{version}-py3-none-any.whl"),
            sha256: Some("abc".into()),
            download_url: format!("https://example.test/matrix-{version}.whl"),
            requires_python: requires_python.map(String::from),
            yanked,
            provenance_url: None,
        };

        let html = build_simple_repository_html(
            "matrix",
            &[
                entry("1.0.0", Some(">=3.10"), false),
                entry("1.1.0", Some(">=3.12"), true),
                entry("0.9.0", None, false),
            ],
        );

        assert!(
            html.contains("#sha256=abc\" data-requires-python=\"&gt;=3.10\">"),
            "{html}"
        );
        assert!(
            html.contains("data-requires-python=\"&gt;=3.12\" data-yanked=\"\">"),
            "a yanked version stays on the page, and says so: {html}"
        );
        // No declaration is no attribute: an empty one would claim the
        // distribution runs on everything.
        assert!(html.contains("matrix-0.9.0.whl#sha256=abc\">"), "{html}");
        assert_eq!(html.matches("data-yanked").count(), 1, "{html}");
        assert_eq!(html.matches("data-requires-python").count(), 2, "{html}");
    }

    /// PEP 740's `data-provenance` is the only way a client finds the evidence
    /// file, and it must appear on exactly the files whose evidence is stored.
    #[test]
    fn the_project_page_advertises_provenance_only_for_the_files_that_have_it() {
        let entry = |version: &str, provenance_url: Option<&str>| PyPIVersionEntry {
            version: version.into(),
            filename: format!("matrix-{version}-py3-none-any.whl"),
            sha256: Some("abc".into()),
            download_url: format!("https://example.test/matrix-{version}.whl"),
            requires_python: None,
            yanked: false,
            provenance_url: provenance_url.map(String::from),
        };

        let html = build_simple_repository_html(
            "matrix",
            &[
                entry(
                    "1.0.0",
                    Some("https://example.test/matrix-1.0.0.whl.provenance"),
                ),
                entry("1.1.0", None),
            ],
        );

        assert!(
            html.contains(
                "data-provenance=\"https://example.test/matrix-1.0.0.whl.provenance\">\
                 matrix-1.0.0-py3-none-any.whl</a>"
            ),
            "{html}"
        );
        assert_eq!(html.matches("data-provenance").count(), 1, "{html}");
    }

    #[test]
    fn the_root_index_links_every_project_with_its_trailing_slash() {
        let html = build_simple_root_html(&[PyPIProjectEntry {
            name: "Matrix_PyPI".into(),
            url: "https://example.test/simple/matrix-pypi/".into(),
        }]);

        assert!(
            html.contains("<a href=\"https://example.test/simple/matrix-pypi/\">Matrix_PyPI</a>"),
            "{html}"
        );
    }
}

#[cfg(test)]
mod pep_740_tests {
    use super::*;
    use base64::Engine as _;

    const FILENAME: &str = "matrix_twine-1.2.3-py3-none-any.whl";
    const DIGEST: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    fn statement(name: &str, sha256: &str, predicate_type: &str) -> String {
        b64(&serde_json::to_vec(&serde_json::json!({
            "_type": "https://in-toto.io/Statement/v1",
            "subject": [{ "name": name, "digest": { "sha256": sha256 } }],
            "predicateType": predicate_type,
            "predicate": {},
        }))
        .unwrap())
    }

    fn attestation(name: &str, sha256: &str, predicate_type: &str) -> serde_json::Value {
        serde_json::json!({
            "version": 1,
            "verification_material": {
                "certificate": b64(b"a DER certificate"),
                "transparency_entries": [{ "logIndex": "1" }],
            },
            "envelope": {
                "statement": statement(name, sha256, predicate_type),
                "signature": b64(b"a DSSE signature"),
            },
        })
    }

    fn valid() -> serde_json::Value {
        attestation(
            FILENAME,
            DIGEST,
            "https://docs.pypi.org/attestations/publish/v1",
        )
    }

    fn build(attestations: serde_json::Value) -> Result<serde_json::Value, String> {
        let raw = serde_json::to_string(&attestations).unwrap();
        build_pypi_provenance(
            &raw,
            FILENAME,
            DIGEST,
            pypi_upload_token_publisher("owner", "repo"),
        )
        .map(|document| serde_json::from_slice(&document).unwrap())
    }

    #[test]
    fn a_matching_attestation_becomes_a_provenance_document() {
        let provenance = build(serde_json::json!([valid()])).unwrap();

        assert_eq!(provenance["version"], 1);
        let bundle = &provenance["attestation_bundles"][0];
        assert_eq!(bundle["attestations"].as_array().unwrap().len(), 1);
        assert_eq!(bundle["attestations"][0], valid());
        // The upload was authenticated by a write token, not by a verified
        // workload identity, and the document has to say so.
        assert_eq!(bundle["publisher"]["kind"], "ForgeKeep");
        assert_eq!(bundle["publisher"]["trusted_publisher"], false);
        assert_eq!(bundle["publisher"]["claims"], serde_json::json!({}));
    }

    /// The one check nobody downstream can make for the registry: this signed
    /// statement is about *this* upload.
    #[test]
    fn an_attestation_for_another_artifact_is_refused() {
        let foreign_name = build(serde_json::json!([attestation(
            "matrix_twine-9.9.9-py3-none-any.whl",
            DIGEST,
            "https://slsa.dev/provenance/v1"
        )]))
        .unwrap_err();
        assert!(foreign_name.contains("subject names"), "{foreign_name}");

        let foreign_digest = build(serde_json::json!([attestation(
            FILENAME,
            "2222222222222222222222222222222222222222222222222222222222222222",
            "https://slsa.dev/provenance/v1"
        )]))
        .unwrap_err();
        assert!(
            foreign_digest.contains("SHA-256 does not match"),
            "{foreign_digest}"
        );
    }

    #[test]
    fn one_bad_attestation_rejects_the_whole_upload() {
        let mut broken = valid();
        broken["envelope"]["statement"] = serde_json::json!(b64(b"not json"));

        let error = build(serde_json::json!([valid(), broken])).unwrap_err();
        assert!(error.contains("not valid JSON"), "{error}");
    }

    #[test]
    fn structurally_unverifiable_material_is_refused() {
        for (mutate, expected) in [
            (
                Box::new(|a: &mut serde_json::Value| a["version"] = serde_json::json!(2))
                    as Box<dyn Fn(&mut serde_json::Value)>,
                "`version` must be 1",
            ),
            (
                Box::new(|a: &mut serde_json::Value| {
                    a["verification_material"]["transparency_entries"] = serde_json::json!([])
                }),
                "transparency_entries` must not be empty",
            ),
            (
                Box::new(|a: &mut serde_json::Value| {
                    a["verification_material"]["certificate"] = serde_json::json!("not base64!!")
                }),
                "not valid base64",
            ),
            (
                Box::new(|a: &mut serde_json::Value| {
                    a["envelope"]["signature"] = serde_json::json!("")
                }),
                "missing `envelope.signature`",
            ),
            (
                Box::new(|a: &mut serde_json::Value| {
                    a["envelope"]["statement"] = serde_json::json!(statement(
                        FILENAME,
                        DIGEST,
                        "https://example.test/made-up/v1"
                    ))
                }),
                "unsupported predicateType",
            ),
        ] {
            let mut broken = valid();
            mutate(&mut broken);
            let error = build(serde_json::json!([broken])).unwrap_err();
            assert!(
                error.contains(expected),
                "expected {expected:?}, got {error}"
            );
        }
    }

    #[test]
    fn an_empty_or_repeating_attestations_field_is_refused() {
        let empty = build(serde_json::json!([])).unwrap_err();
        assert!(empty.contains("must not be an empty array"), "{empty}");

        let repeated = build(serde_json::json!([valid(), valid()])).unwrap_err();
        assert!(repeated.contains("repeats predicateType"), "{repeated}");

        let not_an_array = build_pypi_provenance(
            "{}",
            FILENAME,
            DIGEST,
            pypi_upload_token_publisher("owner", "repo"),
        )
        .unwrap_err();
        assert!(
            not_an_array.contains("not a valid JSON array"),
            "{not_an_array}"
        );
    }

    #[test]
    fn the_provenance_file_is_named_and_typed_after_its_distribution() {
        assert_eq!(
            pypi_provenance_filename(FILENAME),
            "matrix_twine-1.2.3-py3-none-any.whl.provenance"
        );
        assert_eq!(
            PyPIAdapter.content_type_for_file(&pypi_provenance_filename(FILENAME)),
            "application/json"
        );
    }
}

#[cfg(test)]
mod metadata_truncation_tests {
    use super::parse_rfc822_meta;

    /// A `Description` long enough to be truncated, made of three-byte
    /// characters so byte 500 is not a character boundary. The old
    /// byte-indexed cut panicked here, and the value comes straight out of
    /// the uploaded `METADATA` file, so any publisher could reach it.
    #[test]
    fn a_long_multibyte_description_is_truncated_instead_of_panicking() {
        let description = "中".repeat(600);
        let metadata = format!("Name: matrix-twine\nVersion: 1.2.3\nDescription: {description}\n");

        let parsed = parse_rfc822_meta(&metadata).expect("metadata carries name and version");

        let stored = parsed.description.expect("description field is kept");
        assert!(stored.ends_with("..."), "{stored}");
        assert_eq!(stored.trim_end_matches("...").chars().count(), 500);
    }

    #[test]
    fn a_short_multibyte_description_is_kept_whole() {
        let metadata = "Name: matrix-twine\nVersion: 1.2.3\nDescription: 中文说明\n";

        let parsed = parse_rfc822_meta(metadata).expect("metadata carries name and version");

        assert_eq!(parsed.description.as_deref(), Some("中文说明"));
    }
}
