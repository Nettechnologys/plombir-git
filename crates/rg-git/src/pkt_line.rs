//! Git pkt-line protocol implementation.
//!
//! Pkt-line format: 4 hex digits for length (including the 4-byte header),
//! followed by payload data. A line of "0000" is a flush packet.

use std::fmt;

use anyhow::{bail, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum payload size per pkt-line (65516 bytes, per git protocol).
pub const MAX_PKT_LINE_LEN: usize = 65516;

/// A pkt-line: data, flush, or special types for V2 protocol.
#[derive(Debug, Clone, PartialEq)]
pub enum PktLine {
    Data(Vec<u8>),
    Flush,
    /// Delimiter packet (0001) - separates sections in V2
    Delim,
    /// Response-end packet (0002) - marks end of response in stateless connections
    ResponseEnd,
}

impl PktLine {
    /// Create a data pkt-line from bytes.
    pub fn data(data: &[u8]) -> Self {
        PktLine::Data(data.to_vec())
    }

    /// Create a text pkt-line from a string (with trailing newline).
    pub fn text(text: &str) -> Self {
        let mut v = text.as_bytes().to_vec();
        if !v.ends_with(b"\n") {
            v.push(b'\n');
        }
        PktLine::Data(v)
    }
}

impl fmt::Display for PktLine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PktLine::Data(data) => {
                // Try to display as text
                match std::str::from_utf8(data) {
                    Ok(s) => write!(f, "Data({})", s.trim_end()),
                    Err(_) => write!(f, "Data({} bytes)", data.len()),
                }
            }
            PktLine::Flush => write!(f, "Flush"),
            PktLine::Delim => write!(f, "Delim"),
            PktLine::ResponseEnd => write!(f, "ResponseEnd"),
        }
    }
}

/// Write a single pkt-line to a writer.
pub async fn write_pkt_line<W: AsyncWrite + Unpin>(writer: &mut W, pkt: &PktLine) -> Result<()> {
    match pkt {
        PktLine::Data(data) => {
            if data.len() > MAX_PKT_LINE_LEN {
                bail!(
                    "pkt-line data too large: {} bytes (max {})",
                    data.len(),
                    MAX_PKT_LINE_LEN
                );
            }
            let len = data.len() + 4; // +4 for the length header itself
            let header = format!("{:04x}", len);
            writer.write_all(header.as_bytes()).await?;
            writer.write_all(data).await?;
        }
        PktLine::Flush => {
            writer.write_all(b"0000").await?;
        }
        PktLine::Delim => {
            writer.write_all(b"0001").await?;
        }
        PktLine::ResponseEnd => {
            writer.write_all(b"0002").await?;
        }
    }
    Ok(())
}

/// Write a flush packet (0000).
pub async fn write_flush<W: AsyncWrite + Unpin>(writer: &mut W) -> Result<()> {
    writer.write_all(b"0000").await?;
    Ok(())
}

/// Read a single pkt-line from an async reader.
///
/// Accepts any `AsyncRead + Unpin` directly (with or without BufReader).
/// Using a `BufReader` is recommended for performance when reading many small
/// pkt-lines over a network stream.
pub async fn read_pkt_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<PktLine> {
    let mut header = [0u8; 4];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            // Connection closed gracefully
            return Ok(PktLine::Flush);
        }
        Err(e) => return Err(e.into()),
    }

    let header_str = std::str::from_utf8(&header)?;
    let len: usize = match u32::from_str_radix(header_str, 16) {
        Ok(0) => return Ok(PktLine::Flush),
        Ok(1) => return Ok(PktLine::Delim), // 0001 = delimiter
        Ok(2) => return Ok(PktLine::ResponseEnd), // 0002 = response end
        Ok(n) => n as usize,
        Err(_) => bail!("invalid pkt-line header: {:?}", header),
    };

    if len < 4 {
        bail!("invalid pkt-line length: {}", len);
    }

    let payload_len = len - 4;
    if payload_len == 0 {
        return Ok(PktLine::Data(Vec::new()));
    }

    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload).await?;
    Ok(PktLine::Data(payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use tokio::io::BufReader;

    /// Helper: encode a pkt-line into bytes (sync version for tests).
    fn encode_pkt_line_bytes(data: &[u8]) -> Vec<u8> {
        let len = data.len() + 4;
        let header = format!("{:04x}", len);
        let mut out = header.into_bytes();
        out.extend_from_slice(data);
        out
    }

    /// Helper: write multiple pkt-lines into a buffer for reading.
    fn make_reader(packets: &[PktLine]) -> BufReader<Cursor<Vec<u8>>> {
        let mut buf = Vec::new();
        // We use a sync approximation: build the raw bytes directly.
        for pkt in packets {
            match pkt {
                PktLine::Data(data) => {
                    buf.extend_from_slice(&encode_pkt_line_bytes(data));
                }
                PktLine::Flush => buf.extend_from_slice(b"0000"),
                PktLine::Delim => buf.extend_from_slice(b"0001"),
                PktLine::ResponseEnd => buf.extend_from_slice(b"0002"),
            }
        }
        BufReader::new(Cursor::new(buf))
    }

    #[tokio::test]
    async fn test_read_data_pkt_line() {
        let mut reader = make_reader(&[PktLine::data(b"hello world\n"), PktLine::Flush]);
        let pkt = read_pkt_line(&mut reader).await.unwrap();
        match pkt {
            PktLine::Data(d) => assert_eq!(d, b"hello world\n"),
            other => panic!("expected Data, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn test_read_flush_pkt_line() {
        let mut reader = make_reader(&[PktLine::Flush]);
        let pkt = read_pkt_line(&mut reader).await.unwrap();
        assert!(matches!(pkt, PktLine::Flush));
    }

    #[tokio::test]
    async fn test_read_delim_pkt_line() {
        let mut reader = make_reader(&[PktLine::Delim]);
        let pkt = read_pkt_line(&mut reader).await.unwrap();
        assert!(matches!(pkt, PktLine::Delim));
    }

    #[tokio::test]
    async fn test_read_response_end_pkt_line() {
        let mut reader = make_reader(&[PktLine::ResponseEnd]);
        let pkt = read_pkt_line(&mut reader).await.unwrap();
        assert!(matches!(pkt, PktLine::ResponseEnd));
    }

    #[tokio::test]
    async fn test_read_empty_data_pkt_line() {
        // A pkt-line with just the 4-byte header (length=4, payload=0) should return empty data.
        let buf = Vec::from(b"0004".as_slice());
        let mut reader = BufReader::new(Cursor::new(buf));
        let pkt = read_pkt_line(&mut reader).await.unwrap();
        assert!(matches!(pkt, PktLine::Data(ref d) if d.is_empty()));
    }

    #[tokio::test]
    async fn test_write_and_read_roundtrip() {
        use tokio::io::duplex;
        let (mut writer, read_end) = duplex(1024);
        let data = PktLine::text("agent=forgekeep/0.1");
        write_pkt_line(&mut writer, &data).await.unwrap();
        writer.flush().await.unwrap();

        let mut reader = BufReader::new(read_end);
        let pkt = read_pkt_line(&mut reader).await.unwrap();
        assert_eq!(pkt, PktLine::text("agent=forgekeep/0.1"));
    }

    #[test]
    fn test_pkt_line_text_adds_newline() {
        let pkt = PktLine::text("hello");
        assert_eq!(pkt, PktLine::data(b"hello\n"));
    }

    #[test]
    fn test_pkt_line_text_preserves_newline() {
        let pkt = PktLine::text("hello\n");
        assert_eq!(pkt, PktLine::data(b"hello\n"));
    }

    #[test]
    fn test_pkt_line_display() {
        assert_eq!(format!("{}", PktLine::Flush), "Flush");
        assert_eq!(format!("{}", PktLine::Delim), "Delim");
        assert_eq!(format!("{}", PktLine::ResponseEnd), "ResponseEnd");
        assert_eq!(format!("{}", PktLine::data(b"hello\n")), "Data(hello)");
        assert_eq!(
            format!("{}", PktLine::data(b"\xff\xfe\xfd")),
            "Data(3 bytes)"
        );
    }

    // --- Malformed / truncated input must return Err, never panic (CWE-252/755) ---

    #[tokio::test]
    async fn test_read_invalid_hex_header_errors() {
        // A non-hex length header is malformed; the parser must return Err, not panic.
        let mut reader = BufReader::new(Cursor::new(Vec::from(b"zzzz".as_slice())));
        let result = read_pkt_line(&mut reader).await;
        assert!(
            result.is_err(),
            "expected Err on non-hex header, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_read_non_utf8_header_errors() {
        // A header with non-UTF-8 bytes must be rejected gracefully.
        let mut reader = BufReader::new(Cursor::new(vec![0xffu8, 0xfe, 0xfd, 0xfc]));
        let result = read_pkt_line(&mut reader).await;
        assert!(
            result.is_err(),
            "expected Err on non-UTF-8 header, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_read_length_below_minimum_errors() {
        // A declared length < 4 is impossible (the header itself is 4 bytes).
        let mut reader = BufReader::new(Cursor::new(Vec::from(b"0003".as_slice())));
        let result = read_pkt_line(&mut reader).await;
        assert!(
            result.is_err(),
            "expected Err on length < 4, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_read_truncated_payload_errors() {
        // Header claims a 12-byte payload but only 5 bytes follow, then EOF.
        // read_exact on the payload must surface an Err, not panic.
        let mut buf = Vec::from(b"0010".as_slice()); // len=16 → payload_len=12
        buf.extend_from_slice(b"short");
        let mut reader = BufReader::new(Cursor::new(buf));
        let result = read_pkt_line(&mut reader).await;
        assert!(
            result.is_err(),
            "expected Err on truncated payload, got {result:?}"
        );
    }

    #[tokio::test]
    async fn test_read_truncated_header_is_graceful_eof() {
        // A partial (1-3 byte) header followed by EOF is treated as a graceful
        // connection close (Flush), matching read_exact's UnexpectedEof handling.
        let mut reader = BufReader::new(Cursor::new(Vec::from(b"00".as_slice())));
        let pkt = read_pkt_line(&mut reader)
            .await
            .expect("partial header must not error");
        assert!(matches!(pkt, PktLine::Flush));
    }

    // --- Fuzz / property tests: no bounded input must ever panic (CWE-248/755) ---
    //
    // The parser is the single trust boundary for every Git wire consumer
    // (upload-pack, receive-pack, protocol v2, sideband — see grep in the card),
    // so hardening it here hardens all of them. We can't pull in `proptest`
    // (no workspace dependency, and CI must stay deterministic), so we drive a
    // small seeded xorshift PRNG. Same seed → identical corpus on every run, so
    // a failure is always reproducible from the seed printed in the panic.

    /// Deterministic, dependency-free PRNG (xorshift64*). Seed must be non-zero.
    struct Rng(u64);

    impl Rng {
        fn new(seed: u64) -> Self {
            debug_assert!(seed != 0, "xorshift seed must be non-zero");
            Rng(seed)
        }

        fn next_u64(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        fn byte(&mut self) -> u8 {
            (self.next_u64() & 0xff) as u8
        }

        /// Uniform-ish value in `0..n` (n > 0).
        fn below(&mut self, n: usize) -> usize {
            (self.next_u64() % n as u64) as usize
        }
    }

    #[tokio::test]
    async fn fuzz_read_pkt_line_never_panics_on_random_bytes() {
        // Property: on any finite byte slice, read_pkt_line returns Ok(..) or
        // Err(..) — it must never panic, and it must always terminate.
        let mut rng = Rng::new(0x9E37_79B9_7F4A_7C15);
        for _ in 0..4000 {
            let len = rng.below(72);
            let buf: Vec<u8> = (0..len).map(|_| rng.byte()).collect();
            let mut reader = BufReader::new(Cursor::new(buf));
            // The `let _ =` is the assertion: reaching here means no panic/hang.
            drop(read_pkt_line(&mut reader).await);
        }
    }

    #[tokio::test]
    async fn fuzz_structured_headers_never_panic() {
        // Bias the corpus toward *valid-looking* 4-hex headers with mismatched
        // payloads — this sweeps the whole 0x0000..=0xffff length space, including
        // the "oversized" region above MAX_PKT_LINE_LEN and short/truncated
        // payloads that must surface as a graceful Err, not a panic or a giant
        // (but still bounded, ≤64 KiB) allocation that hangs.
        let mut rng = Rng::new(0xD1B5_4A32_D192_ED03);
        for _ in 0..4000 {
            let declared = (rng.next_u64() & 0xffff) as usize;
            let mut buf = format!("{:04x}", declared).into_bytes();
            let payload_len = rng.below(80);
            buf.extend((0..payload_len).map(|_| rng.byte()));
            let mut reader = BufReader::new(Cursor::new(buf));
            drop(read_pkt_line(&mut reader).await);
        }
    }
}
