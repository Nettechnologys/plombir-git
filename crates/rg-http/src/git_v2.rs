//! Git Smart Protocol V2 HTTP handling.
//!
//! Protocol V2 over HTTP uses the same endpoints as V1, but:
//! 1. Client sends `Git-Protocol: version=2` header
//! 2. Server responds with V2 capability advertisement
//! 3. Subsequent requests use V2 command format
//!
//! Reference: <https://git-scm.com/docs/protocol-v2>

use axum::http::HeaderMap;

/// Check if client wants Protocol V2 based on HTTP headers.
pub fn wants_protocol_v2(headers: &HeaderMap) -> bool {
    if let Some(git_protocol) = headers.get("Git-Protocol") {
        if let Ok(protocol) = git_protocol.to_str() {
            return protocol.contains("version=2");
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use rg_git::protocol::v2::ADVERTISED_CAPABILITIES;

    /// Build V2 capability advertisement synchronously.
    /// Uses the same format as `build_v2_capability_advertisement` in lib.rs;
    /// kept as a parity check that `ADVERTISED_CAPABILITIES` still advertises
    /// the fetch features the HTTP advertisement relies on.
    fn build_v2_capability_sync() -> Vec<u8> {
        let mut buf = Vec::new();

        // Smart HTTP: info/refs response starts with pkt-line wrapped # service= header
        let service_line = "# service=git-upload-pack\n";
        let service_len = service_line.len() + 4;
        buf.extend_from_slice(format!("{:04x}{}", service_len, service_line).as_bytes());
        buf.extend_from_slice(b"0000"); // flush after service header

        // Helper to write pkt-line data
        let write_pkt = |buf: &mut Vec<u8>, text: &str| {
            let payload = text.as_bytes();
            let len = payload.len() + 4 + 1; // +4 for hex header, +1 for trailing \n
            buf.extend_from_slice(format!("{:04x}{}\n", len, text).as_bytes());
        };

        write_pkt(&mut buf, "version 2");
        for capability in ADVERTISED_CAPABILITIES {
            write_pkt(&mut buf, capability);
        }
        buf.extend_from_slice(b"0000");

        buf
    }

    #[test]
    fn http_advertisement_matches_supported_fetch_features() {
        let output = String::from_utf8(build_v2_capability_sync()).unwrap();

        assert!(output.contains("fetch=shallow filter\n"));
    }
}
