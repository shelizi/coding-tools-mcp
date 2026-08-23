use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;

use super::{ExecSession, OutputEvent, SESSION_EVENT_BYTES};

impl ExecSession {
    fn notify_exit(&self) {
        self.notify_change();
    }

    pub async fn spawn_readers(self: &Arc<Self>) {
        let stdout = {
            let mut guard = self.child.lock().await;
            guard.take_stdout()
        };
        let stderr = {
            let mut guard = self.child.lock().await;
            guard.take_stderr()
        };
        if let Some(stream) = stdout {
            let session = Arc::clone(self);
            let task = tokio::spawn(async move {
                session.read_stream(stream, true).await;
            });
            self.reader_tasks.lock().await.push(task);
        }
        if let Some(stream) = stderr {
            let session = Arc::clone(self);
            let task = tokio::spawn(async move {
                session.read_stream(stream, false).await;
            });
            self.reader_tasks.lock().await.push(task);
        }
    }

    pub fn spawn_exit_waiter(self: &Arc<Self>) {
        if self.exit_waiter_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let session = Arc::clone(self);
        tokio::spawn(async move {
            let status = {
                let mut child = session.child.lock().await;
                child.wait().await.ok()
            };
            if let Some(status) = status {
                session.record_exit_status(status);
            } else {
                session.exited.store(true, Ordering::Release);
                session.mark_termination_reason("crashed");
                session.notify_exit();
                session.notify_change();
            }
        });
    }

    pub async fn wait_until_exited(&self) {
        let mut changes = self.change_tx.subscribe();
        while !self.has_exited() {
            if self.has_exited() {
                break;
            }
            if changes.changed().await.is_err() {
                break;
            }
        }
    }

    pub async fn wait_for_readers(&self) {
        let mut tasks = self.reader_tasks.lock().await;
        while let Some(task) = tasks.pop() {
            let _ = tokio::time::timeout(Duration::from_millis(500), task).await;
        }
    }

    async fn read_stream<T>(&self, mut stream: T, is_stdout: bool)
    where
        T: tokio::io::AsyncRead + Unpin,
    {
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    let (stream_name, stream) = if is_stdout {
                        ("stdout", &self.stdout)
                    } else {
                        ("stderr", &self.stderr)
                    };
                    let (stream_offset, prefix) =
                        stream.lock().expect("process output lock").append(chunk);
                    self.push_event(stream_name, stream_offset, prefix, chunk);
                }
                Err(_) => break,
            }
        }
    }

    fn push_event(&self, stream: &'static str, stream_offset: usize, prefix: Vec<u8>, data: &[u8]) {
        let mut first_output_at = self.first_output_at.lock().expect("first output lock");
        if first_output_at.is_none() {
            *first_output_at = Some(Instant::now());
        }
        drop(first_output_at);
        let mut state = self.events.lock().expect("events lock");
        let sequence = state.next_sequence;
        state.next_sequence += 1;
        state.retained_bytes += data.len();
        state.events.push_back(OutputEvent {
            sequence,
            stream,
            stream_offset,
            prefix,
            data: data.to_vec(),
        });
        while state.retained_bytes > SESSION_EVENT_BYTES {
            if let Some(event) = state.events.pop_front() {
                state.retained_bytes = state.retained_bytes.saturating_sub(event.data.len());
            } else {
                break;
            }
        }
        drop(state);
        self.notify_change();
    }

    async fn run_backend_kill_hook(&self) -> bool {
        let Some(hook) = self.kill_hook.clone() else {
            return false;
        };
        tokio::task::spawn_blocking(move || hook())
            .await
            .is_ok_and(|result| result.is_ok())
    }

    pub async fn kill_and_wait(&self) {
        let backend_cancelled = self.run_backend_kill_hook().await;
        if !self.exit_waiter_started.load(Ordering::Acquire) {
            let status = {
                let mut child = self.child.lock().await;
                let _ = child.start_kill();
                child.wait().await.ok()
            };
            if let Some(status) = status {
                self.record_exit_status(status);
            }
            return;
        }

        if backend_cancelled
            && tokio::time::timeout(Duration::from_secs(2), self.wait_until_exited())
                .await
                .is_ok()
        {
            return;
        }

        if let Some(pid) = self.process_id {
            terminate_process(pid, true).await;
            if tokio::time::timeout(Duration::from_secs(5), self.wait_until_exited())
                .await
                .is_err()
            {
                terminate_process(pid, true).await;
                let _ =
                    tokio::time::timeout(Duration::from_secs(5), self.wait_until_exited()).await;
            }
        }
    }

    pub async fn refresh_status(&self) {
        if self.exit_waiter_started.load(Ordering::Acquire) {
            return;
        }
        let mut child = self.child.lock().await;
        if let Ok(Some(status)) = child.try_wait() {
            self.record_exit_status(status);
        }
    }

    fn record_exit_status(&self, status: std::process::ExitStatus) {
        *self.exit_code.lock().expect("exit_code lock") = status.code();
        self.exited.store(true, Ordering::Release);
        *self.stdin_open.lock().expect("stdin_open lock") = false;
        let mut reason = self.termination_reason.lock().expect("termination lock");
        if reason.is_none() {
            *reason = Some("exited".into());
        }
        drop(reason);
        self.notify_exit();
        self.notify_change();
    }

    fn has_events_after(&self, cursor: u64) -> bool {
        self.latest_cursor() > cursor
    }

    fn has_output(&self) -> bool {
        self.first_output_at
            .lock()
            .expect("first output lock")
            .is_some()
    }

    pub(super) fn wait_condition_satisfied(&self, cursor: u64, until: &str) -> bool {
        match until {
            "finalized" => self.is_finalized(),
            "exit" => self.has_exited(),
            _ => {
                self.has_events_after(cursor)
                    || (cursor == 0 && self.has_output())
                    || self.has_exited()
            }
        }
    }

    pub async fn wait_for_change(&self, cursor: u64, timeout: Duration, until: &str) -> bool {
        let deadline = Instant::now() + timeout;
        let mut changes = self.change_tx.subscribe();
        loop {
            if self.wait_condition_satisfied(cursor, until) {
                return true;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return false;
            }
            let recheck_interval = remaining.min(Duration::from_millis(50));
            match tokio::time::timeout(recheck_interval, changes.changed()).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return false,
                Err(_) => {
                    // A bounded state recheck prevents a missed or coalesced wakeup
                    // from delaying already-buffered output until the full timeout.
                }
            }
        }
    }
}

pub(super) async fn terminate_process(pid: u32, force: bool) {
    #[cfg(windows)]
    {
        let _ = force;
        let _ = tokio::task::spawn_blocking(move || {
            crate::platform::platform().terminate_process_tree(pid)
        })
        .await;
    }

    #[cfg(unix)]
    unsafe {
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        let _ = libc::kill(pid as libc::pid_t, signal);
    }
}
