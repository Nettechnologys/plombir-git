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

/// Write a delimiter packet (0001) - used in V2 protocol.
pub async fn write_delim<W: AsyncWrite + Unpin>(writer: &mut W) -> Result<()> {
    writer.write_all(b"0001").await?;
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

/// Read pkt-lines until flush. Returns all data lines (excluding flush).
///
/// Accepts any `AsyncRead + Unpin` directly.
pub async fn read_pkt_lines_until_flush<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Vec<PktLine>> {
    let mut lines = Vec::new();
    loop {
        let pkt = read_pkt_line(reader).await?;
        match pkt {
            PktLine::Flush => break,
            _ => lines.push(pkt),
        }
    }
    Ok(lines)
}

/// Read a single text line (non-flush pkt-line) as a string.
///
/// Accepts any `AsyncRead + Unpin` directly.
pub async fn read_text_line<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<String>> {
    let pkt = read_pkt_line(reader).await?;
    match pkt {
        PktLine::Flush => Ok(None),
        PktLine::Data(data) => {
            let text = String::from_utf8(data)?;
            Ok(Some(text))
        }
        PktLine::Delim => Ok(Some(String::new())), // Treat as empty line
        PktLine::ResponseEnd => Ok(None),          // End of response
    }
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
    async fn test_read_multiple_then_flush() {
        let mut reader = make_reader(&[
            PktLine::data(b"line one\n"),
            PktLine::data(b"line two\n"),
            PktLine::Flush,
        ]);
        let lines = read_pkt_lines_until_flush(&mut reader).await.unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], PktLine::data(b"line one\n"));
        assert_eq!(lines[1], PktLine::data(b"line two\n"));
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
        let data = PktLine::text("agent=ironforge/0.1");
        write_pkt_line(&mut writer, &data).await.unwrap();
        writer.flush().await.unwrap();

        let mut reader = BufReader::new(read_end);
        let pkt = read_pkt_line(&mut reader).await.unwrap();
        assert_eq!(pkt, PktLine::text("agent=ironforge/0.1"));
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

    #[tokio::test]
    async fn test_read_text_line_returns_string() {
        let mut reader = make_reader(&[PktLine::data(b"some text\n"), PktLine::Flush]);
        let line = read_text_line(&mut reader).await.unwrap();
        assert_eq!(line, Some("some text\n".to_string()));
    }

    #[tokio::test]
    async fn test_read_text_line_on_flush_returns_none() {
        let mut reader = make_reader(&[PktLine::Flush]);
        let line = read_text_line(&mut reader).await.unwrap();
        assert!(line.is_none());
    }
}
