//! The server's half of a live log an external runner sends while its job
//! runs (ADR-0001 D10, card_e925c7cedd46).
//!
//! The runner sends the log in numbered pieces (`x-job-log-seq: 0, 1, …`),
//! one at a time, retrying a piece until it is answered. The server masks
//! them — it holds the job's secrets, the runner does not know which of its
//! variables they are — and the masking has to see across pieces: a secret
//! cut by the border of two pieces is whole in neither. So per job this keeps:
//!
//! - the number of the next piece it expects: a piece it has already taken
//!   (a retry whose answer was lost) is acknowledged and not written again;
//! - a [`MaskStream`] holding back the end of what arrived until it can no
//!   longer be the start of a secret. Only what the stream emits is written
//!   and broadcast, so nothing unmasked reaches the database or a WebSocket.
//!
//! A piece is taken only once its text is written: if the write fails, the
//! number and the stream stay where they were and the runner's retry of the
//! same piece is a first attempt.
//!
//! The final log the runner uploads whole when the job ends **closes** the
//! job's entry first, under the same lock, so no live piece can land after it.
//!
//! The state is per process and bounded: at most [`MAX_JOBS`] jobs, each
//! holding what its stream holds (bounded by its longest secret form, see
//! `rg_process::log_mask`). A server restart forgets it; the runner's next
//! piece then starts a fresh stream, and the final upload replaces whatever
//! the live view showed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rg_core::auth::log_mask::MaskStream;

/// Jobs whose live log is followed at once.
pub const MAX_JOBS: usize = 4096;

/// An entry nobody touched for this long is dropped to make room.
const IDLE_ENTRY: Duration = Duration::from_secs(60 * 60);

/// Where a job's live log stands.
pub enum LiveState {
    /// No piece taken yet (or the server restarted since).
    New,
    Open {
        next_seq: u64,
        stream: MaskStream,
    },
    /// The final log arrived; later pieces are refused.
    Closed,
}

pub struct LiveEntry {
    pub state: LiveState,
    touched: Instant,
}

/// What became of one numbered piece.
#[derive(Debug, PartialEq, Eq)]
pub enum Piece {
    /// The text to write and broadcast now (possibly empty: all held back).
    Take(String),
    /// Already taken: acknowledge, write nothing.
    Duplicate,
    /// The job's log is complete.
    Closed,
}

#[derive(Default)]
pub struct LiveJobLogs {
    jobs: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<LiveEntry>>>>,
}

impl LiveJobLogs {
    /// The entry of `job_id`, made if it has none. Locking it orders every
    /// write to the job's live log.
    pub fn entry(&self, job_id: i64) -> Arc<tokio::sync::Mutex<LiveEntry>> {
        let mut jobs = self
            .jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = jobs.get(&job_id) {
            return entry.clone();
        }
        if jobs.len() >= MAX_JOBS {
            evict(&mut jobs);
        }
        let entry = Arc::new(tokio::sync::Mutex::new(LiveEntry {
            state: LiveState::New,
            touched: Instant::now(),
        }));
        jobs.insert(job_id, entry.clone());
        entry
    }

    /// Jobs followed now.
    #[cfg(test)]
    fn followed(&self) -> usize {
        self.jobs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len()
    }
}

/// Drop what was idle for [`IDLE_ENTRY`], or else the least recently
/// touched entry no request holds.
fn evict(jobs: &mut HashMap<i64, Arc<tokio::sync::Mutex<LiveEntry>>>) {
    let now = Instant::now();
    jobs.retain(|_, entry| match entry.try_lock() {
        Ok(entry) => now.duration_since(entry.touched) < IDLE_ENTRY,
        Err(_) => true,
    });
    if jobs.len() < MAX_JOBS {
        return;
    }
    let oldest = jobs
        .iter()
        .filter_map(|(id, entry)| entry.try_lock().ok().map(|entry| (*id, entry.touched)))
        .min_by_key(|(_, touched)| *touched)
        .map(|(id, _)| id);
    if let Some(id) = oldest {
        jobs.remove(&id);
    }
}

impl LiveEntry {
    /// Read piece `seq` with `text`: what to write, without taking it yet.
    /// `start` makes the stream of a job seen for the first time.
    /// [`Self::commit`] takes it once the write succeeded.
    pub fn read(
        &mut self,
        seq: u64,
        text: &str,
        start: impl FnOnce() -> MaskStream,
    ) -> (Piece, Option<MaskStream>) {
        self.touched = Instant::now();
        let mut stream = match &self.state {
            LiveState::Closed => return (Piece::Closed, None),
            LiveState::Open { next_seq, .. } if seq < *next_seq => return (Piece::Duplicate, None),
            // The next piece. A later one means pieces were lost to this
            // process (a restart): what the old stream held back is not
            // joined to text that does not follow it.
            LiveState::Open { next_seq, stream } if seq == *next_seq => stream.clone(),
            LiveState::Open { stream, .. } => stream.restarted(),
            LiveState::New => start(),
        };
        let emitted = stream.push(text);
        (Piece::Take(emitted), Some(stream))
    }

    /// Take piece `seq`: its text was written.
    pub fn commit(&mut self, seq: u64, stream: MaskStream) {
        self.state = LiveState::Open {
            next_seq: seq + 1,
            stream,
        };
    }

    /// The final log is being written: no live piece after it.
    pub fn close(&mut self) {
        self.touched = Instant::now();
        self.state = LiveState::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rg_core::auth::log_mask::LogMask;

    fn mask() -> LogMask {
        LogMask::new(&["secret-value-123".to_string()])
    }

    fn send(entry: &mut LiveEntry, seq: u64, text: &str) -> Piece {
        let (piece, stream) = entry.read(seq, text, || mask().stream());
        if let (Piece::Take(_), Some(stream)) = (&piece, stream) {
            entry.commit(seq, stream);
        }
        piece
    }

    fn fresh() -> LiveEntry {
        LiveEntry {
            state: LiveState::New,
            touched: Instant::now(),
        }
    }

    #[test]
    fn a_secret_cut_by_two_pieces_is_masked_across_them() {
        let mut entry = fresh();
        let Piece::Take(first) = send(&mut entry, 0, "key=secret-") else {
            panic!("taken")
        };
        assert!(!first.contains("secret-"), "{first}");
        let Piece::Take(second) = send(&mut entry, 1, "value-123 and more text") else {
            panic!("taken")
        };
        let written = format!("{first}{second}");
        assert!(written.starts_with("key=***"), "{written}");
        assert!(!written.contains("value-123"), "{written}");
    }

    #[test]
    fn a_piece_sent_twice_is_written_once() {
        let mut entry = fresh();
        let piece = "a line that is long enough to come out\n".repeat(4);
        assert!(matches!(send(&mut entry, 0, &piece), Piece::Take(_)));
        assert_eq!(send(&mut entry, 0, &piece), Piece::Duplicate);
        assert!(matches!(send(&mut entry, 1, &piece), Piece::Take(_)));
        assert_eq!(send(&mut entry, 1, &piece), Piece::Duplicate);
    }

    #[test]
    fn a_piece_whose_write_failed_is_read_again_as_new() {
        let mut entry = fresh();
        let (first, _) = entry.read(0, "hello world, long enough line\n", || mask().stream());
        // Not committed: the write failed.
        let (again, _) = entry.read(0, "hello world, long enough line\n", || mask().stream());
        assert_eq!(first, again);
    }

    #[test]
    fn a_gap_in_the_numbers_keeps_masking_with_the_jobs_secrets() {
        let mut entry = fresh();
        send(&mut entry, 0, "first line, long enough to come out\n");
        let (piece, _) = entry.read(5, "secret-value-123 and a tail long enough\n", || {
            panic!("the job's mask is already known")
        });
        let Piece::Take(text) = piece else {
            panic!("taken")
        };
        assert!(text.starts_with("***"), "{text}");
    }

    #[test]
    fn nothing_is_taken_after_the_final_log() {
        let mut entry = fresh();
        send(&mut entry, 0, "first\n");
        entry.close();
        assert_eq!(send(&mut entry, 1, "late\n"), Piece::Closed);
        assert_eq!(send(&mut entry, 0, "first\n"), Piece::Closed);
    }

    #[test]
    fn the_jobs_followed_stay_bounded() {
        let logs = LiveJobLogs::default();
        for job in 0..(MAX_JOBS as i64 + 10) {
            drop(logs.entry(job));
        }
        assert!(logs.followed() <= MAX_JOBS, "{}", logs.followed());
    }
}
