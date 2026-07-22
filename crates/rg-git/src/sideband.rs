//! Git sideband-64k protocol implementation.
//!
//! Sideband multiplexes multiple data streams (data, progress, error)
//! over a single channel. Each pkt-line has a 1-byte band number prefix:
//! - Band 1: Packfile data
//! - Band 2: Progress messages (displayed on stderr by client)
//! - Band 3: Error messages (fatal errors)

use anyhow::Result;
use tokio::io::{AsyncWrite, AsyncWriteExt};

use crate::pkt_line::{write_flush, write_pkt_line, PktLine};

/// Maximum sideband data size per pkt-line (65516 - 1 band byte = 65515 user bytes).
/// Must not exceed MAX_PKT_LINE_LEN (65516) including the band byte prefix.
const SIDEBAND_MAX: usize = 65515;

/// Write sideband data (band 1) — used for packfile data.
pub async fn write_sideband_data<W: AsyncWrite + Unpin>(writer: &mut W, data: &[u8]) -> Result<()> {
    for chunk in data.chunks(SIDEBAND_MAX) {
        let mut payload = vec![1u8]; // band 1
        payload.extend_from_slice(chunk);
        write_pkt_line(writer, &PktLine::Data(payload)).await?;
    }
    writer.flush().await?;
    Ok(())
}

/// Write sideband progress (band 2) — displayed on client stderr.
pub async fn write_sideband_progress<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &str,
) -> Result<()> {
    let mut payload = vec![2u8]; // band 2
    payload.extend_from_slice(message.as_bytes());
    write_pkt_line(writer, &PktLine::Data(payload)).await?;
    Ok(())
}

/// Write sideband error (band 3) — fatal error message.
pub async fn write_sideband_error<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &str,
) -> Result<()> {
    let mut payload = vec![3u8]; // band 3
    payload.extend_from_slice(message.as_bytes());
    write_pkt_line(writer, &PktLine::Data(payload)).await?;
    Ok(())
}

/// Write the sideband flush packet (signals end of multiplexed data).
pub async fn write_sideband_flush<W: AsyncWrite + Unpin>(writer: &mut W) -> Result<()> {
    write_flush(writer).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pkt_line::{read_pkt_line, PktLine};
    use tokio::io::{duplex, BufReader};

    #[tokio::test]
    async fn test_sideband_data_write_and_read() {
        let (mut writer, reader) = duplex(1024);
        write_sideband_data(&mut writer, b"hello pack data")
            .await
            .unwrap();

        let mut buf_reader = BufReader::new(reader);
        let pkt = read_pkt_line(&mut buf_reader).await.unwrap();
        if let PktLine::Data(data) = pkt {
            assert_eq!(data[0], 1u8); // band 1
            assert_eq!(&data[1..], b"hello pack data");
        } else {
            panic!("expected Data pkt-line");
        }
    }

    #[tokio::test]
    async fn test_sideband_progress_write_and_read() {
        let (mut writer, reader) = duplex(1024);
        write_sideband_progress(&mut writer, "counting objects")
            .await
            .unwrap();

        let mut buf_reader = BufReader::new(reader);
        let pkt = read_pkt_line(&mut buf_reader).await.unwrap();
        if let PktLine::Data(data) = pkt {
            assert_eq!(data[0], 2u8); // band 2
            assert_eq!(&data[1..], b"counting objects");
        } else {
            panic!("expected Data pkt-line");
        }
    }

    #[tokio::test]
    async fn test_sideband_error_write_and_read() {
        let (mut writer, reader) = duplex(1024);
        write_sideband_error(&mut writer, "fatal error occurred")
            .await
            .unwrap();

        let mut buf_reader = BufReader::new(reader);
        let pkt = read_pkt_line(&mut buf_reader).await.unwrap();
        if let PktLine::Data(data) = pkt {
            assert_eq!(data[0], 3u8); // band 3
            assert_eq!(&data[1..], b"fatal error occurred");
        } else {
            panic!("expected Data pkt-line");
        }
    }

    #[tokio::test]
    async fn test_sideband_flush_write_and_read() {
        let (mut writer, reader) = duplex(1024);
        write_sideband_flush(&mut writer).await.unwrap();

        let mut buf_reader = BufReader::new(reader);
        let pkt = read_pkt_line(&mut buf_reader).await.unwrap();
        assert!(matches!(pkt, PktLine::Flush));
    }

    #[tokio::test]
    async fn test_sideband_chunk_size_within_limit() {
        // Verify runtime chunking behavior for the maximum-size payload.
        let payload = vec![b'x'; SIDEBAND_MAX];
        let (mut writer, reader) = duplex(SIDEBAND_MAX + 1024);
        write_sideband_data(&mut writer, &payload).await.unwrap();

        drop(writer);
        let mut reader = BufReader::new(reader);
        let pkt = read_pkt_line(&mut reader).await.expect("read pkt line");
        if let PktLine::Data(data) = pkt {
            assert_eq!(data[0], 1u8, "expected band 1");
            assert_eq!(
                data.len() - 1,
                payload.len(),
                "expected exactly one data chunk"
            );
        } else {
            panic!("expected Data pkt-line");
        }

        let eof = read_pkt_line(&mut reader).await;
        assert!(
            matches!(eof, Ok(PktLine::Flush)),
            "expected stream flush at end"
        );
    }
}
