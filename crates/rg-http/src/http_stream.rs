//! Shared HTTP response-body helpers — the download-side slow-drip defense, in
//! two shapes for the two ways a payload comes into being.
//!
//! [`buffered_body_with_idle`] is for a handler that has *already* buffered its
//! whole payload into a `Vec` because it genuinely needed it whole — to run an
//! integrity hash over it, say. Handing that `Vec` here instead of to
//! `Body::from` turns a single in-memory frame a slow client can pin
//! indefinitely into a backpressure-sensitive, idle-guarded stream.
//!
//! [`reader_body_with_idle`] is for a payload that is still being produced, and
//! must not be collected at all: a clone is sized by the repository, so the
//! upload-pack transport streams `git pack-objects` straight through to the
//! socket. It adds the piece a copy loop lacks — the producer's verdict,
//! delivered after the last byte, so a late failure breaks the body instead of
//! ending it as if the answer were complete.

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
/// slow-drip *upload* that `stage_git_body` defends.
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

/// Stream a still-running producer's output as the response body, under the
/// same idle guard [`buffered_body_with_idle`] gives a finished one.
///
/// The sibling above exists for payloads that are *already* whole. Upload-pack
/// is the opposite case: `git pack-objects` streams the clone out as it builds
/// it, so collecting it first would make one clone cost a repository of server
/// memory — the whole point of not collecting it is that nothing between git
/// and the socket ever holds more than a chunk.
///
/// `head` is the part already read (the caller reads it to decide the status
/// code — see `git_http::stream_upload_pack_response`), `reader` is the rest,
/// and `completion` is the producer's verdict, awaited *after* the reader ends.
///
/// The verdict is the half a plain copy loop gets wrong. A response that has
/// begun cannot be un-sent, so a producer that fails late has no status code
/// left to fail with; ending the stream normally would hand the client a
/// well-formed, complete-looking, truncated answer. Instead the error is yielded
/// into the body, which hyper turns into a connection abort with no terminating
/// chunk — the client sees a broken transfer, which is the truth.
///
/// Only the *send* is idle-bounded, not the read: a stalled client is what this
/// defends against, whereas a slow producer is git legitimately thinking, and is
/// already bounded by the caller's wall-clock timeout. `idle_secs == 0` disables
/// the bound.
pub(crate) fn reader_body_with_idle<R, F>(
    head: axum::body::Bytes,
    reader: R,
    completion: F,
    idle_secs: u64,
) -> Body
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    F: std::future::Future<Output = std::io::Result<()>> + Send + 'static,
{
    use tokio::io::AsyncReadExt as _;

    let (tx, rx) =
        tokio::sync::mpsc::channel::<std::io::Result<axum::body::Bytes>>(RESPONSE_CHANNEL_DEPTH);
    let idle = (idle_secs > 0).then(|| std::time::Duration::from_secs(idle_secs));

    tokio::spawn(async move {
        let mut reader = reader;
        let mut chunk = head;

        loop {
            if !chunk.is_empty() && !send_chunk(&tx, chunk, idle, idle_secs).await {
                return;
            }

            let mut buf = vec![0u8; RESPONSE_CHUNK_BYTES];
            match reader.read(&mut buf).await {
                Ok(0) => break,
                Ok(read) => {
                    buf.truncate(read);
                    chunk = axum::body::Bytes::from(buf);
                }
                Err(error) => {
                    // Breaking the body is the only signal left; the receiver
                    // being gone already means the client stopped listening.
                    drop(tx.send(Err(error)).await);
                    return;
                }
            }
        }

        if let Err(error) = completion.await {
            drop(tx.send(Err(error)).await);
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let frame_stream = futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
    Body::new(http_body_util::StreamBody::new(frame_stream))
}

/// Hand one chunk to the response stream under the idle bound, reporting
/// whether the stream is still worth feeding.
///
/// `false` means stop: either the client stalled past `idle` (the coupling
/// point described on [`buffered_body_with_idle`] — hyper stops draining, the
/// channel fills, `send` blocks) or the receiver is gone entirely.
async fn send_chunk(
    tx: &tokio::sync::mpsc::Sender<std::io::Result<axum::body::Bytes>>,
    chunk: axum::body::Bytes,
    idle: Option<std::time::Duration>,
    idle_secs: u64,
) -> bool {
    let send = tx.send(Ok(chunk));
    let sent = match idle {
        Some(dur) => match tokio::time::timeout(dur, send).await {
            Ok(res) => res,
            Err(_elapsed) => {
                tracing::warn!(
                    idle_secs,
                    "response idle timeout — slow client stopped reading, dropped streamed payload"
                );
                return false;
            }
        },
        None => send.await,
    };
    sent.is_ok()
}

/// Where a streamed git subprocess came from, for the log line a failure
/// discovered mid-stream leaves behind.
///
/// By the time any of this is known the status code is spent, so the warn line
/// is the only report an operator gets: it has to name *which* repository — and,
/// for the CI workspace download, *which* job — was handed a truncated payload.
pub(crate) struct GitStreamSource {
    /// The CI job the stream belongs to, where there is one.
    pub job_id: Option<i64>,
    /// The repository git was run in.
    pub repo_path: std::path::PathBuf,
    /// What the payload is, in operator words: `workspace tar`, `repository
    /// archive`.
    pub what: &'static str,
}

/// A spawned git child with its pipes split out, ready to be streamed.
///
/// The child travels with the stream rather than being dropped by the handler:
/// `spawn_async` sets `kill_on_drop`, so dropping it early would kill git
/// mid-archive. Holding it here means the process lives exactly as long as the
/// body that reads it, and is reaped by [`git_child_body_with_idle`].
pub(crate) struct GitChildStream {
    /// The child itself, minus the pipes below.
    pub child: tokio::process::Child,
    /// git's stdout, taken out of the child so the caller may read a head chunk
    /// from it before committing to a status code.
    pub stdout: tokio::process::ChildStdout,
    /// The concurrent stderr drain. It must run from the moment git starts: the
    /// pipe is small, and an unread full one deadlocks git mid-write.
    pub stderr: tokio::task::JoinHandle<Vec<u8>>,
    /// Bytes already read from `stdout` by the caller, replayed as the first
    /// frame of the body. Empty when the caller read nothing.
    pub head: axum::body::Bytes,
}

/// Close stdin, start the stderr drain and take stdout off a freshly spawned
/// git child.
///
/// The three do not belong to the caller separately — forget the stdin close and
/// git waits for an EOF that never comes; forget the stderr drain and a chatty
/// git blocks writing into a full pipe with the archive half-written. Both are
/// mistakes with no symptom until a repository is big or broken enough, so the
/// prelude is one call that both download paths make.
pub(crate) fn split_git_child(mut child: tokio::process::Child) -> std::io::Result<GitChildStream> {
    use tokio::io::AsyncReadExt as _;

    // Neither download writes to git; close stdin so it never waits on EOF.
    drop(child.stdin.take());

    let pipe = child.stderr.take();
    let stderr = tokio::spawn(async move {
        let mut buf = Vec::new();
        if let Some(mut pipe) = pipe {
            if let Err(error) = pipe.read_to_end(&mut buf).await {
                tracing::warn!(%error, "failed to drain git stderr");
            }
        }
        buf
    });

    let stdout = child.stdout.take().ok_or_else(|| {
        std::io::Error::other("spawned git child has no stdout pipe to stream from")
    })?;

    Ok(GitChildStream {
        child,
        stdout,
        stderr,
        head: axum::body::Bytes::new(),
    })
}

/// Why the stdout pump stopped, which decides how git is reaped.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PumpEnd {
    /// git closed stdout: everything it meant to write has been forwarded, and
    /// its exit status is the verdict on whether that was the whole payload.
    Eof,
    /// The stream was torn down early — an idle trip, a read error, or a client
    /// that stopped listening. git is ours to kill, and its status afterwards
    /// says nothing.
    Aborted,
}

/// Stream a spawned git child's stdout as an idle-guarded response body.
///
/// Instead of collecting git's output into a `Vec` sized by the repository, the
/// producer pumps stdout through a bounded channel in [`RESPONSE_CHUNK_BYTES`]
/// slices. Two idle points are bounded by `idle_secs`:
/// - the **read** from git's stdout — a hung git trips it (an async-spawned git
///   has no `git_cmd_secs` wall-clock, unlike the synchronous gateway call);
/// - the **send** to the client — a stalled reader stops draining, so the
///   bounded channel fills and the blocked `send` trips.
///
/// On either trip (or the client disconnecting) the producer returns, dropping
/// the child → `kill_on_drop` reaps git and frees the pipe.
/// `idle_secs == 0` disables the bound.
///
/// **A late git failure breaks the body.** A response that has begun cannot be
/// un-sent, so a git that exits non-zero after the first byte has no status code
/// left to fail with — and ending the stream normally would hand the client a
/// well-formed, complete-looking, truncated archive. The error is yielded into
/// the body instead, which hyper turns into a connection abort with no
/// terminating chunk: a broken transfer, which is the truth. That verdict is
/// only read on a clean EOF; after an abort we killed git ourselves, so its
/// non-zero status is our own signal and says nothing about the payload.
pub(crate) fn git_child_body_with_idle(
    stream: GitChildStream,
    idle_secs: u64,
    source: GitStreamSource,
) -> Body {
    let (tx, rx) =
        tokio::sync::mpsc::channel::<std::io::Result<axum::body::Bytes>>(RESPONSE_CHANNEL_DEPTH);
    let idle = (idle_secs > 0).then(|| std::time::Duration::from_secs(idle_secs));

    tokio::spawn(async move {
        let GitChildStream {
            mut child,
            mut stdout,
            stderr,
            head,
        } = stream;

        let ended = pump_child_stdout(&mut stdout, &tx, head, idle, idle_secs, &source).await;

        // Only kill what we interrupted: on a clean EOF git has already written
        // everything and is exiting, and a `start_kill` racing that exit turns a
        // complete archive into a signalled — that is, failed — status.
        if ended == PumpEnd::Aborted {
            if let Err(error) = child.start_kill() {
                tracing::debug!(%error, "git process already exited before kill");
            }
        }

        match child.wait().await {
            Ok(status) if status.success() => {}
            Ok(status) => {
                let drained = stderr.await.unwrap_or_default();
                tracing::warn!(
                    job_id = source.job_id,
                    repo = %source.repo_path.display(),
                    what = source.what,
                    code = ?status.code(),
                    stderr = %String::from_utf8_lossy(&drained).trim(),
                    "git exited non-zero after the response had begun — breaking the body so the \
                     client cannot read a truncated payload as a complete one"
                );
                if ended == PumpEnd::Eof {
                    drop(
                        tx.send(Err(std::io::Error::other(
                            "git exited non-zero mid-response",
                        )))
                        .await,
                    );
                }
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    job_id = source.job_id,
                    repo = %source.repo_path.display(),
                    what = source.what,
                    "could not reap the git process that produced this response"
                );
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let frame_stream = futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
    Body::new(http_body_util::StreamBody::new(frame_stream))
}

/// Forward `head`, then everything left on `stdout`, chunk by chunk under the
/// idle bound — reporting whether git closed the stream or we tore it down.
async fn pump_child_stdout<R>(
    stdout: &mut R,
    tx: &tokio::sync::mpsc::Sender<std::io::Result<axum::body::Bytes>>,
    head: axum::body::Bytes,
    idle: Option<std::time::Duration>,
    idle_secs: u64,
    source: &GitStreamSource,
) -> PumpEnd
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt as _;

    if !head.is_empty() && !send_chunk(tx, head, idle, idle_secs).await {
        return PumpEnd::Aborted;
    }

    let mut buf = vec![0u8; RESPONSE_CHUNK_BYTES];
    loop {
        // Read a chunk, bounded by the idle window (catches a hung git).
        let read = match idle {
            Some(dur) => match tokio::time::timeout(dur, stdout.read(&mut buf)).await {
                Ok(result) => result,
                Err(_elapsed) => {
                    tracing::warn!(
                        job_id = source.job_id,
                        repo = %source.repo_path.display(),
                        what = source.what,
                        idle_secs,
                        "git read idle timeout — killing git"
                    );
                    return PumpEnd::Aborted;
                }
            },
            None => stdout.read(&mut buf).await,
        };
        let n = match read {
            Ok(0) => return PumpEnd::Eof,
            Ok(n) => n,
            Err(error) => {
                tracing::warn!(
                    %error,
                    job_id = source.job_id,
                    repo = %source.repo_path.display(),
                    what = source.what,
                    "git stdout read failed — the payload is truncated"
                );
                return PumpEnd::Aborted;
            }
        };
        // Send, bounded by the idle window (catches a stalled client: hyper
        // stops draining the stream → the channel fills → `send` blocks).
        if !send_chunk(
            tx,
            axum::body::Bytes::copy_from_slice(&buf[..n]),
            idle,
            idle_secs,
        )
        .await
        {
            return PumpEnd::Aborted;
        }
    }
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
    use super::{
        buffered_body_with_idle, reader_body_with_idle, sha256_verified_stream,
        RESPONSE_CHUNK_BYTES,
    };
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

    /// The streaming twin delivers head + reader byte for byte, across far more
    /// data than either the channel or one chunk can hold — the case the git
    /// transport is built on, where the producer is still running.
    #[tokio::test]
    async fn reader_body_delivers_head_then_the_rest_of_the_stream() {
        let head: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        let tail: Vec<u8> = (0..RESPONSE_CHUNK_BYTES * 5 + 77)
            .map(|i| ((i + 7) % 241) as u8)
            .collect();

        let (mut producer, reader) = tokio::io::duplex(4096);
        let written = tail.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            producer.write_all(&written).await.unwrap();
        });

        let body = reader_body_with_idle(
            axum::body::Bytes::from(head.clone()),
            reader,
            async { Ok(()) },
            30,
        );

        let mut expected = head;
        expected.extend_from_slice(&tail);
        assert_eq!(drain_body(body).await, expected);
    }

    /// A producer that fails *after* the response began must break the body:
    /// the bytes already sent stay sent, and the stream ends with an error
    /// rather than a terminating chunk, so the client cannot read a partial
    /// answer as a complete one.
    #[tokio::test]
    async fn reader_body_breaks_the_stream_on_a_failed_completion() {
        use http_body_util::BodyExt;

        let (producer, reader) = tokio::io::duplex(64);
        // Closing the write half immediately ends the reader, so the verdict is
        // reached with the head already delivered.
        drop(producer);

        let mut body = reader_body_with_idle(
            axum::body::Bytes::from_static(b"partial git protocol response"),
            reader,
            async { Err(std::io::Error::other("producer failed late")) },
            30,
        );

        let first = body
            .frame()
            .await
            .expect("head frame")
            .expect("head is delivered")
            .into_data()
            .expect("data frame");
        assert_eq!(first.as_ref(), b"partial git protocol response");

        let verdict = body.frame().await.expect("verdict frame");
        assert!(
            verdict.is_err(),
            "a late producer failure must end the body with an error"
        );
    }

    /// The idle guard covers the streaming twin too: a client that stops
    /// reading parks the producer on a full channel, and the trip drops the
    /// stream instead of holding the connection until the kernel gives up.
    /// Same observation rule as the buffered test above — the stall is only
    /// visible while nobody reads.
    #[tokio::test(start_paused = true)]
    async fn reader_body_trips_on_stalled_reader() {
        use http_body_util::BodyExt;

        let total = RESPONSE_CHUNK_BYTES * 50;
        let (mut producer, reader) = tokio::io::duplex(RESPONSE_CHUNK_BYTES);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt as _;
            drop(producer.write_all(&vec![0xCDu8; total]).await);
        });

        let mut body = reader_body_with_idle(
            axum::body::Bytes::new(),
            reader,
            async { Ok(()) },
            2, // 2s idle window
        );

        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(Duration::from_secs(3)).await;
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }

        let mut received = 0usize;
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame.expect("frame error").into_data() {
                received += data.len();
            }
        }
        assert!(
            received < total,
            "stalled reader must not receive the whole stream: got {received} of {total}"
        );
    }
}

#[cfg(test)]
mod git_child_stream_tests {
    use super::*;
    use axum::body::Bytes;
    use http_body_util::BodyExt;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, ReadBuf};

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    struct FailsAfterPartialChunk {
        yielded: bool,
    }

    impl AsyncRead for FailsAfterPartialChunk {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<std::io::Result<()>> {
            if self.yielded {
                return Poll::Ready(Err(std::io::Error::other(
                    "injected archive stdout failure",
                )));
            }
            self.yielded = true;
            buf.put_slice(b"partial tar");
            Poll::Ready(Ok(()))
        }
    }

    fn source(repo: &std::path::Path) -> GitStreamSource {
        GitStreamSource {
            job_id: Some(1),
            repo_path: repo.to_path_buf(),
            what: "workspace tar",
        }
    }

    /// Collect a whole streamed body, the way a client that keeps reading does.
    async fn collect(body: Body) -> Bytes {
        body.collect().await.unwrap().to_bytes()
    }

    fn gw() -> &'static rg_git::cli_gateway::GitCommandGateway {
        rg_git::cli_gateway::global_gateway().as_ref().unwrap()
    }

    /// Run git via the sanctioned gateway (keeps the raw-git-invocation guard green).
    fn git(args: &[&str], cwd: Option<&std::path::Path>) {
        let out = gw().run(args, cwd).unwrap();
        assert!(out.success(), "git {args:?}: {}", out.stderr_str().trim());
    }

    fn seed_repo(repo: &std::path::Path, big: bool) -> Vec<u8> {
        git(&["init", "--initial-branch=main"], Some(repo));
        git(&["config", "user.name", "Archive Test"], Some(repo));
        git(&["config", "user.email", "archive@example.com"], Some(repo));
        let blob = if big {
            // Poorly-compressible content so the tar spans several 64 KiB reads,
            // exercising the multi-chunk streaming loop.
            let mut blob = Vec::with_capacity(300 * 1024);
            let mut x: u32 = 0x1234_5678;
            for _ in 0..(300 * 1024) {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                blob.push((x & 0xff) as u8);
            }
            blob
        } else {
            b"hello\n".to_vec()
        };
        std::fs::write(repo.join("big.bin"), &blob).unwrap();
        std::fs::write(repo.join("README.md"), "archive parity\n").unwrap();
        git(&["add", "."], Some(repo));
        git(&["commit", "-m", "content"], Some(repo));
        blob
    }

    /// The streamed tar must be byte-identical to a buffered `git archive` — a
    /// truncated or reordered stream would corrupt the runner's workspace.
    #[tokio::test]
    async fn streamed_archive_matches_buffered_git_archive_byte_for_byte() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        seed_repo(repo, true);

        let buffered = gw()
            .run(&["archive", "--format=tar", "HEAD"], Some(repo))
            .unwrap();
        assert!(buffered.success());
        let buffered = buffered.stdout;

        let child = gw()
            .spawn_async(&["archive", "--format=tar", "HEAD"], Some(repo))
            .await
            .unwrap();
        let stream = split_git_child(child).unwrap();
        let streamed = collect(git_child_body_with_idle(stream, 30, source(repo))).await;

        assert_eq!(
            streamed.len(),
            buffered.len(),
            "streamed tar length must match buffered"
        );
        assert_eq!(
            &streamed[..],
            &buffered[..],
            "streamed tar must be byte-identical to buffered git archive"
        );
    }

    /// `idle_secs == 0` disables the idle bound; the full tar still streams.
    #[tokio::test]
    async fn streamed_archive_idle_disabled_delivers_full_tar() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        seed_repo(repo, false);

        let buffered = gw()
            .run(&["archive", "--format=tar", "HEAD"], Some(repo))
            .unwrap()
            .stdout;
        let child = gw()
            .spawn_async(&["archive", "--format=tar", "HEAD"], Some(repo))
            .await
            .unwrap();
        let stream = split_git_child(child).unwrap();
        let streamed = collect(git_child_body_with_idle(stream, 0, source(repo))).await;
        assert_eq!(&streamed[..], &buffered[..]);
    }

    /// Once response headers are on the wire, a read failure cannot become a
    /// different status. The server log is therefore the operator's only copy
    /// of the underlying errno and the job/repository it corrupted.
    #[tokio::test]
    async fn stdout_read_failure_logs_cause_and_context_with_or_without_idle_guard() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let repo_path = std::path::Path::new("/repos/acme/widgets.git");

        for idle in [None, Some(std::time::Duration::from_secs(30))] {
            let mut stdout = FailsAfterPartialChunk { yielded: false };
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);

            let source = GitStreamSource {
                job_id: Some(42),
                repo_path: repo_path.to_path_buf(),
                what: "workspace tar",
            };
            let ended = pump_child_stdout(&mut stdout, &tx, Bytes::new(), idle, 30, &source).await;
            assert!(
                ended == PumpEnd::Aborted,
                "a failed read must not read as a clean end of stream"
            );
            drop(tx);

            assert_eq!(
                rx.recv().await.unwrap().unwrap(),
                Bytes::from_static(b"partial tar")
            );
            assert!(rx.recv().await.is_none(), "the failed reader must stop");
        }

        let rendered = logs.text();
        assert_eq!(
            rendered
                .matches("git stdout read failed — the payload is truncated")
                .count(),
            2,
            "{rendered}"
        );
        assert!(
            rendered.contains("injected archive stdout failure"),
            "{rendered}"
        );
        assert!(rendered.contains("job_id=42"), "{rendered}");
        assert!(rendered.contains("/repos/acme/widgets.git"), "{rendered}");
    }

    /// The archive endpoint reads the first chunk itself to decide the status
    /// code, then hands it back as `head`. Replaying it must reproduce git's
    /// output exactly — a head dropped, duplicated or appended at the end is a
    /// corrupt archive that still looks like a complete download.
    #[tokio::test]
    async fn a_head_read_by_the_caller_is_replayed_in_front_of_the_rest() {
        use tokio::io::AsyncReadExt as _;

        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        seed_repo(repo, true);

        let buffered = gw()
            .run(&["archive", "--format=tar", "HEAD"], Some(repo))
            .unwrap()
            .stdout;

        let child = gw()
            .spawn_async(&["archive", "--format=tar", "HEAD"], Some(repo))
            .await
            .unwrap();
        let mut stream = split_git_child(child).unwrap();

        // Deliberately not a chunk boundary: the split between head and tail
        // must not be observable in the result.
        let mut head = vec![0u8; 1000];
        let read = stream.stdout.read(&mut head).await.unwrap();
        assert!(read > 0, "git archive wrote nothing to stdout");
        head.truncate(read);
        stream.head = Bytes::from(head);

        let streamed = collect(git_child_body_with_idle(stream, 30, source(repo))).await;
        assert_eq!(
            &streamed[..],
            &buffered[..],
            "head + tail must equal the whole archive"
        );
    }

    /// A git that fails after the first byte has no status code left to fail
    /// with, so the body must break rather than end cleanly — otherwise the
    /// client reads a truncated archive as a complete one. `--format=tar` on a
    /// blob resolves far enough to start writing and then dies.
    #[tokio::test]
    async fn a_late_git_failure_breaks_the_body_instead_of_ending_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        seed_repo(repo, false);

        // Stand in for the mid-stream failure: a head the caller already sent
        // plus a child that exits non-zero without writing anything itself.
        let child = gw()
            .spawn_async(
                &["archive", "--format=tar", "refs/heads/nosuchref"],
                Some(repo),
            )
            .await
            .unwrap();
        let mut stream = split_git_child(child).unwrap();
        stream.head = Bytes::from_static(b"partial tar");

        let body = git_child_body_with_idle(stream, 30, source(repo));
        let error = body
            .collect()
            .await
            .expect_err("a non-zero git exit after the head must break the body");
        assert!(
            format!("{error}").contains("mid-response"),
            "the broken body must carry the reason: {error}"
        );
    }
}
