//! Idle-timeout wrapper for a streaming git transport.
//!
//! [`IdleTimeout`] wraps an [`AsyncRead`] + [`AsyncWrite`] and trips when a
//! single read or write makes **no progress** for longer than an idle window.
//! It is the second watchdog alongside the wall-clock bound (`with_git_timeout`
//! in `rg-http` / `rg-ssh`): the wall-clock caps *total* time, while the idle
//! timeout catches a **slow-drip** peer that dribbles a byte every few seconds —
//! progressing just often enough to stay under the wall-clock budget while
//! pinning a `git` subprocess + connection for the whole window.
//!
//! ## Where this bites (and where it doesn't)
//!
//! This is a wrapper over the raw transport stream. It provides real protection
//! only where that stream is the **network** itself — i.e. the SSH transport
//! (`ChannelStream`), where a slow-drip `git push` upload stalls a wrapped
//! `read` and a slow-drip `git fetch` download stalls a wrapped `write`. When
//! the tripped read/write returns `ErrorKind::TimedOut`, the protocol handler
//! errors out and drops its `git` child → `kill_on_drop` reaps the subprocess,
//! exactly like the wall-clock path.
//!
//! The HTTP transport buffers the whole request/response body in memory (axum
//! `Bytes`), so the git handlers there never face the network directly — their
//! idle defense lives one layer up, at the request-body buffering step. See
//! `rg-http`'s `git_http` for that.
//!
//! ## Semantics
//!
//! The idle timer is armed only while the inner I/O is `Pending`, and is reset
//! on every `Ready` poll (data moved, or a clean EOF). Total transfer time may
//! far exceed the idle window as long as no single gap between progress does —
//! mirroring `tower_http`'s `TimeoutBody`. Read and write directions each own
//! an independent timer, so a quiet read direction can't trip a busy write one.
//! An idle window of `None` (constructed from `secs == 0`) is a transparent
//! pass-through — the wrapper adds no timer and never trips.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::time::Sleep;

/// See the [module docs](self). `T` must be `Unpin` (every transport we wrap —
/// `ChannelStream`, tokio duplex halves, `BufReader` over them — already is),
/// which lets the wrapper stay `Unpin` and project without `unsafe`.
pub struct IdleTimeout<T> {
    inner: T,
    /// `None` = disabled (transparent pass-through).
    idle: Option<Duration>,
    /// Armed lazily on the first `Pending` read; reset to `None` on progress.
    read_sleep: Option<Pin<Box<Sleep>>>,
    /// Independent write-direction timer (shared by write + flush).
    write_sleep: Option<Pin<Box<Sleep>>>,
}

impl<T> IdleTimeout<T> {
    /// Wrap `inner` with an explicit idle window. `None` disables the timer.
    pub fn new(inner: T, idle: Option<Duration>) -> Self {
        Self {
            inner,
            idle,
            read_sleep: None,
            write_sleep: None,
        }
    }

    /// Wrap `inner` with an idle window given in whole seconds. `secs == 0`
    /// disables the timer (transparent pass-through), matching the `0 = off`
    /// convention of the wall-clock `with_git_timeout` helpers.
    pub fn from_secs(inner: T, secs: u64) -> Self {
        Self::new(inner, (secs > 0).then(|| Duration::from_secs(secs)))
    }

    /// Borrow the wrapped stream (e.g. to send an SSH exit-status before the
    /// wrapper is dropped).
    pub fn get_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Unwrap, returning the inner stream.
    pub fn into_inner(self) -> T {
        self.inner
    }
}

/// Poll `slot`'s timer against `idle`, arming it if needed. Returns `true` when
/// the idle window has elapsed (caller should surface `TimedOut`).
fn idle_elapsed(
    slot: &mut Option<Pin<Box<Sleep>>>,
    idle: Option<Duration>,
    cx: &mut Context<'_>,
) -> bool {
    let Some(idle) = idle else {
        return false; // disabled → never trips
    };
    let sleep = slot.get_or_insert_with(|| Box::pin(tokio::time::sleep(idle)));
    sleep.as_mut().poll(cx).is_ready()
}

fn timed_out(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        format!("git stream idle timeout: no {what} progress within the idle window"),
    )
}

impl<T: AsyncRead + Unpin> AsyncRead for IdleTimeout<T> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_read(cx, buf) {
            // Progress (bytes read, or a clean EOF) → disarm the idle timer.
            Poll::Ready(res) => {
                this.read_sleep = None;
                Poll::Ready(res)
            }
            Poll::Pending => {
                if idle_elapsed(&mut this.read_sleep, this.idle, cx) {
                    this.read_sleep = None;
                    return Poll::Ready(Err(timed_out("read")));
                }
                Poll::Pending
            }
        }
    }
}

impl<T: AsyncWrite + Unpin> AsyncWrite for IdleTimeout<T> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_write(cx, buf) {
            Poll::Ready(res) => {
                this.write_sleep = None;
                Poll::Ready(res)
            }
            Poll::Pending => {
                if idle_elapsed(&mut this.write_sleep, this.idle, cx) {
                    this.write_sleep = None;
                    return Poll::Ready(Err(timed_out("write")));
                }
                Poll::Pending
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_flush(cx) {
            Poll::Ready(res) => {
                this.write_sleep = None;
                Poll::Ready(res)
            }
            Poll::Pending => {
                if idle_elapsed(&mut this.write_sleep, this.idle, cx) {
                    this.write_sleep = None;
                    return Poll::Ready(Err(timed_out("flush")));
                }
                Poll::Pending
            }
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Teardown: forward unguarded. Shutdown is best-effort channel close;
        // bounding it isn't the slow-drip concern and a stuck shutdown is
        // already covered by the caller's wall-clock future.
        let this = self.get_mut();
        Pin::new(&mut this.inner).poll_shutdown(cx)
    }
}

/// Detect whether an error chain was produced by an idle-timeout trip, so the
/// transport layer can log/return it distinctly from an ordinary I/O failure.
/// Walks the `anyhow` source chain looking for an `io::Error` with
/// [`io::ErrorKind::TimedOut`].
pub fn is_idle_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<io::Error>()
            .is_some_and(|e| e.kind() == io::ErrorKind::TimedOut)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A slow-drip read trips the idle timer close to the window, not the far
    /// larger wall-clock budget: the writer sends one byte, then goes quiet for
    /// much longer than the idle window, so the *second* read has no progress
    /// and must fail with `TimedOut`.
    #[tokio::test(start_paused = true)]
    async fn slow_drip_read_trips_on_idle() {
        let (client, mut server) = tokio::io::duplex(64);
        // Server dribbles one byte, then stalls for 10s (>> 100ms idle).
        tokio::spawn(async move {
            server.write_all(&[0x42]).await.unwrap();
            tokio::time::sleep(Duration::from_secs(10)).await;
            let _ = server.write_all(&[0x43]).await;
        });

        let mut reader = IdleTimeout::from_secs(client, /* 1s window (paused clock) */ 1);
        let mut byte = [0u8; 1];
        // First byte arrives.
        reader.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte[0], 0x42);

        // Second read stalls past the idle window → TimedOut, well before the
        // writer's 10s quiet period would end.
        let err = reader.read_exact(&mut byte).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "slow-drip must trip idle");
    }

    /// Continuous-but-slow traffic (bytes arriving faster than the idle window)
    /// must NOT trip: a legit large clone/push over a slow-but-live link.
    #[tokio::test(start_paused = true)]
    async fn continuous_progress_does_not_trip() {
        let (client, mut server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            for i in 0..20u8 {
                // 300ms gap between bytes, under the 1s idle window every time.
                tokio::time::sleep(Duration::from_millis(300)).await;
                if server.write_all(&[i]).await.is_err() {
                    return;
                }
            }
        });

        let mut reader = IdleTimeout::from_secs(client, 1);
        let mut buf = [0u8; 20];
        // Reads every byte without a single gap exceeding the idle window,
        // even though the total transfer (20 × 300ms = 6s) dwarfs the window.
        reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, core::array::from_fn::<u8, 20, _>(|i| i as u8));
    }

    /// `secs == 0` (idle `None`) is a transparent pass-through: a long stall
    /// that would trip a live window is tolerated.
    #[tokio::test(start_paused = true)]
    async fn disabled_window_never_trips() {
        let (client, mut server) = tokio::io::duplex(64);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            let _ = server.write_all(&[0x99]).await;
        });

        let mut reader = IdleTimeout::from_secs(client, 0); // disabled
        let mut byte = [0u8; 1];
        // Even after an hour of silence the read eventually succeeds (no trip).
        reader.read_exact(&mut byte).await.unwrap();
        assert_eq!(byte[0], 0x99);
    }

    /// A stalled writer (peer never drains) trips the write-direction timer.
    /// `PendingWriter` always reports `Pending`, emulating full backpressure.
    struct PendingWriter {
        polls: Arc<AtomicUsize>,
    }
    impl AsyncWrite for PendingWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.polls.fetch_add(1, Ordering::SeqCst);
            Poll::Pending
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Pending
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stalled_write_trips_on_idle() {
        let polls = Arc::new(AtomicUsize::new(0));
        let mut writer = IdleTimeout::from_secs(
            PendingWriter {
                polls: polls.clone(),
            },
            1,
        );
        let err = writer.write_all(b"packdata").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "stalled write must trip idle");
        assert!(polls.load(Ordering::SeqCst) >= 1, "inner writer must have been polled");
    }

    #[test]
    fn is_idle_timeout_matches_only_timedout() {
        let idle = anyhow::Error::from(timed_out("read")).context("send_packfile");
        assert!(is_idle_timeout(&idle));

        let other = anyhow::Error::from(io::Error::new(io::ErrorKind::BrokenPipe, "reset"))
            .context("send_packfile");
        assert!(!is_idle_timeout(&other));
    }
}
