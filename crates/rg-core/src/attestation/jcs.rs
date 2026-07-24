//! Minimal RFC 8785 (JSON Canonicalization Scheme) serializer.
//!
//! Why hand-rolled instead of pulling a crate: the payload we canonicalize is
//! wholly server-generated (an in-toto statement — strings, integers, nested
//! objects/arrays), so we only need the subset of JCS that covers those types,
//! and keeping it in-tree makes the attestation module a self-contained,
//! offline-buildable unit with frozen cross-language test vectors.
//!
//! Correctness notes:
//! - Object keys are sorted by their UTF-16 code units, exactly as RFC 8785
//!   §3.2.3 requires (ECMAScript `Array.prototype.sort` order over property
//!   names). For the ASCII keys we emit this coincides with byte order, but we
//!   implement the UTF-16 comparison anyway so the serializer stays correct for
//!   any key.
//! - Scalar formatting (string escaping, `true`/`false`/`null`, integers) is
//!   delegated to `serde_json`, whose compact output already matches RFC 8785
//!   for these types (minimal string escapes, no insignificant whitespace).
//! - Non-integer numbers are **rejected** rather than emitted. RFC 8785 number
//!   canonicalization requires ECMAScript `Number::toString` (ryu-js); we never
//!   sign a float, so failing loudly is safer than emitting a non-canonical
//!   form that another language would serialize differently.

use anyhow::{bail, Result};
use serde_json::Value;

/// Serialize `value` into its RFC 8785 canonical byte form.
pub fn to_canonical_bytes(value: &Value) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    write_value(value, &mut out)?;
    Ok(out)
}

fn write_value(value: &Value, out: &mut Vec<u8>) -> Result<()> {
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(b) => out.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                out.extend_from_slice(u.to_string().as_bytes());
            } else if let Some(i) = n.as_i64() {
                out.extend_from_slice(i.to_string().as_bytes());
            } else {
                // A non-integer number reached the canonicalizer. We never sign
                // floats; refuse rather than emit a non-RFC-8785 rendering.
                bail!("JCS: non-integer numbers are not supported in signed payloads");
            }
        }
        Value::String(s) => write_string(s, out),
        Value::Array(items) => {
            out.push(b'[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_value(item, out)?;
            }
            out.push(b']');
        }
        Value::Object(map) => {
            // Sort keys by UTF-16 code units (RFC 8785 §3.2.3).
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort_by(|a, b| cmp_utf16(a, b));
            out.push(b'{');
            for (i, key) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(b',');
                }
                write_string(key, out);
                out.push(b':');
                write_value(&map[*key], out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}

/// Emit a JSON string with RFC 8785-conformant escaping. `serde_json`'s own
/// string serializer already produces this (minimal short escapes, raw UTF-8
/// for everything else), so we reuse it rather than re-implement the escape
/// table.
fn write_string(s: &str, out: &mut Vec<u8>) {
    // Serializing a bare string never fails and yields the quoted, escaped form.
    let encoded = Value::String(s.to_string()).to_string();
    out.extend_from_slice(encoded.as_bytes());
}

/// Compare two strings by their UTF-16 code-unit sequences.
fn cmp_utf16(a: &str, b: &str) -> std::cmp::Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn sorts_object_keys_and_strips_whitespace() {
        let v = json!({ "b": 1, "a": 2, "c": { "z": 1, "y": 2 } });
        let bytes = to_canonical_bytes(&v).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"a":2,"b":1,"c":{"y":2,"z":1}}"#
        );
    }

    #[test]
    fn arrays_preserve_order() {
        let v = json!({ "list": [3, 1, 2], "s": "x" });
        let bytes = to_canonical_bytes(&v).unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            r#"{"list":[3,1,2],"s":"x"}"#
        );
    }

    #[test]
    fn escapes_strings_minimally_and_keeps_unicode() {
        let v = json!({ "k": "a\"b\\c\n\tλ" });
        let bytes = to_canonical_bytes(&v).unwrap();
        // Quote, backslash and control chars use short escapes; λ (U+03BB)
        // stays as raw UTF-8.
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "{\"k\":\"a\\\"b\\\\c\\n\\tλ\"}"
        );
    }

    #[test]
    fn utf16_key_ordering_is_deterministic() {
        // Keys with a non-BMP char sort by UTF-16 code units, not code points.
        let v = json!({ "\u{1F600}": 1, "\u{FFFF}": 2 });
        // U+FFFF is a single UTF-16 unit 0xFFFF; U+1F600 is a surrogate pair
        // starting at 0xD83D, which is < 0xFFFF, so the emoji key sorts first.
        let bytes = to_canonical_bytes(&v).unwrap();
        let s = String::from_utf8(bytes).unwrap();
        assert!(s.find('\u{1F600}').unwrap() < s.find('\u{FFFF}').unwrap());
    }

    #[test]
    fn rejects_float_in_payload() {
        let v = json!({ "x": 1.5 });
        assert!(to_canonical_bytes(&v).is_err());
    }
}
