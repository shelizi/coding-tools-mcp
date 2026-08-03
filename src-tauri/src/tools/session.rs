use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{watch, Mutex as AsyncMutex, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

use crate::mcp::{record_async_session_finalized, AsyncSessionTelemetry};
use crate::tools::workspace::{tool_ok, WorkspaceError};

const SESSION_BUFFER_BYTES: usize = 1_048_576;
const SESSION_EVENT_BYTES: usize = 1_048_576;
pub const DEFAULT_ACTIVE_SESSION_LIMIT: usize = 512;
const MAX_RETAINED_FINALIZED_SESSIONS: usize = 128;
const FINALIZED_SESSION_RETENTION: Duration = Duration::from_secs(900);
#[cfg(not(test))]
pub const DETACHED_SESSION_GRACE: Duration = Duration::from_secs(90);
#[cfg(test)]
pub const DETACHED_SESSION_GRACE: Duration = Duration::from_millis(250);

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[derive(Default)]
struct SessionRegistry {
    sessions: HashMap<String, Arc<ExecSession>>,
    operation_index: HashMap<String, String>,
    fingerprint_index: HashMap<String, String>,
}

pub struct SessionStore {
    registry: Mutex<SessionRegistry>,
    active_slots: Arc<Semaphore>,
    active_session_limit: usize,
}

impl Default for SessionStore {
    fn default() -> Self {
        Self {
            registry: Mutex::new(SessionRegistry::default()),
            active_slots: Arc::new(Semaphore::new(DEFAULT_ACTIVE_SESSION_LIMIT)),
            active_session_limit: DEFAULT_ACTIVE_SESSION_LIMIT,
        }
    }
}

impl SessionStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_active_session_limit(limit: usize) -> Self {
        let limit = limit.clamp(1, u16::MAX as usize);
        Self {
            registry: Mutex::new(SessionRegistry::default()),
            active_slots: Arc::new(Semaphore::new(limit)),
            active_session_limit: limit,
        }
    }

    pub fn insert(&self, session: ExecSession) -> Arc<ExecSession> {
        let arc = Arc::new(session);
        let mut registry = self.registry.lock().expect("sessions registry lock");
        prune_finalized_sessions(&mut registry);
        if let Some(operation_id) = arc.operation_id.as_ref() {
            registry
                .operation_index
                .insert(operation_id.clone(), arc.session_id.clone());
        }
        if let Some(fingerprint) = arc.command_fingerprint.as_ref() {
            registry
                .fingerprint_index
                .insert(fingerprint.clone(), arc.session_id.clone());
        }
        registry
            .sessions
            .insert(arc.session_id.clone(), arc.clone());
        arc
    }

    pub async fn acquire_active_slot(&self) -> Result<OwnedSemaphorePermit, WorkspaceError> {
        match tokio::time::timeout(
            Duration::from_secs(1),
            self.active_slots.clone().acquire_owned(),
        )
        .await
        {
            Ok(Ok(permit)) => Ok(permit),
            Ok(Err(error)) => Err(WorkspaceError::ToolDetails {
                code: "SESSION_ADMISSION_CLOSED",
                message: format!("Session admission closed: {error}"),
                category: "runtime",
                retryable: true,
                details: json!({
                    "stage": "session_admission",
                    "active_session_limit": self.active_session_limit,
                    "suggestion": "重启 MCP 运行时后重试"
                }),
            }),
            Err(_) => Err(WorkspaceError::ToolDetails {
                code: "SESSION_LIMIT_REACHED",
                message: format!(
                    "Active command session limit reached ({}).",
                    self.active_session_limit
                ),
                category: "runtime",
                retryable: true,
                details: json!({
                    "stage": "session_admission",
                    "active_session_limit": self.active_session_limit,
                    "active_session_slots_available": self.active_slots.available_permits(),
                    "suggestion": "等待现有命令完成，或使用 kill_session 终止不再需要的长任务"
                }),
            }),
        }
    }

    pub fn active_session_limit(&self) -> usize {
        self.active_session_limit
    }

    pub fn active_slots_available(&self) -> usize {
        self.active_slots.available_permits()
    }

    pub fn get(&self, session_id: &str) -> Result<Arc<ExecSession>, WorkspaceError> {
        self.get_with_metrics(session_id)
            .map(|(session, _)| session)
    }

    pub fn get_with_metrics(
        &self,
        session_id: &str,
    ) -> Result<(Arc<ExecSession>, u128), WorkspaceError> {
        let started = Instant::now();
        let registry = self.registry.lock().expect("sessions registry lock");
        let lock_wait_ms = started.elapsed().as_millis();
        registry
            .sessions
            .get(session_id)
            .cloned()
            .map(|session| (session, lock_wait_ms))
            .ok_or_else(|| WorkspaceError::Tool {
                code: "SESSION_NOT_FOUND",
                message: format!("Session not found: {session_id}"),
                category: "not_found",
                retryable: false,
            })
    }

    pub fn get_by_operation(&self, operation_id: &str) -> Option<Arc<ExecSession>> {
        let mut registry = self.registry.lock().expect("sessions registry lock");
        prune_finalized_sessions(&mut registry);
        let session_id = registry.operation_index.get(operation_id)?.clone();
        registry.sessions.get(&session_id).cloned()
    }

    pub fn get_by_fingerprint(&self, fingerprint: &str) -> Option<Arc<ExecSession>> {
        let mut registry = self.registry.lock().expect("sessions registry lock");
        prune_finalized_sessions(&mut registry);
        let session_id = registry.fingerprint_index.get(fingerprint)?.clone();
        registry.sessions.get(&session_id).cloned()
    }

    pub fn list(&self, include_finalized: bool, limit: usize) -> Vec<Arc<ExecSession>> {
        let mut registry = self.registry.lock().expect("sessions registry lock");
        prune_finalized_sessions(&mut registry);
        let mut sessions = registry
            .sessions
            .values()
            .filter(|session| include_finalized || !session.is_finalized())
            .cloned()
            .collect::<Vec<_>>();
        sessions.sort_by_key(|session| std::cmp::Reverse(session.started_ts_ms));
        sessions.truncate(limit);
        sessions
    }

    pub fn contains(&self, session_id: &str) -> bool {
        let registry = self.registry.lock().expect("sessions registry lock");
        registry.sessions.contains_key(session_id)
    }

    pub fn remove(&self, session_id: &str) {
        let mut registry = self.registry.lock().expect("sessions registry lock");
        registry.sessions.remove(session_id);
        registry
            .operation_index
            .retain(|_, indexed_session_id| indexed_session_id != session_id);
        registry
            .fingerprint_index
            .retain(|_, indexed_session_id| indexed_session_id != session_id);
    }
}

fn prune_finalized_sessions(registry: &mut SessionRegistry) {
    registry.sessions.retain(|_, session| {
        !session
            .finalized_at()
            .is_some_and(|finished| finished.elapsed() >= FINALIZED_SESSION_RETENTION)
    });

    let mut finalized = registry
        .sessions
        .iter()
        .filter_map(|(session_id, session)| {
            session
                .finalized_at()
                .map(|finished| (session_id.clone(), finished))
        })
        .collect::<Vec<_>>();
    if finalized.len() > MAX_RETAINED_FINALIZED_SESSIONS {
        finalized.sort_by_key(|(_, finished)| *finished);
        let remove_count = finalized.len() - MAX_RETAINED_FINALIZED_SESSIONS;
        for (session_id, _) in finalized.into_iter().take(remove_count) {
            registry.sessions.remove(&session_id);
        }
    }
    let retained_session_ids = registry.sessions.keys().cloned().collect::<HashSet<_>>();
    registry
        .operation_index
        .retain(|_, session_id| retained_session_ids.contains(session_id));
    registry
        .fingerprint_index
        .retain(|_, session_id| retained_session_ids.contains(session_id));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputMode {
    Delta,
    Tail,
    All,
    None,
    Summary,
}

impl OutputMode {
    fn parse(value: Option<&str>, default: Self) -> Self {
        match value {
            Some("delta") => Self::Delta,
            Some("tail") => Self::Tail,
            Some("all") => Self::All,
            Some("none") => Self::None,
            Some("summary") => Self::Summary,
            _ => default,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Delta => "delta",
            Self::Tail => "tail",
            Self::All => "all",
            Self::None => "none",
            Self::Summary => "summary",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct OutputOptions {
    pub mode: OutputMode,
    pub cursor: u64,
    pub max_output_bytes: usize,
    pub tail_lines: usize,
}

impl OutputOptions {
    pub fn from_args(args: &Value, default_mode: OutputMode) -> Self {
        Self {
            mode: OutputMode::parse(
                args.get("output_mode").and_then(Value::as_str),
                default_mode,
            ),
            cursor: args.get("cursor").and_then(Value::as_u64).unwrap_or(0),
            max_output_bytes: args
                .get("max_output_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(65_536)
                .clamp(1, 1_048_576) as usize,
            tail_lines: args
                .get("tail_lines")
                .and_then(Value::as_u64)
                .unwrap_or(100)
                .clamp(1, 10_000) as usize,
        }
    }

    pub fn tail(max_output_bytes: usize) -> Self {
        Self {
            mode: OutputMode::Tail,
            cursor: 0,
            max_output_bytes,
            tail_lines: 100,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ProcessOutputEncoding {
    #[default]
    Unknown,
    Utf16Le,
    Utf16Be,
}

impl ProcessOutputEncoding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "utf-8",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
        }
    }

    fn is_utf16(self) -> bool {
        matches!(self, Self::Utf16Le | Self::Utf16Be)
    }
}

#[derive(Debug, Default)]
struct ProcessOutputStream {
    data: Vec<u8>,
    total_bytes: usize,
    encoding: ProcessOutputEncoding,
}

#[derive(Clone, Debug)]
struct ProcessOutputSnapshot {
    data: Vec<u8>,
    total_bytes: usize,
    encoding: ProcessOutputEncoding,
}

impl ProcessOutputStream {
    fn append(&mut self, chunk: &[u8]) -> (usize, Vec<u8>) {
        let stream_offset = self.total_bytes;
        let retained_start = self.total_bytes.saturating_sub(self.data.len());
        let previous_len = self.data.len();
        self.data.extend_from_slice(chunk);
        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        if self.encoding == ProcessOutputEncoding::Unknown {
            self.encoding = detect_process_output_encoding(&self.data);
        }
        let prefix =
            process_output_prefix(&self.data[..previous_len], self.encoding, retained_start);
        trim_process_buffer(
            &mut self.data,
            SESSION_BUFFER_BYTES,
            self.encoding,
            self.total_bytes,
        );
        (stream_offset, prefix)
    }

    fn snapshot(&self) -> ProcessOutputSnapshot {
        ProcessOutputSnapshot {
            data: self.data.clone(),
            total_bytes: self.total_bytes,
            encoding: self.encoding,
        }
    }
}

#[derive(Clone, Debug)]
struct OutputEvent {
    sequence: u64,
    stream: &'static str,
    stream_offset: usize,
    prefix: Vec<u8>,
    data: Vec<u8>,
}

#[derive(Debug)]
struct EventState {
    next_sequence: u64,
    retained_bytes: usize,
    events: VecDeque<OutputEvent>,
}

impl Default for EventState {
    fn default() -> Self {
        Self {
            next_sequence: 1,
            retained_bytes: 0,
            events: VecDeque::new(),
        }
    }
}

struct EventBatch {
    events: Vec<OutputEvent>,
    next_cursor: u64,
    cursor_expired: bool,
    has_more: bool,
}

pub struct ExecSession {
    pub session_id: String,
    pub(crate) child: AsyncMutex<Child>,
    process_id: Option<u32>,
    #[cfg(target_os = "windows")]
    _process_tree_guard: Option<crate::platform::ProcessTreeGuard>,
    process_tree_contained: bool,
    pub stdin: AsyncMutex<Option<ChildStdin>>,
    stdin_open: Mutex<bool>,
    interactive: bool,
    stdout: Mutex<ProcessOutputStream>,
    stderr: Mutex<ProcessOutputStream>,
    events: Mutex<EventState>,
    change_generation: AtomicU64,
    change_tx: watch::Sender<u64>,
    exit_waiter_started: AtomicBool,
    pub started_at: Instant,
    first_output_at: Mutex<Option<Instant>>,
    sensitive_output: AtomicBool,
    pub exit_code: Mutex<Option<i32>>,
    exited: AtomicBool,
    termination_reason: Mutex<Option<String>>,
    reader_tasks: AsyncMutex<Vec<tokio::task::JoinHandle<()>>>,
    post_checks_pending: AtomicBool,
    post_check_result: Mutex<Option<Value>>,
    finalized: AtomicBool,
    finalized_at: Mutex<Option<Instant>>,
    active_slot: Mutex<Option<OwnedSemaphorePermit>>,
    telemetry_profile_id: Option<String>,
    telemetry_command_kind: String,
    started_ts_ms: u64,
    operation_id: Option<String>,
    command_fingerprint: Option<String>,
    resource_lock_group: Option<String>,
    resource_lock_target: Option<String>,
    operation_lock_wait_ms: u128,
    resource_lock_wait_ms: u128,
    attachment_generation: AtomicU64,
    detached_generation: AtomicU64,
}

impl ExecSession {
    fn notify_change(&self) {
        let generation = self.change_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.change_tx.send_replace(generation);
    }

    fn notify_exit(&self) {
        self.notify_change();
    }

    pub fn new(child: Child) -> Self {
        Self::new_with_mode_and_checks(child, false, false)
    }

    pub fn new_with_mode(child: Child, interactive: bool) -> Self {
        Self::new_with_mode_and_checks(child, interactive, false)
    }

    pub fn new_with_mode_and_checks(
        mut child: Child,
        interactive: bool,
        has_post_checks: bool,
    ) -> Self {
        let session_id = Uuid::new_v4().to_string();
        let process_id = child.id();
        #[cfg(target_os = "windows")]
        let process_tree_guard = process_id.and_then(crate::platform::attach_process_tree);
        #[cfg(target_os = "windows")]
        let process_tree_contained = process_tree_guard.is_some();
        #[cfg(not(target_os = "windows"))]
        let process_tree_contained = false;
        let stdin = child.stdin.take();
        let stdin_open = stdin.is_some();
        let (change_tx, _) = watch::channel(0u64);
        Self {
            session_id,
            child: AsyncMutex::new(child),
            process_id,
            #[cfg(target_os = "windows")]
            _process_tree_guard: process_tree_guard,
            process_tree_contained,
            stdin: AsyncMutex::new(stdin),
            stdin_open: Mutex::new(stdin_open),
            interactive,
            stdout: Mutex::new(ProcessOutputStream::default()),
            stderr: Mutex::new(ProcessOutputStream::default()),
            events: Mutex::new(EventState::default()),
            change_generation: AtomicU64::new(0),
            change_tx,
            exit_waiter_started: AtomicBool::new(false),
            started_at: Instant::now(),
            first_output_at: Mutex::new(None),
            sensitive_output: AtomicBool::new(false),
            exit_code: Mutex::new(None),
            exited: AtomicBool::new(false),
            termination_reason: Mutex::new(None),
            reader_tasks: AsyncMutex::new(Vec::new()),
            post_checks_pending: AtomicBool::new(has_post_checks),
            post_check_result: Mutex::new(None),
            finalized: AtomicBool::new(false),
            finalized_at: Mutex::new(None),
            active_slot: Mutex::new(None),
            telemetry_profile_id: None,
            telemetry_command_kind: "process".to_string(),
            started_ts_ms: unix_timestamp_ms(),
            operation_id: None,
            command_fingerprint: None,
            resource_lock_group: None,
            resource_lock_target: None,
            operation_lock_wait_ms: 0,
            resource_lock_wait_ms: 0,
            attachment_generation: AtomicU64::new(1),
            detached_generation: AtomicU64::new(0),
        }
    }

    pub fn with_execution_identity(
        mut self,
        operation_id: Option<String>,
        command_fingerprint: String,
        resource_lock_group: Option<String>,
        resource_lock_target: Option<String>,
        operation_lock_wait_ms: u128,
        resource_lock_wait_ms: u128,
    ) -> Self {
        self.operation_id = operation_id;
        self.command_fingerprint = Some(command_fingerprint);
        self.resource_lock_group = resource_lock_group;
        self.resource_lock_target = resource_lock_target;
        self.operation_lock_wait_ms = operation_lock_wait_ms;
        self.resource_lock_wait_ms = resource_lock_wait_ms;
        self
    }

    pub fn operation_id(&self) -> Option<&str> {
        self.operation_id.as_deref()
    }

    pub fn command_fingerprint(&self) -> Option<&str> {
        self.command_fingerprint.as_deref()
    }

    pub fn touch_attachment(&self) {
        self.attachment_generation.fetch_add(1, Ordering::AcqRel);
        self.detached_generation.store(0, Ordering::Release);
    }

    pub fn mark_detached(&self) -> u64 {
        let generation = self.attachment_generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.detached_generation
            .store(generation, Ordering::Release);
        generation
    }

    pub fn is_still_detached(&self, generation: u64) -> bool {
        generation != 0
            && self.detached_generation.load(Ordering::Acquire) == generation
            && self.attachment_generation.load(Ordering::Acquire) == generation
    }

    pub fn with_active_slot(self, permit: OwnedSemaphorePermit) -> Self {
        *self.active_slot.lock().expect("active slot lock") = Some(permit);
        self
    }

    pub fn with_sensitive_output(self, sensitive: bool) -> Self {
        self.sensitive_output.store(sensitive, Ordering::Release);
        self
    }

    pub fn with_telemetry(mut self, profile_id: &str, command_kind: &str) -> Self {
        self.telemetry_profile_id = Some(profile_id.to_string());
        self.telemetry_command_kind = command_kind.to_string();
        self
    }

    fn has_sensitive_output(&self) -> bool {
        self.sensitive_output.load(Ordering::Acquire)
    }

    pub async fn spawn_readers(self: &Arc<Self>) {
        let stdout = {
            let mut guard = self.child.lock().await;
            guard.stdout.take()
        };
        let stderr = {
            let mut guard = self.child.lock().await;
            guard.stderr.take()
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

    pub async fn kill_and_wait(&self) {
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

    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    pub fn is_finalized(&self) -> bool {
        self.finalized.load(Ordering::Acquire)
    }

    pub fn finalized_within(&self, duration: Duration) -> bool {
        self.finalized_at()
            .is_some_and(|finished| finished.elapsed() <= duration)
    }

    fn finalized_at(&self) -> Option<Instant> {
        *self.finalized_at.lock().expect("finalized_at lock")
    }

    pub fn post_checks_pending(&self) -> bool {
        self.post_checks_pending.load(Ordering::Acquire)
    }

    pub fn mark_termination_reason(&self, reason: &str) {
        *self.termination_reason.lock().expect("termination lock") = Some(reason.to_string());
        self.notify_change();
    }

    pub fn complete_post_checks(&self, result: Value) {
        *self.post_check_result.lock().expect("post check lock") = Some(result);
        self.post_checks_pending.store(false, Ordering::Release);
        self.finish_session();
    }

    pub fn mark_finalized(&self) {
        self.finish_session();
    }

    fn finish_session(&self) {
        if self.finalized.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut finalized_at = self.finalized_at.lock().expect("finalized_at lock");
        if finalized_at.is_none() {
            *finalized_at = Some(Instant::now());
        }
        drop(finalized_at);
        self.active_slot.lock().expect("active slot lock").take();
        self.record_finalization_telemetry();
        self.notify_change();
    }

    fn record_finalization_telemetry(&self) {
        let Some(profile_id) = self.telemetry_profile_id.as_deref() else {
            return;
        };
        let first_output_ms =
            self.first_output_at
                .lock()
                .expect("first output lock")
                .map(|instant| {
                    instant
                        .saturating_duration_since(self.started_at)
                        .as_millis() as u64
                });
        let termination_reason = self
            .termination_reason
            .lock()
            .expect("termination lock")
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        record_async_session_finalized(AsyncSessionTelemetry {
            profile_id,
            session_id: &self.session_id,
            command_kind: &self.telemetry_command_kind,
            started_ts_ms: self.started_ts_ms,
            child_process_total_ms: self.started_at.elapsed().as_millis() as u64,
            first_output_ms,
            exit_code: *self.exit_code.lock().expect("exit_code lock"),
            termination_reason: &termination_reason,
            stdout_bytes: self.stdout.lock().expect("stdout lock").total_bytes,
            stderr_bytes: self.stderr.lock().expect("stderr lock").total_bytes,
        });
    }

    pub(crate) fn mark_stdin_closed(&self) {
        *self.stdin_open.lock().expect("stdin_open lock") = false;
    }

    pub async fn is_running(&self) -> bool {
        self.refresh_status().await;
        !self.has_exited()
    }

    pub fn latest_cursor(&self) -> u64 {
        let state = self.events.lock().expect("events lock");
        state.next_sequence.saturating_sub(1)
    }

    fn event_batch_after(&self, cursor: u64, max_output_bytes: usize) -> EventBatch {
        let state = self.events.lock().expect("events lock");
        let latest_cursor = state.next_sequence.saturating_sub(1);
        let oldest = state.events.front().map(|event| event.sequence);
        let cursor_expired = oldest.is_some_and(|oldest| cursor.saturating_add(1) < oldest);
        let effective_cursor = if cursor_expired {
            oldest.unwrap_or(1).saturating_sub(1)
        } else {
            cursor
        };
        let mut bytes = 0usize;
        let mut events = Vec::new();
        for event in state
            .events
            .iter()
            .filter(|event| event.sequence > effective_cursor)
        {
            if !events.is_empty() && bytes.saturating_add(event.data.len()) > max_output_bytes {
                break;
            }
            bytes = bytes.saturating_add(event.data.len());
            events.push(event.clone());
        }
        let next_cursor = events
            .last()
            .map(|event| event.sequence)
            .unwrap_or(effective_cursor.min(latest_cursor));
        EventBatch {
            has_more: next_cursor < latest_cursor,
            events,
            next_cursor,
            cursor_expired,
        }
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

    pub async fn wait_for_change(&self, cursor: u64, timeout: Duration, until: &str) -> bool {
        let deadline = Instant::now() + timeout;
        let mut changes = self.change_tx.subscribe();
        loop {
            let ready = match until {
                "finalized" => self.is_finalized(),
                "exit" => self.has_exited(),
                _ => {
                    self.has_events_after(cursor)
                        || (cursor == 0 && self.has_output())
                        || self.has_exited()
                }
            };
            if ready {
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

    fn stream_snapshot(&self, stream: &str) -> ProcessOutputSnapshot {
        if self.has_sensitive_output() {
            let data = b"[REDACTED]".to_vec();
            return ProcessOutputSnapshot {
                total_bytes: data.len(),
                data,
                encoding: ProcessOutputEncoding::Unknown,
            };
        }
        match stream {
            "stderr" => self.stderr.lock().expect("stderr lock").snapshot(),
            _ => self.stdout.lock().expect("stdout lock").snapshot(),
        }
    }

    fn stream_encoding(&self, stream: &str) -> ProcessOutputEncoding {
        match stream {
            "stderr" => self.stderr.lock().expect("stderr lock").encoding,
            _ => self.stdout.lock().expect("stdout lock").encoding,
        }
    }

    pub fn retained_stream_bytes(&self, stream: &str) -> (Vec<u8>, usize) {
        let snapshot = self.stream_snapshot(stream);
        (snapshot.data, snapshot.total_bytes)
    }

    pub fn summary(&self) -> Value {
        self.snapshot_with_options(OutputOptions {
            mode: OutputMode::None,
            cursor: self.latest_cursor(),
            max_output_bytes: 1,
            tail_lines: 1,
        })
    }

    pub fn snapshot(&self, max_output_bytes: usize) -> Value {
        self.snapshot_with_options(OutputOptions::tail(max_output_bytes))
    }

    pub fn snapshot_with_options(&self, options: OutputOptions) -> Value {
        // Delta and metadata-only snapshots read from the event queue and do not
        // need to clone the retained stdout/stderr buffers (up to 2 MiB total).
        let retained_streams = matches!(
            options.mode,
            OutputMode::Summary | OutputMode::Tail | OutputMode::All
        )
        .then(|| {
            (
                self.stream_snapshot("stdout"),
                self.stream_snapshot("stderr"),
            )
        });
        let exit_code = *self.exit_code.lock().expect("exit_code lock");
        let termination_reason = self
            .termination_reason
            .lock()
            .expect("termination lock")
            .clone();
        let reason = termination_reason.as_deref().unwrap_or("running");
        let post_checks = self
            .post_check_result
            .lock()
            .expect("post check lock")
            .clone();
        let verification_ok = if self.post_checks_pending() {
            None
        } else {
            post_checks
                .as_ref()
                .and_then(|value| value.get("ok"))
                .and_then(Value::as_bool)
                .or(Some(true))
        };
        let execution_ok = if self.has_exited() {
            Some(reason == "exited" && exit_code == Some(0))
        } else {
            None
        };
        let command_ok = match (execution_ok, verification_ok) {
            (Some(execution), Some(verification)) => Some(execution && verification),
            _ => None,
        };
        let status = if !self.has_exited() {
            "running"
        } else if self.post_checks_pending() {
            "verifying"
        } else {
            match reason {
                "process_timeout" => "timed_out",
                "killed" => "killed",
                _ => "exited",
            }
        };

        let (
            mut stdout,
            mut stderr,
            stdout_truncated,
            stderr_truncated,
            mut events,
            next_cursor,
            cursor_expired,
            has_more,
        ) = match options.mode {
            OutputMode::Delta => {
                let batch = self.event_batch_after(options.cursor, options.max_output_bytes);
                let stdout = batch
                    .events
                    .iter()
                    .filter(|event| event.stream == "stdout")
                    .flat_map(|event| event.data.iter().copied())
                    .collect::<Vec<_>>();
                let stderr = batch
                    .events
                    .iter()
                    .filter(|event| event.stream == "stderr")
                    .flat_map(|event| event.data.iter().copied())
                    .collect::<Vec<_>>();
                let stdout_encoding = self.stream_encoding("stdout");
                let stderr_encoding = self.stream_encoding("stderr");
                let events = batch
                    .events
                    .iter()
                    .map(|event| {
                        let encoding = if event.stream == "stderr" {
                            stderr_encoding
                        } else {
                            stdout_encoding
                        };
                        json!({
                            "sequence": event.sequence,
                            "stream": event.stream,
                            "stream_offset": event.stream_offset,
                            "decoded_offset": event.stream_offset.saturating_sub(event.prefix.len()),
                            "encoding": encoding.as_str(),
                            "data": decode_output_event(event, encoding)
                        })
                    })
                    .collect::<Vec<_>>();
                let stdout_prefix = batch
                    .events
                    .iter()
                    .find(|event| event.stream == "stdout")
                    .map(|event| event.prefix.as_slice())
                    .unwrap_or_default();
                let stderr_prefix = batch
                    .events
                    .iter()
                    .find(|event| event.stream == "stderr")
                    .map(|event| event.prefix.as_slice())
                    .unwrap_or_default();
                let mut stdout_bytes = stdout_prefix.to_vec();
                stdout_bytes.extend_from_slice(&stdout);
                stdout_bytes.truncate(complete_output_boundary(&stdout_bytes, stdout_encoding));
                let mut stderr_bytes = stderr_prefix.to_vec();
                stderr_bytes.extend_from_slice(&stderr);
                stderr_bytes.truncate(complete_output_boundary(&stderr_bytes, stderr_encoding));
                (
                    decode_process_output_with_encoding(&stdout_bytes, stdout_encoding),
                    decode_process_output_with_encoding(&stderr_bytes, stderr_encoding),
                    false,
                    false,
                    events,
                    batch.next_cursor,
                    batch.cursor_expired,
                    batch.has_more,
                )
            }
            OutputMode::None => (
                String::new(),
                String::new(),
                false,
                false,
                Vec::new(),
                self.latest_cursor(),
                false,
                false,
            ),
            OutputMode::Summary => {
                let (stdout_stream, stderr_stream) = retained_streams
                    .as_ref()
                    .expect("summary snapshots retain streams");
                let stdout = summarize_stream(
                    &stdout_stream.data,
                    options.max_output_bytes,
                    options.tail_lines,
                    stdout_stream.encoding,
                );
                let stderr = summarize_stream(
                    &stderr_stream.data,
                    options.max_output_bytes,
                    options.tail_lines,
                    stderr_stream.encoding,
                );
                (
                    stdout.content,
                    stderr.content,
                    stdout.truncated,
                    stderr.truncated,
                    Vec::new(),
                    self.latest_cursor(),
                    false,
                    false,
                )
            }
            OutputMode::Tail | OutputMode::All => {
                let (stdout_stream, stderr_stream) = retained_streams
                    .as_ref()
                    .expect("tail snapshots retain streams");
                let stdout = truncate_tail(
                    &stdout_stream.data,
                    options.max_output_bytes,
                    stdout_stream.encoding,
                );
                let stderr = truncate_tail(
                    &stderr_stream.data,
                    options.max_output_bytes,
                    stderr_stream.encoding,
                );
                (
                    stdout.content,
                    stderr.content,
                    stdout.truncated,
                    stderr.truncated,
                    Vec::new(),
                    self.latest_cursor(),
                    false,
                    false,
                )
            }
        };

        let sensitive_output = self.has_sensitive_output();
        let mut redaction_count = 0u64;
        if sensitive_output {
            if !stdout.is_empty() {
                stdout = "[REDACTED]".into();
                redaction_count += 1;
            }
            if !stderr.is_empty() {
                stderr = "[REDACTED]".into();
                redaction_count += 1;
            }
            for event in &mut events {
                if let Some(data) = event.get_mut("data") {
                    if !data.as_str().unwrap_or_default().is_empty() {
                        *data = Value::String("[REDACTED]".into());
                        redaction_count += 1;
                    }
                }
            }
        }

        let mut payload = json!({
            "session_id": self.session_id,
            "interactive": self.interactive,
            "stdin_open": *self.stdin_open.lock().expect("stdin_open lock"),
            "status": status,
            "termination_reason": reason,
            "recoverable": matches!(reason, "process_timeout" | "killed" | "spawn_failed" | "server_restart" | "detached_timeout"),
            "suggestion": match reason {
                "process_timeout" => "读取保留输出，调整 timeout_ms 后重试",
                "detached_timeout" => "连接失联超过宽限时间；确认没有可恢复 session 后再使用新的 operation_id 重试",
                "killed" => "确认终止原因后重新执行命令",
                "exited" => "检查 process_exit_code、stderr 与 post_checks",
                "crashed" => "检查 stderr 后重试或恢复工作区",
                _ => "使用 wait_command 等待新输出或进程结束",
            },
            "process_exit_code": exit_code,
            "exit_code": exit_code,
            "request_timed_out": false,
            "process_timed_out": reason == "process_timeout",
            "process_still_running": !self.has_exited(),
            "transport_ok": true,
            "execution_ok": execution_ok,
            "verification_ok": verification_ok,
            "command_ok": command_ok,
            "post_checks_pending": self.post_checks_pending(),
            "post_checks": post_checks,
            "output_mode": options.mode.as_str(),
            "cursor": options.cursor,
            "next_cursor": next_cursor,
            "latest_cursor": self.latest_cursor(),
            "cursor_expired": cursor_expired,
            "has_more_output": has_more,
            "events": events,
            "stdout": stdout,
            "stderr": stderr,
            "stdout_truncated": stdout_truncated,
            "stderr_truncated": stderr_truncated,
            "sensitive_data_redacted": sensitive_output,
            "redaction_count": redaction_count,
            "warnings": if sensitive_output {
                vec!["Sensitive process output was withheld because the command referenced a protected credential source."]
            } else {
                Vec::<&str>::new()
            },
            "elapsed_ms": self.started_at.elapsed().as_millis(),
            "first_output_ms": self
                .first_output_at
                .lock()
                .expect("first output lock")
                .map(|first| first.duration_since(self.started_at).as_millis()),
            "output_refs": {
                "stdout": format!("output://{}/stdout", self.session_id),
                "stderr": format!("output://{}/stderr", self.session_id)
            }
        });
        if let Some(object) = payload.as_object_mut() {
            object.insert("process_id".into(), json!(self.process_id));
            object.insert(
                "process_tree_contained".into(),
                Value::Bool(self.process_tree_contained),
            );
            object.insert("operation_id".into(), json!(self.operation_id));
            object.insert(
                "command_fingerprint".into(),
                json!(self.command_fingerprint),
            );
            object.insert(
                "resource_lock_group".into(),
                json!(self.resource_lock_group),
            );
            object.insert(
                "resource_lock_target".into(),
                json!(self.resource_lock_target),
            );
            object.insert(
                "operation_lock_wait_ms".into(),
                json!(self.operation_lock_wait_ms),
            );
            object.insert(
                "resource_lock_wait_ms".into(),
                json!(self.resource_lock_wait_ms),
            );
            object.insert("deduplicated".into(), Value::Bool(false));
            object.insert("attached_to_session_id".into(), Value::Null);
            object.insert(
                "detached".into(),
                Value::Bool(self.detached_generation.load(Ordering::Acquire) != 0),
            );
            object.insert("started_ts_ms".into(), json!(self.started_ts_ms));
        }
        payload
    }
}

async fn terminate_process(pid: u32, force: bool) {
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

fn decode_utf16_unit(pair: &[u8], encoding: ProcessOutputEncoding) -> u16 {
    if encoding == ProcessOutputEncoding::Utf16Le {
        u16::from_le_bytes([pair[0], pair[1]])
    } else {
        u16::from_be_bytes([pair[0], pair[1]])
    }
}

fn is_utf8_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

fn trim_process_buffer(
    buf: &mut Vec<u8>,
    limit: usize,
    encoding: ProcessOutputEncoding,
    total_bytes: usize,
) {
    if buf.len() <= limit {
        return;
    }

    let retained_start = total_bytes.saturating_sub(buf.len());
    let mut drop = buf.len() - limit;
    if encoding.is_utf16() {
        if (retained_start + drop) % 2 != 0 {
            drop = drop.saturating_add(1);
        }
        if drop + 1 < buf.len() {
            let unit = decode_utf16_unit(&buf[drop..drop + 2], encoding);
            if (0xDC00..=0xDFFF).contains(&unit) {
                drop = drop.saturating_add(2);
            }
        }
    } else {
        while drop < buf.len() && is_utf8_continuation(buf[drop]) {
            drop += 1;
        }
    }
    buf.drain(..drop.min(buf.len()));
}

struct Truncated {
    content: String,
    truncated: bool,
}

fn detect_process_output_encoding(bytes: &[u8]) -> ProcessOutputEncoding {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return ProcessOutputEncoding::Utf16Le;
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return ProcessOutputEncoding::Utf16Be;
    }

    let sample = &bytes[..bytes.len().min(4096)];
    let pairs = sample.len() / 2;
    let (even_nuls, odd_nuls) =
        sample
            .chunks_exact(2)
            .fold((0_usize, 0_usize), |(even, odd), pair| {
                (
                    even + usize::from(pair[0] == 0),
                    odd + usize::from(pair[1] == 0),
                )
            });
    if pairs >= 2 && odd_nuls * 3 >= pairs && even_nuls * 10 <= pairs {
        ProcessOutputEncoding::Utf16Le
    } else if pairs >= 2 && even_nuls * 3 >= pairs && odd_nuls * 10 <= pairs {
        ProcessOutputEncoding::Utf16Be
    } else {
        ProcessOutputEncoding::Unknown
    }
}

fn process_output_prefix(
    previous: &[u8],
    encoding: ProcessOutputEncoding,
    absolute_start: usize,
) -> Vec<u8> {
    if previous.is_empty() {
        return Vec::new();
    }
    if encoding.is_utf16() {
        let absolute_end = absolute_start.saturating_add(previous.len());
        let dangling_byte = absolute_end % 2;
        let complete_end = previous.len().saturating_sub(dangling_byte);
        let mut count = dangling_byte;
        if complete_end >= 2 {
            let unit = decode_utf16_unit(&previous[complete_end - 2..complete_end], encoding);
            if (0xD800..=0xDBFF).contains(&unit) {
                count = count.saturating_add(2);
            }
        }
        return previous[previous.len().saturating_sub(count)..].to_vec();
    }

    let start = previous.len().saturating_sub(4);
    let tail = &previous[start..];
    for index in 0..tail.len() {
        if std::str::from_utf8(&tail[index..]).is_ok() {
            return Vec::new();
        }
        if let Err(error) = std::str::from_utf8(&tail[index..]) {
            if error.error_len().is_none() && error.valid_up_to() == 0 {
                return tail[index..].to_vec();
            }
        }
    }
    Vec::new()
}

fn decode_process_output_with_encoding(bytes: &[u8], encoding: ProcessOutputEncoding) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    let encoding = if encoding == ProcessOutputEncoding::Unknown {
        detect_process_output_encoding(bytes)
    } else {
        encoding
    };
    if encoding.is_utf16() {
        let payload = if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
            &bytes[2..]
        } else {
            bytes
        };
        let units = payload
            .chunks_exact(2)
            .map(|pair| {
                if encoding == ProcessOutputEncoding::Utf16Le {
                    u16::from_le_bytes([pair[0], pair[1]])
                } else {
                    u16::from_be_bytes([pair[0], pair[1]])
                }
            })
            .collect::<Vec<_>>();
        String::from_utf16_lossy(&units)
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

fn complete_output_boundary(bytes: &[u8], encoding: ProcessOutputEncoding) -> usize {
    if encoding.is_utf16() {
        let mut end = bytes.len() - (bytes.len() % 2);
        if end >= 2 {
            let unit = decode_utf16_unit(&bytes[end - 2..end], encoding);
            if (0xD800..=0xDBFF).contains(&unit) {
                end -= 2;
            }
        }
        return end;
    }
    match std::str::from_utf8(bytes) {
        Ok(_) => bytes.len(),
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        Err(_) => bytes.len(),
    }
}

fn align_output_start(
    data: &[u8],
    mut offset: usize,
    encoding: ProcessOutputEncoding,
    retained_start: usize,
) -> usize {
    offset = offset.min(data.len());
    if encoding.is_utf16() {
        if (retained_start + offset) % 2 != 0 {
            offset = offset.saturating_sub(1);
        }
        if offset >= 2 && offset + 1 < data.len() {
            let unit = decode_utf16_unit(&data[offset..offset + 2], encoding);
            if (0xDC00..=0xDFFF).contains(&unit) {
                offset -= 2;
            }
        }
    } else {
        while offset > 0 && offset < data.len() && is_utf8_continuation(data[offset]) {
            offset -= 1;
        }
    }
    offset
}

fn bounded_output_end(
    data: &[u8],
    start: usize,
    limit: usize,
    encoding: ProcessOutputEncoding,
) -> usize {
    if start >= data.len() {
        return start.min(data.len());
    }
    let requested_end = data.len().min(start.saturating_add(limit));
    let complete = complete_output_boundary(&data[start..requested_end], encoding);
    if complete > 0 {
        return start + complete;
    }

    let expanded_end = data.len().min(requested_end.saturating_add(4));
    for candidate_end in requested_end.saturating_add(1)..=expanded_end {
        let expanded_complete = complete_output_boundary(&data[start..candidate_end], encoding);
        if expanded_complete > 0 {
            return start + expanded_complete;
        }
    }
    requested_end.max(start + 1).min(data.len())
}

fn decode_process_output(bytes: &[u8]) -> String {
    decode_process_output_with_encoding(bytes, ProcessOutputEncoding::Unknown)
}

fn decode_complete_process_output(bytes: &[u8], encoding: ProcessOutputEncoding) -> String {
    let complete = complete_output_boundary(bytes, encoding);
    decode_process_output_with_encoding(&bytes[..complete], encoding)
}

fn decode_output_event(event: &OutputEvent, encoding: ProcessOutputEncoding) -> String {
    let mut bytes = event.prefix.clone();
    bytes.extend_from_slice(&event.data);
    decode_complete_process_output(&bytes, encoding)
}

fn truncate_decoded_tail(decoded: String, max_bytes: usize) -> Truncated {
    let truncated = decoded.len() > max_bytes;
    let mut start = decoded.len().saturating_sub(max_bytes);
    while start < decoded.len() && !decoded.is_char_boundary(start) {
        start += 1;
    }
    Truncated {
        content: decoded[start..].to_string(),
        truncated,
    }
}

fn truncate_tail(bytes: &[u8], max_bytes: usize, encoding: ProcessOutputEncoding) -> Truncated {
    truncate_decoded_tail(decode_complete_process_output(bytes, encoding), max_bytes)
}

fn summarize_stream(
    bytes: &[u8],
    max_bytes: usize,
    tail_lines: usize,
    encoding: ProcessOutputEncoding,
) -> Truncated {
    let source = decode_complete_process_output(bytes, encoding);
    let mut lines = Vec::<String>::new();
    for line in source
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
    {
        if lines.last().is_some_and(|previous| previous == line) {
            continue;
        }
        lines.push(line.to_string());
    }
    let start = lines.len().saturating_sub(tail_lines);
    let summary = lines[start..].join("\n");
    truncate_decoded_tail(summary, max_bytes)
}

pub fn read_output(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(read_output_async(store, args))
}

pub async fn read_output_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let output_ref = args
        .get("output_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("output_ref is required"))?;
    let Some(rest) = output_ref.strip_prefix("output://") else {
        return Err(WorkspaceError::invalid_argument(
            "output_ref must look like output://<session-id>/stdout or output://<session-id>/stderr",
        ));
    };
    let Some((session_id, ref_stream)) = rest.rsplit_once('/') else {
        return Err(WorkspaceError::invalid_argument(
            "output_ref must include a stream suffix",
        ));
    };
    if ref_stream != "stdout" && ref_stream != "stderr" {
        return Err(WorkspaceError::invalid_argument(
            "output_ref stream must be stdout or stderr",
        ));
    }
    let session = store.get(session_id)?;
    session.touch_attachment();
    session.refresh_status().await;

    let stream = ref_stream;

    let snapshot = session.stream_snapshot(stream);
    let data = snapshot.data;
    let total_stream_bytes = snapshot.total_bytes;
    let encoding = snapshot.encoding;
    let retained_start = total_stream_bytes.saturating_sub(data.len());
    let requested_offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let requested_local_offset = requested_offset
        .max(retained_start)
        .min(total_stream_bytes)
        .saturating_sub(retained_start)
        .min(data.len());
    let local_offset = align_output_start(&data, requested_local_offset, encoding, retained_start);
    let effective_offset = retained_start.saturating_add(local_offset);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(4096)
        .clamp(1, 1_048_576) as usize;
    let local_end = bounded_output_end(&data, local_offset, limit, encoding);
    let chunk = &data[local_offset..local_end];
    let absolute_end = retained_start.saturating_add(local_end);
    let next_offset = (absolute_end < total_stream_bytes).then_some(absolute_end as u64);

    Ok(tool_ok(json!({
        "output_ref": output_ref,
        "stream_output_ref": format!("output://{session_id}/{stream}"),
        "stream": stream,
        "offset": effective_offset,
        "requested_offset": requested_offset,
        "retained_start_offset": retained_start,
        "cursor_expired": requested_offset < retained_start,
        "limit": limit,
        "encoding": encoding.as_str(),
        "content": decode_process_output_with_encoding(chunk, encoding),
        "next_offset": next_offset,
        "total_retained_bytes": data.len(),
        "total_stream_bytes": total_stream_bytes,
        "truncated": next_offset.is_some(),
        "warnings": if requested_offset < retained_start {
            vec!["requested offset expired; response starts at the oldest retained byte"]
        } else if effective_offset != requested_offset {
            vec!["requested offset was aligned to the start of a complete character"]
        } else {
            Vec::<&str>::new()
        }
    })))
}

pub fn resolve_operation(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(resolve_operation_async(store, args))
}

pub async fn resolve_operation_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let operation_id = args.get("operation_id").and_then(Value::as_str);
    let fingerprint = args.get("command_fingerprint").and_then(Value::as_str);
    let (session, resolved_by) = if let Some(operation_id) = operation_id {
        (store.get_by_operation(operation_id), "operation_id")
    } else if let Some(fingerprint) = fingerprint {
        (store.get_by_fingerprint(fingerprint), "command_fingerprint")
    } else {
        return Err(WorkspaceError::invalid_argument(
            "operation_id or command_fingerprint is required",
        ));
    };
    let session = session.ok_or_else(|| WorkspaceError::ToolDetails {
        code: "OPERATION_NOT_FOUND",
        message: "No retained command session matches the requested operation.".into(),
        category: "not_found",
        retryable: false,
        details: json!({
            "operation_id": operation_id,
            "command_fingerprint": fingerprint,
            "retention_seconds": FINALIZED_SESSION_RETENTION.as_secs(),
            "suggestion": "Use list_sessions to inspect retained commands before starting a replacement process."
        }),
    })?;
    session.touch_attachment();
    session.refresh_status().await;
    let mut payload =
        session.snapshot_with_options(OutputOptions::from_args(args, OutputMode::Tail));
    if let Some(object) = payload.as_object_mut() {
        object.insert("resolved_by".into(), json!(resolved_by));
        object.insert("deduplicated".into(), Value::Bool(true));
        object.insert(
            "attached_to_session_id".into(),
            Value::String(session.session_id.clone()),
        );
    }
    Ok(tool_ok(payload))
}

pub fn list_sessions(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let include_finalized = args
        .get("include_finalized")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 1000) as usize;
    let status_filter = args.get("status").and_then(Value::as_str);
    let sessions = store
        .list(include_finalized, limit)
        .into_iter()
        .map(|session| session.summary())
        .filter(|summary| {
            status_filter.map_or(true, |status| {
                summary.get("status").and_then(Value::as_str) == Some(status)
            })
        })
        .collect::<Vec<_>>();
    let count = sessions.len();
    Ok(tool_ok(json!({
        "sessions": sessions,
        "count": count,
        "include_finalized": include_finalized,
        "retention_seconds": FINALIZED_SESSION_RETENTION.as_secs()
    })))
}

pub fn wait_command(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(wait_command_async(store, args))
}

pub async fn wait_command_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let session_id = required_session_id(args)?;
    let (session, session_registry_wait_ms) = store.get_with_metrics(session_id)?;
    session.touch_attachment();
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(30_000)
        .min(120_000);
    let heartbeat_ms = args
        .get("heartbeat_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(30_000);
    let effective_wait_ms = if heartbeat_ms == 0 {
        timeout_ms
    } else {
        timeout_ms.min(heartbeat_ms.max(1000))
    };
    let until = args
        .get("until")
        .and_then(Value::as_str)
        .unwrap_or("output_or_exit");
    let options = OutputOptions::from_args(args, OutputMode::Delta);
    let actual_wait_started = Instant::now();
    let changed = session
        .wait_for_change(
            options.cursor,
            Duration::from_millis(effective_wait_ms),
            until,
        )
        .await;
    let actual_wait_ms = actual_wait_started.elapsed().as_millis();
    let snapshot_started = Instant::now();
    let mut payload = session.snapshot_with_options(options);
    let snapshot_ms = snapshot_started.elapsed().as_millis();
    if let Some(object) = payload.as_object_mut() {
        let process_still_running = object
            .get("process_still_running")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let heartbeat = !changed && heartbeat_ms > 0 && process_still_running;
        let next_cursor = object
            .get("next_cursor")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        object.insert(
            "session_registry_wait_ms".into(),
            json!(session_registry_wait_ms),
        );
        object.insert("actual_wait_ms".into(), json!(actual_wait_ms));
        object.insert("snapshot_ms".into(), json!(snapshot_ms));
        object.insert("heartbeat".into(), Value::Bool(heartbeat));
        object.insert(
            "request_timed_out".into(),
            Value::Bool(!changed && !heartbeat),
        );
        object.insert("wait_timeout_ms".into(), json!(timeout_ms));
        object.insert("effective_wait_ms".into(), json!(effective_wait_ms));
        object.insert("heartbeat_ms".into(), json!(heartbeat_ms));
        object.insert("wait_until".into(), json!(until));
        if process_still_running {
            object.insert(
                "next_actions".into(),
                json!([{
                    "tool": "wait_command",
                    "arguments": {
                        "session_id": session_id,
                        "cursor": next_cursor,
                        "timeout_ms": timeout_ms,
                        "heartbeat_ms": if heartbeat_ms == 0 { 10_000 } else { heartbeat_ms },
                        "until": until,
                        "output_mode": "delta"
                    }
                }]),
            );
        }
        object.insert(
            "suggestion".into(),
            json!(if heartbeat {
                "命令仍在运行；沿用 next_actions 可保持连接活跃且不会重复启动命令"
            } else if !changed {
                "本次等待没有新事件；沿用 next_actions 继续既有 session，不要重新调用 exec_command"
            } else if process_still_running {
                "已收到增量输出；沿用 next_actions 继续既有 session"
            } else {
                "进程已结束；检查 process_exit_code 与 post_checks"
            }),
        );
    }
    Ok(tool_ok(payload))
}

pub fn send_input(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(send_input_async(store, args))
}

pub async fn send_input_async(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    let session_id = required_session_id(args)?;
    let session = store.get(session_id)?;
    session.touch_attachment();
    let chars = args.get("chars").and_then(Value::as_str).unwrap_or("");
    let close_stdin = args
        .get("close_stdin")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !session.is_running().await {
        return Err(WorkspaceError::Tool {
            code: "SESSION_CLOSED",
            message: "Session is closed; stdin write blocked.".into(),
            category: "runtime",
            retryable: false,
        });
    }

    let bytes_written = async {
        let mut stdin_guard = session.stdin.lock().await;
        let stdin = stdin_guard.as_mut().ok_or_else(|| WorkspaceError::Tool {
            code: "SESSION_CLOSED",
            message: "Session stdin is closed.".into(),
            category: "runtime",
            retryable: false,
        })?;
        if !chars.is_empty() {
            stdin
                .write_all(chars.as_bytes())
                .await
                .map_err(|_| WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: "Session stdin is closed.".into(),
                    category: "runtime",
                    retryable: false,
                })?;
            stdin.flush().await.map_err(|_| WorkspaceError::Tool {
                code: "SESSION_CLOSED",
                message: "Session stdin is closed.".into(),
                category: "runtime",
                retryable: false,
            })?;
        }
        if close_stdin {
            let _ = stdin.shutdown().await;
            *stdin_guard = None;
            session.mark_stdin_closed();
        }
        Ok::<usize, WorkspaceError>(chars.len())
    }
    .await?;

    let mut payload = session.snapshot_with_options(OutputOptions {
        mode: OutputMode::None,
        cursor: session.latest_cursor(),
        max_output_bytes: 1,
        tail_lines: 1,
    });
    if let Some(object) = payload.as_object_mut() {
        object.insert("bytes_written".into(), json!(bytes_written));
        object.insert("stdin_closed".into(), json!(close_stdin));
        object.insert(
            "suggestion".into(),
            json!("输入已发送；使用 wait_command 获取后续输出"),
        );
    }
    Ok(tool_ok(payload))
}

pub fn kill_session(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(kill_session_async(store, args))
}

pub async fn kill_session_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let session_id = required_session_id(args)?;
    let session = store.get(session_id)?;
    session.touch_attachment();
    let options = OutputOptions::from_args(args, OutputMode::Tail);
    let wait_ms = args
        .get("wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5000)
        .min(30_000);
    let signal = args.get("signal").and_then(Value::as_str).unwrap_or("TERM");

    let running = session.is_running().await;
    let mut killed = false;
    let mut status = "exited";
    let mut evicted = true;

    if running {
        session.mark_termination_reason("killed");
        if session.exit_waiter_started.load(Ordering::Acquire) {
            if let Some(pid) = session.process_id {
                send_session_signal(pid, signal).await;
            }
            let _ =
                tokio::time::timeout(Duration::from_millis(wait_ms), session.wait_until_exited())
                    .await;
        } else {
            session.kill_and_wait().await;
        }
        if session.is_running().await {
            status = "terminating";
            evicted = false;
        } else {
            killed = true;
            status = "killed";
        }
    }

    let mut payload = session.snapshot_with_options(options);
    if let Some(object) = payload.as_object_mut() {
        object.insert("killed".into(), json!(killed));
        object.insert("status".into(), json!(status));
        object.insert("evicted".into(), json!(evicted));
        if status == "terminating" {
            object.insert(
                "warnings".into(),
                json!(["Process did not exit after kill; session retained for retry"]),
            );
        }
    }

    if evicted {
        store.remove(session_id);
    }

    Ok(tool_ok(payload))
}

fn required_session_id(args: &Value) -> Result<&str, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    if let Some(rest) = session_id.strip_prefix("output://") {
        let corrected = rest.rsplit_once('/').map(|(id, _)| id).unwrap_or(rest);
        return Err(WorkspaceError::ToolDetails {
            code: "OUTPUT_REF_USED_AS_SESSION_ID",
            message: "An output_ref was supplied where a session_id is required.".into(),
            category: "validation",
            retryable: true,
            details: json!({
                "received": session_id,
                "corrected_session_id": corrected,
                "suggestion": "Use the top-level session_id for wait_command, send_input, or kill_session; use output_ref only with read_output."
            }),
        });
    }
    Ok(session_id)
}

#[cfg(unix)]
async fn send_session_signal(pid: u32, signal: &str) {
    let sig = match signal {
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        _ => libc::SIGTERM,
    };
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

#[cfg(windows)]
async fn send_session_signal(pid: u32, _signal: &str) {
    terminate_process(pid, true).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn active_session_admission_is_bounded_and_recoverable() {
        const TEST_LIMIT: usize = 4;
        let store = SessionStore::with_active_session_limit(TEST_LIMIT);
        let mut permits = Vec::new();
        for _ in 0..TEST_LIMIT {
            permits.push(
                store
                    .acquire_active_slot()
                    .await
                    .expect("active session permit"),
            );
        }
        assert_eq!(store.active_slots_available(), 0);

        let started = Instant::now();
        let error = store
            .acquire_active_slot()
            .await
            .expect_err("session limit should reject overload");
        assert!(started.elapsed() >= Duration::from_millis(900));
        assert!(matches!(
            error,
            WorkspaceError::ToolDetails {
                code: "SESSION_LIMIT_REACHED",
                ..
            }
        ));

        permits.pop();
        let recovered = store
            .acquire_active_slot()
            .await
            .expect("capacity should recover after permit release");
        assert_eq!(store.active_slots_available(), 0);
        drop(recovered);
        drop(permits);
        assert_eq!(store.active_slots_available(), TEST_LIMIT);
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn finalizing_a_session_releases_its_active_slot() {
        let store = SessionStore::new();
        let permit = store
            .acquire_active_slot()
            .await
            .expect("active session permit");

        #[cfg(windows)]
        let child = tokio::process::Command::new("cmd")
            .args(["/d", "/c", "exit", "0"])
            .spawn()
            .expect("spawn test child");
        #[cfg(unix)]
        let child = tokio::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn test child");

        let session = store.insert(ExecSession::new(child).with_active_slot(permit));
        assert_eq!(
            store.active_slots_available(),
            DEFAULT_ACTIVE_SESSION_LIMIT - 1
        );
        session.kill_and_wait().await;
        session.mark_finalized();
        assert_eq!(store.active_slots_available(), DEFAULT_ACTIVE_SESSION_LIMIT);
        assert!(session.finalized_at().is_some());
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn event_waiter_observes_exit_and_uses_output_uri_refs() {
        #[cfg(windows)]
        let child = tokio::process::Command::new("cmd")
            .args(["/d", "/c", "exit", "0"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn test child");
        #[cfg(unix)]
        let child = tokio::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn test child");

        let session = Arc::new(ExecSession::new(child));
        session.spawn_readers().await;
        session.spawn_exit_waiter();
        tokio::time::timeout(Duration::from_secs(5), session.wait_until_exited())
            .await
            .expect("event waiter timeout");
        assert!(session.has_exited());

        let snapshot = session.snapshot_with_options(OutputOptions::tail(4096));
        assert!(snapshot["output_refs"]["stdout"]
            .as_str()
            .expect("stdout ref")
            .starts_with("output://"));
        assert!(snapshot["output_refs"]["stderr"]
            .as_str()
            .expect("stderr ref")
            .ends_with("/stderr"));
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn sensitive_sessions_redact_snapshots_and_retained_output() {
        #[cfg(windows)]
        let child = tokio::process::Command::new("cmd")
            .args(["/d", "/c", "echo bare-secret-value"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn test child");
        #[cfg(unix)]
        let child = tokio::process::Command::new("sh")
            .args(["-c", "printf bare-secret-value"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("spawn test child");

        let session = Arc::new(ExecSession::new(child).with_sensitive_output(true));
        session.spawn_readers().await;
        session.spawn_exit_waiter();
        tokio::time::timeout(Duration::from_secs(5), session.wait_until_exited())
            .await
            .expect("sensitive session timeout");
        session.wait_for_readers().await;

        let snapshot = session.snapshot(4096);
        assert_eq!(snapshot["stdout"], "[REDACTED]", "{snapshot}");
        assert_eq!(snapshot["sensitive_data_redacted"], true, "{snapshot}");
        assert!(!snapshot.to_string().contains("bare-secret-value"));

        let (retained, _) = session.retained_stream_bytes("stdout");
        assert_eq!(retained, b"[REDACTED]");
    }

    #[test]
    fn process_output_decoder_preserves_utf8() {
        assert_eq!(
            decode_process_output("WSL UTF-8 測試 ✓".as_bytes()),
            "WSL UTF-8 測試 ✓"
        );
    }

    #[test]
    fn process_output_decoder_handles_utf16le_with_and_without_bom() {
        let text = "預設發行版本: Ubuntu-24.04\r\n";
        let encoded = text
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let mut with_bom = vec![0xFF, 0xFE];
        with_bom.extend_from_slice(&encoded);

        assert_eq!(decode_process_output(&with_bom), text);
        assert_eq!(decode_process_output(&encoded), text);
        assert_eq!(
            truncate_tail(&with_bom, 4096, ProcessOutputEncoding::Utf16Le).content,
            text
        );
        assert_eq!(
            summarize_stream(&with_bom, 4096, 10, ProcessOutputEncoding::Utf16Le,).content,
            text.trim_end()
        );
    }

    #[test]
    fn process_output_decoder_reconstructs_split_utf8_character() {
        let text = "前綴✓後綴";
        let bytes = text.as_bytes();
        let split = bytes
            .windows(3)
            .position(|window| window == "✓".as_bytes())
            .expect("check mark bytes")
            + 1;
        let prefix = process_output_prefix(&bytes[..split], ProcessOutputEncoding::Unknown, 0);
        let mut second = prefix;
        second.extend_from_slice(&bytes[split..]);

        assert_eq!(
            decode_process_output_with_encoding(&second, ProcessOutputEncoding::Unknown),
            "✓後綴"
        );
        assert_eq!(
            complete_output_boundary(&bytes[..split], ProcessOutputEncoding::Unknown),
            "前綴".len()
        );
    }

    #[test]
    fn process_output_decoder_reconstructs_split_utf16_surrogate_pair() {
        let text = "A😀B";
        let encoded = text
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let split = 4;
        let prefix = process_output_prefix(&encoded[..split], ProcessOutputEncoding::Utf16Le, 0);
        let mut second = prefix;
        second.extend_from_slice(&encoded[split..]);

        assert_eq!(
            decode_process_output_with_encoding(&second, ProcessOutputEncoding::Utf16Le),
            "😀B"
        );
        assert_eq!(
            complete_output_boundary(&encoded[..5], ProcessOutputEncoding::Utf16Le),
            2
        );
    }

    #[test]
    fn process_output_events_defer_partial_characters() {
        let bytes = "✓".as_bytes();
        let first = OutputEvent {
            sequence: 1,
            stream: "stdout",
            stream_offset: 0,
            prefix: Vec::new(),
            data: bytes[..1].to_vec(),
        };
        let second = OutputEvent {
            sequence: 2,
            stream: "stdout",
            stream_offset: 1,
            prefix: process_output_prefix(&bytes[..1], ProcessOutputEncoding::Unknown, 0),
            data: bytes[1..].to_vec(),
        };

        assert_eq!(
            decode_output_event(&first, ProcessOutputEncoding::Unknown),
            ""
        );
        assert_eq!(
            truncate_tail(&bytes[..1], 4096, ProcessOutputEncoding::Unknown).content,
            ""
        );
        assert_eq!(
            summarize_stream(&bytes[..1], 4096, 10, ProcessOutputEncoding::Unknown,).content,
            ""
        );
        assert_eq!(
            decode_output_event(&second, ProcessOutputEncoding::Unknown),
            "✓"
        );
    }

    #[test]
    fn process_output_stream_recovers_split_utf16_bom() {
        let mut stream = ProcessOutputStream::default();
        let (_, first_prefix) = stream.append(&[0xFF]);
        let (second_offset, second_prefix) = stream.append(&[0xFE, b'A', 0]);
        let second = OutputEvent {
            sequence: 2,
            stream: "stdout",
            stream_offset: second_offset,
            prefix: second_prefix,
            data: vec![0xFE, b'A', 0],
        };

        assert!(first_prefix.is_empty());
        assert_eq!(stream.encoding, ProcessOutputEncoding::Utf16Le);
        assert_eq!(decode_output_event(&second, stream.encoding), "A");
    }

    #[test]
    fn retained_output_trimming_preserves_character_boundaries() {
        let mut utf8 = "A✓B".as_bytes().to_vec();
        let utf8_total = utf8.len();
        trim_process_buffer(&mut utf8, 4, ProcessOutputEncoding::Unknown, utf8_total);
        assert_eq!(decode_process_output(&utf8), "✓B");

        let mut utf16 = "A😀B"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        let utf16_total = utf16.len();
        trim_process_buffer(&mut utf16, 4, ProcessOutputEncoding::Utf16Le, utf16_total);
        assert_eq!(
            decode_process_output_with_encoding(&utf16, ProcessOutputEncoding::Utf16Le),
            "B"
        );
    }

    #[test]
    fn output_pagination_advances_past_small_multibyte_limits() {
        let utf8 = "✓B".as_bytes();
        assert_eq!(
            bounded_output_end(utf8, 0, 1, ProcessOutputEncoding::Unknown),
            "✓".len()
        );
        assert_eq!(
            align_output_start(utf8, 1, ProcessOutputEncoding::Unknown, 0),
            0
        );

        let utf16 = "😀B"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            bounded_output_end(&utf16, 0, 2, ProcessOutputEncoding::Utf16Le),
            4
        );
        assert_eq!(
            align_output_start(&utf16, 2, ProcessOutputEncoding::Utf16Le, 0),
            0
        );
    }

    #[test]
    fn output_ref_is_rejected_as_a_session_id_with_a_correction() {
        let error = required_session_id(&json!({
            "session_id": "output://abc-123/stdout"
        }))
        .expect_err("output ref must not be accepted as session id");
        let value = error.to_error_value();
        assert_eq!(value["code"], "OUTPUT_REF_USED_AS_SESSION_ID");
        assert_eq!(value["details"]["corrected_session_id"], "abc-123");
    }
}
