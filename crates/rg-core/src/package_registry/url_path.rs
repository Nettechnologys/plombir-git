//! Percent-encoding for the URLs the registry hands to package clients.
//!
//! Every protocol document this crate builds — an npm packument, a Composer
//! `packages.json`, a NuGet registration index — carries absolute URLs the
//! client will then fetch. Those URLs must land on the route the table
//! declares, and a package name is not a safe path component: npm's scoped
//! names (`@scope/name`) and Composer's `vendor/package` both carry a literal
//! slash, which turns one route segment into two and makes the client's own
//! download 404 on a path the server never advertised.
//!
//! Encoding here is the mirror of what axum's `Path` extractor does on the way
//! in — it percent-decodes each segment — so a name written as `@scope%2Fname`
//! arrives at the handler as the stored `@scope/name` and matches the row.

/// Percent-encode `value` so it occupies exactly ONE path segment.
///
/// Everything outside RFC 3986's unreserved set is escaped, `/` included — the
/// whole point being that a slash inside a package name must not become a path
/// separator. Names that are already safe (`matrix-npm`, `1.0.0`) pass through
/// byte-identical, so encoding an existing URL never changes it.
pub fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(byte as char)
            }
            _ => {
                encoded.push('%');
                encoded.push(HEX[(byte >> 4) as usize] as char);
                encoded.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::encode_path_segment;

    /// A name that is already a legal segment must survive untouched, or every
    /// URL the registry has ever published would change shape at once.
    #[test]
    fn ordinary_names_and_versions_pass_through_unchanged() {
        assert_eq!(encode_path_segment("matrix-npm"), "matrix-npm");
        assert_eq!(encode_path_segment("1.0.0"), "1.0.0");
        assert_eq!(
            encode_path_segment("matrix-npm-1.0.0.tgz"),
            "matrix-npm-1.0.0.tgz"
        );
        assert_eq!(encode_path_segment("Some_Package.Id"), "Some_Package.Id");
    }

    /// The case the encoding exists for: a slash inside the name must not
    /// become a path separator, and `@` must not be left to a client's own
    /// normalization.
    #[test]
    fn a_scoped_name_becomes_one_segment() {
        assert_eq!(
            encode_path_segment("@scope/name"),
            "%40scope%2Fname",
            "a scoped npm name must occupy one route segment"
        );
        assert_eq!(encode_path_segment("vendor/package"), "vendor%2Fpackage");
    }

    /// Characters that would end the path or start a query must not reach the
    /// URL raw either.
    #[test]
    fn query_and_fragment_delimiters_are_escaped() {
        assert_eq!(encode_path_segment("a?b#c"), "a%3Fb%23c");
        assert_eq!(encode_path_segment("a b"), "a%20b");
        assert_eq!(encode_path_segment("1.0.0+build"), "1.0.0%2Bbuild");
    }
}
