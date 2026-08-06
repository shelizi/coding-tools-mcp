use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use coding_tools_tunnel_protocol::{
    auth_signing_payload, is_hop_by_hop_header, valid_client_id, ClientHello, ControlMessage,
    DeviceAuthProof, EnrollmentRequest, EnrollmentResponse, HeaderPair, TunnelService,
    WorkerDemand, WorkerPolicy, CLIENT_ID_HEADER, ENROLL_PATH_PREFIX, MAX_REQUEST_BODY_BYTES,
    PROTOCOL_VERSION, SERVICE_HEADER, WS_PATH, WS_SUBPROTOCOL,
};
use ed25519_dalek::{Signer, SigningKey};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use rand_core::OsRng;
use reqwest::header::{HeaderName, HeaderValue, HOST};
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{interval, sleep, timeout, Instant, Interval, MissedTickBehavior};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::secret::SecretStore;
use crate::workspace::WorkspaceProfile;

use super::TunnelServiceKind;

const INITIAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(15);
const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(45);
const WEBSOCKET_CLOSE_TIMEOUT: Duration = Duration::from_secs(1);
const DEMAND_HINT_TTL: Duration = Duration::from_secs(3);
const DEVICE_IDENTITY_KEY: &str = "builtin_tunnel_device_identity";
const ENROLLMENT_URL_KEY: &str = "builtin_tunnel_enrollment_url";

pub(crate) fn behavioral_parity_fixture() -> serde_json::Value {
    serde_json::json!({
        "local_connect_timeout_ms": LOCAL_CONNECT_TIMEOUT.as_millis(),
        "demand_hint_ttl_ms": DEMAND_HINT_TTL.as_millis()
    })
}

type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type ClientSink = SplitSink<ClientWebSocket, Message>;
type ClientStream = SplitStream<ClientWebSocket>;

struct HeartbeatTracker {
    last_activity: Instant,
}

impl HeartbeatTracker {
    fn new_at(now: Instant) -> Self {
        Self { last_activity: now }
    }

    fn record_activity_at(&mut self, now: Instant) {
        self.last_activity = now;
    }

    fn record_activity(&mut self) {
        self.record_activity_at(Instant::now());
    }

    fn expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_activity) >= HEARTBEAT_TIMEOUT
    }
}

#[derive(Clone)]
pub struct BuiltinTunnelConfig {
    pub public_url: String,
    pub websocket_url: String,
    pub client_id: String,
    pub service: TunnelService,
    pub route_prefix: String,
    pub local_base_url: String,
    device_id: String,
    signing_key: Arc<SigningKey>,
    log_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredDeviceIdentity {
    device_id: String,
    client_id: String,
    private_key: String,
    enrolled: bool,
}

struct BuiltinTunnelBaseConfig {
    public_url: String,
    client_id: String,
    service: TunnelService,
    local_base_url: String,
    log_path: PathBuf,
}

pub struct BuiltinTunnelHandle {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    metrics: Arc<BuiltinTunnelMetrics>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinTunnelSnapshot {
    pub configured_workers: usize,
    pub connected_workers: usize,
    pub idle_workers: usize,
    pub busy_workers: usize,
    pub recycled_workers: u64,
    pub policy_revision: u64,
    pub last_error: Option<String>,
}

impl BuiltinTunnelSnapshot {
    pub fn availability_state(&self, task_running: bool) -> &'static str {
        if !task_running {
            "stopped"
        } else if self.connected_workers == 0 {
            "reconnecting"
        } else {
            "running"
        }
    }
}

struct BuiltinTunnelMetrics {
    configured_workers: AtomicUsize,
    connected_workers: AtomicUsize,
    idle_workers: AtomicUsize,
    busy_workers: AtomicUsize,
    recycled_workers: AtomicU64,
    policy_revision: AtomicU64,
    last_error: Mutex<Option<String>>,
}

impl BuiltinTunnelMetrics {
    fn new(configured_workers: usize) -> Self {
        Self {
            configured_workers: AtomicUsize::new(configured_workers),
            connected_workers: AtomicUsize::new(0),
            idle_workers: AtomicUsize::new(0),
            busy_workers: AtomicUsize::new(0),
            recycled_workers: AtomicU64::new(0),
            policy_revision: AtomicU64::new(0),
            last_error: Mutex::new(None),
        }
    }

    fn set_policy(&self, policy: &WorkerPolicy) {
        self.configured_workers
            .store(usize::from(policy.max_workers), Ordering::Release);
        self.policy_revision
            .store(policy.revision, Ordering::Release);
    }

    fn set_pool_counts(&self, idle: usize, busy: usize) {
        self.idle_workers.store(idle, Ordering::Release);
        self.busy_workers.store(busy, Ordering::Release);
    }

    fn record_recycle(&self) {
        self.recycled_workers.fetch_add(1, Ordering::AcqRel);
    }

    fn set_last_error(&self, error: Option<String>) {
        *self
            .last_error
            .lock()
            .unwrap_or_else(|guard| guard.into_inner()) = error;
    }

    fn snapshot(&self) -> BuiltinTunnelSnapshot {
        BuiltinTunnelSnapshot {
            configured_workers: self.configured_workers.load(Ordering::Acquire),
            connected_workers: self.connected_workers.load(Ordering::Acquire),
            idle_workers: self.idle_workers.load(Ordering::Acquire),
            busy_workers: self.busy_workers.load(Ordering::Acquire),
            recycled_workers: self.recycled_workers.load(Ordering::Acquire),
            policy_revision: self.policy_revision.load(Ordering::Acquire),
            last_error: self
                .last_error
                .lock()
                .unwrap_or_else(|guard| guard.into_inner())
                .clone(),
        }
    }
}

struct ConnectedWorkerGuard {
    metrics: Arc<BuiltinTunnelMetrics>,
}

impl ConnectedWorkerGuard {
    fn new(metrics: Arc<BuiltinTunnelMetrics>) -> Self {
        metrics.connected_workers.fetch_add(1, Ordering::AcqRel);
        Self { metrics }
    }
}

impl Drop for ConnectedWorkerGuard {
    fn drop(&mut self) {
        self.metrics
            .connected_workers
            .fetch_sub(1, Ordering::AcqRel);
    }
}

impl BuiltinTunnelHandle {
    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    pub fn snapshot(&self) -> BuiltinTunnelSnapshot {
        self.metrics.snapshot()
    }

    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        let mut task = self.task;
        if timeout(Duration::from_secs(5), &mut task).await.is_err() {
            task.abort();
            let _ = task.await;
        }
    }
}

pub fn validate_builtin_tunnel(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<()> {
    let (public_url, service) = match kind {
        TunnelServiceKind::Mcp => (profile.tunnel.public_url.as_str(), TunnelService::Mcp),
        TunnelServiceKind::Actions => (profile.actions.public_url.as_str(), TunnelService::Actions),
    };
    parse_builtin_endpoint(public_url, service)?;
    Ok(())
}

fn builtin_base_config(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<BuiltinTunnelBaseConfig> {
    let (public_url, local_port, bind_address, service) = match kind {
        TunnelServiceKind::Mcp => (
            profile.tunnel.public_url.as_str(),
            profile.runtime.local_port,
            profile.runtime.bind_address.as_str(),
            TunnelService::Mcp,
        ),
        TunnelServiceKind::Actions => (
            profile.actions.public_url.as_str(),
            profile.actions.local_port,
            profile.actions.bind_address.as_str(),
            TunnelService::Actions,
        ),
    };
    let endpoint = parse_builtin_endpoint(public_url, service)?;
    Ok(BuiltinTunnelBaseConfig {
        public_url: endpoint.public_url,
        client_id: endpoint.client_id,
        service,
        local_base_url: format!("http://{}:{local_port}", local_connect_host(bind_address)),
        log_path: crate::platform::platform()
            .app_config_dir()?
            .join("logs")
            .join(&profile.id)
            .join(match kind {
                TunnelServiceKind::Mcp => "builtin-tunnel.log",
                TunnelServiceKind::Actions => "actions-builtin-tunnel.log",
            }),
    })
}

pub async fn spawn_builtin_tunnel(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<(BuiltinTunnelHandle, String)> {
    let base = builtin_base_config(profile, kind)?;
    let identity = load_or_enroll_device_identity(&profile.id, &base).await?;
    let endpoint =
        builtin_endpoint_for_client(&base.public_url, base.service, &identity.client_id)?;
    let signing_key = decode_signing_key(&identity.private_key)?;
    let config = BuiltinTunnelConfig {
        public_url: endpoint.public_url,
        websocket_url: endpoint.websocket_url,
        client_id: endpoint.client_id,
        service: base.service,
        route_prefix: endpoint.route_prefix,
        local_base_url: base.local_base_url,
        device_id: identity.device_id,
        signing_key: Arc::new(signing_key),
        log_path: base.log_path,
    };
    let public_url = config.public_url.clone();
    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let (shutdown, shutdown_rx) = watch::channel(false);
    let (status_tx, mut status_rx) = mpsc::channel::<Result<(), String>>(32);
    let mut task = tokio::spawn(run_worker_pool(
        config,
        shutdown_rx,
        status_tx,
        metrics.clone(),
    ));

    let deadline = Instant::now() + INITIAL_CONNECT_TIMEOUT;
    let mut last_error = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop_startup_task(&shutdown, &mut task).await;
            return Err(AppError::Message(format!(
                "內建 WSS 隧道連線逾時{}",
                last_error
                    .map(|error| format!("：{error}"))
                    .unwrap_or_default()
            )));
        }
        match timeout(remaining, status_rx.recv()).await {
            Ok(Some(Ok(()))) => {
                return Ok((
                    BuiltinTunnelHandle {
                        shutdown,
                        task,
                        metrics,
                    },
                    public_url,
                ));
            }
            Ok(Some(Err(error))) => last_error = Some(error),
            Ok(None) => {
                stop_startup_task(&shutdown, &mut task).await;
                return Err(AppError::Message(
                    "內建 WSS 隧道 worker 在連線前停止。".into(),
                ));
            }
            Err(_) => continue,
        }
    }
}

async fn stop_startup_task(shutdown: &watch::Sender<bool>, task: &mut JoinHandle<()>) {
    let _ = shutdown.send(true);
    if timeout(Duration::from_secs(2), &mut *task).await.is_err() {
        task.abort();
        let _ = task.await;
    }
}

async fn load_or_enroll_device_identity(
    profile_id: &str,
    config: &BuiltinTunnelBaseConfig,
) -> AppResult<StoredDeviceIdentity> {
    let stored = SecretStore::get(profile_id, DEVICE_IDENTITY_KEY)?
        .map(|raw| {
            serde_json::from_str::<StoredDeviceIdentity>(&raw)
                .map_err(|error| AppError::Message(format!("內建隧道裝置身分格式損壞：{error}")))
        })
        .transpose()?;
    let enrollment_url =
        SecretStore::get(profile_id, ENROLLMENT_URL_KEY)?.filter(|value| !value.trim().is_empty());

    if let Some(identity) = stored
        .as_ref()
        .filter(|identity| identity.enrolled && enrollment_url.is_none())
    {
        return Ok(identity.clone());
    }

    let mut identity = match stored {
        Some(identity) if !identity.enrolled => identity,
        _ => {
            let signing_key = SigningKey::generate(&mut OsRng);
            let identity = StoredDeviceIdentity {
                device_id: Uuid::new_v4().simple().to_string(),
                client_id: config.client_id.clone(),
                private_key: URL_SAFE_NO_PAD.encode(signing_key.to_bytes()),
                enrolled: false,
            };
            save_device_identity(profile_id, &identity)?;
            identity
        }
    };

    let enrollment_url = enrollment_url.ok_or_else(|| {
        AppError::Message("內建 WSS 隧道尚未註冊。請貼上伺服器產生的一次性註冊連結。".into())
    })?;
    let enrollment_url = parse_enrollment_url(&config.public_url, &enrollment_url)?;
    let signing_key = decode_signing_key(&identity.private_key)?;
    let request = EnrollmentRequest {
        device_id: identity.device_id.clone(),
        client_id: identity.client_id.clone(),
        device_name: local_device_name(),
        public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
    };
    let response = reqwest::Client::builder()
        .connect_timeout(WEBSOCKET_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| AppError::Message(format!("無法建立裝置註冊連線：{error}")))?
        .post(enrollment_url)
        .json(&request)
        .send()
        .await
        .map_err(|error| AppError::Message(format!("裝置註冊連線失敗：{error}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(AppError::Message(format!(
            "內建隧道裝置註冊失敗（{status}）：{}",
            detail.trim()
        )));
    }
    let enrolled = response
        .json::<EnrollmentResponse>()
        .await
        .map_err(|error| AppError::Message(format!("裝置註冊回應格式無效：{error}")))?;
    if enrolled.device_id != identity.device_id {
        return Err(AppError::Message(
            "內建隧道伺服器回傳了不一致的裝置 ID。".into(),
        ));
    }
    let enrolled_client_id = if enrolled.client_id.trim().is_empty() {
        identity.client_id.clone()
    } else {
        enrolled.client_id.trim().to_string()
    };
    if !valid_client_id(&enrolled_client_id) {
        return Err(AppError::Message(
            "內建隧道伺服器回傳了無效的 Client ID。".into(),
        ));
    }

    identity.client_id = enrolled_client_id;
    identity.enrolled = true;
    save_device_identity(profile_id, &identity)?;
    SecretStore::set(profile_id, ENROLLMENT_URL_KEY, "")?;
    Ok(identity)
}

fn save_device_identity(profile_id: &str, identity: &StoredDeviceIdentity) -> AppResult<()> {
    let encoded = serde_json::to_string(identity)?;
    SecretStore::set(profile_id, DEVICE_IDENTITY_KEY, &encoded)
}

fn decode_signing_key(value: &str) -> AppResult<SigningKey> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value.trim().as_bytes())
        .map_err(|_| AppError::Message("內建隧道裝置私鑰格式無效。".into()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| AppError::Message("內建隧道裝置私鑰長度無效。".into()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn parse_enrollment_url(public_url: &str, value: &str) -> AppResult<reqwest::Url> {
    let public = reqwest::Url::parse(public_url)
        .map_err(|_| AppError::Message("內建隧道公開網址格式無效。".into()))?;
    let enrollment = reqwest::Url::parse(value.trim())
        .map_err(|_| AppError::Message("一次性註冊連結格式無效。".into()))?;
    if enrollment.scheme() != "https"
        || enrollment.username() != ""
        || enrollment.password().is_some()
        || enrollment.query().is_some()
        || enrollment.fragment().is_some()
        || enrollment.host_str() != public.host_str()
        || enrollment.port_or_known_default() != public.port_or_known_default()
    {
        return Err(AppError::Message(
            "一次性註冊連結必須使用與內建隧道相同的 HTTPS 網域與連接埠。".into(),
        ));
    }
    let prefix = format!("{ENROLL_PATH_PREFIX}/");
    let code = enrollment
        .path()
        .strip_prefix(&prefix)
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 128
                && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or_else(|| {
            AppError::Message("一次性註冊連結必須使用 /_tunnel/enroll/<code> 路徑。".into())
        })?;
    if code.contains('/') {
        return Err(AppError::Message("一次性註冊碼格式無效。".into()));
    }
    Ok(enrollment)
}

fn local_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Coding Tools MCP".into())
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

async fn run_worker_pool(
    config: BuiltinTunnelConfig,
    mut shutdown: watch::Receiver<bool>,
    status_tx: mpsc::Sender<Result<(), String>>,
    metrics: Arc<BuiltinTunnelMetrics>,
) {
    let mut workers = JoinSet::<(usize, bool)>::new();
    let mut managed = HashMap::<usize, ManagedWorker>::new();
    let (event_tx, mut event_rx) = mpsc::channel(128);
    let (policy_tx, mut policy_rx) = watch::channel::<Option<WorkerPolicy>>(None);
    let mut next_worker_index = 0_usize;
    let mut idle_excess_since = None;
    let mut last_policy_revision = None;
    let mut last_scale_up_block = None;
    let mut demand_target = 0_usize;
    let mut demand_seen_at = None;
    let mut last_pressure_at = None;
    let mut reconcile_tick = interval(Duration::from_secs(1));
    reconcile_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let bootstrap_worker = spawn_managed_worker(
        &mut workers,
        &mut managed,
        &mut next_worker_index,
        &config,
        &shutdown,
        &status_tx,
        &metrics,
        &event_tx,
        &policy_tx,
    );
    append_log(
        &config.log_path,
        &format!(
            "event=worker_spawn reason=bootstrap worker_indices={bootstrap_worker} before_total=0 after_total=1"
        ),
    );

    while !*shutdown.borrow() {
        tokio::select! {
            _ = shutdown.changed() => break,
            event = event_rx.recv() => {
                match event {
                    Some(WorkerEvent::State { worker_index, state }) => {
                        if let Some(worker) = managed.get_mut(&worker_index) {
                            let was_busy = worker.state == PoolWorkerState::Busy;
                            let was_connecting = worker.state == PoolWorkerState::Connecting;
                            if state == PoolWorkerState::Connecting && !was_connecting {
                                worker.connecting_since = Instant::now();
                            }
                            worker.state = state;
                            if state == PoolWorkerState::Busy
                                || (was_busy && state == PoolWorkerState::Idle)
                            {
                                last_pressure_at = Some(Instant::now());
                            }
                        }
                    }
                    Some(WorkerEvent::Demand(demand)) => {
                        demand_target = usize::from(demand.desired_workers);
                        demand_seen_at = Some(Instant::now());
                        if demand.queued_requests > 0 {
                            last_pressure_at = Some(Instant::now());
                        }
                        append_log(
                            &config.log_path,
                            &format!(
                                "event=worker_demand queued_requests={} oldest_queue_wait_ms={} desired_workers={} current_total={}",
                                demand.queued_requests,
                                demand.oldest_queue_wait_ms,
                                demand.desired_workers,
                                managed.len(),
                            ),
                        );
                    }
                    None => {}
                }
            }
            changed = policy_rx.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            joined = workers.join_next(), if !workers.is_empty() => {
                match joined {
                    Some(Ok((worker_index, recycled))) => {
                        managed.remove(&worker_index);
                        if recycled {
                            metrics.record_recycle();
                        }
                    }
                    Some(Err(error)) => {
                        metrics.set_last_error(Some(format!("worker task failed: {error}")));
                        managed.retain(|_, worker| worker.state != PoolWorkerState::Retiring);
                    }
                    None => {}
                }
            }
            _ = reconcile_tick.tick() => {}
        }

        let Some(policy) = policy_rx.borrow().clone() else {
            continue;
        };
        if last_policy_revision != Some(policy.revision) {
            append_log(
                &config.log_path,
                &format!(
                    "event=worker_policy_applied service={} revision={} start_workers={} min_idle_workers={} max_idle_workers={} max_workers={} max_pending_requests={} worker_acquire_timeout_ms={} max_connecting_workers={} connecting_capacity_grace_ms={} scale_down_step={} burst_warm_workers={} burst_warm_seconds={} scale_down_delay_seconds={} max_requests_per_worker={} max_lifetime_seconds={}",
                    config.service.as_str(),
                    policy.revision,
                    policy.start_workers,
                    policy.min_idle_workers,
                    policy.max_idle_workers,
                    policy.max_workers,
                    policy.max_pending_requests,
                    policy.worker_acquire_timeout_ms,
                    policy.max_connecting_workers,
                    policy.connecting_capacity_grace_ms,
                    policy.scale_down_step,
                    policy.burst_warm_workers,
                    policy.burst_warm_seconds,
                    policy.scale_down_delay_seconds,
                    policy.max_requests_per_worker,
                    policy.max_lifetime_seconds,
                ),
            );
            last_policy_revision = Some(policy.revision);
            last_scale_up_block = None;
        }
        metrics.set_policy(&policy);
        let now = Instant::now();
        let connecting_grace = Duration::from_millis(policy.connecting_capacity_grace_ms);
        let counts = pool_counts(&managed);
        metrics.set_pool_counts(counts.idle, counts.busy);
        let demand_active = demand_seen_at
            .is_some_and(|seen| now.saturating_duration_since(seen) <= DEMAND_HINT_TTL);
        let desired_workers = if demand_active {
            demand_target.min(usize::from(policy.max_workers))
        } else {
            0
        };
        let effective_connecting = effective_connecting_workers(&managed, connecting_grace);
        let max_connecting = configured_max_connecting(&policy);
        let warm_floor = configured_burst_warm_floor(&policy);
        let warm_active = policy.burst_warm_seconds > 0
            && last_pressure_at.is_some_and(|seen| {
                now.saturating_duration_since(seen) < Duration::from_secs(policy.burst_warm_seconds)
            });
        let scale_down_floor = if warm_active {
            warm_floor
        } else {
            usize::from(policy.max_idle_workers)
        };
        if counts.total > scale_down_floor && counts.idle > 0 {
            idle_excess_since.get_or_insert(now);
        } else {
            idle_excess_since = None;
        }
        let idle_excess_elapsed = idle_excess_since.is_some_and(|started| {
            now.saturating_duration_since(started)
                >= Duration::from_secs(policy.scale_down_delay_seconds)
        });
        let adjustment = pool_adjustment(
            &policy,
            counts,
            effective_connecting,
            max_connecting,
            desired_workers,
            idle_excess_elapsed,
            scale_down_floor,
        );

        let blocked = scale_up_block(
            &policy,
            counts,
            effective_connecting,
            max_connecting,
            desired_workers,
            adjustment,
        );
        if blocked != last_scale_up_block {
            if let Some(reason) = blocked {
                append_log(
                    &config.log_path,
                    &format!(
                        "event=scale_up_blocked reason={} total={} connecting={} effective_connecting={} idle={} busy={} desired_workers={} min_idle_workers={} max_connecting_workers={} max_workers={} policy_revision={}",
                        reason.as_str(),
                        counts.total,
                        counts.connecting,
                        effective_connecting,
                        counts.idle,
                        counts.busy,
                        desired_workers,
                        policy.min_idle_workers,
                        max_connecting,
                        policy.max_workers,
                        policy.revision,
                    ),
                );
            }
            last_scale_up_block = blocked;
        }

        let mut remaining_retirements = adjustment.retire;
        let mut retired_worker_indices = Vec::with_capacity(adjustment.retire);
        for (worker_index, worker) in managed.iter_mut() {
            if remaining_retirements == 0 {
                break;
            }
            if worker.state == PoolWorkerState::Idle {
                worker.state = PoolWorkerState::Retiring;
                let _ = worker.retire.send(true);
                retired_worker_indices.push(*worker_index);
                remaining_retirements -= 1;
            }
        }
        if adjustment.retire > 0 {
            retired_worker_indices.sort_unstable();
            let after_total = counts.total.saturating_sub(retired_worker_indices.len());
            append_log(
                &config.log_path,
                &format!(
                    "event=scale_down reason={} requested={} retired={} worker_indices={} before_total={} after_total={} connecting={} idle={} busy={} scale_down_floor={} warm_active={} scale_down_step={} max_idle_workers={} max_workers={} policy_revision={}",
                    scale_down_reason(&policy, counts, idle_excess_elapsed, warm_active),
                    adjustment.retire,
                    retired_worker_indices.len(),
                    join_worker_indices(&retired_worker_indices),
                    counts.total,
                    after_total,
                    counts.connecting,
                    counts.idle,
                    counts.busy,
                    scale_down_floor,
                    warm_active,
                    policy.scale_down_step,
                    policy.max_idle_workers,
                    policy.max_workers,
                    policy.revision,
                ),
            );
            if after_total <= scale_down_floor {
                idle_excess_since = None;
            }
        }
        let mut spawned_worker_indices = Vec::with_capacity(adjustment.spawn);
        for _ in 0..adjustment.spawn {
            spawned_worker_indices.push(spawn_managed_worker(
                &mut workers,
                &mut managed,
                &mut next_worker_index,
                &config,
                &shutdown,
                &status_tx,
                &metrics,
                &event_tx,
                &policy_tx,
            ));
        }
        if adjustment.spawn > 0 {
            append_log(
                &config.log_path,
                &format!(
                    "event=scale_up reason={} requested={} spawned={} worker_indices={} before_total={} after_total={} connecting={} effective_connecting={} idle={} busy={} desired_workers={} start_workers={} min_idle_workers={} max_connecting_workers={} max_workers={} policy_revision={}",
                    scale_up_reason(&policy, counts, effective_connecting, desired_workers),
                    adjustment.spawn,
                    spawned_worker_indices.len(),
                    join_worker_indices(&spawned_worker_indices),
                    counts.total,
                    counts.total.saturating_add(spawned_worker_indices.len()),
                    counts.connecting,
                    effective_connecting,
                    counts.idle,
                    counts.busy,
                    desired_workers,
                    policy.start_workers,
                    policy.min_idle_workers,
                    max_connecting,
                    policy.max_workers,
                    policy.revision,
                ),
            );
        }
    }

    for worker in managed.values() {
        let _ = worker.retire.send(true);
    }
    workers.abort_all();
    while workers.join_next().await.is_some() {}
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolWorkerState {
    Connecting,
    Idle,
    Busy,
    Retiring,
}

struct ManagedWorker {
    state: PoolWorkerState,
    connecting_since: Instant,
    retire: watch::Sender<bool>,
}

enum WorkerEvent {
    State {
        worker_index: usize,
        state: PoolWorkerState,
    },
    Demand(WorkerDemand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerConnectionExit {
    Shutdown,
    ScaleDown,
    Recycle,
}

#[allow(clippy::too_many_arguments)]
fn spawn_managed_worker(
    workers: &mut JoinSet<(usize, bool)>,
    managed: &mut HashMap<usize, ManagedWorker>,
    next_worker_index: &mut usize,
    config: &BuiltinTunnelConfig,
    shutdown: &watch::Receiver<bool>,
    status_tx: &mpsc::Sender<Result<(), String>>,
    metrics: &Arc<BuiltinTunnelMetrics>,
    event_tx: &mpsc::Sender<WorkerEvent>,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
) -> usize {
    let worker_index = *next_worker_index;
    *next_worker_index = next_worker_index.saturating_add(1);
    let (retire, retire_rx) = watch::channel(false);
    managed.insert(
        worker_index,
        ManagedWorker {
            state: PoolWorkerState::Connecting,
            connecting_since: Instant::now(),
            retire,
        },
    );
    workers.spawn(worker_reconnect_loop(
        config.clone(),
        shutdown.clone(),
        retire_rx,
        status_tx.clone(),
        worker_index,
        metrics.clone(),
        event_tx.clone(),
        policy_tx.clone(),
    ));
    worker_index
}

fn pool_counts(workers: &HashMap<usize, ManagedWorker>) -> PoolCounts {
    let connecting = workers
        .values()
        .filter(|worker| worker.state == PoolWorkerState::Connecting)
        .count();
    let idle = workers
        .values()
        .filter(|worker| worker.state == PoolWorkerState::Idle)
        .count();
    let busy = workers
        .values()
        .filter(|worker| worker.state == PoolWorkerState::Busy)
        .count();
    PoolCounts {
        total: connecting + idle + busy,
        connecting,
        idle,
        busy,
    }
}

fn effective_connecting_workers(workers: &HashMap<usize, ManagedWorker>, grace: Duration) -> usize {
    if grace.is_zero() {
        return workers
            .values()
            .filter(|worker| worker.state == PoolWorkerState::Connecting)
            .count();
    }
    let now = Instant::now();
    workers
        .values()
        .filter(|worker| {
            worker.state == PoolWorkerState::Connecting
                && now.saturating_duration_since(worker.connecting_since) <= grace
        })
        .count()
}

#[allow(clippy::too_many_arguments)]
async fn worker_reconnect_loop(
    config: BuiltinTunnelConfig,
    mut shutdown: watch::Receiver<bool>,
    mut retire: watch::Receiver<bool>,
    status_tx: mpsc::Sender<Result<(), String>>,
    worker_index: usize,
    metrics: Arc<BuiltinTunnelMetrics>,
    event_tx: mpsc::Sender<WorkerEvent>,
    policy_tx: watch::Sender<Option<WorkerPolicy>>,
) -> (usize, bool) {
    let worker_id = format!("{}-{worker_index}-{}", config.client_id, Uuid::new_v4());
    let mut delay = INITIAL_RECONNECT_DELAY;
    let mut attempt = 0_u64;

    while !*shutdown.borrow() && !*retire.borrow() {
        let _ = event_tx
            .send(WorkerEvent::State {
                worker_index,
                state: PoolWorkerState::Connecting,
            })
            .await;
        let mut connected = false;
        match run_connected_worker(
            &config,
            &worker_id,
            &mut shutdown,
            &mut retire,
            &status_tx,
            metrics.clone(),
            &mut connected,
            worker_index,
            &event_tx,
            &policy_tx,
        )
        .await
        {
            Ok(WorkerConnectionExit::Shutdown) => return (worker_index, false),
            Ok(WorkerConnectionExit::ScaleDown) => return (worker_index, false),
            Ok(WorkerConnectionExit::Recycle) => return (worker_index, true),
            Err(error) => {
                append_log(&config.log_path, &format!("worker {worker_index}: {error}"));
                metrics.set_last_error(Some(error.clone()));
                let _ = status_tx.send(Err(error)).await;
            }
        }

        if connected {
            delay = next_reconnect_base(delay, true);
            attempt = 0;
        } else {
            attempt = attempt.saturating_add(1);
        }
        let sleep_for = reconnect_delay(delay, worker_index, attempt);

        tokio::select! {
            _ = shutdown.changed() => return (worker_index, false),
            _ = retire.changed() => return (worker_index, false),
            _ = sleep(sleep_for) => {},
        }
        delay = next_reconnect_base(delay, false);
    }
    (worker_index, false)
}

#[allow(clippy::too_many_arguments)]
async fn run_connected_worker(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
    shutdown: &mut watch::Receiver<bool>,
    retire: &mut watch::Receiver<bool>,
    status_tx: &mpsc::Sender<Result<(), String>>,
    metrics: Arc<BuiltinTunnelMetrics>,
    connected: &mut bool,
    worker_index: usize,
    event_tx: &mpsc::Sender<WorkerEvent>,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
) -> Result<WorkerConnectionExit, String> {
    let mut request = config
        .websocket_url
        .clone()
        .into_client_request()
        .map_err(|error| error.to_string())?;
    request.headers_mut().insert(
        CLIENT_ID_HEADER,
        config
            .client_id
            .parse()
            .map_err(|error| format!("invalid client id header: {error}"))?,
    );
    request.headers_mut().insert(
        SERVICE_HEADER,
        config
            .service
            .as_str()
            .parse()
            .map_err(|error| format!("invalid service header: {error}"))?,
    );
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        WS_SUBPROTOCOL
            .parse()
            .map_err(|error| format!("invalid tunnel subprotocol: {error}"))?,
    );

    let (socket, response) = timeout(WEBSOCKET_CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| "WSS connection timed out".to_string())?
        .map_err(|error| error.to_string())?;
    if response
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        != Some(WS_SUBPROTOCOL)
    {
        return Err("server did not accept coding-tools-tunnel-v3".into());
    }
    let (mut sink, mut stream) = socket.split();
    let (nonce, expires_at_unix_ms) = match receive_control(&mut sink, &mut stream).await? {
        ControlMessage::Challenge {
            nonce,
            expires_at_unix_ms,
        } => (nonce, expires_at_unix_ms),
        ControlMessage::Error { message, .. } => return Err(message),
        _ => return Err("server did not issue a device authentication challenge".into()),
    };
    if unix_ms() > expires_at_unix_ms {
        return Err("server authentication challenge already expired".into());
    }
    let mut proof = DeviceAuthProof {
        hello: ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_id: config.client_id.clone(),
            service: config.service,
            worker_id: worker_id.to_string(),
        },
        device_id: config.device_id.clone(),
        signature: String::new(),
    };
    proof.signature = URL_SAFE_NO_PAD.encode(
        config
            .signing_key
            .sign(&auth_signing_payload(&nonce, &proof))
            .to_bytes(),
    );
    send_control(&mut sink, &ControlMessage::Authenticate(proof)).await?;
    let initial_policy = match receive_control(&mut sink, &mut stream).await? {
        ControlMessage::HelloAck {
            protocol_version,
            worker_policy,
        } if protocol_version == PROTOCOL_VERSION => worker_policy,
        ControlMessage::Error { message, .. } => return Err(message),
        _ => return Err("server did not acknowledge tunnel device authentication".into()),
    };
    initial_policy.validate()?;
    policy_tx.send_replace(Some(initial_policy.clone()));
    send_control(&mut sink, &ControlMessage::Ready).await?;
    *connected = true;
    metrics.set_last_error(None);
    let _connected_guard = ConnectedWorkerGuard::new(metrics);
    let _ = status_tx.send(Ok(())).await;
    let _ = event_tx
        .send(WorkerEvent::State {
            worker_index,
            state: PoolWorkerState::Idle,
        })
        .await;
    append_log(&config.log_path, &format!("worker connected: {worker_id}"));

    let http = reqwest::Client::builder()
        .connect_timeout(LOCAL_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let mut heartbeat = HeartbeatTracker::new_at(Instant::now());
    let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);
    heartbeat_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let connected_at = Instant::now();
    let mut completed_requests = 0_u64;

    loop {
        if *shutdown.borrow() {
            close_client_websocket(&mut sink, &mut stream).await;
            return Ok(WorkerConnectionExit::Shutdown);
        }
        if *retire.borrow() {
            close_client_websocket(&mut sink, &mut stream).await;
            return Ok(WorkerConnectionExit::ScaleDown);
        }
        let Some(request) = receive_request(
            &mut sink,
            &mut stream,
            shutdown,
            retire,
            &mut heartbeat,
            &mut heartbeat_interval,
            policy_tx,
            event_tx,
            worker_index,
            connected_at,
            completed_requests,
        )
        .await?
        else {
            close_client_websocket(&mut sink, &mut stream).await;
            let policy = policy_tx
                .borrow()
                .clone()
                .unwrap_or_else(|| initial_policy.clone());
            return Ok(if *shutdown.borrow() {
                WorkerConnectionExit::Shutdown
            } else if worker_should_recycle(
                &policy,
                worker_index as u64,
                completed_requests,
                Instant::now().saturating_duration_since(connected_at),
            ) {
                WorkerConnectionExit::Recycle
            } else {
                WorkerConnectionExit::ScaleDown
            });
        };
        forward_request(
            config,
            &http,
            request,
            &mut sink,
            &mut stream,
            shutdown,
            &mut heartbeat,
            &mut heartbeat_interval,
        )
        .await?;
        completed_requests = completed_requests.saturating_add(1);
        let policy = policy_tx
            .borrow()
            .clone()
            .unwrap_or_else(|| initial_policy.clone());
        if worker_should_recycle(
            &policy,
            worker_index as u64,
            completed_requests,
            Instant::now().saturating_duration_since(connected_at),
        ) {
            close_client_websocket(&mut sink, &mut stream).await;
            return Ok(WorkerConnectionExit::Recycle);
        }
        send_control(&mut sink, &ControlMessage::Ready).await?;
        let _ = event_tx
            .send(WorkerEvent::State {
                worker_index,
                state: PoolWorkerState::Idle,
            })
            .await;
    }
}

async fn close_client_websocket(sink: &mut ClientSink, stream: &mut ClientStream) {
    if sink.send(Message::Close(None)).await.is_err() {
        return;
    }
    // Dropping immediately after the Close frame can produce a TCP reset on
    // Windows. Give the peer a bounded window to complete the close handshake.
    let _ = timeout(WEBSOCKET_CLOSE_TIMEOUT, async {
        while let Some(frame) = stream.next().await {
            match frame {
                Ok(Message::Close(_)) | Err(_) => break,
                _ => {}
            }
        }
    })
    .await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PoolCounts {
    total: usize,
    connecting: usize,
    idle: usize,
    busy: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PoolAdjustment {
    spawn: usize,
    retire: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScaleUpBlock {
    ConnectingLimitReached,
    MaxWorkersReached,
}

impl ScaleUpBlock {
    fn as_str(self) -> &'static str {
        match self {
            Self::ConnectingLimitReached => "connecting_limit_reached",
            Self::MaxWorkersReached => "max_workers_reached",
        }
    }
}

fn configured_max_connecting(policy: &WorkerPolicy) -> usize {
    let maximum = usize::from(policy.max_workers).max(1);
    if policy.max_connecting_workers == 0 {
        maximum.min(4)
    } else {
        usize::from(policy.max_connecting_workers)
            .min(maximum)
            .max(1)
    }
}

fn configured_burst_warm_floor(policy: &WorkerPolicy) -> usize {
    let maximum = usize::from(policy.max_workers);
    if policy.burst_warm_workers == 0 {
        usize::from(policy.start_workers)
            .max(usize::from(policy.max_idle_workers).saturating_mul(2))
            .min(maximum)
    } else {
        usize::from(policy.burst_warm_workers).min(maximum)
    }
}

#[allow(clippy::too_many_arguments)]
fn pool_adjustment(
    policy: &WorkerPolicy,
    counts: PoolCounts,
    effective_connecting: usize,
    max_connecting: usize,
    desired_workers: usize,
    idle_excess_elapsed: bool,
    scale_down_floor: usize,
) -> PoolAdjustment {
    debug_assert_eq!(counts.total, counts.connecting + counts.idle + counts.busy);
    let maximum = usize::from(policy.max_workers);
    let startup_needed = usize::from(policy.start_workers).saturating_sub(counts.total);
    let spare_needed = usize::from(policy.min_idle_workers)
        .saturating_sub(counts.idle.saturating_add(effective_connecting));
    let demand_needed = desired_workers.saturating_sub(counts.total);
    let requested_spawn = startup_needed.max(spare_needed).max(demand_needed);
    let connecting_budget = max_connecting.saturating_sub(effective_connecting);
    let spawn = requested_spawn
        .min(maximum.saturating_sub(counts.total))
        .min(connecting_budget);

    let above_maximum = counts.total.saturating_sub(maximum).min(counts.idle);
    let staged_idle_excess = if idle_excess_elapsed && above_maximum == 0 {
        counts
            .total
            .saturating_sub(scale_down_floor)
            .min(counts.idle)
            .min(usize::from(policy.scale_down_step))
    } else {
        0
    };
    PoolAdjustment {
        spawn,
        retire: above_maximum.max(staged_idle_excess),
    }
}

fn scale_up_reason(
    policy: &WorkerPolicy,
    counts: PoolCounts,
    effective_connecting: usize,
    desired_workers: usize,
) -> &'static str {
    if desired_workers > counts.total {
        return "server_demand";
    }
    let startup_deficit = counts.total < usize::from(policy.start_workers);
    let idle_deficit = counts.idle + effective_connecting < usize::from(policy.min_idle_workers);
    match (startup_deficit, idle_deficit) {
        (true, true) => "startup_and_idle_reserve",
        (true, false) => "startup",
        (false, true) => "idle_reserve",
        (false, false) => "none",
    }
}

fn scale_down_reason(
    policy: &WorkerPolicy,
    counts: PoolCounts,
    idle_excess_elapsed: bool,
    warm_active: bool,
) -> &'static str {
    if counts.total > usize::from(policy.max_workers) {
        "max_workers_reduced"
    } else if idle_excess_elapsed && warm_active {
        "burst_warm_staged"
    } else if idle_excess_elapsed {
        "idle_excess_elapsed"
    } else {
        "none"
    }
}

#[allow(clippy::too_many_arguments)]
fn scale_up_block(
    policy: &WorkerPolicy,
    counts: PoolCounts,
    effective_connecting: usize,
    max_connecting: usize,
    desired_workers: usize,
    adjustment: PoolAdjustment,
) -> Option<ScaleUpBlock> {
    let startup_deficit = counts.total < usize::from(policy.start_workers);
    let idle_deficit = counts.idle + effective_connecting < usize::from(policy.min_idle_workers);
    let demand_deficit = desired_workers > counts.total;
    if adjustment.spawn > 0 || !(startup_deficit || idle_deficit || demand_deficit) {
        return None;
    }
    if counts.total >= usize::from(policy.max_workers) {
        return Some(ScaleUpBlock::MaxWorkersReached);
    }
    if effective_connecting >= max_connecting {
        return Some(ScaleUpBlock::ConnectingLimitReached);
    }
    None
}

fn join_worker_indices(indices: &[usize]) -> String {
    indices
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn jittered_limit(base: u64, seed: u64, percent: u8) -> u64 {
    if base == 0 || percent == 0 {
        return base;
    }
    let spread = u64::from(percent).min(50);
    let mixed = seed
        .wrapping_add(1)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left((seed % 63) as u32);
    let offset = mixed % (spread.saturating_mul(2) + 1);
    let factor = 100_u64.saturating_sub(spread).saturating_add(offset);
    base.saturating_mul(factor).saturating_add(99) / 100
}

fn worker_should_recycle(
    policy: &WorkerPolicy,
    seed: u64,
    completed_requests: u64,
    connected_for: Duration,
) -> bool {
    let request_limit = jittered_limit(
        policy.max_requests_per_worker,
        seed,
        policy.recycle_jitter_percent,
    );
    let lifetime_limit = jittered_limit(
        policy.max_lifetime_seconds,
        seed,
        policy.recycle_jitter_percent,
    );
    (request_limit != 0 && completed_requests >= request_limit)
        || (lifetime_limit != 0 && connected_for >= Duration::from_secs(lifetime_limit))
}

fn next_reconnect_base(current: Duration, connected: bool) -> Duration {
    if connected {
        INITIAL_RECONNECT_DELAY
    } else {
        (current * 2).min(MAX_RECONNECT_DELAY)
    }
}

fn reconnect_delay(base: Duration, worker_index: usize, attempt: u64) -> Duration {
    let mixed = (worker_index as u64 + 1)
        .wrapping_mul(0x9E37_79B9)
        .rotate_left((attempt % 31) as u32)
        ^ attempt.wrapping_mul(0x85EB_CA6B);
    let percent = 80 + mixed % 21;
    let millis = base.as_millis().saturating_mul(u128::from(percent)) / 100;
    Duration::from_millis(millis.max(1).min(MAX_RECONNECT_DELAY.as_millis()) as u64)
}

struct IncomingRequest {
    request_id: String,
    method: String,
    path_and_query: String,
    headers: Vec<HeaderPair>,
    body: Vec<u8>,
}

#[allow(clippy::too_many_arguments)]
async fn receive_request(
    sink: &mut ClientSink,
    stream: &mut ClientStream,
    shutdown: &mut watch::Receiver<bool>,
    retire: &mut watch::Receiver<bool>,
    heartbeat: &mut HeartbeatTracker,
    heartbeat_interval: &mut Interval,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
    event_tx: &mpsc::Sender<WorkerEvent>,
    worker_index: usize,
    connected_at: Instant,
    completed_requests: u64,
) -> Result<Option<IncomingRequest>, String> {
    let Some(head) = receive_live_control(
        sink,
        stream,
        shutdown,
        retire,
        heartbeat,
        heartbeat_interval,
        policy_tx,
        connected_at,
        worker_index as u64,
        completed_requests,
    )
    .await?
    else {
        return Ok(None);
    };
    let ControlMessage::RequestHead {
        request_id,
        method,
        path_and_query,
        headers,
        demand,
    } = head
    else {
        return Err("expected request_head".into());
    };
    if let Some(demand) = demand {
        let _ = event_tx.send(WorkerEvent::Demand(demand)).await;
    }
    let _ = event_tx
        .send(WorkerEvent::State {
            worker_index,
            state: PoolWorkerState::Busy,
        })
        .await;

    let mut body = Vec::new();
    loop {
        let message = tokio::select! {
            _ = shutdown.changed() => return Err("tunnel stopped".into()),
            _ = heartbeat_interval.tick() => {
                send_heartbeat(sink, heartbeat).await?;
                continue;
            },
            message = stream.next() => message,
        };
        heartbeat.record_activity();
        match message {
            Some(Ok(Message::Binary(chunk))) => {
                if body.len().saturating_add(chunk.len()) > MAX_REQUEST_BODY_BYTES {
                    send_control(
                        sink,
                        &ControlMessage::Error {
                            request_id: Some(request_id.clone()),
                            message: "request body exceeds built-in tunnel limit".into(),
                        },
                    )
                    .await?;
                    return Err("request body exceeds built-in tunnel limit".into());
                }
                body.extend_from_slice(&chunk);
            }
            Some(Ok(Message::Text(text))) => {
                let control: ControlMessage =
                    serde_json::from_str(text.as_str()).map_err(|error| error.to_string())?;
                match control {
                    ControlMessage::RequestEnd { request_id: end_id } if end_id == request_id => {
                        break
                    }
                    ControlMessage::Cancel {
                        request_id: cancel_id,
                    } if cancel_id == request_id => return Err("request cancelled".into()),
                    _ => return Err("unexpected control message while receiving request".into()),
                }
            }
            Some(Ok(Message::Ping(payload))) => sink
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.to_string()),
        }
    }

    Ok(Some(IncomingRequest {
        request_id,
        method,
        path_and_query,
        headers,
        body,
    }))
}

async fn forward_request(
    config: &BuiltinTunnelConfig,
    http: &reqwest::Client,
    request: IncomingRequest,
    sink: &mut ClientSink,
    stream: &mut ClientStream,
    shutdown: &mut watch::Receiver<bool>,
    heartbeat: &mut HeartbeatTracker,
    heartbeat_interval: &mut Interval,
) -> Result<(), String> {
    let request_id = request.request_id.clone();
    if !request.path_and_query.starts_with('/') || request.path_and_query.starts_with("//") {
        return Err("server supplied an invalid relative request path".into());
    }
    let local_path = local_path_for_request(config, &request.path_and_query)?;
    let url = format!("{}{}", config.local_base_url, local_path);
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut builder = http.request(method, url).body(request.body);
    for header in request.headers {
        if is_hop_by_hop_header(&header.name) || header.name.eq_ignore_ascii_case(HOST.as_str()) {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(header.name.as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_str(&header.value) else {
            continue;
        };
        builder = builder.header(name, value);
    }

    let response_future = builder.send();
    tokio::pin!(response_future);
    let response = loop {
        tokio::select! {
            _ = shutdown.changed() => return Err("tunnel stopped".into()),
            _ = heartbeat_interval.tick() => {
                send_heartbeat(sink, heartbeat).await?;
            },
            response = &mut response_future => break response.map_err(|error| error.to_string())?,
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    heartbeat.record_activity();
                    let control: ControlMessage = serde_json::from_str(text.as_str())
                        .map_err(|error| error.to_string())?;
                    if matches!(control, ControlMessage::Cancel { request_id: ref id } if id == &request_id) {
                        return Ok(());
                    }
                    return Err("unexpected control message while waiting for local response".into());
                }
                Some(Ok(Message::Ping(payload))) => sink.send(Message::Pong(payload))
                    .await.map_err(|error| error.to_string()).map(|_| heartbeat.record_activity())?,
                Some(Ok(Message::Pong(_))) => heartbeat.record_activity(),
                Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(error.to_string()),
            }
        }
    };

    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            if is_hop_by_hop_header(name.as_str()) {
                return None;
            }
            value.to_str().ok().map(|value| HeaderPair {
                name: name.as_str().to_string(),
                value: value.to_string(),
            })
        })
        .collect();
    send_control(
        sink,
        &ControlMessage::ResponseHead {
            request_id: request_id.clone(),
            status,
            headers,
        },
    )
    .await?;

    let mut body = response.bytes_stream();
    loop {
        tokio::select! {
            _ = shutdown.changed() => return Err("tunnel stopped".into()),
            _ = heartbeat_interval.tick() => {
                send_heartbeat(sink, heartbeat).await?;
            },
            chunk = body.next() => match chunk {
                Some(Ok(chunk)) => sink.send(Message::Binary(chunk))
                    .await.map_err(|error| error.to_string())?,
                Some(Err(error)) => {
                    send_control(sink, &ControlMessage::Error {
                        request_id: Some(request_id.clone()),
                        message: error.to_string(),
                    }).await?;
                    return Ok(());
                }
                None => break,
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    heartbeat.record_activity();
                    let control: ControlMessage = serde_json::from_str(text.as_str())
                        .map_err(|error| error.to_string())?;
                    if matches!(control, ControlMessage::Cancel { request_id: ref id } if id == &request_id) {
                        return Ok(());
                    }
                    return Err("unexpected control message while streaming local response".into());
                }
                Some(Ok(Message::Ping(payload))) => sink.send(Message::Pong(payload))
                    .await.map_err(|error| error.to_string()).map(|_| heartbeat.record_activity())?,
                Some(Ok(Message::Pong(_))) => heartbeat.record_activity(),
                Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(error.to_string()),
            }
        }
    }
    send_control(
        sink,
        &ControlMessage::ResponseEnd {
            request_id: request_id.clone(),
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn receive_live_control(
    sink: &mut ClientSink,
    stream: &mut ClientStream,
    shutdown: &mut watch::Receiver<bool>,
    retire: &mut watch::Receiver<bool>,
    heartbeat: &mut HeartbeatTracker,
    heartbeat_interval: &mut Interval,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
    connected_at: Instant,
    worker_seed: u64,
    completed_requests: u64,
) -> Result<Option<ControlMessage>, String> {
    loop {
        if *shutdown.borrow() || *retire.borrow() {
            return Ok(None);
        }
        let policy = policy_tx.borrow().clone();
        if policy.as_ref().is_some_and(|policy| {
            worker_should_recycle(
                policy,
                worker_seed,
                completed_requests,
                Instant::now().saturating_duration_since(connected_at),
            )
        }) {
            return Ok(None);
        }
        let recycle_at = policy.as_ref().and_then(|policy| {
            let seconds = jittered_limit(
                policy.max_lifetime_seconds,
                worker_seed,
                policy.recycle_jitter_percent,
            );
            (seconds != 0).then(|| connected_at + Duration::from_secs(seconds))
        });
        let recycle_timer = async move {
            match recycle_at {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(recycle_timer);
        let message = tokio::select! {
            _ = shutdown.changed() => return Ok(None),
            _ = retire.changed() => return Ok(None),
            _ = &mut recycle_timer => return Ok(None),
            _ = heartbeat_interval.tick() => {
                send_heartbeat(sink, heartbeat).await?;
                continue;
            },
            message = stream.next() => message,
        };
        heartbeat.record_activity();
        match message {
            Some(Ok(Message::Text(text))) => {
                let control: ControlMessage =
                    serde_json::from_str(text.as_str()).map_err(|error| error.to_string())?;
                if let ControlMessage::PolicyUpdate { worker_policy } = control {
                    worker_policy.validate()?;
                    policy_tx.send_replace(Some(worker_policy));
                    continue;
                }
                return Ok(Some(control));
            }
            Some(Ok(Message::Ping(payload))) => sink
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
            Some(Ok(_)) => return Err("expected text control message".into()),
            Some(Err(error)) => return Err(error.to_string()),
        }
    }
}

async fn send_heartbeat(sink: &mut ClientSink, heartbeat: &HeartbeatTracker) -> Result<(), String> {
    if heartbeat.expired_at(Instant::now()) {
        return Err("WSS heartbeat timed out".into());
    }
    sink.send(Message::Ping(Vec::new().into()))
        .await
        .map_err(|error| error.to_string())
}

async fn receive_control(
    sink: &mut ClientSink,
    stream: &mut ClientStream,
) -> Result<ControlMessage, String> {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(text.as_str()).map_err(|error| error.to_string());
            }
            Some(Ok(Message::Ping(payload))) => sink
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
            Some(Ok(_)) => return Err("expected text control message".into()),
            Some(Err(error)) => return Err(error.to_string()),
        }
    }
}

async fn send_control(sink: &mut ClientSink, message: &ControlMessage) -> Result<(), String> {
    let encoded = serde_json::to_string(message).map_err(|error| error.to_string())?;
    sink.send(Message::Text(encoded.into()))
        .await
        .map_err(|error| error.to_string())
}

fn local_path_for_request(
    config: &BuiltinTunnelConfig,
    path_and_query: &str,
) -> Result<String, String> {
    if config.service == TunnelService::Mcp {
        return Ok(path_and_query.to_string());
    }
    let (path, query) = path_and_query
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((path_and_query, None));
    let suffix = path
        .strip_prefix(&config.route_prefix)
        .ok_or_else(|| "Actions request does not match registered route".to_string())?;
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return Err("Actions route prefix matched a partial segment".into());
    }
    let local = if suffix.is_empty() { "/" } else { suffix };
    Ok(match query {
        Some(query) => format!("{local}?{query}"),
        None => local.to_string(),
    })
}

struct ParsedBuiltinEndpoint {
    public_url: String,
    websocket_url: String,
    client_id: String,
    route_prefix: String,
}

fn builtin_endpoint_for_client(
    value: &str,
    service: TunnelService,
    client_id: &str,
) -> AppResult<ParsedBuiltinEndpoint> {
    if !valid_client_id(client_id) {
        return Err(AppError::Message(
            "內建隧道 Client ID 只能包含英文字母、數字、- 與 _。".into(),
        ));
    }
    let parsed = parse_builtin_endpoint(value, service)?;
    let mut url = reqwest::Url::parse(&parsed.public_url)
        .map_err(|_| AppError::Message("內建隧道公開網址格式無效。".into()))?;
    let path = match service {
        TunnelService::Mcp => format!("/builtin/clients/{client_id}/mcp"),
        TunnelService::Actions => format!("/builtin/actions/{client_id}"),
    };
    url.set_path(&path);
    parse_builtin_endpoint(url.as_str(), service)
}

fn parse_builtin_endpoint(value: &str, service: TunnelService) -> AppResult<ParsedBuiltinEndpoint> {
    let mut url = reqwest::Url::parse(value.trim())
        .map_err(|_| AppError::Message("內建隧道公開網址格式無效。".into()))?;
    if url.scheme() != "https" {
        return Err(AppError::Message("內建隧道公開網址必須使用 HTTPS。".into()));
    }
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::Message(
            "內建隧道公開網址不得包含帳號、密碼、query 或 fragment。".into(),
        ));
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.filter(|part| !part.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    let (client_id, route_prefix) = match service {
        TunnelService::Mcp
            if (segments.len() == 3 || segments.len() == 4)
                && segments[0] == "builtin"
                && segments[1] == "clients"
                && (segments.len() == 3 || segments[3] == "mcp") =>
        {
            (
                segments[2].to_string(),
                format!("/builtin/clients/{}", segments[2]),
            )
        }
        TunnelService::Actions
            if segments.len() == 3 && segments[0] == "builtin" && segments[1] == "actions" =>
        {
            (
                segments[2].to_string(),
                format!("/builtin/actions/{}", segments[2]),
            )
        }
        TunnelService::Mcp => {
            return Err(AppError::Message(
                "內建 MCP 網址必須使用 /builtin/clients/<client-id>/mcp。".into(),
            ));
        }
        TunnelService::Actions => {
            return Err(AppError::Message(
                "內建 Actions 網址必須使用 /builtin/actions/<client-id>。".into(),
            ));
        }
    };
    if !valid_client_id(&client_id) {
        return Err(AppError::Message(
            "內建隧道 Client ID 只能包含英文字母、數字、- 與 _。".into(),
        ));
    }

    let public_path = match service {
        TunnelService::Mcp => format!("{route_prefix}/mcp"),
        TunnelService::Actions => route_prefix.clone(),
    };
    url.set_path(&public_path);
    let public_url = url.as_str().trim_end_matches('/').to_string();
    url.set_scheme("wss")
        .map_err(|_| AppError::Message("無法建立內建 WSS 網址。".into()))?;
    url.set_path(WS_PATH);
    let websocket_url = url.to_string();

    Ok(ParsedBuiltinEndpoint {
        public_url,
        websocket_url,
        client_id,
        route_prefix,
    })
}

fn local_connect_host(bind_address: &str) -> String {
    match bind_address.trim() {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1".into(),
        value if value.contains(':') && !value.starts_with('[') => format!("[{value}]"),
        value => value.to_string(),
    }
}

fn append_log(path: &Path, line: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default();
        let _ = writeln!(file, "[{timestamp}] {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn single_worker_policy() -> WorkerPolicy {
        let mut policy = WorkerPolicy::default_for(TunnelService::Mcp);
        policy.start_workers = 1;
        policy.min_idle_workers = 1;
        policy.max_idle_workers = 1;
        policy.max_workers = 1;
        policy
    }

    #[test]
    fn parses_namespaced_mcp_endpoint() {
        let endpoint = parse_builtin_endpoint(
            "https://tunnel.example.com/builtin/clients/pc-a/mcp",
            TunnelService::Mcp,
        )
        .unwrap();
        assert_eq!(endpoint.client_id, "pc-a");
        assert_eq!(endpoint.route_prefix, "/builtin/clients/pc-a");
        assert_eq!(
            endpoint.websocket_url,
            "wss://tunnel.example.com/_tunnel/v1"
        );
    }

    #[test]
    fn upgrades_namespaced_mcp_base_url_to_endpoint() {
        let endpoint = parse_builtin_endpoint(
            "https://tunnel.example.com/builtin/clients/pc-a",
            TunnelService::Mcp,
        )
        .unwrap();
        assert_eq!(
            endpoint.public_url,
            "https://tunnel.example.com/builtin/clients/pc-a/mcp"
        );
        assert_eq!(endpoint.client_id, "pc-a");
    }

    #[test]
    fn replaces_bootstrap_client_id_with_server_assigned_id() {
        let endpoint = builtin_endpoint_for_client(
            "https://tunnel.example.com/builtin/clients/workspace-placeholder/mcp",
            TunnelService::Mcp,
            "pc-a",
        )
        .unwrap();
        assert_eq!(
            endpoint.public_url,
            "https://tunnel.example.com/builtin/clients/pc-a/mcp"
        );
        assert_eq!(endpoint.client_id, "pc-a");
    }

    #[test]
    fn actions_requests_strip_only_the_registered_prefix() {
        let config = BuiltinTunnelConfig {
            public_url: "https://example.com/builtin/actions/pc-a".into(),
            websocket_url: "wss://example.com/_tunnel/v1".into(),
            client_id: "pc-a".into(),
            service: TunnelService::Actions,
            route_prefix: "/builtin/actions/pc-a".into(),
            local_base_url: "http://127.0.0.1:7001".into(),
            device_id: "device-1".into(),
            signing_key: Arc::new(SigningKey::from_bytes(&[7_u8; 32])),
            log_path: PathBuf::new(),
        };
        assert_eq!(
            local_path_for_request(&config, "/builtin/actions/pc-a/openapi.json?x=1").unwrap(),
            "/openapi.json?x=1"
        );
        assert!(local_path_for_request(&config, "/builtin/actions/pc-ab").is_err());
    }

    #[test]
    fn enrollment_link_must_match_the_public_origin_and_path() {
        let url = parse_enrollment_url(
            "https://tunnel.example.com/builtin/clients/pc-a/mcp",
            "https://tunnel.example.com/_tunnel/enroll/abc123",
        )
        .unwrap();
        assert_eq!(url.path(), "/_tunnel/enroll/abc123");
        assert!(parse_enrollment_url(
            "https://tunnel.example.com/builtin/clients/pc-a/mcp",
            "https://other.example/_tunnel/enroll/abc123",
        )
        .is_err());
        assert!(parse_enrollment_url(
            "https://tunnel.example.com/builtin/clients/pc-a/mcp",
            "https://tunnel.example.com/_tunnel/enroll/abc123?copy=1",
        )
        .is_err());
    }

    #[test]
    fn rejects_non_namespaced_builtin_urls() {
        assert!(
            parse_builtin_endpoint("https://example.com/clients/pc-a/mcp", TunnelService::Mcp)
                .is_err()
        );
    }

    #[test]
    fn server_policy_updates_pool_metrics() {
        let metrics = BuiltinTunnelMetrics::new(1);
        let mut policy = WorkerPolicy::default_for(TunnelService::Mcp);
        policy.max_workers = 24;
        policy.revision = 7;
        metrics.set_policy(&policy);
        metrics.set_pool_counts(3, 2);

        let snapshot = metrics.snapshot();
        assert_eq!(snapshot.configured_workers, 24);
        assert_eq!(snapshot.idle_workers, 3);
        assert_eq!(snapshot.busy_workers, 2);
        assert_eq!(snapshot.policy_revision, 7);
    }

    #[test]
    fn reconnect_backoff_is_jittered_bounded_and_resets_after_connect() {
        let base = Duration::from_secs(8);
        let worker_zero = reconnect_delay(base, 0, 3);
        let worker_one = reconnect_delay(base, 1, 3);

        assert!(worker_zero >= Duration::from_millis(6_400));
        assert!(worker_zero <= base);
        assert!(worker_one >= Duration::from_millis(6_400));
        assert!(worker_one <= base);
        assert_ne!(worker_zero, worker_one);
        assert_eq!(next_reconnect_base(base, true), Duration::from_secs(1));
        assert_eq!(
            next_reconnect_base(Duration::from_secs(10), false),
            MAX_RECONNECT_DELAY
        );
    }

    #[test]
    fn connected_worker_guard_keeps_live_count_exact() {
        let metrics = Arc::new(BuiltinTunnelMetrics::new(8));
        assert_eq!(metrics.snapshot().connected_workers, 0);

        let first = ConnectedWorkerGuard::new(metrics.clone());
        let second = ConnectedWorkerGuard::new(metrics.clone());
        assert_eq!(metrics.snapshot().connected_workers, 2);

        drop(first);
        assert_eq!(metrics.snapshot().connected_workers, 1);
        drop(second);
        assert_eq!(metrics.snapshot().connected_workers, 0);
        assert_eq!(metrics.snapshot().configured_workers, 8);
    }

    #[test]
    fn availability_state_distinguishes_running_from_reconnecting() {
        let reconnecting = BuiltinTunnelSnapshot {
            configured_workers: 8,
            connected_workers: 0,
            idle_workers: 0,
            busy_workers: 0,
            recycled_workers: 0,
            policy_revision: 1,
            last_error: Some("offline".into()),
        };
        let running = BuiltinTunnelSnapshot {
            configured_workers: 8,
            connected_workers: 2,
            idle_workers: 1,
            busy_workers: 1,
            recycled_workers: 3,
            policy_revision: 2,
            last_error: None,
        };

        assert_eq!(reconnecting.availability_state(true), "reconnecting");
        assert_eq!(running.availability_state(true), "running");
        assert_eq!(running.availability_state(false), "stopped");
    }

    #[test]
    fn heartbeat_deadline_moves_forward_with_server_activity() {
        let started = Instant::now();
        let mut heartbeat = HeartbeatTracker::new_at(started);

        assert!(!heartbeat.expired_at(started + Duration::from_secs(44)));
        assert!(heartbeat.expired_at(started + Duration::from_secs(45)));

        heartbeat.record_activity_at(started + Duration::from_secs(30));
        assert!(!heartbeat.expired_at(started + Duration::from_secs(60)));
        assert!(heartbeat.expired_at(started + Duration::from_secs(75)));
    }

    #[test]
    fn dynamic_pool_plan_uses_demand_connecting_limits_and_staged_shrink() {
        let policy = coding_tools_tunnel_protocol::WorkerPolicy::default_for(TunnelService::Mcp);
        let max_connecting = configured_max_connecting(&policy);
        assert_eq!(max_connecting, 4);
        assert_eq!(configured_burst_warm_floor(&policy), 8);

        assert_eq!(
            pool_adjustment(
                &policy,
                PoolCounts {
                    total: 1,
                    connecting: 1,
                    idle: 0,
                    busy: 0,
                },
                1,
                max_connecting,
                0,
                false,
                4,
            ),
            PoolAdjustment {
                spawn: 3,
                retire: 0,
            }
        );
        assert_eq!(
            pool_adjustment(
                &policy,
                PoolCounts {
                    total: 4,
                    connecting: 0,
                    idle: 1,
                    busy: 3,
                },
                0,
                max_connecting,
                16,
                false,
                8,
            ),
            PoolAdjustment {
                spawn: 4,
                retire: 0,
            }
        );
        let connecting_limited = PoolCounts {
            total: 8,
            connecting: 4,
            idle: 0,
            busy: 4,
        };
        let connecting_adjustment =
            pool_adjustment(&policy, connecting_limited, 4, max_connecting, 16, false, 8);
        assert_eq!(connecting_adjustment.spawn, 0);
        assert_eq!(
            scale_up_block(
                &policy,
                connecting_limited,
                4,
                max_connecting,
                16,
                connecting_adjustment,
            ),
            Some(ScaleUpBlock::ConnectingLimitReached)
        );

        for (total, floor, expected_retire) in [(16, 8, 4), (12, 8, 4), (8, 8, 0), (8, 4, 4)] {
            assert_eq!(
                pool_adjustment(
                    &policy,
                    PoolCounts {
                        total,
                        connecting: 0,
                        idle: total,
                        busy: 0,
                    },
                    0,
                    max_connecting,
                    0,
                    true,
                    floor,
                )
                .retire,
                expected_retire,
            );
        }

        let maximum = PoolCounts {
            total: usize::from(policy.max_workers),
            connecting: 0,
            idle: 0,
            busy: usize::from(policy.max_workers),
        };
        assert_eq!(
            scale_up_block(
                &policy,
                maximum,
                0,
                max_connecting,
                usize::from(policy.max_workers).saturating_add(1),
                PoolAdjustment {
                    spawn: 0,
                    retire: 0,
                },
            ),
            Some(ScaleUpBlock::MaxWorkersReached)
        );
    }

    #[test]
    fn stale_connecting_workers_stop_counting_as_idle_reserve() {
        let (_retire_tx, retire_rx) = watch::channel(false);
        let (second_tx, _second_rx) = watch::channel(false);
        let now = Instant::now();
        let workers = HashMap::from([
            (
                1,
                ManagedWorker {
                    state: PoolWorkerState::Connecting,
                    connecting_since: now,
                    retire: _retire_tx,
                },
            ),
            (
                2,
                ManagedWorker {
                    state: PoolWorkerState::Connecting,
                    connecting_since: now - Duration::from_secs(2),
                    retire: second_tx,
                },
            ),
        ]);
        drop(retire_rx);
        assert_eq!(
            effective_connecting_workers(&workers, Duration::from_secs(1)),
            1
        );
        let policy = WorkerPolicy::default_for(TunnelService::Mcp);
        let counts = pool_counts(&workers);
        let adjustment = pool_adjustment(
            &policy,
            counts,
            1,
            configured_max_connecting(&policy),
            4,
            false,
            usize::from(policy.max_idle_workers),
        );
        assert_eq!(adjustment.spawn, 2);
    }

    #[test]
    fn worker_recycle_limits_are_jittered_and_checked_only_at_idle_boundaries() {
        let low = jittered_limit(500, 7, 10);
        let high = jittered_limit(500, 8, 10);
        assert!((450..=550).contains(&low));
        assert!((450..=550).contains(&high));
        assert_ne!(low, high);

        let policy = coding_tools_tunnel_protocol::WorkerPolicy::default_for(TunnelService::Mcp);
        assert!(!worker_should_recycle(
            &policy,
            7,
            449,
            Duration::from_secs(10)
        ));
        assert!(worker_should_recycle(
            &policy,
            7,
            low,
            Duration::from_secs(10)
        ));
        assert!(worker_should_recycle(
            &policy,
            7,
            1,
            Duration::from_secs(jittered_limit(3_600, 7, 10))
        ));
    }

    #[tokio::test]
    async fn worker_pool_bootstraps_grows_and_gracefully_shrinks_from_server_policy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind websocket test server");
        let address = listener.local_addr().expect("test server address");
        let mut grow_policy = single_worker_policy();
        grow_policy.start_workers = 3;
        grow_policy.min_idle_workers = 2;
        grow_policy.max_idle_workers = 3;
        grow_policy.max_workers = 3;
        let (policy_tx, policy_rx) = watch::channel(grow_policy.clone());
        let (ready_tx, mut ready_rx) = mpsc::channel(3);
        let (closed_tx, mut closed_rx) = mpsc::channel(3);
        let server = tokio::spawn(async move {
            let mut handlers = JoinSet::new();
            for connection_index in 0..3 {
                let (stream, _) = listener.accept().await.expect("accept worker");
                let ready_tx = ready_tx.clone();
                let closed_tx = closed_tx.clone();
                let mut updates = policy_rx.clone();
                handlers.spawn(async move {
                    let mut socket = tokio_tungstenite::accept_hdr_async(
                        stream,
                        |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                         mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                            response.headers_mut().insert(
                                SEC_WEBSOCKET_PROTOCOL,
                                WS_SUBPROTOCOL.parse().expect("subprotocol header"),
                            );
                            Ok(response)
                        },
                    )
                    .await
                    .expect("accept websocket");
                    socket
                        .send(Message::Text(
                            serde_json::to_string(&ControlMessage::Challenge {
                                nonce: format!("grow-{connection_index}"),
                                expires_at_unix_ms: unix_ms().saturating_add(10_000),
                            })
                            .expect("challenge json")
                            .into(),
                        ))
                        .await
                        .expect("send challenge");
                    assert!(matches!(
                        socket.next().await.expect("authenticate frame"),
                        Ok(Message::Text(_))
                    ));
                    let initial_policy = updates.borrow().clone();
                    socket
                        .send(Message::Text(
                            serde_json::to_string(&ControlMessage::HelloAck {
                                protocol_version: PROTOCOL_VERSION,
                                worker_policy: initial_policy,
                            })
                            .expect("hello ack json")
                            .into(),
                        ))
                        .await
                        .expect("send hello ack");
                    let ready = socket.next().await.expect("ready frame").expect("ready");
                    assert!(matches!(ready, Message::Text(_)));
                    ready_tx.send(()).await.expect("report ready");

                    updates.changed().await.expect("policy update");
                    let updated_policy = updates.borrow().clone();
                    socket
                        .send(Message::Text(
                            serde_json::to_string(&ControlMessage::PolicyUpdate {
                                worker_policy: updated_policy,
                            })
                            .expect("policy update json")
                            .into(),
                        ))
                        .await
                        .expect("send policy update");
                    while let Some(message) = socket.next().await {
                        if matches!(message, Ok(Message::Close(_))) {
                            break;
                        }
                    }
                    let _ = closed_tx.send(()).await;
                });
            }
            drop(ready_tx);
            drop(closed_tx);
            while handlers.join_next().await.is_some() {}
        });

        let log_dir = tempfile::tempdir().expect("log tempdir");
        let config = BuiltinTunnelConfig {
            public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
            websocket_url: format!("ws://{address}{WS_PATH}"),
            client_id: "pc-a".into(),
            service: TunnelService::Mcp,
            route_prefix: "/builtin/clients/pc-a".into(),
            local_base_url: "http://127.0.0.1:1".into(),
            device_id: "device-1".into(),
            signing_key: Arc::new(SigningKey::from_bytes(&[29_u8; 32])),
            log_path: log_dir.path().join("builtin-grow-test.log"),
        };
        let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (status_tx, _status_rx) = mpsc::channel(8);
        let pool_log_path = config.log_path.clone();
        let pool = tokio::spawn(run_worker_pool(
            config,
            shutdown_rx,
            status_tx,
            metrics.clone(),
        ));

        timeout(Duration::from_secs(4), async {
            for _ in 0..3 {
                ready_rx.recv().await.expect("worker ready");
            }
        })
        .await
        .expect("pool growth deadline");
        assert_eq!(metrics.snapshot().configured_workers, 3);
        assert_eq!(metrics.snapshot().connected_workers, 3);

        let mut shrink_policy = single_worker_policy();
        shrink_policy.revision = 2;
        policy_tx
            .send(shrink_policy)
            .expect("publish shrink policy");
        timeout(Duration::from_secs(4), async {
            while metrics.snapshot().connected_workers > 1 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            for _ in 0..2 {
                closed_rx.recv().await.expect("retired worker");
            }
        })
        .await
        .expect("pool shrink deadline");
        assert_eq!(metrics.snapshot().configured_workers, 1);
        assert_eq!(metrics.snapshot().connected_workers, 1);

        let _ = shutdown_tx.send(true);
        timeout(Duration::from_secs(2), pool)
            .await
            .expect("pool shutdown")
            .expect("pool task");
        timeout(Duration::from_secs(2), server)
            .await
            .expect("server shutdown")
            .expect("server task");
        let pool_log = std::fs::read_to_string(pool_log_path).expect("pool audit log");
        assert!(pool_log.contains("event=worker_policy_applied"));
        assert!(pool_log.contains("event=scale_up"));
        assert!(pool_log.contains("reason=startup"));
        assert!(pool_log.contains("event=scale_down"));
        assert!(pool_log.contains("reason=max_workers_reduced"));
    }

    #[tokio::test]
    async fn worker_recycles_after_request_limit_and_pool_replaces_it() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind local http server");
        let local_address = local_listener.local_addr().expect("local http address");
        let local_server = tokio::spawn(async move {
            let (mut stream, _) = local_listener.accept().await.expect("accept local request");
            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.expect("read local request");
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET "));
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .expect("write local response");
        });

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind websocket test server");
        let address = listener.local_addr().expect("test server address");
        let mut recycle_policy = single_worker_policy();
        recycle_policy.max_requests_per_worker = 1;
        recycle_policy.max_lifetime_seconds = 0;
        recycle_policy.recycle_jitter_percent = 0;
        let (replacement_tx, mut replacement_rx) = mpsc::channel(1);
        let server = tokio::spawn(async move {
            for connection_index in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept worker");
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        response.headers_mut().insert(
                            SEC_WEBSOCKET_PROTOCOL,
                            WS_SUBPROTOCOL.parse().expect("subprotocol header"),
                        );
                        Ok(response)
                    },
                )
                .await
                .expect("accept websocket");
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ControlMessage::Challenge {
                            nonce: format!("recycle-{connection_index}"),
                            expires_at_unix_ms: unix_ms().saturating_add(10_000),
                        })
                        .expect("challenge json")
                        .into(),
                    ))
                    .await
                    .expect("send challenge");
                assert!(matches!(
                    socket.next().await.expect("authenticate frame"),
                    Ok(Message::Text(_))
                ));
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ControlMessage::HelloAck {
                            protocol_version: PROTOCOL_VERSION,
                            worker_policy: recycle_policy.clone(),
                        })
                        .expect("hello ack json")
                        .into(),
                    ))
                    .await
                    .expect("send hello ack");
                let ready = socket.next().await.expect("ready frame").expect("ready");
                assert_eq!(
                    serde_json::from_str::<ControlMessage>(ready.into_text().unwrap().as_ref())
                        .expect("ready json"),
                    ControlMessage::Ready
                );

                if connection_index == 0 {
                    socket
                        .send(Message::Text(
                            serde_json::to_string(&ControlMessage::RequestHead {
                                request_id: "request-1".into(),
                                method: "GET".into(),
                                path_and_query: "/builtin/clients/pc-a/mcp".into(),
                                headers: Vec::new(),
                                demand: None,
                            })
                            .expect("request head")
                            .into(),
                        ))
                        .await
                        .expect("send request head");
                    socket
                        .send(Message::Text(
                            serde_json::to_string(&ControlMessage::RequestEnd {
                                request_id: "request-1".into(),
                            })
                            .expect("request end")
                            .into(),
                        ))
                        .await
                        .expect("send request end");
                    let mut response_finished = false;
                    while let Some(message) = socket.next().await {
                        match message.expect("response frame") {
                            Message::Text(text) => {
                                let control = serde_json::from_str::<ControlMessage>(&text)
                                    .expect("response control");
                                if matches!(control, ControlMessage::ResponseEnd { .. }) {
                                    response_finished = true;
                                }
                            }
                            Message::Close(_) => break,
                            _ => {}
                        }
                    }
                    assert!(response_finished);
                } else {
                    replacement_tx.send(()).await.expect("replacement ready");
                    while socket.next().await.is_some() {}
                }
            }
        });

        let log_dir = tempfile::tempdir().expect("log tempdir");
        let config = BuiltinTunnelConfig {
            public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
            websocket_url: format!("ws://{address}{WS_PATH}"),
            client_id: "pc-a".into(),
            service: TunnelService::Mcp,
            route_prefix: "/builtin/clients/pc-a".into(),
            local_base_url: format!("http://{local_address}"),
            device_id: "device-1".into(),
            signing_key: Arc::new(SigningKey::from_bytes(&[31_u8; 32])),
            log_path: log_dir.path().join("builtin-recycle-test.log"),
        };
        let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (status_tx, _status_rx) = mpsc::channel(8);
        let pool = tokio::spawn(run_worker_pool(
            config,
            shutdown_rx,
            status_tx,
            metrics.clone(),
        ));

        timeout(Duration::from_secs(4), replacement_rx.recv())
            .await
            .expect("replacement deadline")
            .expect("replacement worker");
        assert_eq!(metrics.snapshot().recycled_workers, 1);
        assert_eq!(metrics.snapshot().connected_workers, 1);

        let _ = shutdown_tx.send(true);
        timeout(Duration::from_secs(2), pool)
            .await
            .expect("pool shutdown")
            .expect("pool task");
        timeout(Duration::from_secs(2), server)
            .await
            .expect("server shutdown")
            .expect("server task");
        timeout(Duration::from_secs(2), local_server)
            .await
            .expect("local server shutdown")
            .expect("local server task");
    }

    #[tokio::test]
    async fn worker_pool_reconnects_after_authenticated_socket_closes() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind websocket test server");
        let address = listener.local_addr().expect("test server address");
        let server = tokio::spawn(async move {
            for connection_index in 0..2 {
                let (stream, _) = listener.accept().await.expect("accept worker");
                let mut socket = tokio_tungstenite::accept_hdr_async(
                    stream,
                    |_request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                     mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                        response.headers_mut().insert(
                            SEC_WEBSOCKET_PROTOCOL,
                            WS_SUBPROTOCOL.parse().expect("subprotocol header"),
                        );
                        Ok(response)
                    },
                )
                .await
                .expect("accept websocket");
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ControlMessage::Challenge {
                            nonce: format!("nonce-{connection_index}"),
                            expires_at_unix_ms: unix_ms().saturating_add(10_000),
                        })
                        .expect("challenge json")
                        .into(),
                    ))
                    .await
                    .expect("send challenge");
                let authenticate = socket
                    .next()
                    .await
                    .expect("authenticate frame")
                    .expect("authenticate frame");
                assert!(matches!(authenticate, Message::Text(_)));
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ControlMessage::HelloAck {
                            protocol_version: PROTOCOL_VERSION,
                            worker_policy: single_worker_policy(),
                        })
                        .expect("hello ack json")
                        .into(),
                    ))
                    .await
                    .expect("send hello ack");
                let ready = socket
                    .next()
                    .await
                    .expect("ready frame")
                    .expect("ready frame");
                let Message::Text(ready) = ready else {
                    panic!("expected ready text");
                };
                assert_eq!(
                    serde_json::from_str::<ControlMessage>(ready.as_ref()).expect("ready json"),
                    ControlMessage::Ready
                );

                if connection_index == 0 {
                    socket.close(None).await.expect("close first socket");
                } else {
                    while socket.next().await.is_some() {}
                }
            }
        });

        let log_dir = tempfile::tempdir().expect("log tempdir");
        let config = BuiltinTunnelConfig {
            public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
            websocket_url: format!("ws://{address}{WS_PATH}"),
            client_id: "pc-a".into(),
            service: TunnelService::Mcp,
            route_prefix: "/builtin/clients/pc-a".into(),
            local_base_url: "http://127.0.0.1:1".into(),
            device_id: "device-1".into(),
            signing_key: Arc::new(SigningKey::from_bytes(&[23_u8; 32])),
            log_path: log_dir.path().join("builtin-reconnect-test.log"),
        };
        let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (status_tx, mut status_rx) = mpsc::channel(8);
        let pool = tokio::spawn(run_worker_pool(
            config,
            shutdown_rx,
            status_tx,
            metrics.clone(),
        ));

        assert_eq!(status_rx.recv().await.expect("first connection"), Ok(()));
        let saw_reconnect = timeout(Duration::from_secs(3), async {
            let mut saw_disconnect = false;
            loop {
                match status_rx.recv().await.expect("worker status") {
                    Ok(()) if saw_disconnect => return true,
                    Ok(()) => {}
                    Err(_) => saw_disconnect = true,
                }
            }
        })
        .await
        .expect("reconnect deadline");
        assert!(saw_reconnect);
        assert_eq!(metrics.snapshot().connected_workers, 1);

        let _ = shutdown_tx.send(true);
        timeout(Duration::from_secs(2), pool)
            .await
            .expect("pool shutdown")
            .expect("pool task");
        timeout(Duration::from_secs(2), server)
            .await
            .expect("server shutdown")
            .expect("server task");
    }
}
