use std::io;
use std::sync::Arc;

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, OwnedHandle};
#[cfg(windows)]
use std::os::windows::process::ExitStatusExt;

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Child;

#[cfg(windows)]
use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    GetExitCodeProcess, TerminateProcess, WaitForSingleObject,
};

pub(crate) type ProcessStdin = Box<dyn AsyncWrite + Send + Unpin>;
pub(crate) type ProcessOutput = Box<dyn AsyncRead + Send + Unpin>;
pub(crate) type ProcessKillHook = Arc<dyn Fn() -> io::Result<()> + Send + Sync>;

pub(crate) struct ProcessChildOutput {
    pub status: std::process::ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Backend-neutral process/stdio ownership used by `ExecSession`.
///
/// Native and WSL commands currently enter through the Tokio variant. The Windows
/// AppContainer backend will add a raw-handle variant that is launched through
/// `CreateProcessW + STARTUPINFOEX` without changing session/control semantics.
pub(crate) struct ProcessChild {
    process: ProcessKind,
    stdin: Option<ProcessStdin>,
    stdout: Option<ProcessOutput>,
    stderr: Option<ProcessOutput>,
    process_id: Option<u32>,
    #[cfg(target_os = "windows")]
    _process_tree_guard: Option<crate::platform::ProcessTreeGuard>,
    _backend_lifetimes: Vec<Box<dyn Send>>,
    kill_hook: Option<ProcessKillHook>,
    process_tree_contained: bool,
}

enum ProcessKind {
    Tokio(Child),
    #[cfg(windows)]
    Windows(Arc<OwnedHandle>),
}

impl ProcessChild {
    pub(crate) fn from_tokio(mut child: Child) -> Self {
        let process_id = child.id();
        #[cfg(target_os = "windows")]
        let process_tree_guard = process_id.and_then(crate::platform::attach_process_tree);
        #[cfg(target_os = "windows")]
        let process_tree_contained = process_tree_guard.is_some();
        #[cfg(not(target_os = "windows"))]
        let process_tree_contained = false;

        let stdin = child
            .stdin
            .take()
            .map(|stream| Box::new(stream) as ProcessStdin);
        let stdout = child
            .stdout
            .take()
            .map(|stream| Box::new(stream) as ProcessOutput);
        let stderr = child
            .stderr
            .take()
            .map(|stream| Box::new(stream) as ProcessOutput);

        Self {
            process: ProcessKind::Tokio(child),
            stdin,
            stdout,
            stderr,
            process_id,
            #[cfg(target_os = "windows")]
            _process_tree_guard: process_tree_guard,
            process_tree_contained,
            _backend_lifetimes: Vec::new(),
            kill_hook: None,
        }
    }

    #[cfg(windows)]
    pub(crate) fn from_windows_handles(
        process: OwnedHandle,
        process_id: u32,
        stdin: Option<OwnedHandle>,
        stdout: Option<OwnedHandle>,
        stderr: Option<OwnedHandle>,
        process_tree_guard: crate::platform::ProcessTreeGuard,
    ) -> Self {
        Self {
            process: ProcessKind::Windows(Arc::new(process)),
            stdin: stdin.map(owned_handle_writer),
            stdout: stdout.map(owned_handle_reader),
            stderr: stderr.map(owned_handle_reader),
            process_id: Some(process_id),
            _process_tree_guard: Some(process_tree_guard),
            process_tree_contained: true,
            _backend_lifetimes: Vec::new(),
            kill_hook: None,
        }
    }

    pub(crate) fn with_backend_lifetime(mut self, guard: impl Send + 'static) -> Self {
        self._backend_lifetimes.push(Box::new(guard));
        self
    }

    pub(crate) fn release_backend_lifetimes(&mut self) {
        self._backend_lifetimes.clear();
    }

    pub(crate) fn with_kill_hook(mut self, hook: ProcessKillHook) -> Self {
        self.kill_hook = Some(hook);
        self
    }

    pub(crate) fn kill_hook(&self) -> Option<ProcessKillHook> {
        self.kill_hook.clone()
    }

    pub(crate) fn with_process_tree_contained(mut self, contained: bool) -> Self {
        self.process_tree_contained = contained;
        self
    }

    pub(crate) fn id(&self) -> Option<u32> {
        self.process_id
    }

    pub(crate) fn process_tree_contained(&self) -> bool {
        self.process_tree_contained
    }

    pub(crate) fn take_stdin(&mut self) -> Option<ProcessStdin> {
        self.stdin.take()
    }

    pub(crate) fn take_stdout(&mut self) -> Option<ProcessOutput> {
        self.stdout.take()
    }

    pub(crate) fn take_stderr(&mut self) -> Option<ProcessOutput> {
        self.stderr.take()
    }

    #[cfg(test)]
    pub(crate) async fn wait_with_output(mut self) -> io::Result<ProcessChildOutput> {
        self.wait_with_output_mut().await
    }

    pub(crate) async fn wait_with_output_mut(&mut self) -> io::Result<ProcessChildOutput> {
        self.stdin.take();
        let stdout = self.take_stdout();
        let stderr = self.take_stderr();
        let stdout_read = read_all(stdout);
        let stderr_read = read_all(stderr);
        let (status, stdout, stderr) = tokio::join!(self.wait(), stdout_read, stderr_read);
        Ok(ProcessChildOutput {
            status: status?,
            stdout: stdout?,
            stderr: stderr?,
        })
    }

    pub(crate) async fn cancel(&mut self) -> io::Result<()> {
        let hook_result = if let Some(hook) = self.kill_hook.clone() {
            Some(
                tokio::task::spawn_blocking(move || hook())
                    .await
                    .map_err(|error| {
                        io::Error::other(format!("process kill hook join failed: {error}"))
                    })?,
            )
        } else {
            None
        };
        let local_result = self.start_kill();
        match hook_result {
            Some(Ok(())) => Ok(()),
            Some(Err(error)) => Err(error),
            None => local_result,
        }
    }

    pub(crate) async fn wait(&mut self) -> io::Result<std::process::ExitStatus> {
        match &mut self.process {
            ProcessKind::Tokio(child) => child.wait().await,
            #[cfg(windows)]
            ProcessKind::Windows(process) => wait_windows_process(process.clone()).await,
        }
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<std::process::ExitStatus>> {
        match &mut self.process {
            ProcessKind::Tokio(child) => child.try_wait(),
            #[cfg(windows)]
            ProcessKind::Windows(process) => try_wait_windows_process(process),
        }
    }

    pub(crate) fn start_kill(&mut self) -> io::Result<()> {
        match &mut self.process {
            ProcessKind::Tokio(child) => child.start_kill(),
            #[cfg(windows)]
            ProcessKind::Windows(process) => terminate_windows_process(process),
        }
    }
}

#[cfg(windows)]
fn owned_handle_reader(handle: OwnedHandle) -> ProcessOutput {
    let file: std::fs::File = handle.into();
    Box::new(tokio::fs::File::from_std(file))
}

#[cfg(windows)]
fn owned_handle_writer(handle: OwnedHandle) -> ProcessStdin {
    let file: std::fs::File = handle.into();
    Box::new(tokio::fs::File::from_std(file))
}

#[cfg(windows)]
fn raw_windows_handle(handle: &OwnedHandle) -> HANDLE {
    HANDLE(handle.as_raw_handle())
}

#[cfg(windows)]
async fn wait_windows_process(process: Arc<OwnedHandle>) -> io::Result<std::process::ExitStatus> {
    tokio::task::spawn_blocking(move || {
        let handle = raw_windows_handle(process.as_ref());
        let wait = unsafe { WaitForSingleObject(handle, u32::MAX) };
        if wait != WAIT_OBJECT_0 {
            return Err(io::Error::last_os_error());
        }
        windows_exit_status(handle)
    })
    .await
    .map_err(|error| io::Error::other(format!("Windows process waiter failed: {error}")))?
}

#[cfg(windows)]
fn try_wait_windows_process(
    process: &Arc<OwnedHandle>,
) -> io::Result<Option<std::process::ExitStatus>> {
    let handle = raw_windows_handle(process.as_ref());
    let wait = unsafe { WaitForSingleObject(handle, 0) };
    if wait == WAIT_TIMEOUT {
        return Ok(None);
    }
    if wait != WAIT_OBJECT_0 {
        return Err(io::Error::last_os_error());
    }
    windows_exit_status(handle).map(Some)
}

#[cfg(windows)]
fn windows_exit_status(handle: HANDLE) -> io::Result<std::process::ExitStatus> {
    let mut code = 0u32;
    unsafe { GetExitCodeProcess(handle, &mut code) }
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(std::process::ExitStatus::from_raw(code))
}

#[cfg(windows)]
fn terminate_windows_process(process: &Arc<OwnedHandle>) -> io::Result<()> {
    unsafe { TerminateProcess(raw_windows_handle(process.as_ref()), 1) }
        .map_err(|error| io::Error::other(error.to_string()))
}

async fn read_all(stream: Option<ProcessOutput>) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    let Some(mut stream) = stream else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    stream.read_to_end(&mut output).await?;
    Ok(output)
}

impl From<Child> for ProcessChild {
    fn from(child: Child) -> Self {
        Self::from_tokio(child)
    }
}

#[cfg(test)]
mod lifetime_tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tokio::process::Command;

    use super::ProcessChild;

    struct DropMarker(Arc<AtomicUsize>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    #[tokio::test]
    async fn backend_lifetime_is_released_only_when_explicitly_requested() {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd.exe");
            command.args(["/d", "/c", "exit 0"]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "exit 0"]);
            command
        };

        let drops = Arc::new(AtomicUsize::new(0));
        let child = command.spawn().expect("spawn backend lifetime child");
        let mut child =
            ProcessChild::from_tokio(child).with_backend_lifetime(DropMarker(Arc::clone(&drops)));
        child.wait().await.expect("wait backend lifetime child");
        assert_eq!(drops.load(Ordering::Acquire), 0);
        child.release_backend_lifetimes();
        assert_eq!(drops.load(Ordering::Acquire), 1);
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use std::mem;
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use std::ptr;

    use windows::core::{HSTRING, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT,
    };
    use windows::Win32::Security::SECURITY_ATTRIBUTES;
    use windows::Win32::System::Pipes::CreatePipe;
    use windows::Win32::System::Threading::{
        CreateProcessW, ResumeThread, CREATE_NO_WINDOW, CREATE_SUSPENDED, PROCESS_INFORMATION,
        STARTF_USESTDHANDLES, STARTUPINFOW,
    };

    use super::*;

    struct TestPipes {
        child_stdin: HANDLE,
        parent_stdin: HANDLE,
        parent_stdout: HANDLE,
        child_stdout: HANDLE,
        parent_stderr: HANDLE,
        child_stderr: HANDLE,
    }

    impl TestPipes {
        fn new() -> windows::core::Result<Self> {
            let mut attributes = SECURITY_ATTRIBUTES {
                nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: ptr::null_mut(),
                bInheritHandle: true.into(),
            };
            let mut pipes = Self {
                child_stdin: HANDLE::default(),
                parent_stdin: HANDLE::default(),
                parent_stdout: HANDLE::default(),
                child_stdout: HANDLE::default(),
                parent_stderr: HANDLE::default(),
                child_stderr: HANDLE::default(),
            };
            unsafe {
                CreatePipe(
                    &mut pipes.child_stdin,
                    &mut pipes.parent_stdin,
                    Some(&mut attributes),
                    0,
                )?;
                CreatePipe(
                    &mut pipes.parent_stdout,
                    &mut pipes.child_stdout,
                    Some(&mut attributes),
                    0,
                )?;
                CreatePipe(
                    &mut pipes.parent_stderr,
                    &mut pipes.child_stderr,
                    Some(&mut attributes),
                    0,
                )?;
                SetHandleInformation(
                    pipes.parent_stdin,
                    HANDLE_FLAG_INHERIT.0,
                    Default::default(),
                )?;
                SetHandleInformation(
                    pipes.parent_stdout,
                    HANDLE_FLAG_INHERIT.0,
                    Default::default(),
                )?;
                SetHandleInformation(
                    pipes.parent_stderr,
                    HANDLE_FLAG_INHERIT.0,
                    Default::default(),
                )?;
            }
            Ok(pipes)
        }

        fn close_child_ends(&mut self) {
            unsafe {
                let _ = CloseHandle(self.child_stdin);
                let _ = CloseHandle(self.child_stdout);
                let _ = CloseHandle(self.child_stderr);
            }
            self.child_stdin = HANDLE::default();
            self.child_stdout = HANDLE::default();
            self.child_stderr = HANDLE::default();
        }

        unsafe fn take_parent_stdin(&mut self) -> OwnedHandle {
            let handle = OwnedHandle::from_raw_handle(self.parent_stdin.0 as RawHandle);
            self.parent_stdin = HANDLE::default();
            handle
        }

        unsafe fn take_parent_stdout(&mut self) -> OwnedHandle {
            let handle = OwnedHandle::from_raw_handle(self.parent_stdout.0 as RawHandle);
            self.parent_stdout = HANDLE::default();
            handle
        }

        unsafe fn take_parent_stderr(&mut self) -> OwnedHandle {
            let handle = OwnedHandle::from_raw_handle(self.parent_stderr.0 as RawHandle);
            self.parent_stderr = HANDLE::default();
            handle
        }
    }

    impl Drop for TestPipes {
        fn drop(&mut self) {
            for handle in [
                self.child_stdin,
                self.parent_stdin,
                self.parent_stdout,
                self.child_stdout,
                self.parent_stderr,
                self.child_stderr,
            ] {
                if !handle.is_invalid() {
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn raw_windows_child_uses_shared_async_stdio_wait_and_containment() {
        let mut pipes = TestPipes::new().expect("pipes");
        let startup = STARTUPINFOW {
            cb: mem::size_of::<STARTUPINFOW>() as u32,
            dwFlags: STARTF_USESTDHANDLES,
            hStdInput: pipes.child_stdin,
            hStdOutput: pipes.child_stdout,
            hStdError: pipes.child_stderr,
            ..Default::default()
        };
        let application = HSTRING::from(r"C:\Windows\System32\cmd.exe");
        let mut command_line = r#"cmd.exe /d /c "echo raw-child-out& echo raw-child-err 1>&2""#
            .encode_utf16()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let mut process = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                &application,
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                true,
                CREATE_NO_WINDOW | CREATE_SUSPENDED,
                None,
                None,
                &startup,
                &mut process,
            )
            .expect("CreateProcessW");
        }
        pipes.close_child_ends();
        let tree = crate::platform::attach_process_tree_handle(process.hProcess)
            .expect("pre-resume Job Object containment");
        let resumed = unsafe { ResumeThread(process.hThread) };
        assert_ne!(resumed, u32::MAX, "ResumeThread");
        unsafe {
            let _ = CloseHandle(process.hThread);
        }
        let process_handle =
            unsafe { OwnedHandle::from_raw_handle(process.hProcess.0 as RawHandle) };
        let child = ProcessChild::from_windows_handles(
            process_handle,
            process.dwProcessId,
            Some(unsafe { pipes.take_parent_stdin() }),
            Some(unsafe { pipes.take_parent_stdout() }),
            Some(unsafe { pipes.take_parent_stderr() }),
            tree,
        );
        assert!(child.process_tree_contained());
        let output = child.wait_with_output().await.expect("raw child output");
        assert_eq!(output.status.code(), Some(0));
        assert!(String::from_utf8_lossy(&output.stdout).contains("raw-child-out"));
        assert!(String::from_utf8_lossy(&output.stderr).contains("raw-child-err"));
    }
}
