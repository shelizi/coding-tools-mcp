use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::process::{Child, Command};

#[cfg(windows)]
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

#[cfg(windows)]
const STARTUP_SLOTS: usize = 4;
#[cfg(windows)]
const START_INTERVAL: Duration = Duration::from_millis(75);
#[cfg(windows)]
pub(crate) const STARTUP_PROBE_WINDOW: Duration = Duration::from_millis(125);
#[cfg(windows)]
const FAILURE_WINDOW: Duration = Duration::from_secs(10);
#[cfg(windows)]
const CIRCUIT_BREAKER_THRESHOLD: usize = 3;
#[cfg(windows)]
const CIRCUIT_BREAKER_DELAY: Duration = Duration::from_secs(3);
#[cfg(windows)]
const RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(250),
    Duration::from_millis(750),
    Duration::from_millis(1_500),
];
#[cfg(windows)]
pub(crate) const STATUS_DLL_INIT_FAILED: i32 = 0xC000_0142_u32 as i32;

#[derive(Clone, Debug, Default)]
pub(crate) struct StartupDiagnostics {
    pub attempts: usize,
    pub gate_wait_ms: u128,
    pub retry_delays_ms: Vec<u64>,
    pub error_dialog_suppressed: bool,
}

impl StartupDiagnostics {
    pub(crate) fn absorb(&mut self, attempt: &Self) {
        self.attempts += attempt.attempts;
        self.gate_wait_ms += attempt.gate_wait_ms;
        self.retry_delays_ms
            .extend_from_slice(&attempt.retry_delays_ms);
        self.error_dialog_suppressed |= attempt.error_dialog_suppressed;
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "attempts": self.attempts,
            "retry_count": self.attempts.saturating_sub(1),
            "gate_wait_ms": self.gate_wait_ms,
            "retry_delays_ms": self.retry_delays_ms,
            "error_dialog_suppressed": self.error_dialog_suppressed,
            "startup_slots": if cfg!(windows) { 4 } else { 0 },
            "start_interval_ms": if cfg!(windows) { 75 } else { 0 }
        })
    }
}

#[derive(Default)]
pub(crate) struct StartupSlotGuard {
    #[cfg(windows)]
    _permit: Option<OwnedSemaphorePermit>,
}

pub(crate) struct StartedChild {
    pub child: Child,
    pub diagnostics: StartupDiagnostics,
    pub startup_guard: StartupSlotGuard,
}

pub(crate) struct StartupPermission {
    diagnostics: StartupDiagnostics,
    startup_guard: StartupSlotGuard,
}

#[derive(Debug)]
pub(crate) enum ProcessStartError {
    Spawn(io::Error),
    LoaderInitialization {
        exit_code: i32,
        diagnostics: StartupDiagnostics,
    },
}

impl std::fmt::Display for ProcessStartError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(formatter, "{error}"),
            Self::LoaderInitialization { .. } => write!(
                formatter,
                "Windows could not initialize the child process (0xc0000142) after retries"
            ),
        }
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct StartupState {
    next_start: Instant,
    loader_failures: VecDeque<Instant>,
    circuit_open_until: Option<Instant>,
}

#[cfg(windows)]
impl StartupState {
    fn new(now: Instant) -> Self {
        Self {
            next_start: now,
            loader_failures: VecDeque::new(),
            circuit_open_until: None,
        }
    }

    fn prune_failures(&mut self, now: Instant) {
        while self
            .loader_failures
            .front()
            .is_some_and(|failure| now.duration_since(*failure) > FAILURE_WINDOW)
        {
            self.loader_failures.pop_front();
        }
    }

    fn ready_at(&mut self, now: Instant) -> Instant {
        self.prune_failures(now);
        let circuit_ready = self.circuit_open_until.unwrap_or(now);
        self.next_start.max(circuit_ready)
    }

    fn reserve_start(&mut self, now: Instant) {
        self.next_start = now + START_INTERVAL;
        if self.circuit_open_until.is_some_and(|until| until <= now) {
            self.circuit_open_until = None;
        }
    }

    fn record_loader_failure(&mut self, now: Instant) {
        self.prune_failures(now);
        self.loader_failures.push_back(now);
        if self.loader_failures.len() >= CIRCUIT_BREAKER_THRESHOLD {
            let open_until = now + CIRCUIT_BREAKER_DELAY;
            self.circuit_open_until = Some(
                self.circuit_open_until
                    .map_or(open_until, |current| current.max(open_until)),
            );
        }
    }
}

#[cfg(windows)]
struct ProcessStartController {
    slots: Arc<Semaphore>,
    state: Mutex<StartupState>,
    jitter_sequence: AtomicU64,
}

#[cfg(windows)]
impl ProcessStartController {
    fn new() -> Self {
        Self {
            slots: Arc::new(Semaphore::new(STARTUP_SLOTS)),
            state: Mutex::new(StartupState::new(Instant::now())),
            jitter_sequence: AtomicU64::new(0),
        }
    }

    async fn acquire_start_slot(&self) -> (OwnedSemaphorePermit, Duration) {
        let wait_started = Instant::now();
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .expect("process startup semaphore closed");
        loop {
            let delay = {
                let now = Instant::now();
                let mut state = self.state.lock().await;
                let ready_at = state.ready_at(now);
                if ready_at <= now {
                    state.reserve_start(now);
                    None
                } else {
                    Some(ready_at.duration_since(now))
                }
            };
            match delay {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return (permit, wait_started.elapsed()),
            }
        }
    }

    async fn record_loader_failure(&self) {
        self.state
            .lock()
            .await
            .record_loader_failure(Instant::now());
    }

    fn retry_delay(&self, retry_index: usize) -> Duration {
        let base = RETRY_DELAYS[retry_index];
        let sequence = self.jitter_sequence.fetch_add(1, Ordering::Relaxed);
        base + Duration::from_millis(sequence.wrapping_mul(17) % 51)
    }
}

#[cfg(windows)]
fn controller() -> &'static ProcessStartController {
    static CONTROLLER: OnceLock<ProcessStartController> = OnceLock::new();
    CONTROLLER.get_or_init(ProcessStartController::new)
}

#[cfg(windows)]
fn suppress_child_error_dialogs() {
    static CONFIGURED: OnceLock<()> = OnceLock::new();
    CONFIGURED.get_or_init(|| unsafe {
        use windows::Win32::System::Diagnostics::Debug::{
            GetErrorMode, SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
            THREAD_ERROR_MODE,
        };

        let current = GetErrorMode();
        let desired =
            THREAD_ERROR_MODE(current | SEM_FAILCRITICALERRORS.0 | SEM_NOGPFAULTERRORBOX.0);
        SetErrorMode(desired);
    });
}

#[cfg(windows)]
pub(crate) fn is_loader_initialization_failure(exit_code: Option<i32>) -> bool {
    exit_code == Some(STATUS_DLL_INIT_FAILED)
}

pub(crate) async fn acquire_start_permission() -> StartupPermission {
    #[cfg(not(windows))]
    {
        return StartupPermission {
            diagnostics: StartupDiagnostics {
                attempts: 1,
                ..StartupDiagnostics::default()
            },
            startup_guard: StartupSlotGuard::default(),
        };
    }

    #[cfg(windows)]
    {
        suppress_child_error_dialogs();
        let (permit, gate_wait) = controller().acquire_start_slot().await;
        StartupPermission {
            diagnostics: StartupDiagnostics {
                attempts: 1,
                gate_wait_ms: gate_wait.as_millis(),
                retry_delays_ms: Vec::new(),
                error_dialog_suppressed: true,
            },
            startup_guard: StartupSlotGuard {
                _permit: Some(permit),
            },
        }
    }
}

pub(crate) fn spawn_with_permission<F>(
    permission: StartupPermission,
    mut build: F,
) -> Result<StartedChild, ProcessStartError>
where
    F: FnMut() -> Command,
{
    let child = build().spawn().map_err(ProcessStartError::Spawn)?;
    Ok(StartedChild {
        child,
        diagnostics: permission.diagnostics,
        startup_guard: permission.startup_guard,
    })
}

pub(crate) async fn spawn_once_with_control<F>(build: F) -> Result<StartedChild, ProcessStartError>
where
    F: FnMut() -> Command,
{
    let permission = acquire_start_permission().await;
    spawn_with_permission(permission, build)
}

pub(crate) async fn loader_failure_retry_delay(retry_index: usize) -> Option<Duration> {
    #[cfg(not(windows))]
    {
        let _ = retry_index;
        None
    }

    #[cfg(windows)]
    {
        let controller = controller();
        controller.record_loader_failure().await;
        (retry_index < RETRY_DELAYS.len()).then(|| controller.retry_delay(retry_index))
    }
}

pub(crate) async fn spawn_with_control<F>(mut build: F) -> Result<StartedChild, ProcessStartError>
where
    F: FnMut() -> Command,
{
    #[cfg(not(windows))]
    {
        let child = build().spawn().map_err(ProcessStartError::Spawn)?;
        return Ok(StartedChild {
            child,
            diagnostics: StartupDiagnostics {
                attempts: 1,
                ..StartupDiagnostics::default()
            },
            startup_guard: StartupSlotGuard::default(),
        });
    }

    #[cfg(windows)]
    {
        let mut diagnostics = StartupDiagnostics::default();

        loop {
            let started = spawn_once_with_control(&mut build).await?;
            diagnostics.absorb(&started.diagnostics);
            let mut child = started.child;
            let startup_guard = started.startup_guard;

            tokio::time::sleep(STARTUP_PROBE_WINDOW).await;
            let loader_failed = child
                .try_wait()
                .ok()
                .flatten()
                .is_some_and(|status| is_loader_initialization_failure(status.code()));
            drop(startup_guard);

            if !loader_failed {
                return Ok(StartedChild {
                    child,
                    diagnostics,
                    startup_guard: StartupSlotGuard::default(),
                });
            }

            let retry_index = diagnostics.attempts - 1;
            let Some(delay) = loader_failure_retry_delay(retry_index).await else {
                return Err(ProcessStartError::LoaderInitialization {
                    exit_code: STATUS_DLL_INIT_FAILED,
                    diagnostics,
                });
            };

            diagnostics.retry_delays_ms.push(delay.as_millis() as u64);
            eprintln!(
                "child process loader initialization failed (0xc0000142); retrying in {} ms",
                delay.as_millis()
            );
            drop(child);
            tokio::time::sleep(delay).await;
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn recognizes_signed_dll_initialization_status() {
        assert!(is_loader_initialization_failure(Some(
            0xC000_0142_u32 as i32
        )));
        assert!(!is_loader_initialization_failure(Some(1)));
        assert!(!is_loader_initialization_failure(None));
    }

    #[test]
    fn circuit_breaker_opens_after_three_recent_failures() {
        let now = Instant::now();
        let mut state = StartupState::new(now);
        state.record_loader_failure(now);
        state.record_loader_failure(now + Duration::from_millis(1));
        assert!(state.circuit_open_until.is_none());
        state.record_loader_failure(now + Duration::from_millis(2));
        assert!(state
            .circuit_open_until
            .is_some_and(|until| until >= now + CIRCUIT_BREAKER_DELAY));
    }

    #[test]
    fn old_failures_do_not_keep_circuit_open() {
        let now = Instant::now();
        let mut state = StartupState::new(now);
        state.record_loader_failure(now);
        state.record_loader_failure(now + Duration::from_millis(1));
        state.record_loader_failure(now + FAILURE_WINDOW + Duration::from_millis(2));
        assert_eq!(state.loader_failures.len(), 1);
    }

    #[test]
    fn process_error_mode_suppresses_child_failure_dialogs() {
        use windows::Win32::System::Diagnostics::Debug::{
            GetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX,
        };

        suppress_child_error_dialogs();
        let mode = unsafe { GetErrorMode() };
        assert_ne!(mode & SEM_FAILCRITICALERRORS.0, 0);
        assert_ne!(mode & SEM_NOGPFAULTERRORBOX.0, 0);
    }
}
