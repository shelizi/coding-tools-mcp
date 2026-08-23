use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::json;
use uuid::Uuid;

use crate::tools::workspace::{WorkspaceError, WorkspaceResult};

use super::{BACKEND_ID, MAX_SESSION_MOUNTS};

static SESSION_CACHE: OnceLock<Mutex<HashMap<PathBuf, Weak<WslcSessionCoordinator>>>> =
    OnceLock::new();

struct WslcSessionLease {
    name: String,
    shutdown: Option<mpsc::Sender<()>>,
    owner: Option<JoinHandle<()>>,
}

impl WslcSessionLease {
    fn name(&self) -> &str {
        &self.name
    }

    fn start(cli: PathBuf, storage: PathBuf) -> WorkspaceResult<Self> {
        let parent = storage.parent().ok_or_else(|| {
            session_error(
                "SANDBOX_WSLC_SESSION_PREPARE_FAILED",
                "WSLC session storage path has no parent directory.",
                "session_storage",
            )
        })?;
        std::fs::create_dir_all(parent).map_err(|error| {
            session_error(
                "SANDBOX_WSLC_SESSION_PREPARE_FAILED",
                format!(
                    "Failed to create WSLC session storage parent {}: {error}",
                    parent.display()
                ),
                "session_storage",
            )
        })?;

        let name = format!("ctmcp-wslc-session-{}", Uuid::new_v4().simple());
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (shutdown_tx, shutdown_rx) = mpsc::channel();
        let owner_cli = cli.clone();
        let owner_name = name.clone();
        let owner_storage = storage.clone();
        let owner = thread::Builder::new()
            .name("ctmcp-wslc-session".into())
            .spawn(move || {
                session_owner_thread(owner_cli, owner_name, owner_storage, ready_tx, shutdown_rx);
            })
            .map_err(|error| {
                session_error(
                    "SANDBOX_WSLC_SESSION_PREPARE_FAILED",
                    format!("Failed to start WSLC session owner thread: {error}"),
                    "session_start",
                )
            })?;

        match ready_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(())) => Ok(Self {
                name,
                shutdown: Some(shutdown_tx),
                owner: Some(owner),
            }),
            Ok(Err(message)) => {
                let _ = owner.join();
                Err(session_error(
                    "SANDBOX_WSLC_SESSION_PREPARE_FAILED",
                    message,
                    "session_start",
                ))
            }
            Err(error) => {
                let _ = shutdown_tx.send(());
                let _ = owner.join();
                Err(session_error(
                    "SANDBOX_WSLC_SESSION_PREPARE_FAILED",
                    format!("Timed out waiting for WSLC session startup: {error}"),
                    "session_start",
                ))
            }
        }
    }

    fn shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(owner) = self.owner.take() {
            let _ = owner.join();
        }
    }
}

impl Drop for WslcSessionLease {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct SessionState {
    session: WslcSessionLease,
    used_mounts: usize,
    pending_launches: usize,
}

pub(super) struct WslcSessionCoordinator {
    cli: PathBuf,
    storage: PathBuf,
    state: Mutex<SessionState>,
    idle: Condvar,
}

impl WslcSessionCoordinator {
    fn start(cli: PathBuf, storage: PathBuf) -> WorkspaceResult<Arc<Self>> {
        let session = WslcSessionLease::start(cli.clone(), storage.clone())?;
        Ok(Arc::new(Self {
            cli,
            storage,
            state: Mutex::new(SessionState {
                session,
                used_mounts: 0,
                pending_launches: 0,
            }),
            idle: Condvar::new(),
        }))
    }

    pub(super) fn run(&self, args: &[&str]) -> WorkspaceResult<Output> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Command::new(&self.cli)
            .arg("--session")
            .arg(state.session.name())
            .args(args)
            .output()
            .map_err(|error| {
                session_error(
                    "SANDBOX_WSLC_UNAVAILABLE",
                    format!("Failed to execute wslc CLI: {error}"),
                    "cli",
                )
            })
    }

    pub(super) fn reserve_mounts(
        self: &Arc<Self>,
        mount_count: usize,
    ) -> WorkspaceResult<WslcSessionReservation> {
        if mount_count == 0 || mount_count > MAX_SESSION_MOUNTS {
            return Err(session_error(
                "SANDBOX_WSLC_MOUNT_LIMIT",
                format!(
                    "WSLC command requires {mount_count} mounts, but this WSLC build supports 1-{MAX_SESSION_MOUNTS} mounts per session generation."
                ),
                "mount_budget",
            ));
        }

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        while state.used_mounts.saturating_add(mount_count) > MAX_SESSION_MOUNTS {
            while state.pending_launches > 0 {
                state = self
                    .idle
                    .wait(state)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
            }

            // A retained ExecSession may outlive its actual container. Query the
            // named WSLC session itself rather than tying rotation to the Rust child
            // wrapper lifetime. This also protects concurrently running containers.
            let mut busy = true;
            for _ in 0..600 {
                let output = Command::new(&self.cli)
                    .args(["--session", state.session.name(), "list", "-q"])
                    .output()
                    .map_err(|error| {
                        session_error(
                            "SANDBOX_WSLC_UNAVAILABLE",
                            format!("Failed to inspect WSLC session containers: {error}"),
                            "mount_budget",
                        )
                    })?;
                if output.status.success() && output.stdout.iter().all(u8::is_ascii_whitespace) {
                    busy = false;
                    break;
                }
                thread::sleep(Duration::from_millis(50));
            }
            if busy {
                return Err(session_error(
                    "SANDBOX_WSLC_SESSION_BUSY",
                    "WSLC session mount quota is exhausted while containers are still active.",
                    "mount_budget",
                ));
            }

            // WSLC 2.9.x keeps volume attachments charged to a session after an
            // ephemeral container is removed. Reopening the same managed storage
            // resets that preview quota while preserving its image store. Never
            // terminate or rotate the process-global default WSLC session.
            state.session.shutdown();
            state.session = WslcSessionLease::start(self.cli.clone(), self.storage.clone())?;
            state.used_mounts = 0;
        }

        state.used_mounts += mount_count;
        state.pending_launches += 1;
        let name = state.session.name().to_string();
        drop(state);
        Ok(WslcSessionReservation {
            coordinator: Arc::clone(self),
            name,
            released: false,
        })
    }
}

pub(super) struct WslcSessionReservation {
    coordinator: Arc<WslcSessionCoordinator>,
    name: String,
    released: bool,
}

impl WslcSessionReservation {
    pub(super) fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for WslcSessionReservation {
    fn drop(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut state = self
            .coordinator
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.pending_launches = state.pending_launches.saturating_sub(1);
        if state.pending_launches == 0 {
            self.coordinator.idle.notify_all();
        }
    }
}

pub(super) fn acquire(cli: &Path, storage: &Path) -> WorkspaceResult<Arc<WslcSessionCoordinator>> {
    let storage = storage.to_path_buf();
    let mut cache = session_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = cache.get(&storage).and_then(Weak::upgrade) {
        return Ok(existing);
    }

    let coordinator = WslcSessionCoordinator::start(cli.to_path_buf(), storage.clone())?;
    cache.insert(storage, Arc::downgrade(&coordinator));
    Ok(coordinator)
}

fn session_cache() -> &'static Mutex<HashMap<PathBuf, Weak<WslcSessionCoordinator>>> {
    SESSION_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn session_owner_thread(
    cli: PathBuf,
    name: String,
    storage: PathBuf,
    ready: mpsc::SyncSender<Result<(), String>>,
    shutdown: mpsc::Receiver<()>,
) {
    #[cfg(windows)]
    {
        match windows_session::open(&name, &storage) {
            Ok(_session) => {
                if ready.send(Ok(())).is_err() {
                    return;
                }
                let _ = shutdown.recv();
                terminate_session(&cli, &name);
            }
            Err(error) => {
                let _ = ready.send(Err(error));
            }
        }
    }

    #[cfg(not(windows))]
    {
        let _ = cli;
        let _ = name;
        let _ = storage;
        let _ = shutdown;
        let _ = ready.send(Err(
            "Microsoft WSL Containers session management is Windows-only.".into(),
        ));
    }
}

fn terminate_session(cli: &Path, name: &str) {
    let _ = Command::new(cli)
        .args(["--session", name, "system", "session", "terminate"])
        .output();

    // `terminate` normally waits for teardown, but preview builds can return while the
    // session is still visible. Wait briefly before allowing the same storage to reopen.
    for _ in 0..100 {
        let Ok(output) = Command::new(cli)
            .args(["system", "session", "list"])
            .output()
        else {
            break;
        };
        if !String::from_utf8_lossy(&output.stdout).contains(name) {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn session_error(
    code: &'static str,
    message: impl Into<String>,
    stage: &'static str,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: message.into(),
        category: "runtime",
        retryable: false,
        details: json!({
            "backend": BACKEND_ID,
            "stage": stage,
            "fallback_allowed": false,
            "suggestion": "Update or repair WSL Containers, then retry."
        }),
    }
}

#[cfg(windows)]
mod windows_session {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::ptr;

    use windows::core::{Interface, GUID, HRESULT};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoInitializeSecurity, CoUninitialize,
        CLSCTX_LOCAL_SERVER, COINIT_MULTITHREADED, EOAC_STATIC_CLOAKING, RPC_C_AUTHN_LEVEL_DEFAULT,
        RPC_C_IMP_LEVEL_IMPERSONATE,
    };

    const CLSID_WSLC_SESSION_MANAGER: GUID =
        GUID::from_u128(0xa9b7a1b9_0671_405c_95f1_e0612cb4ce8f);
    const WSLC_SESSION_FLAGS_NONE: i32 = 0;
    const WSLC_STORAGE_NONE: i32 = 0;
    const WSLC_NETWORK_NAT: i32 = 1;
    const WSLC_FEATURE_VIRTIOFS: i32 = 8;
    const RPC_E_TOO_LATE: i32 = 0x80010119u32 as i32;

    windows::core::imp::define_interface!(
        IWslcSessionManager,
        IWslcSessionManager_Vtbl,
        0x82a7abc8_6b50_43fc_ab96_15fbbe7e8760
    );
    windows::core::imp::interface_hierarchy!(IWslcSessionManager, windows::core::IUnknown);

    windows::core::imp::define_interface!(
        IWslcSession,
        IWslcSession_Vtbl,
        0xef0661e4_6364_40ea_b433_e2fdf11f3519
    );
    windows::core::imp::interface_hierarchy!(IWslcSession, windows::core::IUnknown);

    #[repr(C)]
    pub struct IWslcSessionManager_Vtbl {
        base__: windows::core::IUnknown_Vtbl,
        get_version: unsafe extern "system" fn(*mut c_void, *mut WslcVersion) -> HRESULT,
        create_session: unsafe extern "system" fn(
            *mut c_void,
            *const WslcSessionSettings,
            i32,
            *mut c_void,
            *mut *mut c_void,
        ) -> HRESULT,
        enter_session: unsafe extern "system" fn(
            *mut c_void,
            *const u16,
            *const u16,
            *mut c_void,
            *mut *mut c_void,
        ) -> HRESULT,
    }

    #[repr(C)]
    pub struct IWslcSession_Vtbl {
        base__: windows::core::IUnknown_Vtbl,
    }

    #[repr(C)]
    #[derive(Default)]
    struct WslcVersion {
        major: u32,
        minor: u32,
        revision: u32,
    }

    #[repr(C)]
    union WslcHandleValue {
        file: *mut c_void,
        pipe: *mut c_void,
        socket: usize,
    }

    #[repr(C)]
    struct WslcHandle {
        kind: i32,
        value: WslcHandleValue,
    }

    #[repr(C)]
    struct WslcSessionSettings {
        display_name: *const u16,
        storage_path: *const u16,
        maximum_storage_size_mb: u64,
        cpu_count: u32,
        memory_mb: u32,
        boot_timeout_ms: u32,
        networking_mode: i32,
        feature_flags: i32,
        host_loopback: *const u8,
        dmesg_output: WslcHandle,
        storage_flags: i32,
        idle_timeout_sec: u32,
        root_vhd_override: *const u16,
        root_vhd_type_override: *const u8,
    }

    struct ComApartment;

    impl ComApartment {
        fn init() -> Result<Self, String> {
            unsafe {
                CoInitializeEx(None, COINIT_MULTITHREADED)
                    .ok()
                    .map_err(|error| format!("CoInitializeEx failed: {error}"))?;
                if let Err(error) = CoInitializeSecurity(
                    None,
                    -1,
                    None,
                    None,
                    RPC_C_AUTHN_LEVEL_DEFAULT,
                    RPC_C_IMP_LEVEL_IMPERSONATE,
                    None,
                    EOAC_STATIC_CLOAKING,
                    None,
                ) {
                    if error.code().0 != RPC_E_TOO_LATE {
                        return Err(format!("CoInitializeSecurity failed: {error}"));
                    }
                }
            }
            Ok(Self)
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    pub(super) struct OwnedSession {
        _session: IWslcSession,
        _manager: IWslcSessionManager,
        _apartment: ComApartment,
    }

    pub(super) fn open(name: &str, storage: &Path) -> Result<OwnedSession, String> {
        let apartment = ComApartment::init()?;
        let manager: IWslcSessionManager = unsafe {
            CoCreateInstance(&CLSID_WSLC_SESSION_MANAGER, None, CLSCTX_LOCAL_SERVER)
                .map_err(|error| format!("Failed to create WSLC session manager: {error}"))?
        };
        let storage_existed = storage.exists();
        let name_w = wide(OsStr::new(name));
        let storage_w = wide(storage.as_os_str());
        let session = if storage_existed {
            unsafe { enter_session(&manager, &name_w, &storage_w) }
        } else {
            unsafe { create_session(&manager, &name_w, &storage_w) }
        }
        .map_err(|error| {
            let action = if storage_existed { "reopen" } else { "create" };
            format!(
                "Failed to {action} WSLC session storage {}: {error}",
                storage.display()
            )
        })?;
        Ok(OwnedSession {
            _session: session,
            _manager: manager,
            _apartment: apartment,
        })
    }

    unsafe fn create_session(
        manager: &IWslcSessionManager,
        name: &[u16],
        storage: &[u16],
    ) -> windows::core::Result<IWslcSession> {
        let settings = WslcSessionSettings {
            display_name: name.as_ptr(),
            storage_path: storage.as_ptr(),
            maximum_storage_size_mb: 32_768,
            cpu_count: 0,
            memory_mb: 0,
            boot_timeout_ms: 30_000,
            // The current WSLC 2.9.4 preview reliably creates custom sessions with
            // NAT transport. Command-level network isolation remains controlled by
            // `wslc run --network <configured>`, including the default `none`.
            networking_mode: WSLC_NETWORK_NAT,
            feature_flags: WSLC_FEATURE_VIRTIOFS,
            host_loopback: ptr::null(),
            dmesg_output: WslcHandle {
                kind: 0,
                value: WslcHandleValue { socket: 0 },
            },
            storage_flags: WSLC_STORAGE_NONE,
            idle_timeout_sec: 0,
            root_vhd_override: ptr::null(),
            root_vhd_type_override: ptr::null(),
        };
        let mut raw = ptr::null_mut();
        (Interface::vtable(manager).create_session)(
            Interface::as_raw(manager),
            &settings,
            WSLC_SESSION_FLAGS_NONE,
            ptr::null_mut(),
            &mut raw,
        )
        .ok()?;
        windows::core::Type::from_abi(raw)
    }

    unsafe fn enter_session(
        manager: &IWslcSessionManager,
        name: &[u16],
        storage: &[u16],
    ) -> windows::core::Result<IWslcSession> {
        let mut raw = ptr::null_mut();
        (Interface::vtable(manager).enter_session)(
            Interface::as_raw(manager),
            name.as_ptr(),
            storage.as_ptr(),
            ptr::null_mut(),
            &mut raw,
        )
        .ok()?;
        windows::core::Type::from_abi(raw)
    }

    fn wide(value: &OsStr) -> Vec<u16> {
        value.encode_wide().chain(std::iter::once(0)).collect()
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn cache_reuses_live_session_for_same_storage() {
        // This is a structural test only; the live COM/WSLC lifecycle is covered by the
        // opt-in integration test in the parent module.
        let _ = session_cache();
    }
}
