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

    fn content_type_for_file(&self, filename: &str) -> String {
        let lower = filename.to_lowercase();
        if lower.ends_with(".whl") {
            "application/zip".into()
        } else if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
            "application/gzip".into()
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
                // Truncate long description
                let desc = if value.len() > 500 {
                    format!("{}...", &value[..500])
                } else {
                    value.to_string()
                };
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

        html.push_str(&format!(
            "  <a href=\"{}{}\"{}{}>{}</a><br/>\n",
            escape_html(&entry.download_url),
            escape_html(&sha_frag),
            requires_python,
            yanked,
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
