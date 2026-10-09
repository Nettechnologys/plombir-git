//! One spelling of `Content-Disposition` for every file this server hands over,
//! and one reading of it for every file a client hands in.
//!
//! Five download handlers used to build the header with
//! `format!("attachment; filename=\"{}\"", name)` and then disagreed about the
//! result. `HeaderValue` accepts only visible ASCII, so a perfectly legal file
//! name — `пакет-1.0.tgz`, `naïve.pdf` — produced a value no header can hold,
//! and each site answered differently: the package download let
//! `axum_core`'s `TryIntoHeaderError` become a `500`, so a package that
//! published successfully could never be downloaded again; the release asset,
//! the CI artifact and the attachment dropped the header behind
//! `if let Ok(..)`, leaving the browser to invent a name from the URL. Neither
//! is the answer the client asked for, and neither says anything went wrong.
//!
//! [`attachment`] is total: it always produces a header a client can read,
//! carrying the real name in the RFC 5987 `filename*` form and a sanitized
//! ASCII spelling in the `filename` form for anything that does not implement
//! it. Quoting is part of the same function, so a name containing `"`, `\` or
//! `;` cannot end a parameter early.
//!
//! [`filename_from_disposition`] is the mirror, and exists for the same reason:
//! `releases.rs` and `packages.rs` each carried a private copy, and they
//! disagreed — one preferred `filename*`, the other returned on whichever of
//! the two it met first. A client that sends both (which RFC 6266 recommends,
//! precisely so old software has something to read) therefore had its file
//! stored under the ASCII fallback on one endpoint and under its real name on
//! the other.

use axum::http::{header, HeaderMap, HeaderValue};

/// The name used when a caller supplies one that survives neither form.
const FALLBACK: &str = "download";

/// The media types a user-uploaded download is served under as they were
/// stored: ones a browser only ever *displays* — never runs as script or
/// style, never renders as a document of our origin.
///
/// Anything else is served as `application/octet-stream`. The stored type is
/// whatever the uploader said (a release asset's `Content-Type`, a multipart
/// part's), and `attachment` plus `nosniff` do not stop `<script src>` from
/// running a file served with a JavaScript type: `script-src 'self'` lets it
/// in, so any later HTML injection would become a full XSS with no nonce
/// needed (card_36b620ab3467).
const PASSIVE_UPLOAD_TYPES: &[&str] = &[
    "application/gzip",
    "application/json",
    "application/pdf",
    "application/zip",
    "image/avif",
    "image/gif",
    "image/jpeg",
    "image/png",
    // Scripts inside an SVG never run from `<img>`; opened directly it is a
    // download, and `UPLOAD_SANDBOX_CSP` holds even if a browser renders it.
    "image/svg+xml",
    "image/webp",
    "text/csv",
    "text/plain",
    "video/mp4",
    "video/quicktime",
    "video/webm",
];

/// `Content-Security-Policy` for a user-uploaded download: whatever a browser
/// makes of the bytes runs nothing and loads nothing. The same policy the raw
/// file route serves committed content under.
pub(crate) const UPLOAD_SANDBOX_CSP: &str = "default-src 'none'; sandbox";

/// The type a user-uploaded file is kept and served under, given the one the
/// uploader sent. See [`PASSIVE_UPLOAD_TYPES`].
///
/// Applied on the way in as well as on the way out: a row that never carried
/// an active type cannot be served under one by a reader that forgot to ask.
pub(crate) fn passive_upload_type(stored: &str) -> &'static str {
    let essence = stored
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    PASSIVE_UPLOAD_TYPES
        .iter()
        .find(|passive| **passive == essence)
        .copied()
        .unwrap_or("application/octet-stream")
}

/// The `Content-Type` a user-uploaded file is served with, given the one it
/// was stored with. See [`passive_upload_type`].
pub(crate) fn served_upload_type(stored: &str) -> HeaderValue {
    HeaderValue::from_static(passive_upload_type(stored))
}

/// The headers every download of bytes this server did not write is served
/// under — one place, so a route cannot carry half of the set.
///
/// `content_type` is what the body is served as ([`served_upload_type`] for
/// anything an uploader chose). `Some(filename)` makes the response an
/// `attachment` named through [`attachment`]; `None` leaves the disposition
/// to the caller — the raw route shows raster images `inline`, and an OCI
/// client has no use for a name at all.
///
/// `nosniff` is also set by the global middleware, but a download must not
/// depend on which router it was mounted under to be safe. The sandboxing
/// policy covers what `nosniff` does not: a browser that *does* render the
/// bytes — a PDF, an SVG opened directly — runs nothing and reaches nothing
/// of this origin. The application's own policy is `script-src 'self'`, so
/// any same-origin response without this is a candidate script.
pub(crate) fn apply_download_headers(
    headers: &mut HeaderMap,
    content_type: HeaderValue,
    filename: Option<&str>,
) {
    headers.insert(header::CONTENT_TYPE, content_type);
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(UPLOAD_SANDBOX_CSP),
    );
    if let Some(filename) = filename {
        headers.insert(header::CONTENT_DISPOSITION, attachment(filename));
    }
}

/// Build the `Content-Disposition` value for a download named `filename`.
///
/// Both parameters are always present. RFC 6266 §4.3 lets a recipient that
/// understands `filename*` ignore `filename`, and requires one that does not to
/// ignore `filename*` — so emitting both is how a non-ASCII name reaches a
/// modern client without breaking an old one.
pub(crate) fn attachment(filename: &str) -> HeaderValue {
    let ascii = ascii_fallback(filename);
    let extended = percent_encode_attr_char(filename);
    let value = format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{extended}");

    // Both halves above emit visible ASCII only, so this conversion has nothing
    // left to reject. It is still written fallibly: an unrepresentable value
    // must cost the *name*, never the download, and never the process.
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// The `filename="…"` half: what a client that has never heard of RFC 5987
/// reads.
///
/// Anything outside printable ASCII becomes `_`, and so do the three characters
/// that would otherwise end the parameter early — `"` closes the quoted string,
/// `\` starts an escape inside it, and `;` separates parameters for every naive
/// splitter. [`filename_from_disposition`] is no longer one of them, but the
/// clients reading this half are not this server.
fn ascii_fallback(filename: &str) -> String {
    let sanitized: String = filename
        .chars()
        .map(|character| match character {
            '"' | '\\' | ';' => '_',
            ' '..='~' => character,
            _ => '_',
        })
        .collect();

    if sanitized.is_empty() {
        FALLBACK.to_string()
    } else {
        sanitized
    }
}

/// The `filename*=UTF-8''…` half: the real name, percent-encoded over its UTF-8
/// bytes, keeping only RFC 5987 `attr-char`.
fn percent_encode_attr_char(filename: &str) -> String {
    let filename = if filename.is_empty() {
        FALLBACK
    } else {
        filename
    };

    let mut encoded = String::with_capacity(filename.len());
    for byte in filename.as_bytes() {
        match byte {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'!'
            | b'#'
            | b'$'
            | b'&'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~' => encoded.push(*byte as char),
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// Read the file name out of a client's `Content-Disposition`.
///
/// `filename*` wins wherever it appears, which is RFC 6266 §4.3: the extended
/// form is the one that can carry a non-ASCII name, and a client sends the
/// plain form beside it as the lossy fallback, not as the answer.
pub(crate) fn filename_from_disposition(disposition: &str) -> Option<String> {
    let mut plain = None;

    for part in split_parameters(disposition) {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("filename*=") {
            // `UTF-8'en'name` — the charset and language are dropped, as every
            // name this server stores is UTF-8 either way.
            if let Some((_charset_and_language, encoded)) = value.split_once("''") {
                if let Ok(decoded) = percent_decode(encoded) {
                    return Some(decoded);
                }
            }
        } else if let Some(value) = part.strip_prefix("filename=") {
            // Kept, not returned: a later `filename*` in the same header still
            // outranks it.
            plain.get_or_insert_with(|| unquote(value));
        }
    }

    plain
}

/// Split a `Content-Disposition` value on the `;` that separate parameters —
/// and only on those.
///
/// A `;` inside a quoted string is part of the value: RFC 6266 §4.1 spells
/// `filename` as `token / quoted-string`, and `release;notes.txt` is a legal
/// file name this server accepts on upload and stores verbatim. Cutting the
/// header on every `;` first and stripping quotes from the pieces afterwards
/// cannot express that name at all — the parameter becomes `filename="release`,
/// the quote-stripping then yields `release`, and the client is answered `201`
/// about a file it never sent.
///
/// A quote that is never closed runs to the end of the header, which keeps a
/// malformed value from silently swallowing the parameters after it.
fn split_parameters(disposition: &str) -> Vec<String> {
    let mut parameters = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;

    for character in disposition.chars() {
        if escaped {
            // Whatever follows `\` is data, `"` and `\` included.
            current.push(character);
            escaped = false;
        } else if quoted && character == '\\' {
            current.push(character);
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
            current.push(character);
        } else if character == ';' && !quoted {
            parameters.push(std::mem::take(&mut current));
        } else {
            current.push(character);
        }
    }
    parameters.push(current);

    parameters
}

/// Read one parameter value: a quoted string with its escapes resolved, or a
/// bare token returned as it stands.
///
/// Only the quotes that delimit the value are removed. `trim_matches('"')`
/// cannot do that job — it eats a quote the name itself ends with, and leaves
/// the `\` of an escaped one behind.
fn unquote(value: &str) -> String {
    let value = value.trim();
    let Some(inner) = value.strip_prefix('"') else {
        return value.to_string();
    };

    let mut unquoted = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        match character {
            '\\' => {
                if let Some(escaped) = characters.next() {
                    unquoted.push(escaped);
                }
            }
            // The closing quote ends the value; a header is not obliged to have
            // one, and an unterminated value reads to the end of the parameter.
            '"' => break,
            _ => unquoted.push(character),
        }
    }

    unquoted
}

fn percent_decode(value: &str) -> Result<String, ()> {
    let mut decoded = Vec::with_capacity(value.len());
    let mut bytes = value.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = hex_value(bytes.next().ok_or(())?)?;
            let low = hex_value(bytes.next().ok_or(())?)?;
            decoded.push((high << 4) | low);
        } else {
            decoded.push(byte);
        }
    }
    String::from_utf8(decoded).map_err(|_| ())
}

fn hex_value(byte: u8) -> Result<u8, ()> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(()),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_download_headers, attachment, filename_from_disposition, served_upload_type,
    };
    use axum::http::{header, HeaderMap, HeaderValue};

    fn header(filename: &str) -> String {
        attachment(filename)
            .to_str()
            .expect("the builder emits visible ASCII")
            .to_string()
    }

    /// The symptom the module exists for: this name used to produce a value
    /// `HeaderValue` refuses, which was a `500` on one endpoint and a missing
    /// header on three others.
    #[test]
    fn a_non_ascii_name_is_carried_by_the_extended_form() {
        let value = header("пакет-1.0.tgz");
        assert_eq!(
            value,
            "attachment; filename=\"_____-1.0.tgz\"; \
             filename*=UTF-8''%D0%BF%D0%B0%D0%BA%D0%B5%D1%82-1.0.tgz"
        );
        assert_eq!(
            filename_from_disposition(&value).as_deref(),
            Some("пакет-1.0.tgz"),
            "the header must round-trip the real name"
        );
    }

    /// An ASCII name is unchanged in the half old clients read, and still
    /// carried in the half new ones prefer.
    #[test]
    fn an_ascii_name_survives_both_halves_unchanged() {
        let value = header("report-2026.pdf");
        assert_eq!(
            value,
            "attachment; filename=\"report-2026.pdf\"; filename*=UTF-8''report-2026.pdf"
        );
        assert_eq!(
            filename_from_disposition(&value).as_deref(),
            Some("report-2026.pdf")
        );
    }

    /// Quoting is the builder's job. Each of these characters ends a parameter
    /// for some parser, so none of them may reach the plain form.
    #[test]
    fn a_name_cannot_end_a_parameter_early() {
        let value = header("a\"b\\c;d e.txt");
        assert_eq!(
            value,
            "attachment; filename=\"a_b_c_d e.txt\"; filename*=UTF-8''a%22b%5Cc%3Bd%20e.txt"
        );
        assert_eq!(
            filename_from_disposition(&value).as_deref(),
            Some("a\"b\\c;d e.txt"),
            "the extended form still carries the name verbatim"
        );
    }

    /// A name with nothing representable left still produces a usable header
    /// rather than an empty `filename=""`.
    #[test]
    fn a_name_with_no_representable_characters_falls_back() {
        assert_eq!(
            header(""),
            "attachment; filename=\"download\"; filename*=UTF-8''download"
        );
    }

    /// The reading half's whole point: `filename*` outranks `filename`
    /// wherever it sits, which is what the two private copies disagreed about.
    #[test]
    fn the_extended_form_outranks_the_plain_one_in_either_order() {
        for disposition in [
            "attachment; filename=\"paket.tgz\"; filename*=UTF-8''%D0%BF%D0%B0%D0%BA%D0%B5%D1%82.tgz",
            "attachment; filename*=UTF-8''%D0%BF%D0%B0%D0%BA%D0%B5%D1%82.tgz; filename=\"paket.tgz\"",
        ] {
            assert_eq!(
                filename_from_disposition(disposition).as_deref(),
                Some("пакет.tgz"),
                "the lossy fallback must not win over the real name: {disposition}"
            );
        }
    }

    /// The reading half's own separator problem, and the mirror of
    /// `a_name_cannot_end_a_parameter_early`: `;` inside a quoted string is
    /// part of the name, not the end of the parameter. Cutting the header on
    /// `;` before reading the quotes stored this file as `release` and answered
    /// `201` without saying so.
    #[test]
    fn a_semicolon_inside_a_quoted_name_belongs_to_the_name() {
        assert_eq!(
            filename_from_disposition(r#"attachment; filename="release;notes.txt""#).as_deref(),
            Some("release;notes.txt")
        );
    }

    /// `\"` is the only way a quoted string can carry a quote, and `\\` a
    /// backslash. `trim_matches('"')` resolved neither: it ate the quote the
    /// name ends with and left the escaping backslash in place.
    #[test]
    fn an_escaped_character_inside_a_quoted_name_is_data() {
        assert_eq!(
            filename_from_disposition(r#"attachment; filename="a\"b.txt""#).as_deref(),
            Some("a\"b.txt")
        );
        assert_eq!(
            filename_from_disposition(r#"attachment; filename="back\\slash.txt""#).as_deref(),
            Some("back\\slash.txt")
        );
    }

    /// A quoted `;` may not swallow what follows it: the extended form sitting
    /// after such a name is still the one that wins.
    #[test]
    fn a_quoted_semicolon_does_not_hide_the_parameters_behind_it() {
        assert_eq!(
            filename_from_disposition(
                r#"attachment; filename="release;notes.txt"; filename*=UTF-8''%D0%BF.txt"#
            )
            .as_deref(),
            Some("п.txt")
        );
    }

    /// An unterminated quote reads to the end of the header rather than
    /// refusing the name — the same reading `split_parameters` gives a
    /// half-written value, and the same one `SearchFilters::parse` gives a
    /// query still being typed.
    #[test]
    fn an_unterminated_quote_reads_to_the_end_of_the_header() {
        assert_eq!(
            filename_from_disposition("attachment; filename=\"release").as_deref(),
            Some("release")
        );
    }

    /// A bare token needs no quotes at all, and must not lose characters to
    /// the unquoting.
    #[test]
    fn an_unquoted_name_survives_unchanged() {
        assert_eq!(
            filename_from_disposition("attachment; filename=plain.txt").as_deref(),
            Some("plain.txt")
        );
    }

    /// A broken extended form is not an answer, so the plain one is still used
    /// rather than the upload being refused for want of a name.
    #[test]
    fn an_undecodable_extended_form_falls_back_to_the_plain_one() {
        assert_eq!(
            filename_from_disposition("attachment; filename=\"paket.tgz\"; filename*=UTF-8''%ZZ")
                .as_deref(),
            Some("paket.tgz")
        );
        assert_eq!(filename_from_disposition("attachment"), None);
    }

    /// card_36b620ab3467: a type the uploader chose is served only when a
    /// browser can do nothing with it but display it.
    #[test]
    fn an_uploaded_file_is_never_served_as_something_a_browser_runs() {
        for active in [
            "text/javascript",
            "application/javascript; charset=utf-8",
            "text/html",
            "text/css",
            "application/xhtml+xml",
            "text/xml",
            "Application/ECMAScript",
            "",
            "nonsense",
        ] {
            assert_eq!(
                served_upload_type(active),
                "application/octet-stream",
                "{active:?}"
            );
        }
        assert_eq!(served_upload_type("image/PNG"), "image/png");
        assert_eq!(
            served_upload_type("text/plain; charset=utf-8"),
            "text/plain"
        );
    }

    /// Security audit finding #8: the set is one thing. A route that reaches
    /// the helper gets the type, `nosniff`, the sandbox and the name together,
    /// and whatever it had set before for any of them is replaced.
    #[test]
    fn a_download_carries_the_whole_header_set() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/javascript"),
        );
        apply_download_headers(
            &mut headers,
            HeaderValue::from_static("application/zip"),
            Some("пакет.zip"),
        );
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/zip"
        );
        assert_eq!(
            headers.get(header::X_CONTENT_TYPE_OPTIONS).unwrap(),
            "nosniff"
        );
        assert_eq!(
            headers.get(header::CONTENT_SECURITY_POLICY).unwrap(),
            "default-src 'none'; sandbox"
        );
        assert_eq!(
            headers.get(header::CONTENT_DISPOSITION).unwrap(),
            "attachment; filename=\"_____.zip\"; filename*=UTF-8''%D0%BF%D0%B0%D0%BA%D0%B5%D1%82.zip"
        );
    }

    /// Without a name the disposition is the caller's: an OCI client never
    /// reads one, and the raw route's inline image must stay inline.
    #[test]
    fn a_download_without_a_name_sets_no_disposition() {
        let mut headers = HeaderMap::new();
        apply_download_headers(
            &mut headers,
            HeaderValue::from_static("application/vnd.oci.image.manifest.v1+json"),
            None,
        );
        assert_eq!(
            headers.get(header::CONTENT_TYPE).unwrap(),
            "application/vnd.oci.image.manifest.v1+json"
        );
        assert!(headers.get(header::CONTENT_DISPOSITION).is_none());
        assert_eq!(
            headers.get(header::CONTENT_SECURITY_POLICY).unwrap(),
            "default-src 'none'; sandbox"
        );
    }
}
