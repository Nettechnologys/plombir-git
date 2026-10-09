//! Size-based log rotation.
//!
//! `[logging].max_size_mb` used to be accepted and ignored: files rotated by
//! day whatever the config said. When an operator asks for a size cap, this
//! writer enforces it — the live file moves to `app.log.1`, older ones shift to
//! `app.log.2` … as soon as it reaches the cap, and at most `max_files` of them
//! are kept (the same knob that bounds the daily rotation).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

/// A `Write` sink that rotates its file once it reaches `max_bytes`.
///
/// The check happens before a write, so a file can only overshoot the cap by
/// the size of the last line the tracing appender handed over — the unit is a
/// log line, not a byte.
#[derive(Debug)]
pub(crate) struct SizeRotatingWriter {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: BufWriter<File>,
    written: u64,
}

impl SizeRotatingWriter {
    /// Opens `path` (creating it, appending to whatever is already there) and
    /// remembers its current size, so an existing file does not need to grow
    /// through a whole cap before the first rotation.
    pub(crate) fn new(path: impl Into<PathBuf>, max_bytes: u64, keep: usize) -> io::Result<Self> {
        let path = path.into();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata().map_err(io::Error::from)?.len();
        Ok(Self {
            path,
            // Zero would rotate on every write; callers validate the knob, and
            // this floor keeps the writer itself from thrashing.
            max_bytes: max_bytes.max(1),
            // Zero kept files would delete the log being written; keep one.
            keep: keep.max(1),
            file: BufWriter::new(file),
            written,
        })
    }

    /// `app.log` → `app.log.1` (the suffix of the live file is kept).
    fn rotated_path(&self, index: usize) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{index}"));
        PathBuf::from(name)
    }

    fn rotate(&mut self) -> io::Result<()> {
        self.file.flush()?;
        // Shift .N-1 → .N downwards, dropping whatever sat at .N.
        fs::remove_file(self.rotated_path(self.keep)).ok();
        for index in (1..self.keep).rev() {
            let from = self.rotated_path(index);
            if from.exists() {
                fs::rename(&from, self.rotated_path(index + 1))?;
            }
        }
        fs::rename(&self.path, self.rotated_path(1))?;
        self.file = BufWriter::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)?,
        );
        self.written = 0;
        Ok(())
    }
}

impl Write for SizeRotatingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written >= self.max_bytes {
            self.rotate()?;
        }
        let written = self.file.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

/// The path a size-rotated log lives at, given the configured file: the suffix
/// is kept, so `.../plombir-git.log` rotates as `plombir-git.log.1` beside it.
#[cfg(test)]
fn rotated_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_before_exceeding_the_cap_and_appends_afterwards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let mut writer = SizeRotatingWriter::new(&path, 20, 3).unwrap();

        writer.write_all(b"0123456789").unwrap(); // 10 bytes, under the cap
        assert!(path.exists());
        assert!(!dir.path().join("app.log.1").exists());

        writer.write_all(b"abcdefghij").unwrap(); // 20 bytes: at the cap
        writer.write_all(b"KLMNOPQRST").unwrap(); // rotates first, then writes
        writer.flush().unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"KLMNOPQRST");
        assert_eq!(
            fs::read(dir.path().join("app.log.1")).unwrap(),
            b"0123456789abcdefghij"
        );
        assert_eq!(
            rotated_names(dir.path()),
            vec!["app.log".to_string(), "app.log.1".to_string()]
        );
    }

    #[test]
    fn keeps_at_most_the_configured_number_of_rotations() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let mut writer = SizeRotatingWriter::new(&path, 4, 2).unwrap();

        // Each round fills the cap ("NNNN") and then pushes past it, which
        // rotates; the trailing "x" stays in the live file until the next
        // round pushes past the cap again.
        for round in 0..5u8 {
            writer.write_all(&[b'0' + round; 4]).unwrap();
            writer.write_all(b"x").unwrap();
        }
        writer.flush().unwrap();

        // The newest rotation holds the round before the last, the one before
        // it the round before that, and older rounds were dropped by `keep`.
        assert_eq!(fs::read(dir.path().join("app.log.1")).unwrap(), b"x4444");
        assert_eq!(fs::read(dir.path().join("app.log.2")).unwrap(), b"x3333");
        assert!(!dir.path().join("app.log.3").exists());
        assert_eq!(fs::read(&path).unwrap(), b"x");
    }

    #[test]
    fn an_existing_file_counts_towards_its_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        fs::write(&path, b"already-here").unwrap();

        let mut writer = SizeRotatingWriter::new(&path, 12, 1).unwrap();
        writer.write_all(b"more").unwrap();
        writer.flush().unwrap();

        assert_eq!(
            fs::read(dir.path().join("app.log.1")).unwrap(),
            b"already-here"
        );
        assert_eq!(fs::read(&path).unwrap(), b"more");
    }

    #[test]
    fn creates_the_file_and_its_directory_is_the_callers_job() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("app.log");
        // The directory is not created by the writer; the daily appender
        // behaves the same way and the caller's error carries the hint.
        assert!(SizeRotatingWriter::new(&path, 10, 1).is_err());

        let missing = dir.path().join("app.log");
        let mut writer = SizeRotatingWriter::new(&missing, 10, 1).unwrap();
        writer.write_all(b"line\n").unwrap();
        writer.flush().unwrap();
        assert_eq!(fs::read(&missing).unwrap(), b"line\n");
    }

    #[test]
    fn zero_caps_are_raised_to_one_instead_of_deleting_the_live_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.log");
        let mut writer = SizeRotatingWriter::new(&path, 0, 0).unwrap();
        writer.write_all(b"a").unwrap();
        writer.write_all(b"bc").unwrap();
        writer.flush().unwrap();

        // keep = 0 must not delete the log being written, and cap = 0 must not
        // rotate on every byte.
        assert_eq!(fs::read(&path).unwrap(), b"bc");
        assert_eq!(fs::read(dir.path().join("app.log.1")).unwrap(), b"a");
        assert!(!dir.path().join("app.log.2").exists());
    }
}
