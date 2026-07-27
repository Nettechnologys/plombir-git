//! Shared HTTP response-body helpers.
//!
//! Home of [`buffered_body_with_idle`] — the download-side slow-drip defense.
//! A handler that has *already* buffered its full payload into a `Vec` (because
//! it needs the whole thing before responding — e.g. to run an integrity hash,
//! or because the git subprocess must fully finish before the status is known)
//! can hand that `Vec` here instead of to `Body::from`, and get a
//! backpressure-sensitive, idle-guarded stream rather than a single in-memory
//! frame that a slow client can pin indefinitely.

use axum::body::Body;

/// Slice size for streaming a buffered response body to the client.
pub(crate) const RESPONSE_CHUNK_BYTES: usize = 64 * 1024;

/// Bounded channel depth for the response streamer. Small on purpose: this is
/// the coupling point where a stalled reader's socket backpressure propagates
/// back to the producer as a blocked `send`, so a shallow queue makes the idle
/// timeout bite promptly instead of after several megabytes have been enqueued.
const RESPONSE_CHANNEL_DEPTH: usize = 4;

/// Deliver an already-buffered response as a **backpressure-sensitive,
/// idle-guarded** stream instead of a single in-memory frame.
///
/// Some handlers legitimately buffer their whole payload before responding: the
/// git upload-pack / v2 handlers need the pack fully produced before they know
/// the `200 / 500 / 504` outcome, and the artifact / cache download handlers
/// verify a sha256 over the complete buffer before serving a byte. Buffering is
/// justified there — but handing the finished `Vec` to `Body::from` turns it
/// into a single frame hyper owns and holds until the client has drained every
/// byte. A client that reads one byte at a time — or stops reading entirely —
/// then pins the whole payload-sized buffer in server memory for as long as it
/// likes. The subprocess is already dead and the check already passed, yet the
/// memory + connection are held unbounded — the download-side twin of the
/// slow-drip *upload* that `buffer_git_body` defends.
///
/// The fix pumps `output` through a bounded channel in [`RESPONSE_CHUNK_BYTES`]
/// slices. Hyper only pulls the next chunk after flushing the previous one to
/// the socket, so a stalled reader stops draining → the channel fills → the
/// producer's `send().await` blocks. We bound that `send` with `idle_secs`; on a
/// stall the producer drops both the unsent remainder and the channel, releasing
/// the payload-sized buffer immediately instead of holding it until the kernel
/// eventually resets the dead TCP connection. A legit slow-but-progressing
/// client drains at least one chunk per idle window and never trips — the same
/// semantics the upload side already has.
///
/// This does not restructure how the payload is produced (the bytes are already
/// in hand), so there is no correctness/quality regression: it only changes how
/// the finished bytes are handed to the socket. Callers that know the length
/// (they always do — the buffer is complete) should still set a `Content-Length`
/// header so clients can detect a truncated download; an idle abort then ends
/// the stream short of that length, which the client sees as a broken transfer.
///
/// `idle_secs == 0` disables the bound (plain, unbounded streaming).
pub(crate) fn buffered_body_with_idle(output: Vec<u8>, idle_secs: u64) -> Body {
    let (tx, rx) =
        tokio::sync::mpsc::channel::<std::io::Result<axum::body::Bytes>>(RESPONSE_CHANNEL_DEPTH);
    let idle = (idle_secs > 0).then(|| std::time::Duration::from_secs(idle_secs));

    tokio::spawn(async move {
        let mut buf = axum::body::Bytes::from(output);
        while !buf.is_empty() {
            let take = buf.len().min(RESPONSE_CHUNK_BYTES);
            // `split_to` moves the head out and shrinks `buf`, so the unsent
            // remainder is all that is retained between iterations.
            let chunk = buf.split_to(take);
            let send = tx.send(Ok(chunk));
            let sent = match idle {
                Some(dur) => match tokio::time::timeout(dur, send).await {
                    Ok(res) => res,
                    // Idle stall: the client stopped draining. Returning drops
                    // `buf` (the unsent tail) and `tx`, which tears down the
                    // response stream and frees the buffered memory.
                    Err(_elapsed) => {
                        tracing::warn!(
                            idle_secs,
                            "response idle timeout — slow client stopped reading, dropped buffered payload"
                        );
                        return;
                    }
                },
                None => send.await,
            };
            // Receiver gone (client disconnected / response dropped): nothing
            // left to feed, so stop and release the remainder.
            if sent.is_err() {
                return;
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let frame_stream = futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
    Body::new(http_body_util::StreamBody::new(frame_stream))
}

#[cfg(test)]
mod tests {
    use super::{buffered_body_with_idle, RESPONSE_CHUNK_BYTES};
    use axum::body::Body;
    use std::time::Duration;

    /// Helper: drain a response `Body` to completion, returning the bytes seen.
    async fn drain_body(mut body: Body) -> Vec<u8> {
        use http_body_util::BodyExt;
        let mut out = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.expect("frame error").into_data() {
                out.extend_from_slice(&data);
            }
        }
        out
    }

    /// A client that drains promptly receives the whole buffered payload, byte
    /// for byte — the idle guard must never corrupt or truncate a healthy
    /// download.
    #[tokio::test]
    async fn buffered_body_delivers_full_body_to_prompt_reader() {
        let output: Vec<u8> = (0..RESPONSE_CHUNK_BYTES * 3 + 123)
            .map(|i| (i % 251) as u8)
            .collect();
        let body = buffered_body_with_idle(output.clone(), 30);
        let got = drain_body(body).await;
        assert_eq!(
            got, output,
            "prompt reader must get the exact buffered bytes"
        );
    }

    /// `idle_secs == 0` disables the bound; delivery still completes intact.
    #[tokio::test]
    async fn buffered_body_disabled_delivers_full_body() {
        let output: Vec<u8> = (0..RESPONSE_CHUNK_BYTES + 7).map(|i| i as u8).collect();
        let got = drain_body(buffered_body_with_idle(output.clone(), 0)).await;
        assert_eq!(got, output);
    }

    /// A slow-drip downloader that stops reading trips the idle window: the
    /// producer drops the unsent tail, so the stalled client receives only what
    /// was already in flight — strictly less than the whole buffer — rather than
    /// pinning the payload-sized `Vec` until the kernel resets the socket.
    ///
    /// The stall must be observed *while no one is reading*: any read frees a
    /// channel slot, which unblocks the producer's `send` and makes the idle
    /// `timeout` see a ready inner future instead of firing. So the test parks
    /// the producer on a full channel, lets the idle window elapse without
    /// reading, and only then drains what little was buffered.
    #[tokio::test(start_paused = true)]
    async fn buffered_body_trips_on_stalled_reader() {
        use http_body_util::BodyExt;

        // Far more chunks than the channel can buffer, so the producer blocks on
        // `send` once the shallow queue fills.
        let total = RESPONSE_CHUNK_BYTES * 50;
        let output = vec![0xABu8; total];
        let mut body = buffered_body_with_idle(output, 2); // 2s idle window

        // Never read: let the producer fill the channel and park on a blocked
        // `send`, then let the idle window elapse so that `send` times out and
        // the producer drops the unsent remainder (closing the stream).
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(3)).await;
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }

        // Now drain only the few chunks that were buffered before the trip; the
        // stream must end (None) well before the full buffer is delivered.
        let mut received = 0usize;
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.expect("frame error").into_data() {
                received += data.len();
            }
        }
        assert!(
            received < total,
            "stalled reader must not receive the whole buffer: got {received} of {total}"
        );
    }
}
