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

/// Stream `inner` through, hashing every byte, and **fail the transfer** if the
/// bytes do not hash to `expected`.
///
/// The buffered downloads (release assets, CI artifacts, cache entries) verify
/// their digest over the complete `Vec` *before* the first byte leaves the
/// handler, so a corrupted blob is a clean `500` there. A payload served
/// straight off the disk cannot do that without buffering it — which is exactly
/// what the streaming path exists to avoid — so the check moves to where the
/// bytes are: the digest is computed as they pass, and the verdict lands at
/// end-of-stream.
///
/// Landing it there is not enough on its own. The handler declares a
/// `Content-Length`, and hyper considers the response finished the moment that
/// many bytes have been written — an error yielded *after* the last chunk is
/// never looked at, and the client gets a complete, well-formed, corrupt file.
/// So the last chunk is **withheld**: each chunk is handed on only once its
/// successor has been read, leaving one chunk in hand when the digest verdict
/// arrives. A match releases it and the transfer ends normally; a mismatch drops
/// it and yields an error instead, so the body stops short of the declared
/// length and the client sees a broken transfer rather than a valid download.
/// One chunk (a few KiB) is the entire memory cost — the streaming path stays a
/// streaming path.
///
/// Detection is still after the fact for the bytes already on the wire, which is
/// unavoidable without buffering the whole payload; the mismatch is therefore
/// also logged with `label`, so an operator learns the blob store is rotting
/// without waiting for a user to report a bad download.
pub(crate) fn sha256_verified_stream<S>(
    inner: S,
    expected: String,
    label: String,
) -> impl futures::Stream<Item = std::io::Result<axum::body::Bytes>>
where
    S: futures::Stream<Item = std::io::Result<axum::body::Bytes>> + Unpin,
{
    use sha2::{Digest, Sha256};

    // `None` marks the stream as finished (either drained or already failed), so
    // `unfold` stops instead of polling a spent inner stream. The third element
    // is the withheld chunk — the one already hashed but not yet handed on.
    let start = Some((inner, Sha256::new(), None::<axum::body::Bytes>));
    futures::stream::unfold(start, move |state| {
        let expected = expected.clone();
        let label = label.clone();
        async move {
            let (mut inner, mut hasher, mut withheld) = state?;
            loop {
                match futures::StreamExt::next(&mut inner).await {
                    Some(Ok(chunk)) => {
                        hasher.update(&chunk);
                        // Release the previous chunk now that a successor exists
                        // — proof this one is not the last. On the very first
                        // chunk there is nothing to release yet, so read on.
                        if let Some(ready) = withheld.replace(chunk) {
                            return Some((Ok(ready), Some((inner, hasher, withheld))));
                        }
                    }
                    Some(Err(error)) => return Some((Err(error), None)),
                    None => {
                        let actual = hex::encode(hasher.finalize());
                        if actual == expected {
                            // Verified: the withheld tail is safe to hand over.
                            return withheld.map(|last| (Ok(last), None));
                        }
                        tracing::error!(
                            %label,
                            %expected,
                            %actual,
                            "integrity check failed — stored bytes do not match the digest recorded at upload; aborting the download"
                        );
                        return Some((
                            Err(std::io::Error::other(format!(
                                "{label} integrity check failed: expected sha256 {expected}, got {actual}"
                            ))),
                            None,
                        ));
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{buffered_body_with_idle, sha256_verified_stream, RESPONSE_CHUNK_BYTES};
    use axum::body::Body;
    use std::time::Duration;

    /// Helper: run `chunks` through the verifying stream against `expected`,
    /// returning the bytes the client would have received and whether the
    /// transfer was broken by an error.
    async fn verify_chunks(chunks: &[&[u8]], expected: &str) -> (Vec<u8>, bool) {
        let inner = futures::stream::iter(
            chunks
                .iter()
                .map(|chunk| Ok(axum::body::Bytes::copy_from_slice(chunk)))
                .collect::<Vec<_>>(),
        );
        let stream =
            sha256_verified_stream(inner, expected.to_string(), "test payload".to_string());
        let mut delivered = Vec::new();
        let mut failed = false;
        let mut stream = std::pin::pin!(stream);
        while let Some(item) = futures::StreamExt::next(&mut stream).await {
            match item {
                Ok(chunk) => delivered.extend_from_slice(&chunk),
                Err(_) => {
                    failed = true;
                    break;
                }
            }
        }
        (delivered, failed)
    }

    fn sha256_hex(data: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(data))
    }

    /// The healthy path must be byte-transparent: withholding the tail chunk is
    /// an ordering trick, not a filter.
    #[tokio::test]
    async fn verified_stream_passes_matching_bytes_through_untouched() {
        let chunks: [&[u8]; 3] = [b"first ", b"second ", b"third"];
        let expected = sha256_hex(b"first second third");
        let (delivered, failed) = verify_chunks(&chunks, &expected).await;
        assert_eq!(delivered, b"first second third");
        assert!(!failed, "a matching digest must not break the transfer");
    }

    /// A mismatch must both fail the stream AND hold back the final chunk — a
    /// complete body plus a trailing error is exactly what hyper discards once
    /// `Content-Length` bytes are on the wire.
    #[tokio::test]
    async fn verified_stream_withholds_the_tail_and_fails_on_mismatch() {
        let chunks: [&[u8]; 3] = [b"first ", b"second ", b"third"];
        let expected = sha256_hex(b"what the row claims");
        let (delivered, failed) = verify_chunks(&chunks, &expected).await;
        assert!(failed, "a digest mismatch must break the transfer");
        assert_eq!(
            delivered, b"first second ",
            "the last chunk must never reach the client"
        );
    }

    /// A payload small enough to arrive in one chunk is the case where the
    /// withholding pays off most: nothing at all reaches the client.
    #[tokio::test]
    async fn verified_stream_delivers_nothing_when_a_single_chunk_mismatches() {
        let chunks: [&[u8]; 1] = [b"the whole file"];
        let expected = sha256_hex(b"a different file");
        let (delivered, failed) = verify_chunks(&chunks, &expected).await;
        assert!(failed);
        assert!(
            delivered.is_empty(),
            "a one-chunk payload must be withheld entirely: {delivered:?}"
        );
    }

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
