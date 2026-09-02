//! Moving `git pack-objects` output to a client without ever holding a pack.
//!
//! Both upload-pack dialects generate their clone/fetch packfile by spawning
//! `git pack-objects --stdout`. git streams that pack out as it builds it, so
//! the server never needs more than a working window of it — but the obvious
//! spelling, `read_to_end` into a `Vec`, throws that away and makes the peak
//! resident cost of one clone the size of the repository. A 2 GiB repository
//! then costs 2 GiB of server RAM per concurrent clone, chosen by whoever runs
//! `git clone`, and the HTTP transport used to keep a second copy on top of it
//! (`card_73f02e2a97ad`).
//!
//! [`stream_pack_objects`] is the one place that reads the pack, so there is a
//! single answer to "how much of a clone is in memory": one
//! [`PACK_STREAM_CHUNK_BYTES`] buffer, plus whatever the writer downstream
//! chooses to hold. Three things it does that a bare copy loop does not:
//!
//! - **The chunk is filled before it is written.** A pipe read returns whatever
//!   is available, so a plain `read`-then-`write` loop would frame the sideband
//!   by the scheduler's whim. Filling first keeps the pkt-line stream identical
//!   to the fully-buffered version — full [`sideband`] packets with one short
//!   packet at the end — which is what makes this change invisible on the wire.
//! - **stderr is drained concurrently and bounded.** git's stderr pipe is small;
//!   leaving it unread while we consume stdout lets a chatty failure fill it and
//!   deadlock the pack. Draining it into an unbounded `Vec` would reopen the
//!   hole this module exists to close from the other side, so the drain stops
//!   recording after [`MAX_STDERR_BYTES`] and keeps consuming.
//! - **The exit status is checked after the last byte, and failing late is not
//!   silent.** A pack that has already started flowing cannot be taken back with
//!   an HTTP status; what it can be given is a sideband band-3 error, which the
//!   client prints and treats as fatal, plus an `Err` for the caller so no
//!   transport reports success. A truncated pack alone would also fail the
//!   client (`index-pack` validates the trailer), but only as a mystery.

use anyhow::{bail, Context, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::process::Child;

use crate::sideband;

/// Bytes moved between `git pack-objects` and the client in one step.
///
/// Deliberately equal to the sideband payload ceiling: a full buffer then maps
/// to exactly one band-1 pkt-line, so the framing a client sees does not depend
/// on how the kernel happened to split the pipe reads.
pub(crate) const PACK_STREAM_CHUNK_BYTES: usize = sideband::SIDEBAND_MAX;

/// How much of `git pack-objects` stderr is kept for the failure message.
///
/// Enough for git's own `fatal:` line and context; past that the drain keeps
/// reading (so the pipe cannot fill) but stops growing.
const MAX_STDERR_BYTES: usize = 8 * 1024;

/// Stream a spawned `git pack-objects --stdout` to `writer`, returning how many
/// pack bytes were sent.
///
/// The caller owns stdin — `--all` wants it closed, `--revs` wants a revision
/// list written to it — and must have done so before calling; this function
/// only ever reads.
///
/// `use_sideband` selects the framing: band-1 pkt-lines, or the raw pack for a
/// client that negotiated no sideband. A non-zero exit is an error either way;
/// with sideband it is *also* announced on band 3 first, so the client learns
/// why its clone stopped instead of only that it did.
pub(crate) async fn stream_pack_objects<W>(
    mut child: Child,
    writer: &mut W,
    use_sideband: bool,
) -> Result<u64>
where
    W: AsyncWrite + Unpin,
{
    let stdout = child.stdout.take().context("no stdout from pack-objects")?;
    // Drain stderr on its own task for the whole life of the pack: git writes
    // there while it is still writing stdout, and an unread full pipe stops the
    // process we are waiting on.
    let stderr = child.stderr.take();
    let stderr_task = tokio::spawn(async move { drain_bounded(stderr).await });

    let streamed = copy_pack(stdout, writer, use_sideband).await;

    let status = child.wait().await.context("git pack-objects wait failed")?;
    let stderr_msg = stderr_task.await.unwrap_or_default();

    // A read/write failure is reported before the exit status: git exiting
    // non-zero *because* its stdout went away would otherwise overwrite the
    // real cause with a consequence of it.
    let streamed = streamed?;

    if !status.success() {
        let detail = String::from_utf8_lossy(&stderr_msg).trim().to_string();
        // Already-sent pack bytes cannot be recalled. Band 3 is the protocol's
        // own way to say the rest is not coming; without it the client sees an
        // unexplained short pack.
        if use_sideband {
            let announced = if detail.is_empty() {
                format!("git pack-objects failed ({status})\n")
            } else {
                format!("git pack-objects failed ({status}): {detail}\n")
            };
            // Best-effort: the peer may already be gone, and the `Err` below is
            // what the server acts on regardless.
            if let Err(error) = sideband::write_sideband_error(writer, &announced).await {
                tracing::warn!(
                    %error,
                    "could not tell the client why pack generation failed"
                );
            }
        }
        tracing::error!(
            %status,
            pack_bytes = streamed,
            stderr = %detail,
            "git pack-objects failed after streaming part of the pack"
        );
        bail!("git pack-objects failed ({status}): {detail}");
    }

    Ok(streamed)
}

/// Copy `stdout` into `writer` a full chunk at a time.
async fn copy_pack<R, W>(mut stdout: R, writer: &mut W, use_sideband: bool) -> Result<u64>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = vec![0u8; PACK_STREAM_CHUNK_BYTES];
    let mut streamed: u64 = 0;

    loop {
        let filled = fill(&mut stdout, &mut buf)
            .await
            .context("failed to read packfile from pack-objects")?;
        if filled == 0 {
            break;
        }
        streamed += filled as u64;
        if use_sideband {
            sideband::write_sideband_data(writer, &buf[..filled]).await?;
        } else {
            writer.write_all(&buf[..filled]).await?;
        }
    }

    if !use_sideband {
        writer.flush().await?;
    }
    Ok(streamed)
}

/// Read until `buf` is full or the reader ends, returning how much was read.
///
/// A short read is not end-of-stream on a pipe, and treating it as a chunk
/// boundary would make the sideband framing depend on scheduling.
async fn fill<R>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<usize>
where
    R: AsyncRead + Unpin,
{
    let mut filled = 0;
    while filled < buf.len() {
        let read = reader.read(&mut buf[filled..]).await?;
        if read == 0 {
            break;
        }
        filled += read;
    }
    Ok(filled)
}

/// Consume `stderr` to the end, keeping at most [`MAX_STDERR_BYTES`] of it.
///
/// The whole pipe is read even once the cap is reached: the point of the drain
/// is that git never blocks writing to it.
async fn drain_bounded<R>(stderr: Option<R>) -> Vec<u8>
where
    R: AsyncRead + Unpin,
{
    let Some(mut stderr) = stderr else {
        return Vec::new();
    };
    let mut kept = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match stderr.read(&mut buf).await {
            Ok(0) => break,
            Ok(read) => {
                let room = MAX_STDERR_BYTES.saturating_sub(kept.len());
                if room > 0 {
                    kept.extend_from_slice(&buf[..read.min(room)]);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to drain git pack-objects stderr");
                break;
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    /// The fill loop must not turn a short pipe read into a chunk boundary:
    /// framing that follows the scheduler is framing no test can pin down.
    #[tokio::test]
    async fn fill_completes_a_chunk_across_short_reads() {
        let (mut client, server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            for _ in 0..8 {
                client.write_all(&[7u8; 16]).await.unwrap();
                tokio::task::yield_now().await;
            }
        });

        let mut reader = BufReader::new(server);
        let mut buf = [0u8; 128];
        assert_eq!(fill(&mut reader, &mut buf).await.unwrap(), 128);
        assert!(buf.iter().all(|byte| *byte == 7));
    }

    /// End of stream is the only thing that ends a fill short.
    #[tokio::test]
    async fn fill_stops_short_only_at_end_of_stream() {
        let (mut client, server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            client.write_all(b"tail").await.unwrap();
        });

        let mut reader = BufReader::new(server);
        let mut buf = [0u8; 128];
        assert_eq!(fill(&mut reader, &mut buf).await.unwrap(), 4);
        assert_eq!(fill(&mut reader, &mut buf).await.unwrap(), 0);
    }

    /// The stderr drain keeps consuming past its cap — that is the half that
    /// stops a chatty git from deadlocking on a full pipe — while what it
    /// retains stays bounded.
    #[tokio::test]
    async fn stderr_drain_is_bounded_but_keeps_reading() {
        let (mut client, server) = tokio::io::duplex(4096);
        let noisy = MAX_STDERR_BYTES * 3;
        tokio::spawn(async move {
            client.write_all(&vec![b'e'; noisy]).await.unwrap();
        });

        let kept = drain_bounded(Some(server)).await;
        assert_eq!(
            kept.len(),
            MAX_STDERR_BYTES,
            "retained bytes must be capped"
        );
    }

    #[tokio::test]
    async fn stderr_drain_of_a_missing_pipe_is_empty() {
        let kept = drain_bounded(Option::<tokio::io::DuplexStream>::None).await;
        assert!(kept.is_empty());
    }
}
