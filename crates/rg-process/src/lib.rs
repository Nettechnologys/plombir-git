//! Scoped child-process trees for commands bounded by cancellation or a deadline.
//!
//! `tokio::process::Command::kill_on_drop(true)` owns only the direct child.
//! Shells are different: a job can already have forked work by the time its
//! deadline elapses, and killing `sh` / PowerShell alone leaves that work alive.
//! This module makes the command and everything it starts one owned resource.

#[cfg(not(any(unix, windows)))]
compile_error!("rg-process supports Unix and Windows process trees only");

use std::process::{Output, Stdio};
use std::time::Duration;

/// Unix permissions for state created by a long-running ForgeKeep process.
///
/// This is a process policy rather than a file helper on purpose. Git, gix,
/// SQLite, package registries and CI job scripts all create persistent files,
/// and several of those writers are outside our code. Installing one `umask`
/// before the first write is the only boundary they all inherit.
///
/// The default is owner-only. The two group modes are explicit escape hatches
/// for deployments where a backup agent or another operator-owned account
/// reaches the state through a shared group; no supported mode grants access to
/// every local account.
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum StateCreationPermissions {
    #[default]
    OwnerOnly,
    GroupReadable,
    GroupWritable,
}

impl StateCreationPermissions {
    /// Install the policy for this process and every child it starts.
    ///
    /// Call this after reading configuration but before the first filesystem
    /// creation. `umask` is process-wide, so changing it after workers start
    /// would race with their opens; ForgeKeep installs it once during each
    /// long-running entrypoint's linear startup.
    pub fn install(self) {
        #[cfg(unix)]
        {
            // SAFETY: `umask` is an always-succeeding libc call that swaps a
            // value in this process's credentials and touches no memory. The
            // caller contract above keeps it out of concurrent write paths.
            unsafe { libc::umask(self.umask() as libc::mode_t) };
        }

        #[cfg(not(unix))]
        {
            let _ = self;
        }
    }

    pub const fn umask(self) -> u32 {
        match self {
            Self::OwnerOnly => 0o077,
            Self::GroupReadable => 0o027,
            Self::GroupWritable => 0o007,
        }
    }

    pub const fn regular_file_mode(self) -> u32 {
        0o666 & !self.umask()
    }

    pub const fn directory_mode(self) -> u32 {
        0o777 & !self.umask()
    }
}

impl std::fmt::Display for StateCreationPermissions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::OwnerOnly => "owner-only",
            Self::GroupReadable => "group-readable",
            Self::GroupWritable => "group-writable",
        };
        formatter.write_str(name)
    }
}

#[cfg(test)]
mod state_creation_permission_tests {
    use super::StateCreationPermissions;

    #[test]
    fn every_supported_policy_keeps_world_access_closed() {
        let policies = [
            StateCreationPermissions::OwnerOnly,
            StateCreationPermissions::GroupReadable,
            StateCreationPermissions::GroupWritable,
        ];

        for policy in policies {
            assert_eq!(policy.regular_file_mode() & 0o007, 0, "{policy}");
            assert_eq!(policy.directory_mode() & 0o007, 0, "{policy}");
        }
        assert_eq!(StateCreationPermissions::OwnerOnly.umask(), 0o077);
        assert_eq!(StateCreationPermissions::GroupReadable.umask(), 0o027);
        assert_eq!(StateCreationPermissions::GroupWritable.umask(), 0o007);
    }
}

/// Result of waiting for a blocking command with a deadline.
#[derive(Debug)]
pub enum TimedOutput {
    Completed(Output),
    TimedOut,
    /// The child wrote more than the caller was willing to hold. The refused
    /// stream stopped being read the moment it would have crossed `limit`, so
    /// what the child wrote after that never entered this process's heap — the
    /// count reported here is the pipe's own read, not the size of any buffer.
    OutputTooLarge {
        stream: LimitedStream,
        limit: u64,
        bytes_read: u64,
    },
}

/// Which pipe grew past its caller-declared ceiling.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LimitedStream {
    Stdout,
    Stderr,
}

impl LimitedStream {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

impl std::fmt::Display for LimitedStream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Stage at which a blocking process-tree execution failed.
#[derive(Debug)]
pub enum ProcessOutputError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
}

impl std::fmt::Display for ProcessOutputError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "failed to spawn process tree: {error}"),
            Self::Wait(error) => {
                write!(formatter, "failed while waiting for process tree: {error}")
            }
        }
    }
}

impl std::error::Error for ProcessOutputError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) | Self::Wait(error) => Some(error),
        }
    }
}

/// Run `command` while owning its whole descendant tree.
///
/// Dropping this future (for example when `tokio::time::timeout` elapses) kills
/// the direct child and every process it started. The tree is also torn down
/// after normal completion, so a background process cannot outlive the job that
/// launched it.
pub async fn output_in_process_tree(
    command: &mut tokio::process::Command,
) -> std::io::Result<Output> {
    // Match `Command::output`: no inherited stdin, captured stdout/stderr. The
    // direct-child guard remains defense in depth if platform setup fails after
    // spawn but before the tree owner is returned.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let (child, tree) = platform::spawn_async(command)?;
    let output = child.wait_with_output().await;
    drop(tree);
    output
}

/// Run a blocking command under a deadline *and* under caller-declared stdout /
/// stderr ceilings, while owning its whole descendant tree.
///
/// The waiter thread owns the `Child`; this thread owns the process-group / Job
/// Object guard. On timeout the guard is dropped first, terminating the tree,
/// and only then is the waiter joined. No mutex needed by the waiter can delay
/// teardown until the command exits naturally.
///
/// The deadline used to be the *only* bound: a `git ls-tree` over a repository
/// with a huge fan-out could answer with a listing hundreds of megabytes long —
/// and did, entirely in this process's memory, before the caller had a chance to
/// look at any of it. The unbounded twin this variant replaced is gone rather
/// than deprecated: a public entry point that drains a pipe into `Vec<u8>` with
/// no ceiling is the defect, and leaving it callable only moves the next
/// occurrence one caller along.
///
/// The reader stops the moment one more byte would cross the limit, then drops
/// its pipe end: on the next write the child sees `EPIPE` (or `SIGPIPE`) and
/// exits shortly after. The count returned is what actually left the kernel
/// into user space, not the size of any buffer allocated on top of it.
pub fn output_in_process_tree_with_timeout_and_limit(
    command: &mut std::process::Command,
    timeout: Duration,
    stdout_limit_bytes: u64,
    stderr_limit_bytes: u64,
) -> Result<TimedOutput, ProcessOutputError> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let (mut child, tree) = platform::spawn_sync(command).map_err(ProcessOutputError::Spawn)?;
    let stdout_pipe = child
        .stdout
        .take()
        .ok_or_else(|| ProcessOutputError::Spawn(std::io::Error::other("stdout was not piped")))?;
    let stderr_pipe = child
        .stderr
        .take()
        .ok_or_else(|| ProcessOutputError::Spawn(std::io::Error::other("stderr was not piped")))?;

    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let waiter = std::thread::Builder::new()
        .name("rg-process-wait-bounded".into())
        .spawn(move || {
            drop(sender.send(wait_with_capped_output(
                child,
                stdout_pipe,
                stderr_pipe,
                stdout_limit_bytes,
                stderr_limit_bytes,
            )));
        })
        .map_err(ProcessOutputError::Wait)?;

    match receiver.recv_timeout(timeout) {
        Ok(result) => {
            // Even a bounded reader that overflowed leaves the tree owning
            // whatever descendants the direct child had spawned; drop first
            // so the guard SIGKILLs anything still holding a captured pipe.
            drop(tree);
            join_waiter(waiter).map_err(ProcessOutputError::Wait)?;
            result.map_err(ProcessOutputError::Wait)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            drop(tree);
            join_waiter(waiter).map_err(ProcessOutputError::Wait)?;
            Ok(TimedOutput::TimedOut)
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            drop(tree);
            join_waiter(waiter).map_err(ProcessOutputError::Wait)?;
            Err(ProcessOutputError::Wait(std::io::Error::other(
                "process wait thread disconnected before reporting an output",
            )))
        }
    }
}

/// What one reader thread returns after draining (or refusing) its pipe.
enum ReaderOutcome {
    Full(Vec<u8>),
    Exceeded { limit: u64, bytes_read: u64 },
}

fn read_capped<R: std::io::Read>(mut reader: R, limit: u64) -> std::io::Result<ReaderOutcome> {
    let mut buf: Vec<u8> = Vec::new();
    let mut total: u64 = 0;
    let mut chunk = [0u8; 8192];
    loop {
        let n = match reader.read(&mut chunk) {
            Ok(0) => return Ok(ReaderOutcome::Full(buf)),
            Ok(n) => n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err),
        };
        let new_total = total.saturating_add(n as u64);
        if new_total > limit {
            // Do NOT append the overflow chunk — the point of the ceiling is
            // that these bytes never enter this process's memory. Dropping
            // `reader` on return closes our end of the pipe, and the child
            // sees SIGPIPE / EPIPE on the next write.
            return Ok(ReaderOutcome::Exceeded {
                limit,
                bytes_read: new_total,
            });
        }
        buf.extend_from_slice(&chunk[..n]);
        total = new_total;
    }
}

fn wait_with_capped_output(
    mut child: std::process::Child,
    stdout_pipe: std::process::ChildStdout,
    stderr_pipe: std::process::ChildStderr,
    stdout_limit: u64,
    stderr_limit: u64,
) -> std::io::Result<TimedOutput> {
    let stdout_handle = std::thread::Builder::new()
        .name("rg-process-read-stdout".into())
        .spawn(move || read_capped(stdout_pipe, stdout_limit))?;
    let stderr_handle = std::thread::Builder::new()
        .name("rg-process-read-stderr".into())
        .spawn(move || read_capped(stderr_pipe, stderr_limit))?;

    let stdout_result = stdout_handle
        .join()
        .map_err(|_| std::io::Error::other("stdout reader thread panicked"))??;
    let stderr_result = stderr_handle
        .join()
        .map_err(|_| std::io::Error::other("stderr reader thread panicked"))??;

    // Reap the child regardless: on overflow, the reader has closed its pipe
    // end, so the next write from the child returns EPIPE and it exits. A
    // child that ignores SIGPIPE hits the outer deadline instead, which is
    // what the process-tree guard is for.
    let status = child.wait()?;

    match (stdout_result, stderr_result) {
        (ReaderOutcome::Exceeded { limit, bytes_read }, _) => Ok(TimedOutput::OutputTooLarge {
            stream: LimitedStream::Stdout,
            limit,
            bytes_read,
        }),
        (_, ReaderOutcome::Exceeded { limit, bytes_read }) => Ok(TimedOutput::OutputTooLarge {
            stream: LimitedStream::Stderr,
            limit,
            bytes_read,
        }),
        (ReaderOutcome::Full(stdout), ReaderOutcome::Full(stderr)) => {
            Ok(TimedOutput::Completed(Output {
                status,
                stdout,
                stderr,
            }))
        }
    }
}

fn join_waiter(waiter: std::thread::JoinHandle<()>) -> std::io::Result<()> {
    waiter
        .join()
        .map_err(|_| std::io::Error::other("process wait thread panicked"))
}

#[cfg(all(test, unix))]
mod bounded_tests {
    use super::*;

    /// Peak resident-set size of this test process so far, in bytes.
    ///
    /// The *peak*, not the current one: an allocation freed by the time the
    /// test finishes is back off the books, and a snapshot taken afterwards
    /// cannot tell "buffered 400 MiB and released it" from "streamed 8 KiB at
    /// a time". `VmHWM` is the high-water mark, which is the number a broken
    /// ceiling would move — and nextest runs every test in its own process, so
    /// the mark belongs to this test alone.
    fn peak_resident_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status
            .lines()
            .find(|line| line.starts_with("VmHWM:"))?
            .strip_prefix("VmHWM:")?;
        let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
        Some(kib * 1024)
    }

    #[test]
    fn a_stream_under_the_ceiling_reads_normally() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf 'hello, forgekeep\\n'"]);

        let result = output_in_process_tree_with_timeout_and_limit(
            &mut command,
            Duration::from_secs(5),
            1024,
            1024,
        )
        .expect("bounded run failed");

        match result {
            TimedOutput::Completed(output) => {
                assert!(output.status.success(), "child exited non-zero");
                assert_eq!(output.stdout, b"hello, forgekeep\n");
                assert!(output.stderr.is_empty(), "unexpected stderr");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn a_stdout_stream_past_the_ceiling_reports_output_too_large() {
        let mut command = std::process::Command::new("sh");
        // Emit 1 MiB of `a`s, more than the ceiling.
        command.args(["-c", "head -c 1048576 /dev/zero | tr '\\0' 'a'"]);

        let result = output_in_process_tree_with_timeout_and_limit(
            &mut command,
            Duration::from_secs(10),
            65_536,
            1024,
        )
        .expect("bounded run failed");

        match result {
            TimedOutput::OutputTooLarge {
                stream,
                limit,
                bytes_read,
            } => {
                assert_eq!(stream, LimitedStream::Stdout, "wrong stream flagged");
                assert_eq!(limit, 65_536, "wrong limit echoed back");
                assert!(
                    bytes_read > limit,
                    "bytes_read {bytes_read} should exceed limit {limit}"
                );
                // The reader stops the moment ONE chunk crosses the ceiling,
                // so bytes_read is bounded by limit + one chunk (~8 KiB) —
                // never the whole 1 MiB the child tried to emit.
                assert!(
                    bytes_read < limit + 64 * 1024,
                    "bytes_read {bytes_read} suggests the reader kept going past the ceiling"
                );
            }
            other => panic!("expected OutputTooLarge, got {other:?}"),
        }
    }

    #[test]
    fn a_stderr_stream_past_the_ceiling_reports_output_too_large_on_stderr() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "head -c 1048576 /dev/zero | tr '\\0' 'e' 1>&2"]);

        let result = output_in_process_tree_with_timeout_and_limit(
            &mut command,
            Duration::from_secs(10),
            1024,
            65_536,
        )
        .expect("bounded run failed");

        match result {
            TimedOutput::OutputTooLarge { stream, limit, .. } => {
                assert_eq!(stream, LimitedStream::Stderr);
                assert_eq!(limit, 65_536);
            }
            other => panic!("expected OutputTooLarge on stderr, got {other:?}"),
        }
    }

    /// The measurement the whole ceiling exists for.
    ///
    /// A subprocess emits 128 MiB of zeros to stdout; the bounded reader
    /// refuses at 4 MiB. If the ceiling is removed (a mutation of
    /// `read_capped` that never returns `Exceeded`), the whole 128 MiB flows
    /// into `Vec<u8>` and this test's process high-water mark grows by
    /// approximately that amount — the number no exit-code or status
    /// assertion can see.
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "reads VmHWM from /proc/self/status"
    )]
    #[test]
    fn bounded_reads_do_not_grow_the_process_by_the_size_of_a_refused_stream() {
        // 128 MiB of `/dev/zero` — well above the 4 MiB ceiling and orders
        // above the ~10 MiB of measurement noise this process holds anyway.
        const REFUSED_BYTES: u64 = 128 * 1024 * 1024;
        const CEILING: u64 = 4 * 1024 * 1024;

        let before = peak_resident_bytes().expect("no /proc/self/status to measure against");

        let mut command = std::process::Command::new("sh");
        command.args(["-c", &format!("head -c {REFUSED_BYTES} /dev/zero")]);

        let result = output_in_process_tree_with_timeout_and_limit(
            &mut command,
            Duration::from_secs(30),
            CEILING,
            1024,
        )
        .expect("bounded run failed");

        assert!(
            matches!(result, TimedOutput::OutputTooLarge { .. }),
            "expected OutputTooLarge, got {result:?}"
        );

        let after = peak_resident_bytes().expect("no /proc/self/status to measure against");
        let grew = after.saturating_sub(before);
        // Generous: the reader keeps at most one chunk beyond the ceiling and
        // the heap has a bit of Vec headroom, but 32 MiB is still four orders
        // of magnitude below the 128 MiB an unbounded reader would swallow.
        let ceiling_for_growth = 32 * 1024 * 1024;
        assert!(
            grew < ceiling_for_growth,
            "refused-stream ceiling did not bind: a {REFUSED_BYTES}-byte stream grew the \
             process by {} MiB — the reader is buffering past the ceiling",
            grew / (1024 * 1024)
        );
    }

    #[test]
    fn the_deadline_still_bites_even_under_a_ceiling() {
        let mut command = std::process::Command::new("sh");
        // A cooperative sleeper: emits nothing, so no ceiling ever triggers;
        // the deadline is what must return.
        command.args(["-c", "sleep 5"]);

        let result = output_in_process_tree_with_timeout_and_limit(
            &mut command,
            Duration::from_millis(200),
            1024,
            1024,
        )
        .expect("bounded run failed");

        assert!(
            matches!(result, TimedOutput::TimedOut),
            "expected TimedOut, got {result:?}"
        );
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::process::CommandExt as _;

    pub(super) struct ProcessTree {
        pgid: i32,
    }

    pub(super) fn spawn_async(
        command: &mut tokio::process::Command,
    ) -> std::io::Result<(tokio::process::Child, ProcessTree)> {
        // `0` means "make the child the leader of a new process group". This is
        // done in the child before exec, so there is no fork-before-ownership
        // race for a fast shell script.
        command.process_group(0);
        let child = command.spawn()?;
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("spawned process has no pid"))?;
        Ok((child, ProcessTree::from_pid(pid)?))
    }

    pub(super) fn spawn_sync(
        command: &mut std::process::Command,
    ) -> std::io::Result<(std::process::Child, ProcessTree)> {
        command.process_group(0);
        let child = command.spawn()?;
        let tree = ProcessTree::from_pid(child.id())?;
        Ok((child, tree))
    }

    impl ProcessTree {
        fn from_pid(pid: u32) -> std::io::Result<Self> {
            let pgid = i32::try_from(pid)
                .map_err(|_| std::io::Error::other("spawned process pid does not fit i32"))?;
            Ok(Self { pgid })
        }
    }

    impl Drop for ProcessTree {
        fn drop(&mut self) {
            // A negative pid addresses the whole process group. SIGKILL is
            // intentional: this guard runs after the job's execution boundary,
            // including timeout cancellation, where waiting for arbitrary
            // user-provided traps would let the job exceed its deadline again.
            //
            // SAFETY: `self.pgid` came from the successfully spawned child and
            // is positive; negating it targets only that child's new group.
            let result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
            if result == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    tracing::warn!(
                        process_group_id = self.pgid,
                        %error,
                        "failed to terminate child process group"
                    );
                }
            }
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::mem::size_of;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
    use std::os::windows::process::CommandExt as _;

    use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenThread, ResumeThread, CREATE_SUSPENDED, THREAD_SUSPEND_RESUME,
    };

    pub(super) struct ProcessTree {
        job: OwnedHandle,
    }

    impl ProcessTree {
        fn new() -> std::io::Result<Self> {
            // SAFETY: null security/name pointers request the documented default
            // unnamed job object. A non-null handle is exclusively owned below.
            let raw_job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
            if raw_job.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            // SAFETY: `raw_job` is a fresh owned Win32 handle and is converted
            // exactly once; `OwnedHandle` closes it on every exit path.
            let job = unsafe { OwnedHandle::from_raw_handle(raw_job.cast()) };
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: the buffer is a live value of the information class's
            // documented type and the byte count matches it exactly.
            let configured = unsafe {
                SetInformationJobObject(
                    job.as_raw_handle().cast(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            };
            if configured == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(Self { job })
        }

        fn assign_async(&self, child: &tokio::process::Child) -> std::io::Result<()> {
            let process = child
                .raw_handle()
                .ok_or_else(|| std::io::Error::other("spawned process has no live handle"))?;
            self.assign_handle(process)
        }

        fn assign_sync(&self, child: &std::process::Child) -> std::io::Result<()> {
            self.assign_handle(child.as_raw_handle())
        }

        fn assign_handle(&self, process: RawHandle) -> std::io::Result<()> {
            // SAFETY: both handles are live for this call. The child was created
            // suspended, so it cannot fork before assignment succeeds.
            let assigned = unsafe {
                AssignProcessToJobObject(self.job.as_raw_handle().cast(), process.cast())
            };
            if assigned == 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        }
    }

    pub(super) fn spawn_async(
        command: &mut tokio::process::Command,
    ) -> std::io::Result<(tokio::process::Child, ProcessTree)> {
        let tree = ProcessTree::new()?;
        // Assignment after a normally-running CreateProcess has a real race: a
        // short PowerShell command can start a child before the parent reaches
        // `AssignProcessToJobObject`. Start suspended, assign, then resume its
        // only thread so ownership exists before user code runs.
        command.creation_flags(CREATE_SUSPENDED);
        let child = command.spawn()?;
        tree.assign_async(&child)?;
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("spawned process has no pid"))?;
        resume_initial_thread(pid)?;
        Ok((child, tree))
    }

    pub(super) fn spawn_sync(
        command: &mut std::process::Command,
    ) -> std::io::Result<(std::process::Child, ProcessTree)> {
        let tree = ProcessTree::new()?;
        command.creation_flags(CREATE_SUSPENDED);
        let mut child = command.spawn()?;
        if let Err(error) = tree.assign_sync(&child) {
            drop(child.kill());
            drop(child.wait());
            return Err(error);
        }
        if let Err(error) = resume_initial_thread(child.id()) {
            drop(tree);
            drop(child.wait());
            return Err(error);
        }
        Ok((child, tree))
    }

    fn resume_initial_thread(pid: u32) -> std::io::Result<()> {
        // SAFETY: the snapshot handle is checked before being wrapped and every
        // THREADENTRY32 call receives the documented initialized byte size.
        let raw_snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if raw_snapshot == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw_snapshot` is a fresh owned handle and is converted once.
        let snapshot = unsafe { OwnedHandle::from_raw_handle(raw_snapshot.cast()) };
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            ..THREADENTRY32::default()
        };
        // SAFETY: `snapshot` and `entry` stay live for the enumeration.
        let mut found = unsafe { Thread32First(snapshot.as_raw_handle().cast(), &mut entry) } != 0;
        while found {
            if entry.th32OwnerProcessID == pid {
                // SAFETY: the thread id came from the live system snapshot. The
                // returned handle, when non-null, is owned by this scope.
                let raw_thread =
                    unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) };
                if raw_thread.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                // SAFETY: `raw_thread` is a fresh owned handle and is converted once.
                let thread = unsafe { OwnedHandle::from_raw_handle(raw_thread.cast()) };
                // SAFETY: this is the initial thread of the process we created
                // with CREATE_SUSPENDED, and its handle grants suspend/resume.
                if unsafe { ResumeThread(thread.as_raw_handle().cast()) } == u32::MAX {
                    return Err(std::io::Error::last_os_error());
                }
                return Ok(());
            }
            // SAFETY: same live snapshot and initialized entry as Thread32First.
            found = unsafe { Thread32Next(snapshot.as_raw_handle().cast(), &mut entry) } != 0;
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("cannot find initial thread for suspended process {pid}"),
        ))
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };

    fn process_is_alive(pid: u32) -> bool {
        // SAFETY: querying a process by id does not transfer any borrowed state;
        // a non-null result is an owned handle wrapped immediately below.
        let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if raw.is_null() {
            return false;
        }
        // SAFETY: `raw` is a fresh owned handle and is converted exactly once.
        let process = unsafe { OwnedHandle::from_raw_handle(raw.cast()) };
        // WAIT_TIMEOUT means the process handle is not signaled and is still live.
        (unsafe { WaitForSingleObject(process.as_raw_handle().cast(), 0) })
            == windows_sys::Win32::Foundation::WAIT_TIMEOUT
    }

    fn assert_process_stops(pid: u32) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while process_is_alive(pid) {
            assert!(
                std::time::Instant::now() < deadline,
                "process {pid} survived Job Object close"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[tokio::test]
    async fn dropping_execution_kills_a_powershell_descendant() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("processes.pid");
        let mut command = tokio::process::Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$child = Start-Process powershell.exe -ArgumentList \
                 '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30' \
                 -PassThru; Set-Content -LiteralPath $env:RG_PROCESS_PID_FILE \
                 -Value \"$PID`n$($child.Id)\"; Wait-Process -Id $child.Id",
            ])
            .env("RG_PROCESS_PID_FILE", &pid_file);

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            output_in_process_tree(&mut command),
        )
        .await;
        assert!(
            result.is_err(),
            "fixture unexpectedly finished before timeout"
        );

        let pids: Vec<u32> = std::fs::read_to_string(&pid_file)
            .unwrap_or_else(|error| panic!("PowerShell did not record its process tree: {error}"))
            .lines()
            .map(|line| line.trim().parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        for pid in pids {
            assert_process_stops(pid);
        }
    }

    #[test]
    fn blocking_timeout_kills_a_powershell_descendant() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("blocking-processes.pid");
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$child = Start-Process powershell.exe -ArgumentList \
                 '-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30' \
                 -PassThru; Set-Content -LiteralPath $env:RG_PROCESS_PID_FILE \
                 -Value \"$PID`n$($child.Id)\"; Wait-Process -Id $child.Id",
            ])
            .env("RG_PROCESS_PID_FILE", &pid_file);

        // No ceiling: what this test is about is the deadline tearing the tree
        // down, and a limit reached first would end the run for the other
        // reason. `bounded_tests` owns the ceilings.
        let result = output_in_process_tree_with_timeout_and_limit(
            &mut command,
            std::time::Duration::from_secs(2),
            u64::MAX,
            u64::MAX,
        )
        .unwrap();
        assert!(matches!(result, TimedOutput::TimedOut));

        let pids: Vec<u32> = std::fs::read_to_string(&pid_file)
            .unwrap_or_else(|error| panic!("PowerShell did not record its process tree: {error}"))
            .lines()
            .map(|line| line.trim().parse().unwrap())
            .collect();
        assert_eq!(pids.len(), 2);
        for pid in pids {
            assert_process_stops(pid);
        }
    }
}
