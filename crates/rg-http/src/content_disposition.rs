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

use axum::http::HeaderValue;

/// The name used when a caller supplies one that survives neither form.
const FALLBACK: &str = "download";

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
/// splitter, [`filename_from_disposition`] included.
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

    for part in disposition.split(';') {
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
            plain.get_or_insert_with(|| value.trim_matches('"').to_string());
        }
    }

    plain
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
    use super::{attachment, filename_from_disposition};

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
}
