//! Scoped child-process trees for commands bounded by an async cancellation.
//!
//! `tokio::process::Command::kill_on_drop(true)` owns only the direct child.
//! Shells are different: a job can already have forked work by the time its
//! deadline elapses, and killing `sh` / PowerShell alone leaves that work alive.
//! This module makes the command and everything it starts one owned resource.

#[cfg(not(any(unix, windows)))]
compile_error!("rg-process supports Unix and Windows process trees only");

use std::process::{Output, Stdio};

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

    let (child, tree) = platform::spawn(command)?;
    let output = child.wait_with_output().await;
    drop(tree);
    output
}

#[cfg(unix)]
mod platform {
    pub(super) struct ProcessTree {
        pgid: i32,
    }

    pub(super) fn spawn(
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
        let pgid = i32::try_from(pid)
            .map_err(|_| std::io::Error::other("spawned process pid does not fit i32"))?;
        Ok((child, ProcessTree { pgid }))
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
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

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

        fn assign(&self, child: &tokio::process::Child) -> std::io::Result<()> {
            let process = child
                .raw_handle()
                .ok_or_else(|| std::io::Error::other("spawned process has no live handle"))?;
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

    pub(super) fn spawn(
        command: &mut tokio::process::Command,
    ) -> std::io::Result<(tokio::process::Child, ProcessTree)> {
        let tree = ProcessTree::new()?;
        // Assignment after a normally-running CreateProcess has a real race: a
        // short PowerShell command can start a child before the parent reaches
        // `AssignProcessToJobObject`. Start suspended, assign, then resume its
        // only thread so ownership exists before user code runs.
        command.creation_flags(CREATE_SUSPENDED);
        let child = command.spawn()?;
        tree.assign(&child)?;
        let pid = child
            .id()
            .ok_or_else(|| std::io::Error::other("spawned process has no pid"))?;
        resume_initial_thread(pid)?;
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
            assert!(
                !process_is_alive(pid),
                "process {pid} survived Job Object close"
            );
        }
    }
}
