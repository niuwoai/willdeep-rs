//! A command's descendants share a kill-on-close job. The leader starts
//! suspended so it cannot launch descendants before job assignment.
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use windows_sys::Win32::Foundation::{HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject,
};
use windows_sys::Win32::System::Threading::{
    CREATE_SUSPENDED, OpenThread, ResumeThread, THREAD_SUSPEND_RESUME,
};

pub(super) struct Job(Option<OwnedHandle>);

impl Job {
    pub(super) fn new(command: &mut tokio::process::Command) -> io::Result<Self> {
        // SAFETY: Null pointers request an unnamed, non-inheritable job.
        let handle = owned(unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) })?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: The handle is live and the buffer has the API's required layout.
        let configured = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        };
        if configured == 0 {
            return Err(io::Error::last_os_error());
        }
        command.creation_flags(CREATE_SUSPENDED);
        Ok(Self(Some(handle)))
    }

    pub(super) fn attach_and_resume(&self, child: &tokio::process::Child) -> io::Result<()> {
        let process = child
            .raw_handle()
            .ok_or_else(|| io::Error::other("missing suspended process handle"))?;
        // SAFETY: Both owned handles remain live throughout assignment.
        if unsafe { AssignProcessToJobObject(self.0.as_ref().unwrap().as_raw_handle(), process) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        resume_primary_thread(
            child
                .id()
                .ok_or_else(|| io::Error::other("missing suspended process id"))?,
        )
    }

    pub(super) fn terminate(&mut self) {
        // Closing the last job handle kills the leader and all descendants.
        self.0.take();
    }
}

fn owned(handle: HANDLE) -> io::Result<OwnedHandle> {
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Every caller transfers a newly created, uniquely owned handle.
    Ok(unsafe { OwnedHandle::from_raw_handle(handle) })
}

fn resume_primary_thread(process_id: u32) -> io::Result<()> {
    // SAFETY: Snapshot creation has no pointer arguments.
    let snapshot = owned(unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) })?;
    let mut entry = THREADENTRY32 {
        dwSize: std::mem::size_of::<THREADENTRY32>() as u32,
        ..Default::default()
    };
    // SAFETY: The snapshot and correctly sized output structure are live.
    let mut present = unsafe { Thread32First(snapshot.as_raw_handle(), &mut entry) };
    while present != 0 {
        if entry.th32OwnerProcessID == process_id {
            // The suspended process has not run user code or created children.
            // SAFETY: OpenThread returns a new non-inheritable owned handle.
            let thread =
                owned(unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID) })?;
            // SAFETY: The thread belongs to our still-live suspended process.
            if unsafe { ResumeThread(thread.as_raw_handle()) } == u32::MAX {
                return Err(io::Error::last_os_error());
            }
            return Ok(());
        }
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        // SAFETY: As for Thread32First above.
        present = unsafe { Thread32Next(snapshot.as_raw_handle(), &mut entry) };
    }
    Err(io::Error::other("suspended process thread was not found"))
}

#[cfg(test)]
mod tests {
    use crate::sandbox::{SandboxPolicy, SandboxSpec};
    use std::time::Duration;

    fn fixture() -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("willdeep-job-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("child.ps1"),
            "Set-Content started yes; Start-Sleep -Seconds 3; Set-Content escaped yes",
        )
        .unwrap();
        root
    }

    fn command(keep_leader: bool) -> String {
        let tail = if keep_leader {
            "while ($true) { Start-Sleep -Seconds 1 }"
        } else {
            "Write-Output finished"
        };
        format!(
            "$p = Start-Process powershell.exe -ArgumentList '-NoProfile -NonInteractive -File child.ps1' -WorkingDirectory (Get-Location).Path -PassThru; while (!(Test-Path started)) {{ Start-Sleep -Milliseconds 20 }}; {tail}"
        )
    }

    #[tokio::test]
    async fn leader_exit_cleans_descendants_before_returning() {
        let root = fixture();
        let output = super::super::run_capture(
            &command(false),
            &root,
            &SandboxSpec::new(SandboxPolicy::Off, []),
            Duration::from_secs(15),
            4096,
        )
        .await
        .unwrap();
        assert!(output.status.success());
        assert!(root.join("started").exists());
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!root.join("escaped").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancellation_cleans_a_started_descendant() {
        let root = fixture();
        let command = command(true);
        let sandbox = SandboxSpec::new(SandboxPolicy::Off, []);
        let mut run = Box::pin(super::super::run_capture(
            &command,
            &root,
            &sandbox,
            Duration::from_secs(30),
            4096,
        ));
        let started = async {
            while !root.join("started").exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        };
        tokio::select! {
            result = &mut run => panic!("leader exited before cancellation: {result:?}"),
            result = tokio::time::timeout(Duration::from_secs(15), started) => result.unwrap(),
        }
        drop(run);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!root.join("escaped").exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
