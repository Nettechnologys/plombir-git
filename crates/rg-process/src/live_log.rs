//! A job's output while it still runs (ADR-0001 D10, card_e925c7cedd46).
//!
//! The captured log ([`crate::CappedJobOutput`]) is still what a job leaves
//! behind: both runners upload or store it whole when the process ends, and it
//! replaces what arrived live. What this module adds is the view before that:
//!
//! 1. The pipe readers offer every read to a [`LiveOutput`]. Offering never
//!    waits: the channel is bounded, and a receiver that falls behind (a slow
//!    or unreachable server) switches the live view off for the rest of the
//!    job instead of holding the job or growing memory. The live log is then a
//!    prefix of the output, never a log with a hole in its middle — a hole is
//!    where a secret cut in two would show both halves.
//! 2. [`forward`] turns the reads into text a line at a time per stream (so
//!    stdout and stderr interleave by whole lines), masks it with a
//!    [`MaskStream`](crate::log_mask::MaskStream) — before anything leaves —
//!    and hands it on in batches, at most every [`FLUSH_INTERVAL`] or
//!    [`MAX_BATCH_BYTES`].
//!
//! A sink that answers "no more" ends the live view the same way.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;

use crate::log_mask::{LogMask, Utf8Stream};

/// Reads held between the pipes and [`forward`]: at most this many of the
/// readers' 8 KiB windows.
pub const LIVE_CHANNEL_CAPACITY: usize = 128;

/// How long text may wait before it is handed on.
pub const FLUSH_INTERVAL: Duration = Duration::from_secs(1);

/// The most text handed on at once.
pub const MAX_BATCH_BYTES: usize = 64 * 1024;

/// A line longer than this is handed on unfinished rather than held.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// Which pipe a read came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// One read of one pipe.
pub struct LivePiece {
    pub stream: Stream,
    pub bytes: Vec<u8>,
}

/// Where the pipe readers offer what they read.
#[derive(Clone)]
pub struct LiveOutput {
    sender: mpsc::Sender<LivePiece>,
    open: Arc<AtomicBool>,
}

impl LiveOutput {
    /// A bounded live view and the receiver [`forward`] reads it from.
    pub fn channel() -> (Self, mpsc::Receiver<LivePiece>) {
        let (sender, receiver) = mpsc::channel(LIVE_CHANNEL_CAPACITY);
        (
            Self {
                sender,
                open: Arc::new(AtomicBool::new(true)),
            },
            receiver,
        )
    }

    /// Offer one read. Never waits; a full or closed channel ends the live
    /// view for good, so what did arrive is a prefix of the output.
    pub fn offer(&self, stream: Stream, bytes: &[u8]) {
        if bytes.is_empty() || !self.open.load(Ordering::Relaxed) {
            return;
        }
        if self
            .sender
            .try_send(LivePiece {
                stream,
                bytes: bytes.to_vec(),
            })
            .is_err()
        {
            self.open.store(false, Ordering::Relaxed);
        }
    }

    /// Whether the live view is still being fed.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Relaxed)
    }
}

/// Puts the two pipes' reads together a line at a time.
#[derive(Default)]
pub struct LiveLines {
    stdout: StreamLines,
    stderr: StreamLines,
}

#[derive(Default)]
struct StreamLines {
    decoder: Utf8Stream,
    partial: String,
}

impl StreamLines {
    fn push(&mut self, bytes: &[u8], out: &mut String) {
        self.partial.push_str(&self.decoder.push(bytes));
        if let Some(end) = self.partial.rfind('\n') {
            out.push_str(&self.partial[..=end]);
            self.partial.drain(..=end);
        }
        if self.partial.len() > MAX_LINE_BYTES {
            out.push_str(&self.partial);
            self.partial.clear();
        }
    }

    fn finish(&mut self, out: &mut String) {
        self.partial.push_str(&self.decoder.finish());
        out.push_str(&self.partial);
        self.partial.clear();
    }
}

impl LiveLines {
    /// The whole lines this read completes.
    pub fn push(&mut self, piece: &LivePiece) -> String {
        let mut out = String::new();
        match piece.stream {
            Stream::Stdout => self.stdout.push(&piece.bytes, &mut out),
            Stream::Stderr => self.stderr.push(&piece.bytes, &mut out),
        }
        out
    }

    /// What is left of both streams' last lines.
    pub fn finish(&mut self) -> String {
        let mut out = String::new();
        self.stdout.finish(&mut out);
        self.stderr.finish(&mut out);
        out
    }
}

/// Where [`forward`] hands masked text.
#[allow(async_fn_in_trait)]
pub trait LiveSink {
    /// Take the next masked batch; `false` ends the live view (the receiver
    /// is gone, or refused it for good).
    async fn send(&mut self, text: String) -> bool;
}

/// Read the live view until the pipes close, and hand it to `sink` masked
/// with `mask`, in batches. Returns once the last batch was handed on — a
/// caller that stores the whole log afterwards waits for this first, so no
/// live text lands after it.
pub async fn forward(
    mut receiver: mpsc::Receiver<LivePiece>,
    mask: &LogMask,
    sink: &mut impl LiveSink,
) {
    let mut lines = LiveLines::default();
    let mut masking = mask.stream();
    let mut batch = String::new();
    let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        let flush = tokio::select! {
            piece = receiver.recv() => match piece {
                Some(piece) => {
                    batch.push_str(&masking.push(&lines.push(&piece)));
                    batch.len() >= MAX_BATCH_BYTES
                }
                None => break,
            },
            _ = ticker.tick() => true,
        };
        if flush && !batch.is_empty() && !sink.send(std::mem::take(&mut batch)).await {
            // Closing the receiver makes the next offer fail, which switches
            // the readers' side off too.
            receiver.close();
            return;
        }
    }
    batch.push_str(&masking.push(&lines.finish()));
    batch.push_str(&masking.finish());
    if !batch.is_empty() {
        sink.send(batch).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Collect(Vec<String>, usize);

    impl LiveSink for Collect {
        async fn send(&mut self, text: String) -> bool {
            self.0.push(text);
            self.0.len() < self.1
        }
    }

    fn piece(stream: Stream, text: &str) -> LivePiece {
        LivePiece {
            stream,
            bytes: text.as_bytes().to_vec(),
        }
    }

    #[test]
    fn lines_of_two_streams_interleave_whole() {
        let mut lines = LiveLines::default();
        assert_eq!(lines.push(&piece(Stream::Stdout, "out-1\nout-")), "out-1\n");
        assert_eq!(lines.push(&piece(Stream::Stderr, "err-1\n")), "err-1\n");
        assert_eq!(lines.push(&piece(Stream::Stdout, "2\n")), "out-2\n");
        assert_eq!(lines.push(&piece(Stream::Stdout, "tail")), "");
        assert_eq!(lines.finish(), "tail");
    }

    #[test]
    fn a_character_split_between_reads_is_not_broken() {
        let mut lines = LiveLines::default();
        let bytes = "ü\n".as_bytes();
        let first = LivePiece {
            stream: Stream::Stdout,
            bytes: bytes[..1].to_vec(),
        };
        let second = LivePiece {
            stream: Stream::Stdout,
            bytes: bytes[1..].to_vec(),
        };
        assert_eq!(lines.push(&first), "");
        assert_eq!(lines.push(&second), "ü\n");
    }

    #[tokio::test]
    async fn a_secret_split_between_reads_is_masked_before_it_leaves() {
        let secret = "secret-value-123";
        let mask = LogMask::new(&[secret.to_string()]);
        let (live, receiver) = LiveOutput::channel();
        live.offer(Stream::Stdout, b"key=secret-");
        live.offer(Stream::Stdout, b"value-123 done\n");
        drop(live);
        let mut sink = Collect(Vec::new(), usize::MAX);
        forward(receiver, &mask, &mut sink).await;
        let text = sink.0.concat();
        assert_eq!(text, "key=*** done\n");
    }

    #[tokio::test]
    async fn a_full_channel_switches_the_live_view_off_instead_of_waiting() {
        let (live, mut receiver) = LiveOutput::channel();
        for _ in 0..LIVE_CHANNEL_CAPACITY {
            live.offer(Stream::Stdout, b"line\n");
        }
        assert!(live.is_open());
        live.offer(Stream::Stdout, b"one too many\n");
        assert!(!live.is_open());
        live.offer(Stream::Stdout, b"after\n");
        drop(live);
        let mut received = 0;
        while let Some(piece) = receiver.recv().await {
            assert_eq!(piece.bytes, b"line\n");
            received += 1;
        }
        assert_eq!(received, LIVE_CHANNEL_CAPACITY);
    }

    #[tokio::test]
    async fn a_sink_that_refuses_ends_the_live_view() {
        let mask = LogMask::new(&[]);
        let (live, receiver) = LiveOutput::channel();
        // One read past a batch: handed on at once, and refused.
        live.offer(Stream::Stdout, &vec![b'x'; MAX_BATCH_BYTES + 1024]);
        let mut sink = Collect(Vec::new(), 1);
        // Returns although `live` still holds the channel open.
        forward(receiver, &mask, &mut sink).await;
        assert_eq!(sink.0.len(), 1);
        assert!(live.is_open());
        live.offer(Stream::Stdout, b"after\n");
        assert!(
            !live.is_open(),
            "an offer after the refusal switches the view off"
        );
    }
}
