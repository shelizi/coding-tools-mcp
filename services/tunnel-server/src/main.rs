mod admin;
mod database;
mod device_auth;
mod node_update;
mod observability;
mod server_identity;
mod worker_policy;

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Request, Response, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use bytes::Bytes;
use coding_tools_tunnel_protocol::{
    decode_multiplex_binary_frame, encode_multiplex_binary_frame, expected_routes,
    is_hop_by_hop_header, is_retry_safe_mcp_request, is_retry_safe_tool_name,
    multiplex_server_ack_signing_payload, multiplex_server_challenge_signing_payload,
    route_matches, server_ack_signing_payload, server_challenge_signing_payload,
    split_server_ack_signing_payload, valid_client_id, ClientHello, ControlMessage,
    EnrollmentRequest, HeaderPair, MultiplexBinaryFrameKind, MultiplexConnectionPolicy,
    MultiplexConnectionRole, MultiplexControlMessage, MultiplexDemand, NodeUpdateOffer,
    NodeUpdateState, SplitSessionAuthProof, TunnelService, WorkerDemand, WorkerPolicy,
    CLIENT_ID_HEADER, CONNECTION_ID_HEADER, DEVICE_ID_HEADER, ENROLL_PATH_PREFIX,
    MAX_REQUEST_BODY_BYTES, MULTIPLEX_CONTROL_WS_SUBPROTOCOL, MULTIPLEX_DATA_WS_SUBPROTOCOL,
    MULTIPLEX_PROTOCOL_VERSION, MULTIPLEX_WS_SUBPROTOCOL, PROTOCOL_VERSION, SERVICE_HEADER,
    SESSION_ID_HEADER, WORKER_ID_HEADER, WS_PATH, WS_SUBPROTOCOL,
};
use database::DatabaseWriter;
use device_auth::{unix_ms, AllowedServices, DeviceAuthError, DeviceRegistry};
use observability::Observability;
use server_identity::ServerIdentity;
use sha2::{Digest, Sha256};
use tokio::sync::watch;
use tokio::sync::{mpsc, oneshot, Mutex, Notify};
use tokio::time::{interval, sleep, timeout, timeout_at, Instant, MissedTickBehavior};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, warn};
use tracing_subscriber::{prelude::*, EnvFilter};
use uuid::Uuid;
use worker_policy::WorkerPolicyStore;

const REQUEST_QUEUE_CAPACITY: usize = 128;
const CONTROL_REQUEST_QUEUE_CAPACITY: usize = 32;
const CONTROL_PENDING_REQUEST_CAPACITY: usize = 32;
const AVAILABLE_WORKER_CAPACITY: usize = 128;
const RESPONSE_BODY_CAPACITY: usize = 16;
const RESPONSE_HEAD_TIMEOUT: Duration = Duration::from_secs(30);
const RECONNECT_GRACE_TIMEOUT: Duration = Duration::from_secs(2);
const WORKER_POLL_INTERVAL: Duration = Duration::from_millis(25);
const MAX_SAFE_REQUEST_ATTEMPTS: u8 = 2;
const AUTH_CHALLENGE_TIMEOUT: Duration = Duration::from_secs(10);
const WORKER_LIVENESS_TIMEOUT: Duration = Duration::from_secs(45);
const DEFAULT_PUBLIC_ORIGIN: &str = "http://127.0.0.1:8088";

#[derive(Clone)]
struct AppState {
    registry: Registry,
    devices: DeviceRegistry,
    policies: WorkerPolicyStore,
    observability: Observability,
    server_identity: ServerIdentity,
    node_updates: node_update::NodeUpdateCatalog,
    multiplex_sessions: SplitSessionRegistry,
    max_request_body_bytes: usize,
    response_head_timeout: Duration,
    reconnect_grace_timeout: Duration,
    worker_liveness_timeout: Duration,
}

#[derive(Clone, Default)]
pub(crate) struct SplitSessionRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<SplitSessionState>>>>,
}

struct SplitSessionState {
    session_id: String,
    device_id: String,
    policy: MultiplexConnectionPolicy,
    routes: Mutex<HashMap<u32, SplitRouteState>>,
    next_route_id: AtomicUsize,
    next_stream_id: AtomicU64,
    active_data_connections: AtomicUsize,
    active_streams: AtomicUsize,
    control_attached: AtomicBool,
    control_generation: AtomicU64,
    control_revision: watch::Sender<u64>,
    route_revision: watch::Sender<u64>,
    shutdown: watch::Sender<bool>,
}

struct SplitRouteState {
    key: ClientKey,
    pool: ClientPool,
    _presence_guard: observability::ClientConnectionGuard,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct SplitSessionSnapshot {
    pub session_id: String,
    pub device_id: String,
    pub control_attached: bool,
    pub data_connections: usize,
    pub active_streams: usize,
    pub stream_capacity: usize,
    pub available_stream_slots: usize,
    pub queued_requests: usize,
    pub oldest_queue_wait_ms: u64,
    pub desired_connections: usize,
    pub streams_per_connection: u16,
    pub max_connections: u16,
    pub routes: Vec<SplitRouteSnapshot>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct SplitRouteSnapshot {
    pub route_id: u32,
    pub client_id: String,
    pub service: String,
    pub pending_requests: usize,
}

impl SplitSessionRegistry {
    async fn create(
        &self,
        device_id: String,
        policy: MultiplexConnectionPolicy,
    ) -> Arc<SplitSessionState> {
        let session_id = Uuid::new_v4().simple().to_string();
        let (control_revision, _) = watch::channel(0_u64);
        let (route_revision, _) = watch::channel(0_u64);
        let (shutdown, _) = watch::channel(false);
        let session = Arc::new(SplitSessionState {
            session_id: session_id.clone(),
            device_id,
            policy,
            routes: Mutex::new(HashMap::new()),
            next_route_id: AtomicUsize::new(1),
            next_stream_id: AtomicU64::new(1),
            active_data_connections: AtomicUsize::new(0),
            active_streams: AtomicUsize::new(0),
            control_attached: AtomicBool::new(true),
            control_generation: AtomicU64::new(1),
            control_revision,
            route_revision,
            shutdown,
        });
        self.inner.lock().await.insert(session_id, session.clone());
        session
    }

    async fn get(&self, session_id: &str) -> Option<Arc<SplitSessionState>> {
        self.inner.lock().await.get(session_id).cloned()
    }

    pub(crate) async fn snapshots(&self) -> Vec<SplitSessionSnapshot> {
        let sessions = self
            .inner
            .lock()
            .await
            .values()
            .cloned()
            .collect::<Vec<_>>();
        let mut snapshots = Vec::with_capacity(sessions.len());
        for session in sessions {
            let routes = session.routes.lock().await;
            let route_snapshots = routes
                .iter()
                .map(|(route_id, route)| SplitRouteSnapshot {
                    route_id: *route_id,
                    client_id: route.key.client_id.clone(),
                    service: route.key.service.as_str().to_string(),
                    pending_requests: route.pool.pending_requests(),
                })
                .collect::<Vec<_>>();
            let queued_requests = route_snapshots
                .iter()
                .map(|route| route.pending_requests)
                .sum();
            let oldest_queue_wait_ms = routes
                .values()
                .map(|route| route.pool.pending_queue.oldest_wait_ms())
                .max()
                .unwrap_or(0);
            let data_connections = session.active_data_connections.load(Ordering::Acquire);
            let active_streams = session.active_streams.load(Ordering::Acquire);
            let streams_per_connection = session.policy.streams_per_connection;
            let stream_capacity =
                data_connections.saturating_mul(usize::from(streams_per_connection));
            let desired_connections = active_streams
                .saturating_add(queued_requests)
                .div_ceil(usize::from(streams_per_connection))
                .max(usize::from(session.policy.start_connections))
                .min(usize::from(session.policy.max_connections));
            snapshots.push(SplitSessionSnapshot {
                session_id: session.session_id.clone(),
                device_id: session.device_id.clone(),
                control_attached: session.control_is_attached(),
                data_connections,
                active_streams,
                stream_capacity,
                available_stream_slots: stream_capacity.saturating_sub(active_streams),
                queued_requests,
                oldest_queue_wait_ms,
                desired_connections,
                streams_per_connection,
                max_connections: session.policy.max_connections,
                routes: route_snapshots,
            });
        }
        snapshots.sort_by(|left, right| left.session_id.cmp(&right.session_id));
        snapshots
    }

    async fn reattach(
        &self,
        session_id: &str,
        device_id: &str,
    ) -> Result<(Arc<SplitSessionState>, u64), String> {
        let inner = self.inner.lock().await;
        let session = inner
            .get(session_id)
            .cloned()
            .ok_or_else(|| "split control session is unavailable".to_string())?;
        if session.device_id != device_id {
            return Err("split control session belongs to a different device".into());
        }
        let generation = session
            .attach_control()
            .ok_or_else(|| "split control session is already attached".to_string())?;
        Ok((session, generation))
    }

    async fn remove_if_detached(&self, session_id: &str, generation: u64) {
        let mut inner = self.inner.lock().await;
        let should_remove = inner.get(session_id).is_some_and(|session| {
            !session.control_is_attached()
                && session.control_generation.load(Ordering::Acquire) == generation
        });
        if should_remove {
            if let Some(session) = inner.remove(session_id) {
                let _ = session.shutdown.send(true);
            }
        }
    }

    async fn remove(&self, session_id: &str) {
        if let Some(session) = self.inner.lock().await.remove(session_id) {
            let _ = session.shutdown.send(true);
        }
    }
}

impl SplitSessionState {
    fn control_is_attached(&self) -> bool {
        self.control_attached.load(Ordering::Acquire)
    }

    fn current_control_generation(&self) -> u64 {
        self.control_generation.load(Ordering::Acquire)
    }

    fn attach_control(&self) -> Option<u64> {
        if self
            .control_attached
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return None;
        }
        let generation = self
            .control_generation
            .fetch_add(1, Ordering::AcqRel)
            .saturating_add(1);
        let next = route_revision_value(&self.control_revision).saturating_add(1);
        self.control_revision.send_replace(next);
        Some(generation)
    }

    fn detach_control(&self, generation: u64) -> bool {
        if self.control_generation.load(Ordering::Acquire) != generation {
            return false;
        }
        if self
            .control_attached
            .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let next = route_revision_value(&self.control_revision).saturating_add(1);
        self.control_revision.send_replace(next);
        true
    }

    async fn register_route(
        &self,
        registry: &Registry,
        observability: &Observability,
        key: ClientKey,
    ) -> Result<(u32, bool), String> {
        let mut routes = self.routes.lock().await;
        if let Some((route_id, _)) = routes.iter().find(|(_, route)| route.key == key) {
            return Ok((*route_id, false));
        }
        let pool = registry
            .register_owned(key.clone(), &self.device_id)
            .await?;
        let data_connections = self.active_data_connections.load(Ordering::Acquire);
        if data_connections > 0 {
            pool.active_workers
                .fetch_add(data_connections, Ordering::AcqRel);
        }
        let route_id = self.next_route_id.fetch_add(1, Ordering::AcqRel).max(1) as u32;
        let presence_guard = observability.connect_client_route(
            &format!("split:{}:{route_id}", self.session_id),
            &self.device_id,
            &key.client_id,
            key.service.as_str(),
            "split",
        );
        routes.insert(
            route_id,
            SplitRouteState {
                key,
                pool,
                _presence_guard: presence_guard,
            },
        );
        let next = route_revision_value(&self.route_revision).saturating_add(1);
        self.route_revision.send_replace(next);
        Ok((route_id, true))
    }

    async fn unregister_route(&self, route_id: u32) -> bool {
        let mut routes = self.routes.lock().await;
        let Some(route) = routes.remove(&route_id) else {
            return false;
        };
        let data_connections = self.active_data_connections.load(Ordering::Acquire);
        if data_connections > 0 {
            route
                .pool
                .active_workers
                .fetch_sub(data_connections, Ordering::AcqRel);
        }
        let next = route_revision_value(&self.route_revision).saturating_add(1);
        self.route_revision.send_replace(next);
        true
    }

    async fn route_snapshot(&self) -> Vec<(u32, ClientPool)> {
        self.routes
            .lock()
            .await
            .iter()
            .map(|(id, route)| (*id, route.pool.clone()))
            .collect()
    }

    async fn attach_data_connection(&self) -> bool {
        if self
            .active_data_connections
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < usize::from(self.policy.max_connections)).then_some(current + 1)
            })
            .is_err()
        {
            return false;
        }
        for (_, pool) in self.route_snapshot().await {
            pool.active_workers.fetch_add(1, Ordering::AcqRel);
        }
        true
    }

    async fn detach_data_connection(&self) {
        self.active_data_connections.fetch_sub(1, Ordering::AcqRel);
        for (_, pool) in self.route_snapshot().await {
            pool.active_workers.fetch_sub(1, Ordering::AcqRel);
        }
    }

    async fn demand(&self) -> MultiplexDemand {
        let routes = self.route_snapshot().await;
        let queued = routes
            .iter()
            .map(|(_, pool)| pool.pending_requests())
            .sum::<usize>();
        let oldest_queue_wait_ms = routes
            .iter()
            .map(|(_, pool)| pool.pending_queue.oldest_wait_ms())
            .max()
            .unwrap_or(0);
        let active = self.active_streams.load(Ordering::Acquire);
        let connections = self.active_data_connections.load(Ordering::Acquire);
        let stream_limit = usize::from(self.policy.streams_per_connection);
        let capacity = connections.saturating_mul(stream_limit);
        let desired = active
            .saturating_add(queued)
            .div_ceil(stream_limit)
            .max(usize::from(self.policy.start_connections))
            .min(usize::from(self.policy.max_connections));
        MultiplexDemand {
            queued_requests: queued.min(usize::from(u16::MAX)) as u16,
            oldest_queue_wait_ms,
            active_streams: active.min(usize::from(u16::MAX)) as u16,
            available_stream_slots: capacity.saturating_sub(active).min(usize::from(u16::MAX))
                as u16,
            desired_connections: desired as u16,
        }
    }
}

fn route_revision_value(sender: &watch::Sender<u64>) -> u64 {
    *sender.borrow()
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct ClientKey {
    client_id: String,
    service: TunnelService,
}

#[derive(Clone)]
struct ClientPool {
    control_request_tx: mpsc::Sender<ProxyJob>,
    execution_request_tx: mpsc::Sender<ProxyJob>,
    available_tx: mpsc::Sender<AvailableWorker>,
    update_tx: watch::Sender<Option<NodeUpdateOffer>>,
    active_workers: Arc<AtomicUsize>,
    control_pending_requests: Arc<AtomicUsize>,
    execution_pending_requests: Arc<AtomicUsize>,
    pending_queue: Arc<PendingQueueTracker>,
}

impl ClientPool {
    fn pending_requests(&self) -> usize {
        self.control_pending_requests.load(Ordering::Acquire)
            + self.execution_pending_requests.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
struct RouteEntry {
    prefix: String,
    key: ClientKey,
    pool: ClientPool,
}

#[derive(Clone)]
struct RouteMatch {
    key: ClientKey,
    pool: ClientPool,
}

#[derive(Clone)]
struct OwnedClientPool {
    owner_device_id: Option<String>,
    pool: ClientPool,
}

#[derive(Default)]
struct RegistryInner {
    pools: HashMap<ClientKey, OwnedClientPool>,
    routes: Vec<RouteEntry>,
}

#[derive(Clone, Default)]
struct Registry {
    inner: Arc<Mutex<RegistryInner>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RequestLane {
    Control,
    Execution,
}

impl RequestLane {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Execution => "execution",
        }
    }
}

const CONTROL_WORKER_RESERVE: usize = 1;

struct ProxyJob {
    request_id: String,
    method: String,
    path_and_query: String,
    headers: Vec<HeaderPair>,
    body: Bytes,
    lane: RequestLane,
    attempt: u8,
    enqueued_at: Instant,
    policy: WorkerPolicy,
    active_workers: Arc<AtomicUsize>,
    demand: Option<WorkerDemand>,
    assigned: Option<oneshot::Sender<AssignmentInfo>>,
    pending_slot: Option<PendingRequestSlot>,
    response_head: oneshot::Sender<Result<ResponseHeadData, String>>,
    response_body: mpsc::Sender<Result<Bytes, io::Error>>,
    cancelled: watch::Receiver<bool>,
}

impl ProxyJob {
    fn abandoned(&self) -> bool {
        self.response_head.is_closed()
            || self
                .assigned
                .as_ref()
                .is_none_or(oneshot::Sender::is_closed)
            || *self.cancelled.borrow()
    }
}

#[derive(Debug, Clone, Copy)]
struct AssignmentInfo {
    queue_wait_ms: u64,
}

#[derive(Default)]
struct PendingQueueTracker {
    next_id: AtomicU64,
    entries: StdMutex<BTreeMap<u64, Instant>>,
    slot_available: Notify,
}

impl PendingQueueTracker {
    fn enter(&self) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        self.entries
            .lock()
            .expect("pending queue tracker poisoned")
            .insert(id, Instant::now());
        id
    }

    fn exit(&self, id: u64) {
        self.entries
            .lock()
            .expect("pending queue tracker poisoned")
            .remove(&id);
    }

    fn oldest_wait_ms(&self) -> u64 {
        let oldest = self
            .entries
            .lock()
            .expect("pending queue tracker poisoned")
            .values()
            .min()
            .copied();
        oldest
            .map(|started| started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0)
    }
}

struct PendingRequestSlot {
    pending_requests: Arc<AtomicUsize>,
    pending_queue: Arc<PendingQueueTracker>,
    pending_id: u64,
    observability: Observability,
    released: bool,
}

impl PendingRequestSlot {
    fn try_new(
        pending_requests: Arc<AtomicUsize>,
        pending_queue: Arc<PendingQueueTracker>,
        maximum: usize,
        observability: Observability,
    ) -> Option<Self> {
        pending_requests
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < maximum).then_some(current + 1)
            })
            .ok()?;
        let pending_id = pending_queue.enter();
        observability.queue_enter();
        Some(Self {
            pending_requests,
            pending_queue,
            pending_id,
            observability,
            released: false,
        })
    }

    async fn acquire_until(
        pending_requests: Arc<AtomicUsize>,
        pending_queue: Arc<PendingQueueTracker>,
        maximum: usize,
        observability: Observability,
        deadline: Instant,
    ) -> Option<Self> {
        loop {
            let notified = pending_queue.slot_available.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(slot) = Self::try_new(
                pending_requests.clone(),
                pending_queue.clone(),
                maximum,
                observability.clone(),
            ) {
                return Some(slot);
            }
            if timeout_at(deadline, notified).await.is_err() {
                return None;
            }
        }
    }
    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.pending_queue.exit(self.pending_id);
        self.pending_requests.fetch_sub(1, Ordering::AcqRel);
        self.observability.queue_exit();
        self.pending_queue.slot_available.notify_one();
    }
}

impl Drop for PendingRequestSlot {
    fn drop(&mut self) {
        self.release();
    }
}

struct ResponseHeadData {
    status: u16,
    headers: Vec<HeaderPair>,
}

#[derive(Clone)]
struct SharedWorkerClaim {
    claimed: Arc<AtomicBool>,
    generation: Arc<AtomicUsize>,
    expected_generation: usize,
}

impl SharedWorkerClaim {
    fn is_current(&self) -> bool {
        self.generation.load(Ordering::Acquire) == self.expected_generation
    }

    fn try_claim(&self) -> bool {
        self.is_current()
            && self
                .claimed
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            && self.is_current()
    }

    fn release(&self) {
        if self.is_current() {
            self.claimed.store(false, Ordering::Release);
        }
    }
}

struct AvailableWorker {
    assign: oneshot::Sender<ProxyJob>,
    shared_claim: Option<SharedWorkerClaim>,
}

impl AvailableWorker {
    fn try_claim(&self) -> bool {
        self.shared_claim
            .as_ref()
            .is_none_or(SharedWorkerClaim::try_claim)
    }

    fn release_claim(&self) {
        if let Some(claim) = &self.shared_claim {
            claim.release();
        }
    }
}

fn discard_closed_workers(workers: &mut VecDeque<AvailableWorker>) -> usize {
    let before = workers.len();
    workers.retain(|worker| {
        !worker.assign.is_closed()
            && worker
                .shared_claim
                .as_ref()
                .is_none_or(SharedWorkerClaim::is_current)
    });
    before.saturating_sub(workers.len())
}

struct ActiveWorkerGuard {
    active_workers: Arc<AtomicUsize>,
}

impl ActiveWorkerGuard {
    fn try_new(active_workers: Arc<AtomicUsize>, maximum: usize) -> Option<Self> {
        active_workers
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
                (current < maximum).then_some(current + 1)
            })
            .ok()?;
        Some(Self { active_workers })
    }
}

impl Drop for ActiveWorkerGuard {
    fn drop(&mut self) {
        self.active_workers.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Registry {
    fn new_pool() -> ClientPool {
        let (control_request_tx, control_request_rx) =
            mpsc::channel(CONTROL_REQUEST_QUEUE_CAPACITY);
        let (execution_request_tx, execution_request_rx) = mpsc::channel(REQUEST_QUEUE_CAPACITY);
        let (available_tx, available_rx) = mpsc::channel(AVAILABLE_WORKER_CAPACITY);
        let (update_tx, _) = watch::channel(None);
        let pool = ClientPool {
            control_request_tx,
            execution_request_tx,
            available_tx,
            update_tx,
            active_workers: Arc::new(AtomicUsize::new(0)),
            control_pending_requests: Arc::new(AtomicUsize::new(0)),
            execution_pending_requests: Arc::new(AtomicUsize::new(0)),
            pending_queue: Arc::new(PendingQueueTracker::default()),
        };
        tokio::spawn(dispatch_requests(
            control_request_rx,
            execution_request_rx,
            available_rx,
        ));
        pool
    }

    fn install_routes(inner: &mut RegistryInner, key: &ClientKey, pool: &ClientPool) {
        let mut replaced = false;
        for route in inner.routes.iter_mut().filter(|route| route.key == *key) {
            route.pool = pool.clone();
            replaced = true;
        }
        if !replaced {
            for prefix in expected_routes(&key.client_id, key.service) {
                inner.routes.push(RouteEntry {
                    prefix,
                    key: key.clone(),
                    pool: pool.clone(),
                });
            }
            inner
                .routes
                .sort_by_key(|route| std::cmp::Reverse(route.prefix.len()));
        }
    }

    #[cfg(test)]
    async fn register(&self, key: ClientKey) -> ClientPool {
        let mut inner = self.inner.lock().await;
        if let Some(entry) = inner.pools.get(&key) {
            return entry.pool.clone();
        }
        let pool = Self::new_pool();
        Self::install_routes(&mut inner, &key, &pool);
        inner.pools.insert(
            key,
            OwnedClientPool {
                owner_device_id: None,
                pool: pool.clone(),
            },
        );
        pool
    }

    async fn register_owned(&self, key: ClientKey, device_id: &str) -> Result<ClientPool, String> {
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.pools.get(&key).cloned() {
            match existing.owner_device_id.as_deref() {
                Some(owner) if owner == device_id => return Ok(existing.pool),
                Some(_) if existing.pool.active_workers.load(Ordering::Acquire) > 0 => {
                    return Err("route is already owned by another device".into());
                }
                None => {
                    if let Some(entry) = inner.pools.get_mut(&key) {
                        entry.owner_device_id = Some(device_id.to_string());
                        return Ok(entry.pool.clone());
                    }
                }
                Some(_) => {
                    let pool = Self::new_pool();
                    Self::install_routes(&mut inner, &key, &pool);
                    inner.pools.insert(
                        key,
                        OwnedClientPool {
                            owner_device_id: Some(device_id.to_string()),
                            pool: pool.clone(),
                        },
                    );
                    return Ok(pool);
                }
            }
        }

        let pool = Self::new_pool();
        Self::install_routes(&mut inner, &key, &pool);
        inner.pools.insert(
            key,
            OwnedClientPool {
                owner_device_id: Some(device_id.to_string()),
                pool: pool.clone(),
            },
        );
        Ok(pool)
    }

    async fn dispatch_node_update(&self, client_id: &str, offer: NodeUpdateOffer) -> usize {
        let inner = self.inner.lock().await;
        let key = ClientKey {
            client_id: client_id.to_string(),
            service: TunnelService::Mcp,
        };
        let Some(entry) = inner.pools.get(&key) else {
            return 0;
        };
        let targets = entry.pool.active_workers.load(Ordering::Acquire);
        entry.pool.update_tx.send_replace(Some(offer));
        targets
    }

    async fn clear_node_update(&self, client_id: &str, update_id: &str) -> bool {
        let inner = self.inner.lock().await;
        let key = ClientKey {
            client_id: client_id.to_string(),
            service: TunnelService::Mcp,
        };
        let Some(entry) = inner.pools.get(&key) else {
            return false;
        };
        let matches_current = entry
            .pool
            .update_tx
            .borrow()
            .as_ref()
            .is_some_and(|offer| offer.update_id == update_id);
        if matches_current {
            entry.pool.update_tx.send_replace(None);
        }
        matches_current
    }

    async fn lookup(&self, path: &str) -> Option<RouteMatch> {
        let inner = self.inner.lock().await;
        inner
            .routes
            .iter()
            .find(|entry| route_matches(&entry.prefix, path))
            .map(|entry| RouteMatch {
                key: entry.key.clone(),
                pool: entry.pool.clone(),
            })
    }
}

async fn dispatch_requests(
    mut control_request_rx: mpsc::Receiver<ProxyJob>,
    mut execution_request_rx: mpsc::Receiver<ProxyJob>,
    mut available_rx: mpsc::Receiver<AvailableWorker>,
) {
    let mut control_jobs: VecDeque<ProxyJob> = VecDeque::new();
    let mut execution_jobs: VecDeque<ProxyJob> = VecDeque::new();
    let mut workers: VecDeque<AvailableWorker> = VecDeque::new();
    let mut cleanup_tick = interval(Duration::from_millis(100));
    cleanup_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        control_jobs.retain(|job| !job.abandoned());
        execution_jobs.retain(|job| !job.abandoned());
        discard_closed_workers(&mut workers);
        while !workers.is_empty() && (!control_jobs.is_empty() || !execution_jobs.is_empty()) {
            let execution_can_run = execution_jobs.front().is_some_and(|job| {
                let connected = job.active_workers.load(Ordering::Acquire);
                let reserve =
                    usize::from(connected > CONTROL_WORKER_RESERVE) * CONTROL_WORKER_RESERVE;
                workers.len() > reserve
            });
            let mut job = if let Some(job) = control_jobs.pop_front() {
                job
            } else if execution_can_run {
                execution_jobs.pop_front().expect("execution queue checked")
            } else {
                break;
            };
            let worker = workers.pop_front().expect("worker queue checked");
            if !worker.try_claim() {
                match job.lane {
                    RequestLane::Control => control_jobs.push_front(job),
                    RequestLane::Execution => execution_jobs.push_front(job),
                }
                continue;
            }
            if job.abandoned() {
                worker.release_claim();
                workers.push_front(worker);
                continue;
            }

            let queued_requests = job.pending_slot.as_ref().map_or(1, |slot| {
                slot.pending_requests.load(Ordering::Acquire).max(1)
            });
            let connected_workers = job.active_workers.load(Ordering::Acquire);
            let idle_workers = workers.len().saturating_add(1).min(connected_workers);
            let busy_workers = connected_workers.saturating_sub(idle_workers);
            let desired_workers = connected_workers
                .max(
                    busy_workers
                        .saturating_add(queued_requests)
                        .saturating_add(usize::from(job.policy.min_idle_workers)),
                )
                .min(usize::from(job.policy.max_workers));
            let queue_wait_ms = job
                .enqueued_at
                .elapsed()
                .as_millis()
                .min(u128::from(u64::MAX)) as u64;
            job.demand = Some(WorkerDemand {
                queued_requests: queued_requests.min(usize::from(u16::MAX)) as u16,
                oldest_queue_wait_ms: queue_wait_ms,
                desired_workers: desired_workers.min(usize::from(u16::MAX)) as u16,
            });
            let assigned = job.assigned.take();
            let mut pending_slot = job.pending_slot.take();
            let shared_claim = worker.shared_claim.clone();
            match worker.assign.send(job) {
                Ok(()) => {
                    if let Some(slot) = pending_slot.as_mut() {
                        slot.release();
                    }
                    if let Some(assigned) = assigned {
                        let _ = assigned.send(AssignmentInfo { queue_wait_ms });
                    }
                }
                Err(mut job) => {
                    if let Some(claim) = &shared_claim {
                        claim.release();
                    }
                    job.assigned = assigned;
                    job.pending_slot = pending_slot;
                    match job.lane {
                        RequestLane::Control => control_jobs.push_front(job),
                        RequestLane::Execution => execution_jobs.push_front(job),
                    }
                    discard_closed_workers(&mut workers);
                }
            }
        }

        tokio::select! {
            biased;
            job = control_request_rx.recv(), if !control_request_rx.is_closed() => match job {
                Some(job) => control_jobs.push_back(job),
                None => {}
            },
            job = execution_request_rx.recv(), if !execution_request_rx.is_closed() => match job {
                Some(job) => execution_jobs.push_back(job),
                None => {}
            },
            worker = available_rx.recv() => match worker {
                Some(worker) => workers.push_back(worker),
                None => break,
            },
            _ = cleanup_tick.tick() => {}
        }
        if control_request_rx.is_closed()
            && execution_request_rx.is_closed()
            && control_jobs.is_empty()
            && execution_jobs.is_empty()
        {
            break;
        }
    }

    for job in control_jobs.into_iter().chain(execution_jobs) {
        let _ = job
            .response_head
            .send(Err("內建隧道 dispatcher 已停止。".into()));
    }
}

#[tokio::main]
async fn main() {
    let database_path = std::env::var_os("CODING_TOOLS_TUNNEL_DB")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("tunnel-data/tunnel.db"));
    let log_directory = std::env::var_os("CODING_TOOLS_TUNNEL_LOG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            database_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| FsPath::new("."))
                .join("logs")
        });
    let _trace_guard = init_tracing(&log_directory).expect("failed to initialize file logging");
    let database = DatabaseWriter::open(&database_path).expect("failed to open tunnel database");
    let server_identity_path = std::env::var_os("CODING_TOOLS_TUNNEL_SERVER_IDENTITY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            database_path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or_else(|| FsPath::new("."))
                .join("server-identity.json")
        });
    let server_identity = ServerIdentity::load_or_create(&server_identity_path)
        .expect("failed to load tunnel server identity");
    let devices = DeviceRegistry::from_writer(database.clone())
        .expect("failed to open tunnel device registry");
    let policies = WorkerPolicyStore::from_writer(database.clone())
        .expect("failed to open worker policy store");
    match handle_cli(&devices) {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }

    let bind = std::env::var("CODING_TOOLS_TUNNEL_BIND")
        .unwrap_or_else(|_| "0.0.0.0:8088".to_string())
        .parse::<SocketAddr>()
        .expect("CODING_TOOLS_TUNNEL_BIND must be a socket address");
    let admin_bind = std::env::var("CODING_TOOLS_TUNNEL_ADMIN_BIND")
        .ok()
        .map(|value| {
            value
                .parse::<SocketAddr>()
                .expect("CODING_TOOLS_TUNNEL_ADMIN_BIND must be a socket address")
        });
    let admin_config = if admin_bind.is_some() {
        Some(
            admin::load_admin_config()
                .expect("failed to load tunnel admin login configuration")
                .expect(
                    "CODING_TOOLS_TUNNEL_ADMIN_USERNAME and CODING_TOOLS_TUNNEL_ADMIN_PASSWORD_FILE or CODING_TOOLS_TUNNEL_ADMIN_PASSWORD are required when the admin listener is enabled",
                ),
        )
    } else {
        None
    };
    let public_origin = std::env::var("CODING_TOOLS_TUNNEL_PUBLIC_ORIGIN")
        .unwrap_or_else(|_| DEFAULT_PUBLIC_ORIGIN.into());
    let node_updates = node_update::NodeUpdateCatalog::from_env(&public_origin)
        .expect("failed to load Node Agent Gitea update configuration");
    let observability = Observability::from_database(database.clone())
        .expect("failed to initialize persistent observability logs");
    observability.log(
        "info",
        "server",
        "tunnel server starting",
        None,
        None,
        None,
        None,
    );
    let state = AppState {
        registry: Registry::default(),
        devices: devices.clone(),
        policies: policies.clone(),
        observability: observability.clone(),
        server_identity,
        node_updates: node_updates.clone(),
        multiplex_sessions: SplitSessionRegistry::default(),
        max_request_body_bytes: std::env::var("CODING_TOOLS_TUNNEL_MAX_BODY_BYTES")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(MAX_REQUEST_BODY_BYTES),
        response_head_timeout: std::env::var("CODING_TOOLS_TUNNEL_RESPONSE_HEAD_TIMEOUT_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(RESPONSE_HEAD_TIMEOUT),
        reconnect_grace_timeout: std::env::var("CODING_TOOLS_TUNNEL_RECONNECT_GRACE_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(RECONNECT_GRACE_TIMEOUT),
        worker_liveness_timeout: WORKER_LIVENESS_TIMEOUT,
    };

    let admin_registry = state.registry.clone();
    node_updates.spawn_polling(observability.clone());
    let admin_multiplex_sessions = state.multiplex_sessions.clone();
    let app = build_app(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .expect("failed to bind tunnel server");
    info!(%bind, "built-in WSS tunnel server listening");

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });
    let public_server =
        axum::serve(listener, app).with_graceful_shutdown(wait_for_shutdown(shutdown_rx.clone()));

    if let Some(admin_bind) = admin_bind {
        let admin_config = admin_config.expect("admin config checked with admin bind");
        let admin_app = admin::build_admin_app(
            devices,
            policies,
            observability,
            admin_registry,
            admin_multiplex_sessions,
            public_origin,
            admin_config,
        )
        .expect("failed to initialize tunnel admin login");
        let admin_listener = tokio::net::TcpListener::bind(admin_bind)
            .await
            .expect("failed to bind tunnel admin server");
        info!(%admin_bind, "built-in WSS tunnel admin server listening");
        let admin_server = axum::serve(admin_listener, admin_app)
            .with_graceful_shutdown(wait_for_shutdown(shutdown_rx));
        tokio::select! {
            result = public_server => result.expect("tunnel server failed"),
            result = admin_server => result.expect("tunnel admin server failed"),
        }
    } else {
        public_server.await.expect("tunnel server failed");
    }
}

fn init_tracing(
    log_directory: &FsPath,
) -> Result<tracing_appender::non_blocking::WorkerGuard, String> {
    std::fs::create_dir_all(log_directory).map_err(|error| {
        format!(
            "could not create log directory {}: {error}",
            log_directory.display()
        )
    })?;
    let file_appender = tracing_appender::rolling::daily(log_directory, "tunnel-server.log");
    let (file_writer, guard) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .lossy(false)
        .finish(file_appender);
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("coding_tools_tunnel_server=info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .init();
    Ok(guard)
}

async fn wait_for_shutdown(mut receiver: watch::Receiver<bool>) {
    while !*receiver.borrow() {
        if receiver.changed().await.is_err() {
            break;
        }
    }
}

fn build_app(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route(node_update::LATEST_PATH, get(node_update::latest))
        .route(node_update::DOWNLOAD_PATH, get(node_update::download))
        .route(WS_PATH, get(websocket_upgrade))
        .route("/_tunnel/enroll/{code}", post(enroll_device))
        .fallback(proxy_request)
        .with_state(state)
}

fn handle_cli(devices: &DeviceRegistry) -> Result<bool, String> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.is_empty() {
        return Ok(false);
    }
    match arguments.as_slice() {
        [group, command, rest @ ..] if group == "enroll" && command == "create" => {
            let client_id = argument_value(rest, "--client-id")
                .ok_or_else(|| "missing --client-id".to_string())?;
            let service_value =
                argument_value(rest, "--service").unwrap_or_else(|| "both".into());
            let services = AllowedServices::parse(&service_value)
                .ok_or_else(|| "--service must be mcp, actions, or both".to_string())?;
            let ttl_seconds = argument_value(rest, "--ttl-seconds")
                .map(|value| value.parse::<u64>())
                .transpose()
                .map_err(|_| "--ttl-seconds must be an integer".to_string())?;
            let grant = devices
                .create_enrollment(&client_id, services, ttl_seconds)
                .map_err(|error| error.to_string())?;
            let origin = std::env::var("CODING_TOOLS_TUNNEL_PUBLIC_ORIGIN")
                .unwrap_or_else(|_| DEFAULT_PUBLIC_ORIGIN.into());
            println!(
                "{}/{}/{}",
                origin.trim_end_matches('/'),
                ENROLL_PATH_PREFIX.trim_start_matches('/'),
                grant.code
            );
            println!("client_id={}", grant.client_id);
            println!("expires_at_unix_ms={}", grant.expires_at_unix_ms);
            println!("services={:?}", grant.services);
            Ok(true)
        }
        [group, command] if group == "devices" && command == "list" => {
            for device in devices.list_devices().map_err(|error| error.to_string())? {
                println!(
                    "{}\t{}\t{}\tmcp={}\tactions={}\tcreated={}\tlast_seen={}\trevoked={}",
                    device.device_id,
                    device.client_id,
                    device.device_name,
                    device.allow_mcp,
                    device.allow_actions,
                    device.created_at_unix_ms,
                    device
                        .last_seen_at_unix_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".into()),
                    device
                        .revoked_at_unix_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "-".into())
                );
            }
            Ok(true)
        }
        [group, command, rest @ ..] if group == "devices" && command == "revoke" => {
            let device_id = argument_value(rest, "--device-id")
                .ok_or_else(|| "missing --device-id".to_string())?;
            if !devices
                .revoke_device(&device_id)
                .map_err(|error| error.to_string())?
            {
                return Err(format!("active device not found: {device_id}"));
            }
            println!("revoked {device_id}");
            Ok(true)
        }
        _ => Err(
            "usage: coding-tools-tunnel-server [enroll create --client-id ID [--service both|mcp|actions] [--ttl-seconds 600] | devices list | devices revoke --device-id ID]"
                .into(),
        ),
    }
}

fn argument_value(arguments: &[String], name: &str) -> Option<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

async fn health() -> &'static str {
    "ok"
}

async fn enroll_device(
    State(state): State<AppState>,
    Path(code): Path<String>,
    Json(request): Json<EnrollmentRequest>,
) -> Response<Body> {
    match state.devices.enroll(code, request).await {
        Ok(mut response) => {
            response.server_id = state.server_identity.server_id().to_string();
            response.server_public_key = state.server_identity.public_key().to_string();
            (StatusCode::CREATED, Json(response)).into_response()
        }
        Err(error) => {
            if matches!(&error, DeviceAuthError::Storage(_)) {
                warn!(%error, "device enrollment storage failed");
            }
            let status = match &error {
                DeviceAuthError::EnrollmentUsed | DeviceAuthError::DeviceIdConflict => {
                    StatusCode::CONFLICT
                }
                DeviceAuthError::EnrollmentExpired => StatusCode::GONE,
                DeviceAuthError::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::BAD_REQUEST,
            };
            status_response(status, error.public_message())
        }
    }
}

#[derive(Clone)]
struct WorkerSocketAuth {
    server_identity: ServerIdentity,
    key: ClientKey,
    device_id: String,
    worker_id: String,
}

async fn websocket_upgrade(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response<Body> {
    if header_offers_protocol(&headers, MULTIPLEX_CONTROL_WS_SUBPROTOCOL)
        || header_offers_protocol(&headers, MULTIPLEX_DATA_WS_SUBPROTOCOL)
        || header_offers_protocol(&headers, MULTIPLEX_WS_SUBPROTOCOL)
    {
        let Some(device_id) = header_text(&headers, DEVICE_ID_HEADER)
            .filter(|value| valid_client_id(value))
            .map(str::to_string)
        else {
            return status_response(StatusCode::BAD_REQUEST, "invalid device id header");
        };
        let Some(connection_id) = header_text(&headers, CONNECTION_ID_HEADER)
            .filter(|value| !value.is_empty() && value.len() <= 128)
            .map(str::to_string)
        else {
            return status_response(StatusCode::BAD_REQUEST, "invalid connection id header");
        };
        if header_offers_protocol(&headers, MULTIPLEX_CONTROL_WS_SUBPROTOCOL) {
            let requested_session_id = header_text(&headers, SESSION_ID_HEADER)
                .filter(|value| !value.is_empty() && value.len() <= 128)
                .map(str::to_string);
            return ws
                .protocols([MULTIPLEX_CONTROL_WS_SUBPROTOCOL])
                .on_upgrade(move |socket| {
                    run_split_control_socket(
                        socket,
                        state,
                        device_id,
                        connection_id,
                        requested_session_id,
                    )
                })
                .into_response();
        }
        if header_offers_protocol(&headers, MULTIPLEX_DATA_WS_SUBPROTOCOL) {
            let Some(session_id) = header_text(&headers, SESSION_ID_HEADER)
                .filter(|value| !value.is_empty() && value.len() <= 128)
                .map(str::to_string)
            else {
                return status_response(StatusCode::BAD_REQUEST, "invalid session id header");
            };
            return ws
                .protocols([MULTIPLEX_DATA_WS_SUBPROTOCOL])
                .on_upgrade(move |socket| {
                    run_split_data_socket(socket, state, device_id, connection_id, session_id)
                })
                .into_response();
        }
        return ws
            .protocols([MULTIPLEX_WS_SUBPROTOCOL])
            .on_upgrade(move |socket| run_multiplex_socket(socket, state, device_id, connection_id))
            .into_response();
    }

    let Some(client_id) = header_text(&headers, CLIENT_ID_HEADER) else {
        return status_response(StatusCode::BAD_REQUEST, "missing client id header");
    };
    if !valid_client_id(client_id) {
        return status_response(StatusCode::BAD_REQUEST, "invalid client id header");
    }
    let Some(service) = header_text(&headers, SERVICE_HEADER).and_then(TunnelService::parse) else {
        return status_response(StatusCode::BAD_REQUEST, "invalid service header");
    };
    let Some(device_id) = header_text(&headers, DEVICE_ID_HEADER)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .map(str::to_string)
    else {
        return status_response(StatusCode::BAD_REQUEST, "invalid device id header");
    };
    let Some(worker_id) = header_text(&headers, WORKER_ID_HEADER)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .map(str::to_string)
    else {
        return status_response(StatusCode::BAD_REQUEST, "invalid worker id header");
    };
    let key = ClientKey {
        client_id: client_id.to_string(),
        service,
    };
    ws.protocols([WS_SUBPROTOCOL])
        .on_upgrade(move |socket| {
            run_worker_socket(
                socket,
                state.registry,
                state.devices,
                state.policies,
                state.observability,
                WorkerSocketAuth {
                    server_identity: state.server_identity,
                    key,
                    device_id,
                    worker_id,
                },
                state.worker_liveness_timeout,
            )
        })
        .into_response()
}

struct MultiplexRouteState {
    key: ClientKey,
    pool: ClientPool,
    _connection_guard: ActiveWorkerGuard,
    _presence_guard: observability::ClientConnectionGuard,
}

struct MultiplexSlotState {
    generation: Arc<AtomicUsize>,
    claimed: Arc<AtomicBool>,
    active: bool,
}

struct MultiplexAssignment {
    slot_index: usize,
    route_id: u32,
    job: ProxyJob,
}

struct MultiplexActiveStream {
    slot_index: usize,
    response_head: Option<oneshot::Sender<Result<ResponseHeadData, String>>>,
    response_body: mpsc::Sender<Result<Bytes, io::Error>>,
}

async fn publish_multiplex_offer(
    slot_index: usize,
    route_id: u32,
    pool: &ClientPool,
    slot: &MultiplexSlotState,
    assignment_tx: &mpsc::Sender<MultiplexAssignment>,
) -> Result<(), String> {
    if slot.active || slot.claimed.load(Ordering::Acquire) {
        return Ok(());
    }
    let expected_generation = slot.generation.load(Ordering::Acquire);
    let claim = SharedWorkerClaim {
        claimed: slot.claimed.clone(),
        generation: slot.generation.clone(),
        expected_generation,
    };
    let (assign, assigned) = oneshot::channel();
    pool.available_tx
        .send(AvailableWorker {
            assign,
            shared_claim: Some(claim),
        })
        .await
        .map_err(|_| "multiplex route dispatcher stopped".to_string())?;
    let assignment_tx = assignment_tx.clone();
    tokio::spawn(relay_multiplex_assignment(
        slot_index,
        route_id,
        assigned,
        assignment_tx,
    ));
    Ok(())
}

async fn relay_multiplex_assignment(
    slot_index: usize,
    route_id: u32,
    mut assigned: oneshot::Receiver<ProxyJob>,
    assignment_tx: mpsc::Sender<MultiplexAssignment>,
) {
    tokio::select! {
        assigned = &mut assigned => {
            if let Ok(job) = assigned {
                let _ = assignment_tx
                    .send(MultiplexAssignment { slot_index, route_id, job })
                    .await;
            }
        }
        _ = assignment_tx.closed() => {}
    }
}

async fn publish_multiplex_slot(
    slot_index: usize,
    routes: &HashMap<u32, MultiplexRouteState>,
    slots: &[MultiplexSlotState],
    assignment_tx: &mpsc::Sender<MultiplexAssignment>,
) -> Result<(), String> {
    let slot = &slots[slot_index];
    for (route_id, route) in routes {
        publish_multiplex_offer(slot_index, *route_id, &route.pool, slot, assignment_tx).await?;
    }
    Ok(())
}

fn reset_multiplex_slot(slot: &mut MultiplexSlotState) {
    slot.active = false;
    slot.generation.fetch_add(1, Ordering::AcqRel);
    slot.claimed.store(false, Ordering::Release);
}

async fn authenticate_split_socket(
    socket: &mut WebSocket,
    devices: &DeviceRegistry,
    server_identity: &ServerIdentity,
    expected_device_id: &str,
    expected_connection_id: &str,
    expected_role: MultiplexConnectionRole,
    expected_session_id: Option<&str>,
) -> Result<(String, SplitSessionAuthProof), String> {
    let nonce = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let expires_at_unix_ms = unix_ms().saturating_add(AUTH_CHALLENGE_TIMEOUT.as_millis() as u64);
    let server_signature = server_identity.sign(&multiplex_server_challenge_signing_payload(
        &nonce,
        expires_at_unix_ms,
        server_identity.server_id(),
        expected_device_id,
        expected_connection_id,
    ));
    send_multiplex_control(
        socket,
        &MultiplexControlMessage::Challenge {
            nonce: nonce.clone(),
            expires_at_unix_ms,
            server_id: server_identity.server_id().to_string(),
            server_signature,
        },
    )
    .await?;
    let proof = match timeout(AUTH_CHALLENGE_TIMEOUT, receive_multiplex_control(socket)).await {
        Ok(Ok(MultiplexControlMessage::SessionAuthenticate(proof))) => proof,
        Ok(Ok(_)) => return Err("expected split session authentication proof".into()),
        Ok(Err(error)) => return Err(error),
        Err(_) => return Err("authentication challenge timed out".into()),
    };
    if proof.device_id != expected_device_id
        || proof.hello.connection_id != expected_connection_id
        || proof.hello.protocol_version != MULTIPLEX_PROTOCOL_VERSION
        || proof.hello.connection_role != expected_role
        || proof.hello.session_id.as_deref() != expected_session_id
    {
        return Err("split authentication proof does not match connection identity".into());
    }
    devices
        .verify_split_session(nonce.clone(), expires_at_unix_ms, proof.clone())
        .await
        .map_err(|error| error.public_message().to_string())?;
    Ok((nonce, proof))
}

async fn run_split_control_socket(
    mut socket: WebSocket,
    state: AppState,
    expected_device_id: String,
    expected_connection_id: String,
    requested_session_id: Option<String>,
) {
    let AppState {
        registry,
        devices,
        server_identity,
        multiplex_sessions: sessions,
        observability,
        reconnect_grace_timeout,
        worker_liveness_timeout: liveness_timeout,
        ..
    } = state;
    let (nonce, proof) = match authenticate_split_socket(
        &mut socket,
        &devices,
        &server_identity,
        &expected_device_id,
        &expected_connection_id,
        MultiplexConnectionRole::Control,
        requested_session_id.as_deref(),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = send_multiplex_error(&mut socket, &error).await;
            return;
        }
    };
    let (session, control_generation, created) =
        if let Some(session_id) = requested_session_id.as_deref() {
            match sessions.reattach(session_id, &proof.device_id).await {
                Ok((session, generation)) => (session, generation, false),
                Err(error) => {
                    let _ = send_multiplex_error(&mut socket, &error).await;
                    return;
                }
            }
        } else {
            let session = sessions
                .create(
                    proof.device_id.clone(),
                    MultiplexConnectionPolicy::default(),
                )
                .await;
            let generation = session.current_control_generation();
            (session, generation, true)
        };
    let policy = session.policy.clone();
    let server_signature = server_identity.sign(&split_server_ack_signing_payload(
        &nonce,
        server_identity.server_id(),
        &proof,
        &session.session_id,
        &policy,
    ));
    if send_multiplex_control(
        &mut socket,
        &MultiplexControlMessage::SessionReady {
            protocol_version: MULTIPLEX_PROTOCOL_VERSION,
            connection_role: MultiplexConnectionRole::Control,
            session_id: session.session_id.clone(),
            connection_policy: policy.clone(),
            server_id: server_identity.server_id().to_string(),
            server_signature,
        },
    )
    .await
    .is_err()
    {
        if created {
            sessions.remove(&session.session_id).await;
        } else {
            schedule_split_session_cleanup(
                sessions.clone(),
                session.clone(),
                control_generation,
                reconnect_grace_timeout,
            );
        }
        return;
    }
    observability.log(
        "info",
        "multiplex",
        "split control session authenticated",
        None,
        None,
        Some(&expected_connection_id),
        None,
    );
    let mut demand_tick = interval(Duration::from_millis(250));
    demand_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = demand_tick.tick() => {
                if send_multiplex_control(&mut socket, &MultiplexControlMessage::CapacityUpdate { demand: session.demand().await }).await.is_err() { break; }
            }
            incoming = timeout(liveness_timeout, socket.recv()) => {
                let incoming = match incoming { Ok(Some(Ok(message))) => message, _ => break };
                match incoming {
                    Message::Text(text) => {
                        let control = match serde_json::from_str::<MultiplexControlMessage>(text.as_str()) {
                            Ok(value) => value,
                            Err(error) => { let _ = send_multiplex_error(&mut socket, &format!("invalid split control frame: {error}")).await; continue; }
                        };
                        match control {
                            MultiplexControlMessage::RouteRegister { registration_id, client_id, service } => {
                                if !valid_client_id(&client_id) || !devices.route_allowed(&proof.device_id, &client_id, service).unwrap_or(false) {
                                    let _ = send_multiplex_error(&mut socket, "route is not authorized for this device").await;
                                    continue;
                                }
                                let (route_id, _) = match session.register_route(&registry, &observability, ClientKey { client_id, service }).await {
                                    Ok(value) => value,
                                    Err(error) => {
                                        let _ = send_multiplex_error(&mut socket, &error).await;
                                        continue;
                                    }
                                };
                                if send_multiplex_control(&mut socket, &MultiplexControlMessage::RouteRegistered {
                                    registration_id, route_id, stream_limit: policy.streams_per_connection,
                                }).await.is_err() { break; }
                            }
                            MultiplexControlMessage::RouteUnregister { route_id } => { session.unregister_route(route_id).await; }
                            MultiplexControlMessage::Error { message, .. } => warn!(session_id = %session.session_id, %message, "split control peer reported error"),
                            _ => { let _ = send_multiplex_error(&mut socket, "unexpected frame on split control connection").await; }
                        }
                    }
                    Message::Ping(payload) => { if socket.send(Message::Pong(payload)).await.is_err() { break; } }
                    Message::Close(_) => break,
                    Message::Pong(_) => {},
                    Message::Binary(_) => { let _ = send_multiplex_error(&mut socket, "binary frames are not allowed on control connection").await; }
                }
            }
        }
    }
    schedule_split_session_cleanup(
        sessions,
        session,
        control_generation,
        reconnect_grace_timeout,
    );
}

fn schedule_split_session_cleanup(
    sessions: SplitSessionRegistry,
    session: Arc<SplitSessionState>,
    control_generation: u64,
    reconnect_grace_timeout: Duration,
) {
    if !session.detach_control(control_generation) {
        return;
    }
    let session_id = session.session_id.clone();
    tokio::spawn(async move {
        sleep(reconnect_grace_timeout).await;
        sessions
            .remove_if_detached(&session_id, control_generation)
            .await;
    });
}

async fn publish_split_slot(
    slot_index: usize,
    session: &SplitSessionState,
    slots: &[MultiplexSlotState],
    assignment_tx: &mpsc::Sender<MultiplexAssignment>,
) -> Result<(), String> {
    if !session.control_is_attached() {
        return Ok(());
    }
    let slot = &slots[slot_index];
    for (route_id, pool) in session.route_snapshot().await {
        publish_multiplex_offer(slot_index, route_id, &pool, slot, assignment_tx).await?;
    }
    Ok(())
}

async fn run_split_data_socket(
    mut socket: WebSocket,
    state: AppState,
    expected_device_id: String,
    expected_connection_id: String,
    expected_session_id: String,
) {
    let AppState {
        devices,
        server_identity,
        multiplex_sessions: sessions,
        observability,
        worker_liveness_timeout: liveness_timeout,
        ..
    } = state;
    let session = match sessions.get(&expected_session_id).await {
        Some(session) if session.device_id == expected_device_id => session,
        _ => {
            let _ = send_multiplex_error(&mut socket, "control session is unavailable").await;
            return;
        }
    };
    let (nonce, proof) = match authenticate_split_socket(
        &mut socket,
        &devices,
        &server_identity,
        &expected_device_id,
        &expected_connection_id,
        MultiplexConnectionRole::Data,
        Some(&expected_session_id),
    )
    .await
    {
        Ok(value) => value,
        Err(error) => {
            let _ = send_multiplex_error(&mut socket, &error).await;
            return;
        }
    };
    if !session.attach_data_connection().await {
        let _ = send_multiplex_error(&mut socket, "data connection limit reached").await;
        return;
    }
    let policy = session.policy.clone();
    let server_signature = server_identity.sign(&split_server_ack_signing_payload(
        &nonce,
        server_identity.server_id(),
        &proof,
        &session.session_id,
        &policy,
    ));
    if send_multiplex_control(
        &mut socket,
        &MultiplexControlMessage::SessionReady {
            protocol_version: MULTIPLEX_PROTOCOL_VERSION,
            connection_role: MultiplexConnectionRole::Data,
            session_id: session.session_id.clone(),
            connection_policy: policy.clone(),
            server_id: server_identity.server_id().to_string(),
            server_signature,
        },
    )
    .await
    .is_err()
    {
        session.detach_data_connection().await;
        return;
    }
    observability.log(
        "info",
        "multiplex",
        "split data connection authenticated",
        None,
        None,
        Some(&expected_connection_id),
        None,
    );
    let stream_limit = usize::from(policy.streams_per_connection);
    let mut slots = (0..stream_limit)
        .map(|_| MultiplexSlotState {
            generation: Arc::new(AtomicUsize::new(1)),
            claimed: Arc::new(AtomicBool::new(false)),
            active: false,
        })
        .collect::<Vec<_>>();
    let (assignment_tx, mut assignment_rx) = mpsc::channel::<MultiplexAssignment>(stream_limit * 2);
    let (cancel_tx, mut cancel_rx) = mpsc::channel::<(u32, u64)>(stream_limit * 2);
    let mut active_streams: HashMap<(u32, u64), MultiplexActiveStream> = HashMap::new();
    let mut control_revision = session.control_revision.subscribe();
    let mut route_revision = session.route_revision.subscribe();
    let mut shutdown = session.shutdown.subscribe();
    for slot_index in 0..slots.len() {
        let _ = publish_split_slot(slot_index, &session, &slots, &assignment_tx).await;
    }
    loop {
        tokio::select! {
            changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { break; } }
            changed = control_revision.changed() => {
                if changed.is_err() { break; }
                if session.control_is_attached() {
                    for slot_index in 0..slots.len() {
                        if !slots[slot_index].active && !slots[slot_index].claimed.load(Ordering::Acquire) {
                            let _ = publish_split_slot(slot_index, &session, &slots, &assignment_tx).await;
                        }
                    }
                } else {
                    for slot in &mut slots {
                        if !slot.active {
                            slot.generation.fetch_add(1, Ordering::AcqRel);
                            slot.claimed.store(false, Ordering::Release);
                        }
                    }
                }
            }
            changed = route_revision.changed() => {
                if changed.is_err() { break; }
                for slot_index in 0..slots.len() {
                    if !slots[slot_index].active && !slots[slot_index].claimed.load(Ordering::Acquire) {
                        let _ = publish_split_slot(slot_index, &session, &slots, &assignment_tx).await;
                    }
                }
            }
            assignment = assignment_rx.recv() => {
                let Some(MultiplexAssignment { slot_index, route_id, job }) = assignment else { break; };
                if slot_index >= slots.len()
                    || slots[slot_index].active
                    || !session.control_is_attached()
                    || session.routes.lock().await.get(&route_id).is_none()
                {
                    let _ = job.response_head.send(Err("split data stream slot is no longer available".into()));
                    if slot_index < slots.len() && !slots[slot_index].active {
                        reset_multiplex_slot(&mut slots[slot_index]);
                    }
                    continue;
                }
                slots[slot_index].active = true;
                let stream_id = session.next_stream_id.fetch_add(1, Ordering::AcqRel).max(1);
                session.active_streams.fetch_add(1, Ordering::AcqRel);
                let ProxyJob { method, path_and_query, headers, body, response_head, response_body, mut cancelled, .. } = job;
                if send_multiplex_control(&mut socket, &MultiplexControlMessage::RequestHead {
                    route_id, stream_id, method, path_and_query, headers, demand: None,
                }).await.is_err() { let _ = response_head.send(Err("split data connection disconnected".into())); break; }
                if !body.is_empty() {
                    match encode_multiplex_binary_frame(MultiplexBinaryFrameKind::RequestBody, route_id, stream_id, body.as_ref()) {
                        Ok(frame) => if socket.send(Message::Binary(Bytes::from(frame))).await.is_err() { let _ = response_head.send(Err("split data connection disconnected".into())); break; },
                        Err(error) => { let _ = response_head.send(Err(error)); reset_multiplex_slot(&mut slots[slot_index]); session.active_streams.fetch_sub(1, Ordering::AcqRel); let _ = publish_split_slot(slot_index, &session, &slots, &assignment_tx).await; continue; }
                    }
                }
                if send_multiplex_control(&mut socket, &MultiplexControlMessage::RequestEnd { route_id, stream_id }).await.is_err() { let _ = response_head.send(Err("split data connection disconnected".into())); break; }
                active_streams.insert((route_id, stream_id), MultiplexActiveStream { slot_index, response_head: Some(response_head), response_body });
                let cancel_tx = cancel_tx.clone();
                tokio::spawn(async move {
                    loop {
                        if *cancelled.borrow() {
                            let _ = cancel_tx.send((route_id, stream_id)).await;
                            break;
                        }
                        if cancelled.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
            cancelled = cancel_rx.recv() => {
                let Some((route_id, stream_id)) = cancelled else { break; };
                if let Some(mut stream) = active_streams.remove(&(route_id, stream_id)) {
                    let _ = send_multiplex_control(&mut socket, &MultiplexControlMessage::Cancel { route_id, stream_id }).await;
                    if let Some(head) = stream.response_head.take() { let _ = head.send(Err("public request cancelled".into())); }
                    reset_multiplex_slot(&mut slots[stream.slot_index]); session.active_streams.fetch_sub(1, Ordering::AcqRel);
                    let _ = publish_split_slot(stream.slot_index, &session, &slots, &assignment_tx).await;
                }
            }
            incoming = timeout(liveness_timeout, socket.recv()) => {
                let incoming = match incoming { Ok(Some(Ok(message))) => message, _ => break };
                match incoming {
                    Message::Text(text) => {
                        let control = match serde_json::from_str::<MultiplexControlMessage>(text.as_str()) { Ok(value) => value, Err(error) => { let _ = send_multiplex_error(&mut socket, &format!("invalid split data control frame: {error}")).await; continue; } };
                        match control {
                            MultiplexControlMessage::ResponseHead { route_id, stream_id, status, headers } => {
                                let Some(stream) = active_streams.get_mut(&(route_id, stream_id)) else { let _ = send_multiplex_error(&mut socket, "response head references unknown split stream").await; continue; };
                                let Some(head) = stream.response_head.take() else { let _ = send_multiplex_error(&mut socket, "duplicate response head").await; continue; };
                                let _ = head.send(Ok(ResponseHeadData { status, headers }));
                            }
                            MultiplexControlMessage::ResponseEnd { route_id, stream_id } => {
                                let Some(mut stream) = active_streams.remove(&(route_id, stream_id)) else { let _ = send_multiplex_error(&mut socket, "response end references unknown split stream").await; continue; };
                                if let Some(head) = stream.response_head.take() { let _ = head.send(Err("response ended before headers".into())); }
                                reset_multiplex_slot(&mut slots[stream.slot_index]); session.active_streams.fetch_sub(1, Ordering::AcqRel);
                                let _ = publish_split_slot(stream.slot_index, &session, &slots, &assignment_tx).await;
                            }
                            MultiplexControlMessage::Error { route_id: Some(route_id), stream_id: Some(stream_id), message } => {
                                if let Some(mut stream) = active_streams.remove(&(route_id, stream_id)) {
                                    if let Some(head) = stream.response_head.take() { let _ = head.send(Err(message.clone())); } else { let _ = stream.response_body.send(Err(io::Error::other(message))).await; }
                                    reset_multiplex_slot(&mut slots[stream.slot_index]); session.active_streams.fetch_sub(1, Ordering::AcqRel);
                                    let _ = publish_split_slot(stream.slot_index, &session, &slots, &assignment_tx).await;
                                }
                            }
                            _ => { let _ = send_multiplex_error(&mut socket, "unexpected frame on split data connection").await; }
                        }
                    }
                    Message::Binary(frame) => {
                        let frame = match decode_multiplex_binary_frame(frame.as_ref()) { Ok(frame) => frame, Err(error) => { let _ = send_multiplex_error(&mut socket, &error).await; continue; } };
                        if frame.kind != MultiplexBinaryFrameKind::ResponseBody { let _ = send_multiplex_error(&mut socket, "split data connection may only send response bodies").await; continue; }
                        let key = (frame.route_id, frame.stream_id);
                        let Some(stream) = active_streams.get_mut(&key) else { let _ = send_multiplex_error(&mut socket, "response body references unknown split stream").await; continue; };
                        if stream.response_head.is_some() { let _ = send_multiplex_error(&mut socket, "response body arrived before response head").await; continue; }
                        if stream.response_body.send(Ok(Bytes::from(frame.payload))).await.is_err() {
                            if let Some(stream) = active_streams.remove(&key) { reset_multiplex_slot(&mut slots[stream.slot_index]); session.active_streams.fetch_sub(1, Ordering::AcqRel); let _ = publish_split_slot(stream.slot_index, &session, &slots, &assignment_tx).await; }
                        }
                    }
                    Message::Ping(payload) => { if socket.send(Message::Pong(payload)).await.is_err() { break; } }
                    Message::Close(_) => break,
                    Message::Pong(_) => {},
                }
            }
        }
    }
    for (_, mut stream) in active_streams {
        session.active_streams.fetch_sub(1, Ordering::AcqRel);
        if let Some(head) = stream.response_head.take() {
            let _ = head.send(Err("split data connection disconnected".into()));
        } else {
            let _ = stream
                .response_body
                .try_send(Err(io::Error::other("split data connection disconnected")));
        }
    }
    session.detach_data_connection().await;
}

async fn run_multiplex_socket(
    mut socket: WebSocket,
    state: AppState,
    expected_device_id: String,
    expected_connection_id: String,
) {
    let AppState {
        registry,
        devices,
        server_identity,
        observability,
        worker_liveness_timeout: liveness_timeout,
        ..
    } = state;
    let nonce = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let expires_at_unix_ms = unix_ms().saturating_add(AUTH_CHALLENGE_TIMEOUT.as_millis() as u64);
    let server_signature = server_identity.sign(&multiplex_server_challenge_signing_payload(
        &nonce,
        expires_at_unix_ms,
        server_identity.server_id(),
        &expected_device_id,
        &expected_connection_id,
    ));
    if send_multiplex_control(
        &mut socket,
        &MultiplexControlMessage::Challenge {
            nonce: nonce.clone(),
            expires_at_unix_ms,
            server_id: server_identity.server_id().to_string(),
            server_signature,
        },
    )
    .await
    .is_err()
    {
        return;
    }

    let proof = match timeout(
        AUTH_CHALLENGE_TIMEOUT,
        receive_multiplex_control(&mut socket),
    )
    .await
    {
        Ok(Ok(MultiplexControlMessage::Authenticate(proof))) => proof,
        Ok(Ok(_)) => {
            let _ =
                send_multiplex_error(&mut socket, "expected device session authentication proof")
                    .await;
            return;
        }
        Ok(Err(error)) => {
            warn!(device_id = %expected_device_id, connection_id = %expected_connection_id, %error, "multiplex authentication failed");
            return;
        }
        Err(_) => {
            let _ = send_multiplex_error(&mut socket, "authentication challenge timed out").await;
            return;
        }
    };
    if proof.device_id != expected_device_id
        || proof.hello.connection_id != expected_connection_id
        || proof.hello.protocol_version != MULTIPLEX_PROTOCOL_VERSION
    {
        let _ = send_multiplex_error(
            &mut socket,
            "authentication proof does not match physical connection identity",
        )
        .await;
        return;
    }
    if let Err(error) = devices
        .verify_device_session(nonce.clone(), expires_at_unix_ms, proof.clone())
        .await
    {
        warn!(device_id = %proof.device_id, connection_id = %proof.hello.connection_id, %error, "multiplex device authentication rejected");
        let _ = send_multiplex_error(&mut socket, error.public_message()).await;
        return;
    }

    let connection_policy = MultiplexConnectionPolicy::default();
    let server_signature = server_identity.sign(&multiplex_server_ack_signing_payload(
        &nonce,
        server_identity.server_id(),
        &proof,
        &connection_policy,
    ));
    if send_multiplex_control(
        &mut socket,
        &MultiplexControlMessage::HelloAck {
            protocol_version: MULTIPLEX_PROTOCOL_VERSION,
            connection_policy: connection_policy.clone(),
            server_id: server_identity.server_id().to_string(),
            server_signature,
        },
    )
    .await
    .is_err()
    {
        return;
    }

    info!(device_id = %proof.device_id, connection_id = %proof.hello.connection_id, "multiplex device connection authenticated");
    observability.log(
        "info",
        "multiplex",
        "device-shared tunnel connection authenticated",
        None,
        None,
        Some(&proof.hello.connection_id),
        None,
    );

    let stream_limit = usize::from(connection_policy.streams_per_connection);
    let mut routes: HashMap<u32, MultiplexRouteState> = HashMap::new();
    let mut next_route_id = 1_u32;
    let mut next_stream_id = 1_u64;
    let mut slots = (0..stream_limit)
        .map(|_| MultiplexSlotState {
            generation: Arc::new(AtomicUsize::new(1)),
            claimed: Arc::new(AtomicBool::new(false)),
            active: false,
        })
        .collect::<Vec<_>>();
    let (assignment_tx, mut assignment_rx) = mpsc::channel::<MultiplexAssignment>(stream_limit * 2);
    let (cancel_tx, mut cancel_rx) = mpsc::channel::<(u32, u64)>(stream_limit * 2);
    let mut active_streams: HashMap<(u32, u64), MultiplexActiveStream> = HashMap::new();

    loop {
        tokio::select! {
            assignment = assignment_rx.recv() => {
                let Some(MultiplexAssignment { slot_index, route_id, job }) = assignment else {
                    break;
                };
                if slot_index >= slots.len() || slots[slot_index].active || !routes.contains_key(&route_id) {
                    let _ = job.response_head.send(Err("multiplex stream slot is no longer available".into()));
                    continue;
                }
                slots[slot_index].active = true;
                let stream_id = next_stream_id;
                next_stream_id = next_stream_id.wrapping_add(1).max(1);
                let active_count = slots.iter().filter(|slot| slot.active).count();
                let route = routes.get(&route_id).expect("route checked");
                let queued = route.pool.pending_requests();
                let desired_connections = active_count
                    .saturating_add(queued)
                    .div_ceil(stream_limit)
                    .max(1)
                    .min(usize::from(connection_policy.max_connections));
                let demand = Some(MultiplexDemand {
                    queued_requests: queued.min(usize::from(u16::MAX)) as u16,
                    oldest_queue_wait_ms: job.enqueued_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                    active_streams: active_count.min(usize::from(u16::MAX)) as u16,
                    available_stream_slots: stream_limit.saturating_sub(active_count).min(usize::from(u16::MAX)) as u16,
                    desired_connections: desired_connections as u16,
                });
                let ProxyJob {
                    method,
                    path_and_query,
                    headers,
                    body,
                    response_head,
                    response_body,
                    mut cancelled,
                    ..
                } = job;
                if send_multiplex_control(
                    &mut socket,
                    &MultiplexControlMessage::RequestHead {
                        route_id,
                        stream_id,
                        method,
                        path_and_query,
                        headers,
                        demand,
                    },
                )
                .await
                .is_err()
                {
                    let _ = response_head.send(Err("multiplex connection disconnected".into()));
                    break;
                }
                if !body.is_empty() {
                    let frame = match encode_multiplex_binary_frame(
                        MultiplexBinaryFrameKind::RequestBody,
                        route_id,
                        stream_id,
                        body.as_ref(),
                    ) {
                        Ok(frame) => frame,
                        Err(error) => {
                            let _ = response_head.send(Err(error));
                            reset_multiplex_slot(&mut slots[slot_index]);
                            let _ = publish_multiplex_slot(slot_index, &routes, &slots, &assignment_tx).await;
                            continue;
                        }
                    };
                    if socket.send(Message::Binary(Bytes::from(frame))).await.is_err() {
                        let _ = response_head.send(Err("multiplex connection disconnected".into()));
                        break;
                    }
                }
                if send_multiplex_control(
                    &mut socket,
                    &MultiplexControlMessage::RequestEnd { route_id, stream_id },
                )
                .await
                .is_err()
                {
                    let _ = response_head.send(Err("multiplex connection disconnected".into()));
                    break;
                }
                active_streams.insert(
                    (route_id, stream_id),
                    MultiplexActiveStream {
                        slot_index,
                        response_head: Some(response_head),
                        response_body,
                    },
                );
                let cancel_tx = cancel_tx.clone();
                tokio::spawn(async move {
                    loop {
                        if *cancelled.borrow() {
                            let _ = cancel_tx.send((route_id, stream_id)).await;
                            break;
                        }
                        if cancelled.changed().await.is_err() {
                            break;
                        }
                    }
                });
            }
            cancelled = cancel_rx.recv() => {
                let Some((route_id, stream_id)) = cancelled else {
                    break;
                };
                if let Some(mut stream) = active_streams.remove(&(route_id, stream_id)) {
                    let _ = send_multiplex_control(
                        &mut socket,
                        &MultiplexControlMessage::Cancel { route_id, stream_id },
                    )
                    .await;
                    if let Some(head) = stream.response_head.take() {
                        let _ = head.send(Err("public request cancelled".into()));
                    }
                    reset_multiplex_slot(&mut slots[stream.slot_index]);
                    let _ = publish_multiplex_slot(stream.slot_index, &routes, &slots, &assignment_tx).await;
                }
            }
            incoming = timeout(liveness_timeout, socket.recv()) => {
                let incoming = match incoming {
                    Ok(Some(Ok(message))) => message,
                    Ok(Some(Err(error))) => {
                        warn!(device_id = %proof.device_id, connection_id = %proof.hello.connection_id, %error, "multiplex socket receive failed");
                        break;
                    }
                    Ok(None) | Err(_) => break,
                };
                match incoming {
                    Message::Text(text) => {
                        let control = match serde_json::from_str::<MultiplexControlMessage>(text.as_str()) {
                            Ok(control) => control,
                            Err(error) => {
                                let _ = send_multiplex_error(&mut socket, &format!("invalid multiplex control frame: {error}")).await;
                                continue;
                            }
                        };
                        match control {
                            MultiplexControlMessage::RouteRegister { registration_id, client_id, service } => {
                                if !valid_client_id(&client_id)
                                    || !devices.route_allowed(&proof.device_id, &client_id, service).unwrap_or(false)
                                {
                                    let _ = send_multiplex_error(&mut socket, "route is not authorized for this device").await;
                                    continue;
                                }
                                let existing = routes.iter().find_map(|(route_id, route)| {
                                    (route.key.client_id == client_id && route.key.service == service).then_some(*route_id)
                                });
                                let route_id = if let Some(route_id) = existing {
                                    route_id
                                } else {
                                    let key = ClientKey { client_id: client_id.clone(), service };
                                    let pool = match registry.register_owned(key.clone(), &proof.device_id).await {
                                        Ok(pool) => pool,
                                        Err(error) => {
                                            let _ = send_multiplex_error(&mut socket, &error).await;
                                            continue;
                                        }
                                    };
                                    let Some(connection_guard) = ActiveWorkerGuard::try_new(
                                        pool.active_workers.clone(),
                                        usize::from(connection_policy.max_connections),
                                    ) else {
                                        let _ = send_multiplex_error(&mut socket, "connection limit reached for this route").await;
                                        continue;
                                    };
                                    let route_id = next_route_id;
                                    next_route_id = next_route_id.saturating_add(1).max(1);
                                    let presence_guard = observability.connect_client_route(
                                        &format!("multiplex:{}:{route_id}", proof.hello.connection_id),
                                        &proof.device_id,
                                        &key.client_id,
                                        key.service.as_str(),
                                        "multiplex",
                                    );
                                    routes.insert(route_id, MultiplexRouteState {
                                        key,
                                        pool,
                                        _connection_guard: connection_guard,
                                        _presence_guard: presence_guard,
                                    });
                                    route_id
                                };
                                if send_multiplex_control(
                                    &mut socket,
                                    &MultiplexControlMessage::RouteRegistered {
                                        registration_id,
                                        route_id,
                                        stream_limit: connection_policy.streams_per_connection,
                                    },
                                )
                                .await
                                .is_err()
                                {
                                    break;
                                }
                                if existing.is_none() {
                                    let route = routes.get(&route_id).expect("new route registered");
                                    for (slot_index, slot) in slots.iter().enumerate() {
                                        if !slot.active && !slot.claimed.load(Ordering::Acquire) {
                                            let _ = publish_multiplex_offer(
                                                slot_index,
                                                route_id,
                                                &route.pool,
                                                slot,
                                                &assignment_tx,
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                            MultiplexControlMessage::RouteUnregister { route_id } => {
                                if active_streams.keys().any(|(active_route_id, _)| *active_route_id == route_id) {
                                    let _ = send_multiplex_error(&mut socket, "cannot unregister a route with active streams").await;
                                } else {
                                    routes.remove(&route_id);
                                }
                            }
                            MultiplexControlMessage::ResponseHead { route_id, stream_id, status, headers } => {
                                let Some(stream) = active_streams.get_mut(&(route_id, stream_id)) else {
                                    let _ = send_multiplex_error(&mut socket, "response head references an unknown stream").await;
                                    continue;
                                };
                                let Some(head) = stream.response_head.take() else {
                                    let _ = send_multiplex_error(&mut socket, "duplicate response head").await;
                                    continue;
                                };
                                let _ = head.send(Ok(ResponseHeadData { status, headers }));
                            }
                            MultiplexControlMessage::ResponseEnd { route_id, stream_id } => {
                                let Some(mut stream) = active_streams.remove(&(route_id, stream_id)) else {
                                    let _ = send_multiplex_error(&mut socket, "response end references an unknown stream").await;
                                    continue;
                                };
                                if let Some(head) = stream.response_head.take() {
                                    let _ = head.send(Err("response ended before headers".into()));
                                }
                                reset_multiplex_slot(&mut slots[stream.slot_index]);
                                let _ = publish_multiplex_slot(stream.slot_index, &routes, &slots, &assignment_tx).await;
                            }
                            MultiplexControlMessage::Error { route_id: Some(route_id), stream_id: Some(stream_id), message } => {
                                if let Some(mut stream) = active_streams.remove(&(route_id, stream_id)) {
                                    if let Some(head) = stream.response_head.take() {
                                        let _ = head.send(Err(message.clone()));
                                    } else {
                                        let _ = stream.response_body.send(Err(io::Error::other(message.clone()))).await;
                                    }
                                    reset_multiplex_slot(&mut slots[stream.slot_index]);
                                    let _ = publish_multiplex_slot(stream.slot_index, &routes, &slots, &assignment_tx).await;
                                }
                            }
                            MultiplexControlMessage::Error { message, .. } => {
                                warn!(device_id = %proof.device_id, connection_id = %proof.hello.connection_id, %message, "multiplex peer reported connection error");
                            }
                            _ => {
                                let _ = send_multiplex_error(&mut socket, "unexpected multiplex control frame from device").await;
                            }
                        }
                    }
                    Message::Binary(frame) => {
                        let frame = match decode_multiplex_binary_frame(frame.as_ref()) {
                            Ok(frame) => frame,
                            Err(error) => {
                                let _ = send_multiplex_error(&mut socket, &error).await;
                                continue;
                            }
                        };
                        if frame.kind != MultiplexBinaryFrameKind::ResponseBody {
                            let _ = send_multiplex_error(&mut socket, "device may only send response body binary frames").await;
                            continue;
                        }
                        let key = (frame.route_id, frame.stream_id);
                        let Some(stream) = active_streams.get_mut(&key) else {
                            let _ = send_multiplex_error(&mut socket, "response body references an unknown stream").await;
                            continue;
                        };
                        if stream.response_head.is_some() {
                            let _ = send_multiplex_error(&mut socket, "response body arrived before response head").await;
                            continue;
                        }
                        if stream.response_body.send(Ok(Bytes::from(frame.payload))).await.is_err() {
                            let stream = active_streams.remove(&key).expect("stream still present");
                            let _ = send_multiplex_control(
                                &mut socket,
                                &MultiplexControlMessage::Cancel { route_id: key.0, stream_id: key.1 },
                            )
                            .await;
                            reset_multiplex_slot(&mut slots[stream.slot_index]);
                            let _ = publish_multiplex_slot(stream.slot_index, &routes, &slots, &assignment_tx).await;
                        }
                    }
                    Message::Ping(payload) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    Message::Pong(_) => {}
                }
            }
        }
    }

    for (_, mut stream) in active_streams {
        if let Some(head) = stream.response_head.take() {
            let _ = head.send(Err("multiplex connection disconnected".into()));
        } else {
            let _ = stream
                .response_body
                .try_send(Err(io::Error::other("multiplex connection disconnected")));
        }
    }
}

async fn run_worker_socket(
    mut socket: WebSocket,
    registry: Registry,
    devices: DeviceRegistry,
    policies: WorkerPolicyStore,
    observability: Observability,
    auth: WorkerSocketAuth,
    worker_liveness_timeout: Duration,
) {
    let WorkerSocketAuth {
        server_identity,
        key,
        device_id: expected_device_id,
        worker_id: expected_worker_id,
    } = auth;
    let nonce = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let expires_at_unix_ms = unix_ms().saturating_add(AUTH_CHALLENGE_TIMEOUT.as_millis() as u64);
    let server_signature = server_identity.sign(&server_challenge_signing_payload(
        &nonce,
        expires_at_unix_ms,
        server_identity.server_id(),
        &expected_device_id,
        &key.client_id,
        key.service,
        &expected_worker_id,
    ));
    if send_control(
        &mut socket,
        &ControlMessage::Challenge {
            nonce: nonce.clone(),
            expires_at_unix_ms,
            server_id: server_identity.server_id().to_string(),
            server_signature,
        },
    )
    .await
    .is_err()
    {
        return;
    }
    let proof = match timeout(AUTH_CHALLENGE_TIMEOUT, receive_control(&mut socket)).await {
        Ok(Ok(ControlMessage::Authenticate(proof))) => proof,
        Ok(Ok(_)) => {
            let _ = send_error(&mut socket, None, "expected device authentication proof").await;
            return;
        }
        Ok(Err(error)) => {
            warn!(client_id = %key.client_id, service = key.service.as_str(), %error, "worker authentication failed");
            return;
        }
        Err(_) => {
            let _ = send_error(&mut socket, None, "authentication challenge timed out").await;
            return;
        }
    };

    if let Err(message) = validate_hello(&proof.hello, &key) {
        let _ = send_error(&mut socket, None, &message).await;
        return;
    }
    if proof.device_id != expected_device_id || proof.hello.worker_id != expected_worker_id {
        let _ = send_error(
            &mut socket,
            None,
            "authentication proof does not match connection identity",
        )
        .await;
        return;
    }
    if let Err(error) = devices
        .verify(nonce.clone(), expires_at_unix_ms, proof.clone())
        .await
    {
        warn!(client_id = %key.client_id, service = key.service.as_str(), device_id = %proof.device_id, %error, "device authentication rejected");
        let _ = send_error(&mut socket, None, error.public_message()).await;
        return;
    }

    let hello = proof.hello.clone();
    let pool = match registry.register_owned(key.clone(), &proof.device_id).await {
        Ok(pool) => pool,
        Err(error) => {
            let _ = send_error(&mut socket, None, &error).await;
            return;
        }
    };
    let mut policy_updates = policies.subscribe(key.service);
    let worker_policy = policy_updates.borrow().clone();
    let server_signature = server_identity.sign(&server_ack_signing_payload(
        &nonce,
        server_identity.server_id(),
        &proof,
        &worker_policy,
    ));
    let Some(_active_worker) = ActiveWorkerGuard::try_new(
        pool.active_workers.clone(),
        usize::from(worker_policy.max_workers),
    ) else {
        let _ = send_error(&mut socket, None, "worker limit reached for this route").await;
        return;
    };
    if send_control(
        &mut socket,
        &ControlMessage::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            worker_policy,
            server_id: server_identity.server_id().to_string(),
            server_signature,
        },
    )
    .await
    .is_err()
    {
        return;
    }
    info!(client_id = %key.client_id, service = key.service.as_str(), worker_id = %hello.worker_id, device_id = %proof.device_id, "worker connected");
    let worker_id = hello.worker_id.clone();
    let _worker_guard = observability.connect_worker(
        &worker_id,
        &proof.device_id,
        &key.client_id,
        key.service.as_str(),
    );
    let mut update_offers = pool.update_tx.subscribe();

    loop {
        match timeout(worker_liveness_timeout, receive_control(&mut socket)).await {
            Ok(Ok(ControlMessage::Ready)) => {
                observability.worker_state(&worker_id, "idle");
            }
            Ok(Ok(ControlMessage::ClientStatus {
                agent_version,
                client_compat_version,
                build_git_sha,
            })) => {
                observability.worker_client_status(
                    &worker_id,
                    &agent_version,
                    &client_compat_version,
                    &build_git_sha,
                );
                continue;
            }
            Ok(Ok(ControlMessage::UpdateStatus {
                update_id,
                version,
                state,
                message,
            })) => {
                observability.worker_update_status(
                    &worker_id,
                    &update_id,
                    &version,
                    node_update_state_name(&state),
                    message.as_deref(),
                );
                if node_update_state_terminal(&state) {
                    registry.clear_node_update(&key.client_id, &update_id).await;
                }
                continue;
            }
            Ok(Ok(ControlMessage::Error { message, .. })) => {
                warn!(client_id = %key.client_id, service = key.service.as_str(), %message, "worker reported an error while idle");
                observability.worker_error(&worker_id, &message);
                observability.log(
                    "warn",
                    "worker",
                    format!("worker reported an error while idle: {message}"),
                    Some(&key.client_id),
                    Some(key.service.as_str()),
                    Some(&worker_id),
                    None,
                );
                continue;
            }
            Ok(Ok(_)) => {
                let _ = send_error(&mut socket, None, "expected ready message").await;
                return;
            }
            Ok(Err(_)) | Err(_) => return,
        }

        let (assign, mut assigned) = oneshot::channel();
        if pool
            .available_tx
            .send(AvailableWorker {
                assign,
                shared_claim: None,
            })
            .await
            .is_err()
        {
            return;
        }

        let job = loop {
            tokio::select! {
                job = &mut assigned => match job {
                    Ok(job) => break job,
                    Err(_) => return,
                },
                incoming = timeout(worker_liveness_timeout, socket.recv()) => {
                    match incoming {
                        Ok(Some(Ok(Message::Ping(payload)))) => {
                            if socket.send(Message::Pong(payload)).await.is_err() {
                                return;
                            }
                        }
                        Ok(Some(Ok(Message::Text(text)))) => {
                            let Ok(control) = serde_json::from_str::<ControlMessage>(text.as_str()) else {
                                return;
                            };
                            match control {
                                ControlMessage::ClientStatus { agent_version, client_compat_version, build_git_sha } => {
                                    observability.worker_client_status(&worker_id, &agent_version, &client_compat_version, &build_git_sha);
                                }
                                ControlMessage::UpdateStatus { update_id, version, state, message } => {
                                    observability.worker_update_status(&worker_id, &update_id, &version, node_update_state_name(&state), message.as_deref());
                                    if node_update_state_terminal(&state) {
                                        registry.clear_node_update(&key.client_id, &update_id).await;
                                    }
                                }
                                ControlMessage::Error { message, .. } => observability.worker_error(&worker_id, &message),
                                _ => return,
                            }
                        }
                        _ => return,
                    }
                }
                changed = update_offers.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let offer = { update_offers.borrow_and_update().clone() };
                    if let Some(offer) = offer {
                        if send_control(&mut socket, &ControlMessage::UpdateOffer(offer)).await.is_err() {
                            return;
                        }
                    }
                }
                changed = policy_updates.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    let worker_policy = policy_updates.borrow_and_update().clone();
                    if send_control(
                        &mut socket,
                        &ControlMessage::PolicyUpdate { worker_policy },
                    )
                    .await
                    .is_err()
                    {
                        return;
                    }
                }
            }
        };

        observability.worker_state(&worker_id, "busy");
        let request_id = job.request_id.clone();
        let attempt = job.attempt;
        if let Err(error) = proxy_job_over_socket(&mut socket, job, worker_liveness_timeout).await {
            warn!(client_id = %key.client_id, service = key.service.as_str(), worker_id = %worker_id, request_id = %request_id, attempt, %error, "worker transaction failed");
            observability.worker_error(&worker_id, &error);
            observability.log(
                "error",
                "worker",
                format!("worker transaction failed on attempt {attempt}: {error}"),
                Some(&key.client_id),
                Some(key.service.as_str()),
                Some(&worker_id),
                Some(&request_id),
            );
            return;
        }
        observability.worker_completed_request(&worker_id);
        observability.worker_state(&worker_id, "idle");
    }
}

fn validate_hello(hello: &ClientHello, key: &ClientKey) -> Result<(), String> {
    if hello.protocol_version != PROTOCOL_VERSION {
        return Err(format!(
            "unsupported protocol version {}; expected {}",
            hello.protocol_version, PROTOCOL_VERSION
        ));
    }
    if hello.client_id != key.client_id || hello.service != key.service {
        return Err("hello identity does not match authenticated headers".into());
    }
    if hello.worker_id.trim().is_empty() || hello.worker_id.len() > 128 {
        return Err("invalid worker id".into());
    }
    Ok(())
}

async fn proxy_job_over_socket(
    socket: &mut WebSocket,
    job: ProxyJob,
    worker_liveness_timeout: Duration,
) -> Result<(), String> {
    let ProxyJob {
        request_id,
        method,
        path_and_query,
        headers,
        body,
        demand,
        response_head,
        response_body,
        mut cancelled,
        ..
    } = job;
    let mut response_head = Some(response_head);

    if let Err(error) = send_control(
        socket,
        &ControlMessage::RequestHead {
            request_id: request_id.clone(),
            method,
            path_and_query,
            headers,
            demand,
        },
    )
    .await
    {
        fail_response_head(&mut response_head, error.clone());
        return Err(error);
    }
    if !body.is_empty() {
        if let Err(error) = socket.send(Message::Binary(body)).await {
            let message = error.to_string();
            fail_response_head(&mut response_head, message.clone());
            return Err(message);
        }
    }
    send_control(
        socket,
        &ControlMessage::RequestEnd {
            request_id: request_id.clone(),
        },
    )
    .await
    .inspect_err(|error| fail_response_head(&mut response_head, error.clone()))?;

    loop {
        let incoming = tokio::select! {
            _ = wait_for_cancellation(&mut cancelled) => {
                let _ = send_control(
                    socket,
                    &ControlMessage::Cancel {
                        request_id: request_id.clone(),
                    },
                )
                .await;
                return Ok(());
            }
            incoming = timeout(worker_liveness_timeout, socket.recv()) => incoming,
        };
        match incoming {
            Ok(Some(Ok(Message::Text(text)))) => {
                let control: ControlMessage =
                    serde_json::from_str(text.as_str()).map_err(|error| error.to_string())?;
                match control {
                    ControlMessage::ResponseHead {
                        request_id: response_id,
                        status,
                        headers,
                    } if response_id == request_id => {
                        let Some(sender) = response_head.take() else {
                            return Err("duplicate response head".into());
                        };
                        let _ = sender.send(Ok(ResponseHeadData { status, headers }));
                    }
                    ControlMessage::ResponseEnd {
                        request_id: response_id,
                    } if response_id == request_id => {
                        if response_head.is_some() {
                            fail_response_head(
                                &mut response_head,
                                "response ended before headers".into(),
                            );
                        }
                        return Ok(());
                    }
                    ControlMessage::Error {
                        request_id: response_id,
                        message,
                    } if response_id
                        .as_deref()
                        .is_none_or(|value| value == request_id) =>
                    {
                        if response_head.is_some() {
                            fail_response_head(&mut response_head, message.clone());
                        } else {
                            let _ = response_body
                                .send(Err(io::Error::other(message.clone())))
                                .await;
                        }
                        return Err(message);
                    }
                    _ => return Err("unexpected control message during response".into()),
                }
            }
            Ok(Some(Ok(Message::Binary(chunk)))) => {
                if response_head.is_some() {
                    fail_response_head(
                        &mut response_head,
                        "response body arrived before headers".into(),
                    );
                    return Err("response body arrived before headers".into());
                }
                if response_body.send(Ok(chunk)).await.is_err() {
                    let _ = send_control(
                        socket,
                        &ControlMessage::Cancel {
                            request_id: request_id.clone(),
                        },
                    )
                    .await;
                    return Err("public response consumer disconnected".into());
                }
            }
            Ok(Some(Ok(Message::Ping(payload)))) => socket
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Ok(Some(Ok(Message::Close(_)))) | Ok(None) => {
                fail_response_head(&mut response_head, "worker disconnected".into());
                return Err("worker disconnected".into());
            }
            Ok(Some(Ok(_))) => {}
            Ok(Some(Err(error))) => {
                let message = error.to_string();
                fail_response_head(&mut response_head, message.clone());
                return Err(message);
            }
            Err(_) => {
                fail_response_head(&mut response_head, "worker heartbeat timed out".into());
                return Err("worker heartbeat timed out".into());
            }
        }
    }
}

async fn wait_for_cancellation(cancelled: &mut watch::Receiver<bool>) {
    loop {
        if *cancelled.borrow() {
            return;
        }
        if cancelled.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

fn classify_request_lane(service: TunnelService, body: &[u8]) -> RequestLane {
    if service != TunnelService::Mcp {
        return RequestLane::Execution;
    }
    let Ok(message) = serde_json::from_slice::<serde_json::Value>(body) else {
        return RequestLane::Execution;
    };
    let Some(method) = message.get("method").and_then(serde_json::Value::as_str) else {
        return RequestLane::Execution;
    };
    if matches!(method, "initialize" | "ping" | "tools/list") {
        return RequestLane::Control;
    }
    if method != "tools/call" {
        return RequestLane::Execution;
    }
    let Some(params) = message.get("params") else {
        return RequestLane::Execution;
    };
    let Some(name) = params.get("name").and_then(serde_json::Value::as_str) else {
        return RequestLane::Execution;
    };
    if matches!(
        name,
        "exec_health_check"
            | "wait_command"
            | "resolve_operation"
            | "list_sessions"
            | "send_input"
            | "kill_session"
            | "read_output"
            | "request_permissions"
    ) {
        return RequestLane::Control;
    }
    if name == "exec_many" {
        let arguments = params
            .get("arguments")
            .and_then(serde_json::Value::as_object);
        let action = arguments
            .and_then(|value| value.get("action"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("run");
        let has_commands = arguments
            .and_then(|value| value.get("commands"))
            .and_then(serde_json::Value::as_array)
            .is_some_and(|commands| !commands.is_empty());
        let has_operation_id = arguments
            .and_then(|value| value.get("operation_id"))
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        if matches!(action, "status" | "cancel" | "forget")
            || (action == "run" && has_operation_id && !has_commands)
        {
            return RequestLane::Control;
        }
    }
    RequestLane::Execution
}

fn request_operation_hash(service: TunnelService, body: &[u8]) -> Option<String> {
    if service != TunnelService::Mcp {
        return None;
    }
    let message = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    let operation_id = message
        .get("params")?
        .get("arguments")?
        .get("operation_id")?
        .as_str()?
        .trim();
    if operation_id.is_empty() {
        return None;
    }
    let digest = Sha256::digest(operation_id.as_bytes());
    Some(
        digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn with_tunnel_request_context(
    mut response: Response<Body>,
    request_id: &str,
    lane: RequestLane,
    operation_hash: Option<&str>,
) -> Response<Body> {
    if let Ok(value) = HeaderValue::from_str(request_id) {
        response.headers_mut().insert("x-tunnel-request-id", value);
    }
    response.headers_mut().insert(
        "x-tunnel-request-lane",
        HeaderValue::from_static(lane.as_str()),
    );
    if let Some(operation_hash) = operation_hash {
        if let Ok(value) = HeaderValue::from_str(operation_hash) {
            response
                .headers_mut()
                .insert("x-tunnel-operation-hash", value);
        }
    }
    response
}

async fn proxy_request(State(state): State<AppState>, request: Request<Body>) -> Response<Body> {
    let path = request.uri().path().to_string();
    let Some(route) = state.registry.lookup(&path).await else {
        return status_response(StatusCode::NOT_FOUND, "no built-in tunnel route");
    };
    let pool = route.pool;
    let client_id = route.key.client_id;
    let service_kind = route.key.service;
    let service = service_kind.as_str().to_string();

    let (parts, body) = request.into_parts();
    let request_id = Uuid::new_v4().to_string();
    let request_method = parts.method.as_str().to_string();
    let request_path = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/")
        .to_string();
    let request_started = std::time::Instant::now();

    if pool.active_workers.load(Ordering::Acquire) == 0 {
        state.observability.log(
            "warn",
            "gateway",
            format!(
                "reconnect_grace_started; grace_ms={}",
                state.reconnect_grace_timeout.as_millis()
            ),
            Some(&client_id),
            Some(&service),
            None,
            Some(&request_id),
        );
        if !wait_for_active_worker(&pool, state.reconnect_grace_timeout).await {
            state.observability.log(
                "error",
                "gateway",
                format!(
                    "reconnect_grace_expired after {} ms",
                    request_started.elapsed().as_millis()
                ),
                Some(&client_id),
                Some(&service),
                None,
                Some(&request_id),
            );
            return status_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "no built-in tunnel worker is connected",
            );
        }
        state.observability.log(
            "info",
            "gateway",
            format!(
                "reconnect_grace_recovered after {} ms; active_workers={}",
                request_started.elapsed().as_millis(),
                pool.active_workers.load(Ordering::Acquire)
            ),
            Some(&client_id),
            Some(&service),
            None,
            Some(&request_id),
        );
    }

    let body = match to_bytes(body, state.max_request_body_bytes).await {
        Ok(body) => body,
        Err(_) => return status_response(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let request_headers = encode_headers(&parts.headers);
    let retry_class =
        automatic_retry_class(&request_method, service_kind, &request_path, body.as_ref());
    let request_lane = classify_request_lane(service_kind, body.as_ref());
    let operation_hash = request_operation_hash(service_kind, body.as_ref());
    let max_attempts = if retry_class.is_some() {
        MAX_SAFE_REQUEST_ATTEMPTS
    } else {
        1
    };
    let mut request_guard = state.observability.begin_request(
        &request_id,
        &request_method,
        &request_path,
        &client_id,
        &service,
    );
    if let Some(operation_hash) = operation_hash.as_deref() {
        state.observability.log(
            "debug",
            "request",
            format!(
                "request_context lane={} operation_hash={operation_hash}",
                request_lane.as_str()
            ),
            Some(&client_id),
            Some(&service),
            None,
            Some(&request_id),
        );
    }
    let mut attempt = 1_u8;

    let (head, body_rx, queue_wait_ms) = loop {
        let policy = state.policies.current(service_kind);
        let (pending_requests, pending_limit) = match request_lane {
            RequestLane::Control => (
                pool.control_pending_requests.clone(),
                CONTROL_PENDING_REQUEST_CAPACITY,
            ),
            RequestLane::Execution => (
                pool.execution_pending_requests.clone(),
                usize::from(policy.max_pending_requests),
            ),
        };
        let acquire_deadline =
            Instant::now() + Duration::from_millis(policy.worker_acquire_timeout_ms);
        let Some(pending_slot) = PendingRequestSlot::acquire_until(
            pending_requests.clone(),
            pool.pending_queue.clone(),
            pending_limit,
            state.observability.clone(),
            acquire_deadline,
        )
        .await
        else {
            state.observability.record_capacity_rejection();
            state.observability.record_worker_acquire_timeout();
            state.observability.log(
                "warn",
                "capacity",
                format!(
                    "worker_acquire_timeout lane={} elapsed_ms={} timeout_ms={} pending={} limit={} active_workers={} control_reserve={}",
                    request_lane.as_str(),
                    request_started.elapsed().as_millis(),
                    policy.worker_acquire_timeout_ms,
                    pending_requests.load(Ordering::Acquire),
                    pending_limit,
                    pool.active_workers.load(Ordering::Acquire),
                    CONTROL_WORKER_RESERVE
                ),
                Some(&client_id),
                Some(&service),
                None,
                Some(&request_id),
            );
            request_guard.finish(StatusCode::SERVICE_UNAVAILABLE.as_u16());
            return with_tunnel_request_context(
                capacity_response(
                    "worker_acquire_timeout",
                    "no bounded queue capacity became available before the worker-acquire deadline",
                ),
                &request_id,
                request_lane,
                operation_hash.as_deref(),
            );
        };
        let (head_tx, head_rx) = oneshot::channel();
        let (body_tx, body_rx) = mpsc::channel(RESPONSE_BODY_CAPACITY);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let (assigned_tx, assigned_rx) = oneshot::channel();
        let job = ProxyJob {
            request_id: request_id.clone(),
            method: request_method.clone(),
            path_and_query: request_path.clone(),
            headers: request_headers.clone(),
            body: body.clone(),
            lane: request_lane,
            attempt,
            enqueued_at: Instant::now(),
            policy: policy.clone(),
            active_workers: pool.active_workers.clone(),
            demand: None,
            assigned: Some(assigned_tx),
            pending_slot: Some(pending_slot),
            response_head: head_tx,
            response_body: body_tx,
            cancelled: cancel_rx,
        };

        let request_tx = match request_lane {
            RequestLane::Control => &pool.control_request_tx,
            RequestLane::Execution => &pool.execution_request_tx,
        };
        match timeout_at(acquire_deadline, request_tx.send(job)).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                state.observability.log(
                    "error",
                    "gateway",
                    format!(
                        "dispatch_failed on attempt {attempt}/{max_attempts} after {} ms: request queue closed",
                        request_started.elapsed().as_millis()
                    ),
                    Some(&client_id),
                    Some(&service),
                    None,
                    Some(&request_id),
                );
                request_guard.finish(StatusCode::SERVICE_UNAVAILABLE.as_u16());
                return status_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "tunnel route is unavailable",
                );
            }
            Err(_) => {
                let _ = cancel_tx.send(true);
                state.observability.record_worker_acquire_timeout();
                state.observability.log(
                    "warn",
                    "capacity",
                    format!(
                        "worker_acquire_timeout while entering dispatcher after {} ms; timeout_ms={}; pending={}; active_workers={}",
                        request_started.elapsed().as_millis(),
                        policy.worker_acquire_timeout_ms,
                        pending_requests.load(Ordering::Acquire),
                        pool.active_workers.load(Ordering::Acquire)
                    ),
                    Some(&client_id),
                    Some(&service),
                    None,
                    Some(&request_id),
                );
                request_guard.finish(StatusCode::SERVICE_UNAVAILABLE.as_u16());
                return with_tunnel_request_context(
                    capacity_response(
                        "worker_acquire_timeout",
                        "the tunnel dispatcher did not accept the request before the queue deadline",
                    ),
                    &request_id,
                    request_lane,
                    operation_hash.as_deref(),
                );
            }
        }

        let assignment = match timeout_at(acquire_deadline, assigned_rx).await {
            Ok(Ok(assignment)) => assignment,
            Ok(Err(_)) => {
                let _ = cancel_tx.send(true);
                state.observability.log(
                    "error",
                    "gateway",
                    "worker assignment channel closed before dispatch",
                    Some(&client_id),
                    Some(&service),
                    None,
                    Some(&request_id),
                );
                request_guard.finish(StatusCode::SERVICE_UNAVAILABLE.as_u16());
                return status_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "tunnel route is unavailable",
                );
            }
            Err(_) => {
                let _ = cancel_tx.send(true);
                state.observability.record_worker_acquire_timeout();
                state.observability.log(
                    "warn",
                    "capacity",
                    format!(
                        "worker_acquire_timeout lane={} after {} ms; timeout_ms={}; pending={}; active_workers={}; control_reserve={}",
                        request_lane.as_str(),
                        request_started.elapsed().as_millis(),
                        policy.worker_acquire_timeout_ms,
                        pending_requests.load(Ordering::Acquire),
                        pool.active_workers.load(Ordering::Acquire),
                        CONTROL_WORKER_RESERVE
                    ),
                    Some(&client_id),
                    Some(&service),
                    None,
                    Some(&request_id),
                );
                request_guard.finish(StatusCode::SERVICE_UNAVAILABLE.as_u16());
                return with_tunnel_request_context(
                    capacity_response(
                        "worker_acquire_timeout",
                        "no tunnel worker became available before the queue deadline",
                    ),
                    &request_id,
                    request_lane,
                    operation_hash.as_deref(),
                );
            }
        };
        state
            .observability
            .record_worker_assignment(assignment.queue_wait_ms);

        match timeout(state.response_head_timeout, head_rx).await {
            Ok(Ok(Ok(head))) => break (head, body_rx, assignment.queue_wait_ms),
            Ok(Ok(Err(message))) => {
                let _ = cancel_tx.send(true);
                state.observability.log(
                    "error",
                    "gateway",
                    format!(
                        "response_head_error on attempt {attempt}/{max_attempts} after assignment; queue_wait_ms={}; elapsed_ms={}: {message}",
                        assignment.queue_wait_ms,
                        request_started.elapsed().as_millis()
                    ),
                    Some(&client_id),
                    Some(&service),
                    None,
                    Some(&request_id),
                );
                if attempt < max_attempts {
                    attempt += 1;
                    state.observability.log(
                        "warn",
                        "gateway",
                        format!(
                            "retrying_request attempt={attempt}/{max_attempts}; reason=response_head_error; retry_class={}; active_workers={}",
                            retry_class.unwrap_or("disabled"),
                            pool.active_workers.load(Ordering::Acquire)
                        ),
                        Some(&client_id),
                        Some(&service),
                        None,
                        Some(&request_id),
                    );
                    continue;
                }
                request_guard.finish(StatusCode::BAD_GATEWAY.as_u16());
                return with_tunnel_request_context(
                    status_response(StatusCode::BAD_GATEWAY, &message),
                    &request_id,
                    request_lane,
                    operation_hash.as_deref(),
                );
            }
            Ok(Err(_)) => {
                let _ = cancel_tx.send(true);
                state.observability.log(
                    "error",
                    "gateway",
                    format!(
                        "response_head_channel_closed on attempt {attempt}/{max_attempts} after assignment; queue_wait_ms={}; elapsed_ms={}",
                        assignment.queue_wait_ms,
                        request_started.elapsed().as_millis()
                    ),
                    Some(&client_id),
                    Some(&service),
                    None,
                    Some(&request_id),
                );
                if attempt < max_attempts {
                    attempt += 1;
                    state.observability.log(
                        "warn",
                        "gateway",
                        format!(
                            "retrying_request attempt={attempt}/{max_attempts}; reason=response_head_channel_closed; retry_class={}; active_workers={}",
                            retry_class.unwrap_or("disabled"),
                            pool.active_workers.load(Ordering::Acquire)
                        ),
                        Some(&client_id),
                        Some(&service),
                        None,
                        Some(&request_id),
                    );
                    continue;
                }
                request_guard.finish(StatusCode::BAD_GATEWAY.as_u16());
                return with_tunnel_request_context(
                    status_response(StatusCode::BAD_GATEWAY, "tunnel worker disconnected"),
                    &request_id,
                    request_lane,
                    operation_hash.as_deref(),
                );
            }
            Err(_) => {
                let _ = cancel_tx.send(true);
                state.observability.log(
                    "error",
                    "gateway",
                    format!(
                        "response_head_timeout on attempt {attempt}/{max_attempts} after worker assignment; queue_wait_ms={}; response_timeout_ms={}; active_workers={}",
                        assignment.queue_wait_ms,
                        state.response_head_timeout.as_millis(),
                        pool.active_workers.load(Ordering::Acquire)
                    ),
                    Some(&client_id),
                    Some(&service),
                    None,
                    Some(&request_id),
                );
                request_guard.finish(StatusCode::GATEWAY_TIMEOUT.as_u16());
                return with_tunnel_request_context(
                    status_response(
                        StatusCode::GATEWAY_TIMEOUT,
                        "assigned tunnel worker did not provide response headers before the deadline",
                    ),
                    &request_id,
                    request_lane,
                    operation_hash.as_deref(),
                );
            }
        }
    };

    let status = match StatusCode::from_u16(head.status) {
        Ok(status) => status,
        Err(error) => {
            state.observability.log(
                "error",
                "gateway",
                format!(
                    "invalid_response_status after {} ms: status={} error={error}",
                    request_started.elapsed().as_millis(),
                    head.status
                ),
                Some(&client_id),
                Some(&service),
                None,
                Some(&request_id),
            );
            StatusCode::BAD_GATEWAY
        }
    };
    request_guard.finish(status.as_u16());
    let mut builder = Response::builder()
        .status(status)
        .header("x-tunnel-queue-wait-ms", queue_wait_ms.to_string())
        .header("x-tunnel-request-id", &request_id)
        .header("x-tunnel-request-lane", request_lane.as_str());
    if let Some(operation_hash) = operation_hash.as_deref() {
        builder = builder.header("x-tunnel-operation-hash", operation_hash);
    }
    for header in head.headers {
        if is_hop_by_hop_header(&header.name) {
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
    builder
        .body(Body::from_stream(ReceiverStream::new(body_rx)))
        .unwrap_or_else(|_| status_response(StatusCode::BAD_GATEWAY, "invalid tunnel response"))
}

async fn wait_for_active_worker(pool: &ClientPool, grace: Duration) -> bool {
    if pool.active_workers.load(Ordering::Acquire) > 0 {
        return true;
    }
    let deadline = Instant::now() + grace;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        sleep(remaining.min(WORKER_POLL_INTERVAL)).await;
        if pool.active_workers.load(Ordering::Acquire) > 0 {
            return true;
        }
    }
}

fn automatic_retry_class(
    method: &str,
    service: TunnelService,
    path_and_query: &str,
    body: &[u8],
) -> Option<&'static str> {
    if matches!(method, "GET" | "HEAD" | "OPTIONS") {
        return Some("safe_http_method");
    }
    if method != "POST" {
        return None;
    }
    match service {
        TunnelService::Mcp if is_retry_safe_mcp_request(body) => Some("mcp_read_only"),
        TunnelService::Actions => action_tool_name(path_and_query)
            .filter(|name| is_retry_safe_tool_name(name))
            .map(|_| "actions_read_only"),
        _ => None,
    }
}

fn action_tool_name(path_and_query: &str) -> Option<&str> {
    let path = path_and_query.split('?').next().unwrap_or(path_and_query);
    let marker = "/actions/";
    let start = path.rfind(marker)? + marker.len();
    let name = &path[start..];
    (!name.is_empty() && !name.contains('/')).then_some(name)
}

fn encode_headers(headers: &HeaderMap) -> Vec<HeaderPair> {
    headers
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
        .collect()
}

fn node_update_state_name(state: &NodeUpdateState) -> &'static str {
    match state {
        NodeUpdateState::Downloading => "downloading",
        NodeUpdateState::Verified => "verified",
        NodeUpdateState::Draining => "draining",
        NodeUpdateState::Scheduled => "scheduled",
        NodeUpdateState::Failed => "failed",
    }
}

fn node_update_state_terminal(state: &NodeUpdateState) -> bool {
    matches!(state, NodeUpdateState::Scheduled | NodeUpdateState::Failed)
}

async fn receive_multiplex_control(
    socket: &mut WebSocket,
) -> Result<MultiplexControlMessage, String> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(text.as_str()).map_err(|error| error.to_string());
            }
            Some(Ok(Message::Ping(payload))) => socket
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Some(Ok(Message::Close(_))) | None => return Err("multiplex connection closed".into()),
            Some(Ok(Message::Binary(_))) => {
                return Err("binary stream frame received before dispatcher is active".into())
            }
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.to_string()),
        }
    }
}

async fn send_multiplex_control(
    socket: &mut WebSocket,
    message: &MultiplexControlMessage,
) -> Result<(), String> {
    let encoded = serde_json::to_string(message).map_err(|error| error.to_string())?;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|error| error.to_string())
}

async fn send_multiplex_error(socket: &mut WebSocket, message: &str) -> Result<(), String> {
    send_multiplex_control(
        socket,
        &MultiplexControlMessage::Error {
            route_id: None,
            stream_id: None,
            message: message.to_string(),
        },
    )
    .await
}

async fn receive_control(socket: &mut WebSocket) -> Result<ControlMessage, String> {
    loop {
        match socket.recv().await {
            Some(Ok(Message::Text(text))) => {
                return serde_json::from_str(text.as_str()).map_err(|error| error.to_string());
            }
            Some(Ok(Message::Ping(payload))) => socket
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
            Some(Ok(_)) => return Err("expected a text control message".into()),
            Some(Err(error)) => return Err(error.to_string()),
        }
    }
}

async fn send_control(socket: &mut WebSocket, message: &ControlMessage) -> Result<(), String> {
    let encoded = serde_json::to_string(message).map_err(|error| error.to_string())?;
    socket
        .send(Message::Text(encoded.into()))
        .await
        .map_err(|error| error.to_string())
}

async fn send_error(
    socket: &mut WebSocket,
    request_id: Option<String>,
    message: &str,
) -> Result<(), String> {
    send_control(
        socket,
        &ControlMessage::Error {
            request_id,
            message: message.to_string(),
        },
    )
    .await
}

fn fail_response_head(
    sender: &mut Option<oneshot::Sender<Result<ResponseHeadData, String>>>,
    message: String,
) {
    if let Some(sender) = sender.take() {
        let _ = sender.send(Err(message));
    }
}

fn header_offers_protocol(headers: &HeaderMap, protocol: &str) -> bool {
    header_text(headers, "sec-websocket-protocol")
        .is_some_and(|value| value.split(',').any(|item| item.trim() == protocol))
}

fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok().map(str::trim)
}

fn capacity_response(reason: &str, message: &str) -> Response<Body> {
    Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header("content-type", "text/plain; charset=utf-8")
        .header("retry-after", "1")
        .header("x-tunnel-error", reason)
        .body(Body::from(message.to_string()))
        .expect("static capacity response")
}

fn status_response(status: StatusCode, message: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header("content-type", "text/plain; charset=utf-8")
        .body(Body::from(message.to_string()))
        .expect("static response")
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine;
    use coding_tools_tunnel_protocol::{DeviceAuthProof, WorkerPolicy};
    use ed25519_dalek::{Signer, SigningKey};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    fn test_server_identity() -> ServerIdentity {
        let directory = tempfile::tempdir().expect("server identity tempdir");
        ServerIdentity::load_or_create(&directory.path().join("server-identity.json"))
            .expect("test server identity")
    }

    #[tokio::test]
    async fn terminal_update_status_clears_only_the_matching_offer() {
        let registry = Registry::default();
        let key = ClientKey {
            client_id: "pc-a".into(),
            service: TunnelService::Mcp,
        };
        let pool = registry.register(key).await;
        let offer = NodeUpdateOffer {
            update_id: "update-1".into(),
            version: "0.29.34".into(),
            url: "https://updates.example.test/ctnode.zip".into(),
            sha256: "a".repeat(64),
            signature: "b".repeat(86),
        };
        registry.dispatch_node_update("pc-a", offer.clone()).await;
        assert_eq!(pool.update_tx.borrow().as_ref(), Some(&offer));

        assert!(!registry.clear_node_update("pc-a", "older-update").await);
        assert_eq!(pool.update_tx.borrow().as_ref(), Some(&offer));

        assert!(registry.clear_node_update("pc-a", "update-1").await);
        assert!(pool.update_tx.borrow().is_none());
        assert!(!registry.clear_node_update("pc-a", "update-1").await);
    }

    #[test]
    fn terminal_update_state_is_scheduled_or_failed_only() {
        assert!(!node_update_state_terminal(&NodeUpdateState::Downloading));
        assert!(!node_update_state_terminal(&NodeUpdateState::Verified));
        assert!(!node_update_state_terminal(&NodeUpdateState::Draining));
        assert!(node_update_state_terminal(&NodeUpdateState::Scheduled));
        assert!(node_update_state_terminal(&NodeUpdateState::Failed));
    }

    #[test]
    fn pending_queue_tracker_advances_to_the_next_oldest_request() {
        let tracker = PendingQueueTracker::default();
        let now = Instant::now();
        {
            let mut entries = tracker.entries.lock().expect("pending queue tracker");
            entries.insert(1, now - Duration::from_millis(80));
            entries.insert(2, now - Duration::from_millis(20));
        }

        assert!(tracker.oldest_wait_ms() >= 70);
        tracker.exit(1);
        let next_wait = tracker.oldest_wait_ms();
        assert!(next_wait >= 10);
        assert!(next_wait < 70);
        tracker.exit(2);
        assert_eq!(tracker.oldest_wait_ms(), 0);
    }

    #[tokio::test]
    async fn split_session_demand_reports_the_oldest_pending_route_wait() {
        let registry = Registry::default();
        let sessions = SplitSessionRegistry::default();
        let session = sessions
            .create(
                "queue-demand-device".into(),
                MultiplexConnectionPolicy::default(),
            )
            .await;
        let key = ClientKey {
            client_id: "queue-demand".into(),
            service: TunnelService::Mcp,
        };
        let observability = Observability::new();
        let (route_id, created) = session
            .register_route(&registry, &observability, key)
            .await
            .expect("register split route");
        assert!(created);
        assert_eq!(observability.connections().len(), 1);
        let pool = session
            .routes
            .lock()
            .await
            .get(&route_id)
            .expect("registered split route")
            .pool
            .clone();
        pool.execution_pending_requests.store(2, Ordering::Release);
        let now = Instant::now();
        {
            let mut entries = pool
                .pending_queue
                .entries
                .lock()
                .expect("pending queue tracker");
            entries.insert(1, now - Duration::from_millis(75));
            entries.insert(2, now - Duration::from_millis(15));
        }

        let demand = session.demand().await;
        assert_eq!(demand.queued_requests, 2);
        assert!(demand.oldest_queue_wait_ms >= 65);

        pool.pending_queue.exit(1);
        let demand = session.demand().await;
        assert!(demand.oldest_queue_wait_ms >= 5);
        assert!(demand.oldest_queue_wait_ms < 65);

        assert!(session.unregister_route(route_id).await);
        assert!(observability.connections().is_empty());
    }

    #[test]
    fn shared_multiplex_slot_has_one_winner_and_rejects_stale_offers_after_reset() {
        let claimed = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicUsize::new(7));
        let offer_a = SharedWorkerClaim {
            claimed: claimed.clone(),
            generation: generation.clone(),
            expected_generation: 7,
        };
        let offer_b = offer_a.clone();

        assert!(offer_a.try_claim());
        assert!(!offer_b.try_claim());

        generation.fetch_add(1, Ordering::AcqRel);
        claimed.store(false, Ordering::Release);
        assert!(!offer_a.try_claim());
        assert!(!offer_b.try_claim());

        let fresh = SharedWorkerClaim {
            claimed,
            generation,
            expected_generation: 8,
        };
        assert!(fresh.try_claim());
    }

    #[tokio::test]
    async fn multiplex_assignment_relay_closes_stale_offer_when_connection_stops() {
        let (mut assign, assigned) = oneshot::channel::<ProxyJob>();
        let (assignment_tx, assignment_rx) = mpsc::channel::<MultiplexAssignment>(1);
        let relay = tokio::spawn(relay_multiplex_assignment(0, 1, assigned, assignment_tx));

        drop(assignment_rx);
        timeout(Duration::from_secs(1), assign.closed())
            .await
            .expect("stale worker offer should close when its data connection stops");
        relay.await.expect("assignment relay task");
        assert!(assign.is_closed());
    }

    #[test]
    fn dispatcher_discards_closed_worker_slots_before_assignment() {
        let (closed_assign, closed_receiver) = oneshot::channel::<ProxyJob>();
        drop(closed_receiver);
        let (live_assign, _live_receiver) = oneshot::channel::<ProxyJob>();
        let mut workers = VecDeque::from([
            AvailableWorker {
                assign: closed_assign,
                shared_claim: None,
            },
            AvailableWorker {
                assign: live_assign,
                shared_claim: None,
            },
        ]);

        assert_eq!(discard_closed_workers(&mut workers), 1);
        assert_eq!(workers.len(), 1);
        assert!(!workers.front().expect("live worker").assign.is_closed());
    }

    async fn start_test_server(
        devices: DeviceRegistry,
        registry: Registry,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let policies = WorkerPolicyStore::from_writer(devices.database_writer())
            .expect("test worker policies");
        let state = AppState {
            registry,
            devices,
            policies,
            observability: Observability::new(),
            server_identity: test_server_identity(),
            node_updates: node_update::NodeUpdateCatalog::disabled(),
            multiplex_sessions: SplitSessionRegistry::default(),
            max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
            response_head_timeout: RESPONSE_HEAD_TIMEOUT,
            reconnect_grace_timeout: RECONNECT_GRACE_TIMEOUT,
            worker_liveness_timeout: WORKER_LIVENESS_TIMEOUT,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test address");
        let server = tokio::spawn(async move {
            axum::serve(listener, build_app(state))
                .await
                .expect("serve");
        });
        (address, server)
    }

    async fn start_test_server_with_policies(
        devices: DeviceRegistry,
        registry: Registry,
        policies: WorkerPolicyStore,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let state = AppState {
            registry,
            devices,
            policies,
            observability: Observability::new(),
            server_identity: test_server_identity(),
            node_updates: node_update::NodeUpdateCatalog::disabled(),
            multiplex_sessions: SplitSessionRegistry::default(),
            max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
            response_head_timeout: RESPONSE_HEAD_TIMEOUT,
            reconnect_grace_timeout: RECONNECT_GRACE_TIMEOUT,
            worker_liveness_timeout: WORKER_LIVENESS_TIMEOUT,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test address");
        let server = tokio::spawn(async move {
            axum::serve(listener, build_app(state))
                .await
                .expect("serve");
        });
        (address, server)
    }

    async fn start_test_server_with_timeouts(
        devices: DeviceRegistry,
        registry: Registry,
        response_head_timeout: Duration,
        reconnect_grace_timeout: Duration,
        worker_liveness_timeout: Duration,
    ) -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let policies = WorkerPolicyStore::from_writer(devices.database_writer())
            .expect("test worker policies");
        let state = AppState {
            registry,
            devices,
            policies,
            observability: Observability::new(),
            server_identity: test_server_identity(),
            node_updates: node_update::NodeUpdateCatalog::disabled(),
            multiplex_sessions: SplitSessionRegistry::default(),
            max_request_body_bytes: MAX_REQUEST_BODY_BYTES,
            response_head_timeout,
            reconnect_grace_timeout,
            worker_liveness_timeout,
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("test address");
        let server = tokio::spawn(async move {
            axum::serve(listener, build_app(state))
                .await
                .expect("serve");
        });
        (address, server)
    }

    async fn enroll_test_device(
        address: SocketAddr,
        devices: &DeviceRegistry,
    ) -> (SigningKey, String) {
        let grant = devices
            .create_enrollment("pc-a", AllowedServices::Both, Some(60))
            .expect("enrollment grant");
        let signing_key = SigningKey::from_bytes(&[17_u8; 32]);
        let enrollment = reqwest::Client::new()
            .post(format!(
                "http://{address}{ENROLL_PATH_PREFIX}/{}",
                grant.code
            ))
            .json(&EnrollmentRequest {
                device_id: "device-resilience".into(),
                client_id: "pc-a".into(),
                device_name: "resilience test".into(),
                public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
            })
            .send()
            .await
            .expect("enrollment request");
        assert_eq!(enrollment.status(), StatusCode::CREATED);
        let enrolled = enrollment
            .json::<coding_tools_tunnel_protocol::EnrollmentResponse>()
            .await
            .expect("enrollment response");
        (signing_key, enrolled.device_id)
    }

    async fn finish_worker_auth(
        worker: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> WorkerPolicy {
        let hello_ack = worker
            .next()
            .await
            .expect("hello ack frame")
            .expect("hello ack");
        let WsMessage::Text(hello_ack) = hello_ack else {
            panic!("expected hello ack text");
        };
        let ControlMessage::HelloAck {
            protocol_version,
            worker_policy,
            ..
        } = serde_json::from_str::<ControlMessage>(hello_ack.as_ref()).expect("hello ack json")
        else {
            panic!("expected hello ack control");
        };
        assert_eq!(protocol_version, PROTOCOL_VERSION);
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::Ready)
                    .expect("ready json")
                    .into(),
            ))
            .await
            .expect("send ready");
        worker_policy
    }

    async fn authenticate_worker(
        address: SocketAddr,
        signing_key: &SigningKey,
        device_id: &str,
    ) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>
    {
        let mut request = format!("ws://{address}{WS_PATH}")
            .into_client_request()
            .expect("websocket request");
        request
            .headers_mut()
            .insert(CLIENT_ID_HEADER, "pc-a".parse().expect("client id"));
        request
            .headers_mut()
            .insert(SERVICE_HEADER, "mcp".parse().expect("service"));
        request
            .headers_mut()
            .insert(DEVICE_ID_HEADER, device_id.parse().expect("device id"));
        request
            .headers_mut()
            .insert(WORKER_ID_HEADER, "worker-1".parse().expect("worker id"));
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            WS_SUBPROTOCOL.parse().expect("subprotocol"),
        );
        let (mut worker, response) = connect_async(request).await.expect("connect worker");
        assert_eq!(
            response
                .headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok()),
            Some(WS_SUBPROTOCOL)
        );

        let challenge = worker
            .next()
            .await
            .expect("challenge frame")
            .expect("challenge");
        let WsMessage::Text(challenge) = challenge else {
            panic!("expected challenge text");
        };
        let ControlMessage::Challenge {
            nonce,
            expires_at_unix_ms,
            ..
        } = serde_json::from_str::<ControlMessage>(challenge.as_ref()).expect("challenge json")
        else {
            panic!("expected challenge");
        };
        assert!(expires_at_unix_ms >= unix_ms());

        let mut proof = DeviceAuthProof {
            hello: ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client_id: "pc-a".into(),
                service: TunnelService::Mcp,
                worker_id: "worker-1".into(),
            },
            device_id: device_id.into(),
            signature: String::new(),
        };
        proof.signature = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(&coding_tools_tunnel_protocol::auth_signing_payload(
                    &nonce, &proof,
                ))
                .to_bytes(),
        );
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::Authenticate(proof))
                    .expect("auth json")
                    .into(),
            ))
            .await
            .expect("send auth");
        worker
    }

    #[tokio::test]
    async fn public_listener_does_not_mount_admin_routes() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let (address, server) = start_test_server(devices, Registry::default()).await;

        for path in ["/api/devices", "/api/enrollments", "/_tunnel/admin"] {
            let response = reqwest::get(format!("http://{address}{path}"))
                .await
                .expect("public request");
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "path={path}");
        }

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn split_v5_keeps_control_independent_and_routes_requests_over_data_socket() {
        use coding_tools_tunnel_protocol::{
            split_session_auth_signing_payload, MultiplexConnectionRole, SplitSessionAuthProof,
            SplitSessionHello,
        };

        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let signing_key = SigningKey::from_bytes(&[53_u8; 32]);
        let device_id = "device-split-v5";
        let grant = devices
            .create_enrollment("workspace-a", AllowedServices::Mcp, Some(60))
            .expect("grant");
        devices
            .enroll(
                grant.code,
                EnrollmentRequest {
                    device_id: device_id.into(),
                    client_id: "workspace-a".into(),
                    device_name: "split v5".into(),
                    public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
                },
            )
            .await
            .expect("enroll");
        let (address, server) = start_test_server(devices, Registry::default()).await;

        async fn split_connect(
            address: SocketAddr,
            signing_key: &SigningKey,
            device_id: &str,
            connection_id: &str,
            role: MultiplexConnectionRole,
            session_id: Option<&str>,
        ) -> tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        > {
            let protocol = match role {
                MultiplexConnectionRole::Control => MULTIPLEX_CONTROL_WS_SUBPROTOCOL,
                MultiplexConnectionRole::Data => MULTIPLEX_DATA_WS_SUBPROTOCOL,
            };
            let mut request = format!("ws://{address}{WS_PATH}")
                .into_client_request()
                .expect("request");
            request
                .headers_mut()
                .insert(DEVICE_ID_HEADER, device_id.parse().unwrap());
            request
                .headers_mut()
                .insert(CONNECTION_ID_HEADER, connection_id.parse().unwrap());
            if let Some(session_id) = session_id {
                request
                    .headers_mut()
                    .insert(SESSION_ID_HEADER, session_id.parse().unwrap());
            }
            request
                .headers_mut()
                .insert(SEC_WEBSOCKET_PROTOCOL, protocol.parse().unwrap());
            let (mut socket, response) =
                connect_async(request).await.expect("connect split socket");
            assert_eq!(
                response
                    .headers()
                    .get(SEC_WEBSOCKET_PROTOCOL)
                    .and_then(|v| v.to_str().ok()),
                Some(protocol)
            );
            let challenge = socket.next().await.unwrap().unwrap();
            let WsMessage::Text(challenge) = challenge else {
                panic!("challenge text")
            };
            let MultiplexControlMessage::Challenge { nonce, .. } =
                serde_json::from_str(challenge.as_ref()).unwrap()
            else {
                panic!("challenge")
            };
            let mut proof = SplitSessionAuthProof {
                hello: SplitSessionHello {
                    protocol_version: MULTIPLEX_PROTOCOL_VERSION,
                    connection_role: role,
                    connection_id: connection_id.into(),
                    session_id: session_id.map(str::to_string),
                },
                device_id: device_id.into(),
                signature: String::new(),
            };
            proof.signature = URL_SAFE_NO_PAD.encode(
                signing_key
                    .sign(&split_session_auth_signing_payload(&nonce, &proof))
                    .to_bytes(),
            );
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&MultiplexControlMessage::SessionAuthenticate(proof))
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            socket
        }

        let mut control = split_connect(
            address,
            &signing_key,
            device_id,
            "control-1",
            MultiplexConnectionRole::Control,
            None,
        )
        .await;
        let session_id = loop {
            let frame = timeout(Duration::from_secs(2), control.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let WsMessage::Text(text) = frame else {
                continue;
            };
            match serde_json::from_str::<MultiplexControlMessage>(text.as_ref()).unwrap() {
                MultiplexControlMessage::SessionReady {
                    connection_role: MultiplexConnectionRole::Control,
                    session_id,
                    ..
                } => break session_id,
                other => panic!("unexpected control handshake frame: {other:?}"),
            }
        };
        control
            .send(WsMessage::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegister {
                    registration_id: "route-a".into(),
                    client_id: "workspace-a".into(),
                    service: TunnelService::Mcp,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let route_id = loop {
            let frame = timeout(Duration::from_secs(2), control.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let WsMessage::Text(text) = frame else {
                continue;
            };
            match serde_json::from_str::<MultiplexControlMessage>(text.as_ref()).unwrap() {
                MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id,
                    ..
                } if registration_id == "route-a" => break route_id,
                MultiplexControlMessage::CapacityUpdate { .. } => {}
                other => panic!("unexpected route control frame: {other:?}"),
            }
        };

        let mut data = split_connect(
            address,
            &signing_key,
            device_id,
            "data-1",
            MultiplexConnectionRole::Data,
            Some(&session_id),
        )
        .await;
        let ready = timeout(Duration::from_secs(2), data.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let WsMessage::Text(ready) = ready else {
            panic!("data ready text")
        };
        assert!(
            matches!(serde_json::from_str::<MultiplexControlMessage>(ready.as_ref()).unwrap(),
            MultiplexControlMessage::SessionReady { connection_role: MultiplexConnectionRole::Data, session_id: ref accepted, .. } if accepted == &session_id)
        );

        let public = tokio::spawn(async move {
            let response = reqwest::Client::new()
                .post(format!(
                    "http://{address}/builtin/clients/workspace-a/mcp?split=1"
                ))
                .body("hello-split")
                .send()
                .await
                .expect("public request");
            (response.status(), response.bytes().await.unwrap())
        });

        let mut stream_id = None;
        let mut body = None;
        let mut ended = false;
        while !ended {
            let frame = timeout(Duration::from_secs(2), data.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            match frame {
                WsMessage::Text(text) => {
                    match serde_json::from_str::<MultiplexControlMessage>(text.as_ref()).unwrap() {
                        MultiplexControlMessage::RequestHead {
                            route_id: actual_route,
                            stream_id: actual_stream,
                            demand,
                            path_and_query,
                            ..
                        } => {
                            assert_eq!(actual_route, route_id);
                            assert!(actual_stream > 0);
                            assert!(demand.is_none());
                            assert!(path_and_query.contains("split=1"));
                            stream_id = Some(actual_stream);
                        }
                        MultiplexControlMessage::RequestEnd {
                            route_id: actual_route,
                            stream_id: actual_stream,
                        } => {
                            assert_eq!(actual_route, route_id);
                            assert_eq!(Some(actual_stream), stream_id);
                            ended = true;
                        }
                        other => panic!("unexpected data request frame: {other:?}"),
                    }
                }
                WsMessage::Binary(frame) => {
                    let frame = decode_multiplex_binary_frame(frame.as_ref()).unwrap();
                    assert_eq!(frame.route_id, route_id);
                    body = Some(frame.payload);
                }
                other => panic!("unexpected data websocket frame: {other:?}"),
            }
        }
        assert_eq!(body.as_deref(), Some(b"hello-split".as_slice()));
        let stream_id = stream_id.unwrap();

        control.send(WsMessage::Close(None)).await.unwrap();
        drop(control);
        sleep(Duration::from_millis(100)).await;

        let paused_public = tokio::spawn(async move {
            let response = reqwest::Client::new()
                .post(format!(
                    "http://{address}/builtin/clients/workspace-a/mcp?split=2"
                ))
                .body("after-rebind")
                .send()
                .await
                .expect("paused public request");
            (response.status(), response.bytes().await.unwrap())
        });
        assert!(
            timeout(Duration::from_millis(150), data.next())
                .await
                .is_err(),
            "data socket received a new assignment while split control was detached"
        );

        let mut control = split_connect(
            address,
            &signing_key,
            device_id,
            "control-2",
            MultiplexConnectionRole::Control,
            Some(&session_id),
        )
        .await;
        let rebound_session = loop {
            let frame = timeout(Duration::from_secs(2), control.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let WsMessage::Text(text) = frame else {
                continue;
            };
            match serde_json::from_str::<MultiplexControlMessage>(text.as_ref()).unwrap() {
                MultiplexControlMessage::SessionReady {
                    connection_role: MultiplexConnectionRole::Control,
                    session_id,
                    ..
                } => break session_id,
                other => panic!("unexpected rebound control handshake frame: {other:?}"),
            }
        };
        assert_eq!(rebound_session, session_id);
        control
            .send(WsMessage::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegister {
                    registration_id: "route-rebind".into(),
                    client_id: "workspace-a".into(),
                    service: TunnelService::Mcp,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let rebound_route_id = loop {
            let frame = timeout(Duration::from_secs(2), control.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let WsMessage::Text(text) = frame else {
                continue;
            };
            match serde_json::from_str::<MultiplexControlMessage>(text.as_ref()).unwrap() {
                MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id,
                    ..
                } if registration_id == "route-rebind" => break route_id,
                MultiplexControlMessage::CapacityUpdate { .. } => {}
                other => panic!("unexpected rebound route control frame: {other:?}"),
            }
        };
        assert_eq!(rebound_route_id, route_id);

        data.send(WsMessage::Text(
            serde_json::to_string(&MultiplexControlMessage::ResponseHead {
                route_id,
                stream_id,
                status: 207,
                headers: vec![],
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        data.send(WsMessage::Binary(Bytes::from(
            encode_multiplex_binary_frame(
                MultiplexBinaryFrameKind::ResponseBody,
                route_id,
                stream_id,
                b"split-ok",
            )
            .unwrap(),
        )))
        .await
        .unwrap();
        data.send(WsMessage::Text(
            serde_json::to_string(&MultiplexControlMessage::ResponseEnd {
                route_id,
                stream_id,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();

        let (status, response_body) = public.await.unwrap();
        assert_eq!(status.as_u16(), 207);
        assert_eq!(response_body.as_ref(), b"split-ok");

        let mut rebound_stream_id = None;
        let mut rebound_body = None;
        let mut rebound_ended = false;
        while !rebound_ended {
            let frame = timeout(Duration::from_secs(2), data.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            match frame {
                WsMessage::Text(text) => {
                    match serde_json::from_str::<MultiplexControlMessage>(text.as_ref()).unwrap() {
                        MultiplexControlMessage::RequestHead {
                            route_id: actual_route,
                            stream_id: actual_stream,
                            path_and_query,
                            ..
                        } => {
                            assert_eq!(actual_route, route_id);
                            assert_ne!(actual_stream, stream_id);
                            assert!(path_and_query.contains("split=2"));
                            rebound_stream_id = Some(actual_stream);
                        }
                        MultiplexControlMessage::RequestEnd {
                            route_id: actual_route,
                            stream_id: actual_stream,
                        } => {
                            assert_eq!(actual_route, route_id);
                            assert_eq!(Some(actual_stream), rebound_stream_id);
                            rebound_ended = true;
                        }
                        other => panic!("unexpected rebound data request frame: {other:?}"),
                    }
                }
                WsMessage::Binary(frame) => {
                    let frame = decode_multiplex_binary_frame(frame.as_ref()).unwrap();
                    assert_eq!(frame.route_id, route_id);
                    rebound_body = Some(frame.payload);
                }
                other => panic!("unexpected rebound data websocket frame: {other:?}"),
            }
        }
        assert_eq!(rebound_body.as_deref(), Some(b"after-rebind".as_slice()));
        let rebound_stream_id = rebound_stream_id.unwrap();
        data.send(WsMessage::Text(
            serde_json::to_string(&MultiplexControlMessage::ResponseHead {
                route_id,
                stream_id: rebound_stream_id,
                status: 208,
                headers: vec![],
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        data.send(WsMessage::Text(
            serde_json::to_string(&MultiplexControlMessage::ResponseEnd {
                route_id,
                stream_id: rebound_stream_id,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        let (rebound_status, rebound_response_body) = paused_public.await.unwrap();
        assert_eq!(rebound_status.as_u16(), 208);
        assert!(rebound_response_body.is_empty());
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn multiplex_v5_authenticates_once_and_registers_multiple_workspace_routes() {
        use coding_tools_tunnel_protocol::{
            device_session_auth_signing_payload, DeviceSessionAuthProof, DeviceSessionHello,
        };

        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let signing_key = SigningKey::from_bytes(&[41_u8; 32]);
        let device_id = "device-shared-v5";
        for (client_id, services) in [
            ("workspace-a", AllowedServices::Mcp),
            ("workspace-b", AllowedServices::Actions),
        ] {
            let grant = devices
                .create_enrollment(client_id, services, Some(60))
                .expect("route grant");
            devices
                .enroll(
                    grant.code,
                    EnrollmentRequest {
                        device_id: device_id.into(),
                        client_id: client_id.into(),
                        device_name: "shared v5 device".into(),
                        public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
                    },
                )
                .await
                .expect("route enrollment");
        }
        let (address, server) = start_test_server(devices, Registry::default()).await;

        let connection_id = "connection-v5-1";
        let mut request = format!("ws://{address}{WS_PATH}")
            .into_client_request()
            .expect("websocket request");
        request
            .headers_mut()
            .insert(DEVICE_ID_HEADER, device_id.parse().expect("device id"));
        request.headers_mut().insert(
            CONNECTION_ID_HEADER,
            connection_id.parse().expect("connection id"),
        );
        request.headers_mut().insert(
            SEC_WEBSOCKET_PROTOCOL,
            MULTIPLEX_WS_SUBPROTOCOL.parse().expect("v5 subprotocol"),
        );
        let (mut socket, response) = connect_async(request).await.expect("connect v5");
        assert_eq!(
            response
                .headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok()),
            Some(MULTIPLEX_WS_SUBPROTOCOL)
        );

        let challenge = socket
            .next()
            .await
            .expect("challenge frame")
            .expect("challenge");
        let WsMessage::Text(challenge) = challenge else {
            panic!("expected v5 challenge text");
        };
        let MultiplexControlMessage::Challenge {
            nonce,
            expires_at_unix_ms,
            ..
        } = serde_json::from_str::<MultiplexControlMessage>(challenge.as_ref())
            .expect("v5 challenge json")
        else {
            panic!("expected v5 challenge");
        };
        assert!(expires_at_unix_ms >= unix_ms());

        let mut proof = DeviceSessionAuthProof {
            hello: DeviceSessionHello {
                protocol_version: MULTIPLEX_PROTOCOL_VERSION,
                connection_id: connection_id.into(),
            },
            device_id: device_id.into(),
            signature: String::new(),
        };
        proof.signature = URL_SAFE_NO_PAD.encode(
            signing_key
                .sign(&device_session_auth_signing_payload(&nonce, &proof))
                .to_bytes(),
        );
        socket
            .send(WsMessage::Text(
                serde_json::to_string(&MultiplexControlMessage::Authenticate(proof))
                    .expect("v5 auth json")
                    .into(),
            ))
            .await
            .expect("send v5 auth");

        let hello_ack = socket
            .next()
            .await
            .expect("hello ack frame")
            .expect("hello ack");
        let WsMessage::Text(hello_ack) = hello_ack else {
            panic!("expected v5 hello ack text");
        };
        let MultiplexControlMessage::HelloAck {
            protocol_version,
            connection_policy,
            ..
        } = serde_json::from_str::<MultiplexControlMessage>(hello_ack.as_ref())
            .expect("v5 hello ack json")
        else {
            panic!("expected v5 hello ack");
        };
        assert_eq!(protocol_version, MULTIPLEX_PROTOCOL_VERSION);
        assert_eq!(connection_policy.streams_per_connection, 16);
        assert_eq!(connection_policy.burst_scale_up_step, 4);

        let register = |registration_id: &str, client_id: &str, service: TunnelService| {
            WsMessage::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegister {
                    registration_id: registration_id.into(),
                    client_id: client_id.into(),
                    service,
                })
                .expect("route register json")
                .into(),
            )
        };
        socket
            .send(register("route-a", "workspace-a", TunnelService::Mcp))
            .await
            .expect("register workspace a");
        let route_a = socket
            .next()
            .await
            .expect("route a frame")
            .expect("route a");
        let WsMessage::Text(route_a) = route_a else {
            panic!("expected route a text");
        };
        let MultiplexControlMessage::RouteRegistered {
            registration_id,
            route_id: route_a_id,
            stream_limit,
        } = serde_json::from_str::<MultiplexControlMessage>(route_a.as_ref())
            .expect("route a response")
        else {
            panic!("expected route a registration");
        };
        assert_eq!(registration_id, "route-a");
        assert_eq!(stream_limit, 16);

        socket
            .send(register("route-b", "workspace-b", TunnelService::Actions))
            .await
            .expect("register workspace b");
        let route_b = socket
            .next()
            .await
            .expect("route b frame")
            .expect("route b");
        let WsMessage::Text(route_b) = route_b else {
            panic!("expected route b text");
        };
        let MultiplexControlMessage::RouteRegistered {
            route_id: route_b_id,
            ..
        } = serde_json::from_str::<MultiplexControlMessage>(route_b.as_ref())
            .expect("route b response")
        else {
            panic!("expected route b registration");
        };
        assert_ne!(route_a_id, route_b_id);

        socket
            .send(register("bad-route", "workspace-b", TunnelService::Mcp))
            .await
            .expect("send unauthorized route");
        let rejected = socket
            .next()
            .await
            .expect("rejection frame")
            .expect("rejection");
        let WsMessage::Text(rejected) = rejected else {
            panic!("expected rejection text");
        };
        assert!(matches!(
            serde_json::from_str::<MultiplexControlMessage>(rejected.as_ref())
                .expect("route rejection json"),
            MultiplexControlMessage::Error { message, .. }
                if message.contains("not authorized")
        ));

        let request_a = tokio::spawn(async move {
            let response = reqwest::Client::new()
                .post(format!(
                    "http://{address}/builtin/clients/workspace-a/mcp?probe=a"
                ))
                .body("alpha")
                .send()
                .await
                .expect("workspace a public request");
            let status = response.status();
            let body = response.bytes().await.expect("workspace a response body");
            (status, body)
        });
        let request_b = tokio::spawn(async move {
            let response = reqwest::Client::new()
                .post(format!(
                    "http://{address}/builtin/actions/workspace-b/run?probe=b"
                ))
                .body("beta")
                .send()
                .await
                .expect("workspace b public request");
            let status = response.status();
            let body = response.bytes().await.expect("workspace b response body");
            (status, body)
        });

        let mut stream_a = None;
        let mut stream_b = None;
        let mut body_a = None;
        let mut body_b = None;
        let mut ended = 0_u8;
        while ended < 2 {
            let frame = timeout(Duration::from_secs(2), socket.next())
                .await
                .expect("multiplex request frame timeout")
                .expect("multiplex request frame")
                .expect("multiplex request frame result");
            match frame {
                WsMessage::Text(text) => {
                    match serde_json::from_str::<MultiplexControlMessage>(text.as_ref())
                        .expect("multiplex request control")
                    {
                        MultiplexControlMessage::RequestHead {
                            route_id,
                            stream_id,
                            path_and_query,
                            ..
                        } if route_id == route_a_id => {
                            assert!(path_and_query.contains("probe=a"));
                            stream_a = Some(stream_id);
                        }
                        MultiplexControlMessage::RequestHead {
                            route_id,
                            stream_id,
                            path_and_query,
                            ..
                        } if route_id == route_b_id => {
                            assert!(path_and_query.contains("probe=b"));
                            stream_b = Some(stream_id);
                        }
                        MultiplexControlMessage::RequestEnd { .. } => ended += 1,
                        other => panic!("unexpected multiplex request control: {other:?}"),
                    }
                }
                WsMessage::Binary(frame) => {
                    let frame = decode_multiplex_binary_frame(frame.as_ref())
                        .expect("multiplex request body frame");
                    assert_eq!(frame.kind, MultiplexBinaryFrameKind::RequestBody);
                    if frame.route_id == route_a_id {
                        body_a = Some(Bytes::from(frame.payload));
                    } else if frame.route_id == route_b_id {
                        body_b = Some(Bytes::from(frame.payload));
                    } else {
                        panic!("unexpected multiplex request route {}", frame.route_id);
                    }
                }
                other => panic!("unexpected multiplex request websocket frame: {other:?}"),
            }
        }
        let stream_a = stream_a.expect("workspace a stream id");
        let stream_b = stream_b.expect("workspace b stream id");
        assert_ne!(stream_a, stream_b);
        assert_eq!(body_a.as_deref(), Some(b"alpha".as_slice()));
        assert_eq!(body_b.as_deref(), Some(b"beta".as_slice()));

        for message in [
            MultiplexControlMessage::ResponseHead {
                route_id: route_a_id,
                stream_id: stream_a,
                status: 201,
                headers: vec![],
            },
            MultiplexControlMessage::ResponseHead {
                route_id: route_b_id,
                stream_id: stream_b,
                status: 202,
                headers: vec![],
            },
        ] {
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&message)
                        .expect("multiplex response head")
                        .into(),
                ))
                .await
                .expect("send multiplex response head");
        }
        socket
            .send(WsMessage::Binary(Bytes::from(
                encode_multiplex_binary_frame(
                    MultiplexBinaryFrameKind::ResponseBody,
                    route_b_id,
                    stream_b,
                    b"pong-b",
                )
                .expect("workspace b response frame"),
            )))
            .await
            .expect("send workspace b response body");
        socket
            .send(WsMessage::Binary(Bytes::from(
                encode_multiplex_binary_frame(
                    MultiplexBinaryFrameKind::ResponseBody,
                    route_a_id,
                    stream_a,
                    b"pong-a",
                )
                .expect("workspace a response frame"),
            )))
            .await
            .expect("send workspace a response body");
        for message in [
            MultiplexControlMessage::ResponseEnd {
                route_id: route_a_id,
                stream_id: stream_a,
            },
            MultiplexControlMessage::ResponseEnd {
                route_id: route_b_id,
                stream_id: stream_b,
            },
        ] {
            socket
                .send(WsMessage::Text(
                    serde_json::to_string(&message)
                        .expect("multiplex response end")
                        .into(),
                ))
                .await
                .expect("send multiplex response end");
        }

        let (status_a, response_a) = request_a.await.expect("workspace a task");
        let (status_b, response_b) = request_b.await.expect("workspace b task");
        assert_eq!(status_a, StatusCode::CREATED);
        assert_eq!(status_b, StatusCode::ACCEPTED);
        assert_eq!(response_a.as_ref(), b"pong-a");
        assert_eq!(response_b.as_ref(), b"pong-b");

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn new_server_keeps_v4_only_client_compatible_end_to_end() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let grant = devices
            .create_enrollment("pc-a", AllowedServices::Both, Some(60))
            .expect("enrollment grant");
        let (address, server) = start_test_server(devices.clone(), Registry::default()).await;
        let signing_key = SigningKey::from_bytes(&[7_u8; 32]);
        let enrollment = reqwest::Client::new()
            .post(format!(
                "http://{address}{ENROLL_PATH_PREFIX}/{}",
                grant.code
            ))
            .json(&EnrollmentRequest {
                device_id: "device-1".into(),
                client_id: "pc-a".into(),
                device_name: "test device".into(),
                public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
            })
            .send()
            .await
            .expect("enrollment request");
        assert_eq!(enrollment.status(), StatusCode::CREATED);
        let enrolled = enrollment
            .json::<coding_tools_tunnel_protocol::EnrollmentResponse>()
            .await
            .expect("enrollment response");
        assert_eq!(enrolled.device_id, "device-1");

        let mut worker = authenticate_worker(address, &signing_key, &enrolled.device_id).await;
        let hello_ack = worker
            .next()
            .await
            .expect("hello ack frame")
            .expect("hello ack");
        let WsMessage::Text(hello_ack) = hello_ack else {
            panic!("expected hello ack text");
        };
        assert!(matches!(
            serde_json::from_str::<ControlMessage>(hello_ack.as_ref()).expect("hello ack json"),
            ControlMessage::HelloAck { protocol_version: PROTOCOL_VERSION, worker_policy, .. }
                if worker_policy == WorkerPolicy::default_for(TunnelService::Mcp)
        ));
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::Ready)
                    .expect("ready json")
                    .into(),
            ))
            .await
            .expect("send ready");

        let public_request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("http://{address}/builtin/clients/pc-a/mcp?probe=1"))
                .header("x-test", "yes")
                .body("ping")
                .send()
                .await
                .expect("public request")
        });

        let request_head = worker
            .next()
            .await
            .expect("request head frame")
            .expect("request head");
        let WsMessage::Text(request_head) = request_head else {
            panic!("expected request head text");
        };
        let ControlMessage::RequestHead {
            request_id,
            method,
            path_and_query,
            headers,
            ..
        } = serde_json::from_str::<ControlMessage>(request_head.as_ref())
            .expect("request head json")
        else {
            panic!("expected request head");
        };
        assert_eq!(method, "POST");
        assert_eq!(path_and_query, "/builtin/clients/pc-a/mcp?probe=1");
        assert!(headers
            .iter()
            .any(|header| header.name == "x-test" && header.value == "yes"));
        assert!(headers.iter().all(|header| header.name != "content-length"));

        let body = worker.next().await.expect("body frame").expect("body");
        assert_eq!(body, WsMessage::Binary(Bytes::from_static(b"ping")));
        let request_end = worker
            .next()
            .await
            .expect("request end frame")
            .expect("request end");
        let WsMessage::Text(request_end) = request_end else {
            panic!("expected request end text");
        };
        assert_eq!(
            serde_json::from_str::<ControlMessage>(request_end.as_ref()).expect("request end json"),
            ControlMessage::RequestEnd {
                request_id: request_id.clone()
            }
        );

        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseHead {
                    request_id: request_id.clone(),
                    status: 201,
                    headers: vec![HeaderPair {
                        name: "content-type".into(),
                        value: "text/plain".into(),
                    }],
                })
                .expect("response head json")
                .into(),
            ))
            .await
            .expect("send response head");
        worker
            .send(WsMessage::Binary(Bytes::from_static(b"pong")))
            .await
            .expect("send response body");
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseEnd { request_id })
                    .expect("response end json")
                    .into(),
            ))
            .await
            .expect("send response end");

        let response = public_request.await.expect("public request task");
        assert_eq!(response.status(), 201);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain"
        );
        assert_eq!(response.text().await.expect("response text"), "pong");

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn same_public_route_rejects_second_live_device_owner() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let signing_key_a = SigningKey::from_bytes(&[31_u8; 32]);
        let signing_key_b = SigningKey::from_bytes(&[32_u8; 32]);
        for (device_id, device_name, signing_key) in [
            ("device-owner-a", "route owner a", &signing_key_a),
            ("device-owner-b", "route owner b", &signing_key_b),
        ] {
            let grant = devices
                .create_enrollment("pc-a", AllowedServices::Mcp, Some(60))
                .expect("enrollment grant");
            devices
                .enroll(
                    grant.code,
                    EnrollmentRequest {
                        device_id: device_id.into(),
                        client_id: "pc-a".into(),
                        device_name: device_name.into(),
                        public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
                    },
                )
                .await
                .expect("enroll route owner");
        }

        let (address, server) = start_test_server(devices, Registry::default()).await;
        let mut owner = authenticate_worker(address, &signing_key_a, "device-owner-a").await;
        finish_worker_auth(&mut owner).await;

        let mut contender = authenticate_worker(address, &signing_key_b, "device-owner-b").await;
        let rejection = timeout(Duration::from_secs(2), contender.next())
            .await
            .expect("second device ownership decision timeout")
            .expect("second device ownership frame")
            .expect("second device ownership result");
        let WsMessage::Text(rejection) = rejection else {
            panic!("expected route ownership rejection text");
        };
        assert_eq!(
            serde_json::from_str::<ControlMessage>(rejection.as_ref())
                .expect("route ownership rejection json"),
            ControlMessage::Error {
                request_id: None,
                message: "route is already owned by another device".into(),
            }
        );

        let public_request = tokio::spawn(async move {
            reqwest::get(format!("http://{address}/builtin/clients/pc-a/mcp?owner=a"))
                .await
                .expect("owner public request")
        });
        let request_head = timeout(Duration::from_secs(2), owner.next())
            .await
            .expect("owner request timeout")
            .expect("owner request frame")
            .expect("owner request result");
        let WsMessage::Text(request_head) = request_head else {
            panic!("expected owner request head text");
        };
        let ControlMessage::RequestHead {
            request_id,
            path_and_query,
            ..
        } = serde_json::from_str::<ControlMessage>(request_head.as_ref())
            .expect("owner request head json")
        else {
            panic!("expected owner request head");
        };
        assert_eq!(path_and_query, "/builtin/clients/pc-a/mcp?owner=a");
        assert!(matches!(
            owner.next().await.expect("owner request end frame").expect("owner request end"),
            WsMessage::Text(text)
                if serde_json::from_str::<ControlMessage>(text.as_ref()).ok()
                    == Some(ControlMessage::RequestEnd { request_id: request_id.clone() })
        ));
        owner
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseHead {
                    request_id: request_id.clone(),
                    status: 204,
                    headers: vec![],
                })
                .expect("owner response head json")
                .into(),
            ))
            .await
            .expect("send owner response head");
        owner
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseEnd { request_id })
                    .expect("owner response end json")
                    .into(),
            ))
            .await
            .expect("send owner response end");
        assert_eq!(
            public_request.await.expect("owner public task").status(),
            204
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn revoked_device_is_rejected_during_websocket_authentication() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let grant = devices
            .create_enrollment("pc-a", AllowedServices::Mcp, Some(60))
            .expect("enrollment grant");
        let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
        devices
            .enroll(
                grant.code,
                EnrollmentRequest {
                    device_id: "device-2".into(),
                    client_id: "pc-a".into(),
                    device_name: "revoked device".into(),
                    public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
                },
            )
            .await
            .expect("enroll device");
        assert!(devices.revoke_device("device-2").expect("revoke device"));
        let (address, server) = start_test_server(devices, Registry::default()).await;
        let mut worker = authenticate_worker(address, &signing_key, "device-2").await;
        let rejection = worker
            .next()
            .await
            .expect("rejection frame")
            .expect("rejection");
        let WsMessage::Text(rejection) = rejection else {
            panic!("expected rejection text");
        };
        assert_eq!(
            serde_json::from_str::<ControlMessage>(rejection.as_ref()).expect("rejection json"),
            ControlMessage::Error {
                request_id: None,
                message: "tunnel device has been revoked".into(),
            }
        );
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn registered_route_without_workers_waits_for_reconnect_grace() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let registry = Registry::default();
        registry
            .register(ClientKey {
                client_id: "pc-a".into(),
                service: TunnelService::Mcp,
            })
            .await;
        let (address, server) = start_test_server_with_timeouts(
            devices,
            registry,
            Duration::from_secs(1),
            Duration::from_millis(125),
            Duration::from_secs(1),
        )
        .await;

        let started = std::time::Instant::now();
        let response = reqwest::get(format!("http://{address}/builtin/clients/pc-a/mcp"))
            .await
            .expect("public request");

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(
            response.text().await.expect("response text"),
            "no built-in tunnel worker is connected"
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn reconnect_grace_accepts_a_worker_that_returns_before_expiry() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let registry = Registry::default();
        registry
            .register(ClientKey {
                client_id: "pc-a".into(),
                service: TunnelService::Mcp,
            })
            .await;
        let (address, server) = start_test_server_with_timeouts(
            devices.clone(),
            registry,
            Duration::from_secs(1),
            Duration::from_millis(500),
            Duration::from_secs(2),
        )
        .await;
        let (signing_key, device_id) = enroll_test_device(address, &devices).await;

        let public_request = tokio::spawn(async move {
            reqwest::get(format!("http://{address}/builtin/clients/pc-a/mcp?grace=1"))
                .await
                .expect("grace public request")
        });
        tokio::time::sleep(Duration::from_millis(75)).await;
        let mut worker = authenticate_worker(address, &signing_key, &device_id).await;
        finish_worker_auth(&mut worker).await;

        let request_head = worker
            .next()
            .await
            .expect("grace request head")
            .expect("grace request head");
        let WsMessage::Text(request_head) = request_head else {
            panic!("expected grace request head text");
        };
        let ControlMessage::RequestHead { request_id, .. } =
            serde_json::from_str::<ControlMessage>(request_head.as_ref())
                .expect("grace request head json")
        else {
            panic!("expected grace request head control");
        };
        let _request_end = worker
            .next()
            .await
            .expect("grace request end")
            .expect("grace request end");
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseHead {
                    request_id: request_id.clone(),
                    status: 204,
                    headers: vec![],
                })
                .expect("grace response head")
                .into(),
            ))
            .await
            .expect("send grace response head");
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseEnd { request_id })
                    .expect("grace response end")
                    .into(),
            ))
            .await
            .expect("send grace response end");

        assert_eq!(
            public_request.await.expect("grace request task").status(),
            StatusCode::NO_CONTENT
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn safe_get_retries_after_worker_disconnects_before_response_head() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let (address, server) = start_test_server_with_timeouts(
            devices.clone(),
            Registry::default(),
            Duration::from_secs(2),
            Duration::from_millis(500),
            Duration::from_secs(2),
        )
        .await;
        let (signing_key, device_id) = enroll_test_device(address, &devices).await;
        let mut first_worker = authenticate_worker(address, &signing_key, &device_id).await;
        finish_worker_auth(&mut first_worker).await;

        let public_request = tokio::spawn(async move {
            reqwest::get(format!("http://{address}/builtin/clients/pc-a/mcp?retry=1"))
                .await
                .expect("retry public request")
        });
        let first_head = first_worker
            .next()
            .await
            .expect("first retry head")
            .expect("first retry head");
        let WsMessage::Text(first_head) = first_head else {
            panic!("expected first retry head text");
        };
        let ControlMessage::RequestHead {
            request_id: first_request_id,
            ..
        } = serde_json::from_str::<ControlMessage>(first_head.as_ref())
            .expect("first retry head json")
        else {
            panic!("expected first retry request head");
        };
        let _first_end = first_worker
            .next()
            .await
            .expect("first retry end")
            .expect("first retry end");
        first_worker
            .send(WsMessage::Close(None))
            .await
            .expect("close first worker");
        drop(first_worker);

        let mut replacement = authenticate_worker(address, &signing_key, &device_id).await;
        finish_worker_auth(&mut replacement).await;
        let retry_head = replacement
            .next()
            .await
            .expect("replacement retry head")
            .expect("replacement retry head");
        let WsMessage::Text(retry_head) = retry_head else {
            panic!("expected replacement retry head text");
        };
        let ControlMessage::RequestHead {
            request_id: retry_request_id,
            ..
        } = serde_json::from_str::<ControlMessage>(retry_head.as_ref())
            .expect("replacement retry head json")
        else {
            panic!("expected replacement retry request head");
        };
        assert_eq!(retry_request_id, first_request_id);
        let _retry_end = replacement
            .next()
            .await
            .expect("replacement retry end")
            .expect("replacement retry end");
        replacement
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseHead {
                    request_id: retry_request_id.clone(),
                    status: 204,
                    headers: vec![],
                })
                .expect("replacement retry response head")
                .into(),
            ))
            .await
            .expect("send replacement retry response head");
        replacement
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseEnd {
                    request_id: retry_request_id,
                })
                .expect("replacement retry response end")
                .into(),
            ))
            .await
            .expect("send replacement retry response end");

        assert_eq!(
            public_request.await.expect("retry request task").status(),
            StatusCode::NO_CONTENT
        );

        server.abort();
        let _ = server.await;
    }

    #[test]
    fn automatic_retry_is_limited_to_read_only_operations() {
        assert_eq!(
            automatic_retry_class("GET", TunnelService::Mcp, "/mcp", b""),
            Some("safe_http_method")
        );

        let read = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": "read_file", "arguments": {"path": "README.md"}}
        }))
        .unwrap();
        let write = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {"name": "apply_patch", "arguments": {"patch": "test"}}
        }))
        .unwrap();

        assert_eq!(
            automatic_retry_class("POST", TunnelService::Mcp, "/mcp", &read),
            Some("mcp_read_only")
        );
        assert_eq!(
            automatic_retry_class("POST", TunnelService::Mcp, "/mcp", &write),
            None
        );
        assert_eq!(
            automatic_retry_class(
                "POST",
                TunnelService::Actions,
                "/builtin/actions/pc-a/actions/read_file",
                b"{}"
            ),
            Some("actions_read_only")
        );
        assert_eq!(
            automatic_retry_class(
                "POST",
                TunnelService::Actions,
                "/builtin/actions/pc-a/actions/apply_patch",
                b"{}"
            ),
            None
        );
        assert_eq!(
            automatic_retry_class("PUT", TunnelService::Mcp, "/mcp", &read),
            None
        );
    }

    #[test]
    fn mcp_control_lane_classification_keeps_reattach_and_session_control_off_execution_lane() {
        let call = |name: &str, arguments: serde_json::Value| {
            serde_json::to_vec(&serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            }))
            .expect("request json")
        };
        for name in [
            "exec_health_check",
            "wait_command",
            "resolve_operation",
            "list_sessions",
            "send_input",
            "kill_session",
            "read_output",
            "request_permissions",
        ] {
            assert_eq!(
                classify_request_lane(TunnelService::Mcp, &call(name, serde_json::json!({}))),
                RequestLane::Control
            );
        }
        assert_eq!(
            classify_request_lane(
                TunnelService::Mcp,
                &call(
                    "exec_many",
                    serde_json::json!({ "operation_id": "graph-1" })
                )
            ),
            RequestLane::Control
        );
        assert_eq!(
            classify_request_lane(
                TunnelService::Mcp,
                &call(
                    "exec_many",
                    serde_json::json!({ "operation_id": "graph-1", "action": "cancel" })
                )
            ),
            RequestLane::Control
        );
        assert_eq!(
            classify_request_lane(
                TunnelService::Mcp,
                &call(
                    "exec_many",
                    serde_json::json!({ "operation_id": "graph-1", "commands": [{ "id": "a", "program": "echo" }] })
                )
            ),
            RequestLane::Execution
        );
        assert_eq!(
            classify_request_lane(
                TunnelService::Mcp,
                &call("exec_command", serde_json::json!({}))
            ),
            RequestLane::Execution
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cold_burst_waits_for_bounded_queue_capacity_while_workers_scale_up() {
        const CONCURRENCY: usize = 128;

        let directory = tempfile::tempdir().expect("tempdir");
        let database_path = directory.path().join("devices.db");
        let devices = DeviceRegistry::open(&database_path).expect("device registry");
        let policies =
            WorkerPolicyStore::from_writer(devices.database_writer()).expect("policy store");
        let mut policy = policies.current(TunnelService::Mcp);
        policy.start_workers = 1;
        policy.min_idle_workers = 1;
        policy.max_idle_workers = 1;
        policy.max_workers = 16;
        policy.max_pending_requests = 32;
        policy.worker_acquire_timeout_ms = 1_000;
        policies
            .update(TunnelService::Mcp, policy)
            .expect("save cold burst policy");

        let registry = Registry::default();
        let pool = registry
            .register(ClientKey {
                client_id: "pc-a".into(),
                service: TunnelService::Mcp,
            })
            .await;
        // The route is connected, but its logical stream offers have not reached the
        // dispatcher yet. This is the cold-start scale-up catch-up window.
        pool.active_workers.store(1, Ordering::Release);
        let (address, server) = start_test_server_with_policies(devices, registry, policies).await;

        let scaling_pool = pool.clone();
        let scaler = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            for _ in 0..CONCURRENCY {
                let (assign, assigned) = oneshot::channel::<ProxyJob>();
                scaling_pool
                    .available_tx
                    .send(AvailableWorker {
                        assign,
                        shared_claim: None,
                    })
                    .await
                    .expect("publish scaled worker slot");
                tokio::spawn(async move {
                    if let Ok(job) = assigned.await {
                        let ProxyJob {
                            response_head,
                            response_body,
                            ..
                        } = job;
                        let _ = response_head.send(Ok(ResponseHeadData {
                            status: StatusCode::UNAUTHORIZED.as_u16(),
                            headers: Vec::new(),
                        }));
                        drop(response_body);
                    }
                });
            }
        });

        let client = reqwest::Client::new();
        let mut requests = Vec::with_capacity(CONCURRENCY);
        for index in 0..CONCURRENCY {
            let client = client.clone();
            requests.push(tokio::spawn(async move {
                client
                    .get(format!(
                        "http://{address}/builtin/clients/pc-a/mcp?cold={index}"
                    ))
                    .send()
                    .await
                    .expect("cold burst request")
            }));
        }

        let mut unauthorized = 0;
        let mut capacity_rejections = 0;
        let mut other = Vec::new();
        for request in requests {
            let response = request.await.expect("cold burst task");
            match response.status() {
                StatusCode::UNAUTHORIZED => unauthorized += 1,
                StatusCode::SERVICE_UNAVAILABLE
                    if response
                        .headers()
                        .get("x-tunnel-error")
                        .and_then(|value| value.to_str().ok())
                        == Some("worker_acquire_timeout") =>
                {
                    capacity_rejections += 1;
                }
                status => other.push(status),
            }
        }

        scaler.await.expect("scale-up task");
        server.abort();
        let _ = server.await;

        assert_eq!(capacity_rejections, 0, "cold scale-up must backpressure instead of rejecting before the worker-acquire deadline");
        assert!(
            other.is_empty(),
            "unexpected cold burst statuses: {other:?}"
        );
        assert_eq!(unauthorized, CONCURRENCY);
    }

    #[tokio::test]
    async fn saturated_route_uses_bounded_queue_and_returns_explicit_503_errors() {
        let directory = tempfile::tempdir().expect("tempdir");
        let database_path = directory.path().join("devices.db");
        let devices = DeviceRegistry::open(&database_path).expect("device registry");
        let policies =
            WorkerPolicyStore::from_writer(devices.database_writer()).expect("policy store");
        let mut policy = policies.current(TunnelService::Mcp);
        policy.start_workers = 1;
        policy.min_idle_workers = 1;
        policy.max_idle_workers = 1;
        policy.max_workers = 4;
        policy.max_pending_requests = 1;
        policy.worker_acquire_timeout_ms = 150;
        policies
            .update(TunnelService::Mcp, policy)
            .expect("save capacity policy");
        let (address, server) =
            start_test_server_with_policies(devices.clone(), Registry::default(), policies).await;
        let (signing_key, device_id) = enroll_test_device(address, &devices).await;
        let mut worker = authenticate_worker(address, &signing_key, &device_id).await;
        finish_worker_auth(&mut worker).await;

        let first_request = tokio::spawn(async move {
            reqwest::get(format!("http://{address}/builtin/clients/pc-a/mcp?busy=1"))
                .await
                .expect("busy public request")
        });
        let first_head = worker.next().await.expect("busy head").expect("busy head");
        let WsMessage::Text(first_head) = first_head else {
            panic!("expected busy request head");
        };
        let ControlMessage::RequestHead {
            demand: Some(demand),
            ..
        } = serde_json::from_str::<ControlMessage>(first_head.as_ref())
            .expect("busy request head json")
        else {
            panic!("expected demand hint on request head");
        };
        assert_eq!(demand.queued_requests, 1);
        assert_eq!(demand.desired_workers, 2);
        let _first_end = worker.next().await.expect("busy end").expect("busy end");

        let waiting_request = tokio::spawn(async move {
            reqwest::get(format!(
                "http://{address}/builtin/clients/pc-a/mcp?waiting=1"
            ))
            .await
            .expect("waiting public request")
        });
        tokio::time::sleep(Duration::from_millis(25)).await;

        let backpressured = reqwest::get(format!(
            "http://{address}/builtin/clients/pc-a/mcp?backpressured=1"
        ))
        .await
        .expect("backpressured request");
        assert_eq!(backpressured.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            backpressured
                .headers()
                .get("x-tunnel-error")
                .and_then(|value| value.to_str().ok()),
            Some("worker_acquire_timeout")
        );
        assert_eq!(
            backpressured
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );

        let waiting = tokio::time::timeout(Duration::from_secs(1), waiting_request)
            .await
            .expect("worker acquire timeout deadline")
            .expect("waiting task");
        assert_eq!(waiting.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            waiting
                .headers()
                .get("x-tunnel-error")
                .and_then(|value| value.to_str().ok()),
            Some("worker_acquire_timeout")
        );
        assert_eq!(
            waiting
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok()),
            Some("1")
        );

        first_request.abort();
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn response_head_timeout_cancels_and_releases_the_worker() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let (address, server) = start_test_server_with_timeouts(
            devices.clone(),
            Registry::default(),
            Duration::from_millis(150),
            Duration::from_millis(100),
            Duration::from_secs(2),
        )
        .await;
        let (signing_key, device_id) = enroll_test_device(address, &devices).await;
        let mut worker = authenticate_worker(address, &signing_key, &device_id).await;
        finish_worker_auth(&mut worker).await;

        let first_request = tokio::spawn(async move {
            reqwest::get(format!("http://{address}/builtin/clients/pc-a/mcp?first=1"))
                .await
                .expect("first public request")
        });
        let first_head = worker
            .next()
            .await
            .expect("first head")
            .expect("first head");
        let WsMessage::Text(first_head) = first_head else {
            panic!("expected first request head");
        };
        let ControlMessage::RequestHead { request_id, .. } =
            serde_json::from_str::<ControlMessage>(first_head.as_ref()).expect("first head json")
        else {
            panic!("expected first request head control");
        };
        let _first_end = worker.next().await.expect("first end").expect("first end");

        let cancelled = tokio::time::timeout(Duration::from_secs(1), worker.next())
            .await
            .expect("cancel deadline")
            .expect("cancel frame")
            .expect("cancel frame result");
        let WsMessage::Text(cancelled) = cancelled else {
            panic!("expected cancel text");
        };
        assert_eq!(
            serde_json::from_str::<ControlMessage>(cancelled.as_ref()).expect("cancel json"),
            ControlMessage::Cancel {
                request_id: request_id.clone()
            }
        );
        assert_eq!(
            first_request.await.expect("first task").status(),
            StatusCode::GATEWAY_TIMEOUT
        );

        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::Ready)
                    .expect("ready json")
                    .into(),
            ))
            .await
            .expect("ready after cancel");
        let second_request = tokio::spawn(async move {
            reqwest::get(format!(
                "http://{address}/builtin/clients/pc-a/mcp?second=1"
            ))
            .await
            .expect("second public request")
        });
        let second_head = worker
            .next()
            .await
            .expect("second head")
            .expect("second head");
        let WsMessage::Text(second_head) = second_head else {
            panic!("expected second request head");
        };
        let ControlMessage::RequestHead {
            request_id: second_id,
            ..
        } = serde_json::from_str::<ControlMessage>(second_head.as_ref()).expect("second head json")
        else {
            panic!("expected second request head control");
        };
        let _second_end = worker
            .next()
            .await
            .expect("second end")
            .expect("second end");
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseHead {
                    request_id: second_id.clone(),
                    status: 204,
                    headers: vec![],
                })
                .expect("second response head")
                .into(),
            ))
            .await
            .expect("send second response head");
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseEnd {
                    request_id: second_id,
                })
                .expect("second response end")
                .into(),
            ))
            .await
            .expect("send second response end");
        assert_eq!(
            second_request.await.expect("second task").status(),
            StatusCode::NO_CONTENT
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn stale_worker_expires_and_same_device_can_reconnect() {
        let directory = tempfile::tempdir().expect("tempdir");
        let devices =
            DeviceRegistry::open(directory.path().join("devices.db")).expect("device registry");
        let (address, server) = start_test_server_with_timeouts(
            devices.clone(),
            Registry::default(),
            Duration::from_secs(1),
            Duration::from_millis(100),
            Duration::from_millis(100),
        )
        .await;
        let (signing_key, device_id) = enroll_test_device(address, &devices).await;
        let mut stale_worker = authenticate_worker(address, &signing_key, &device_id).await;
        finish_worker_auth(&mut stale_worker).await;

        tokio::time::sleep(Duration::from_millis(175)).await;
        let unavailable =
            reqwest::get(format!("http://{address}/builtin/clients/pc-a/mcp?stale=1"))
                .await
                .expect("stale route request");
        assert_eq!(unavailable.status(), StatusCode::SERVICE_UNAVAILABLE);

        let mut replacement = authenticate_worker(address, &signing_key, &device_id).await;
        finish_worker_auth(&mut replacement).await;
        let public_request = tokio::spawn(async move {
            reqwest::get(format!(
                "http://{address}/builtin/clients/pc-a/mcp?reconnected=1"
            ))
            .await
            .expect("reconnected request")
        });
        let request_head = replacement
            .next()
            .await
            .expect("replacement head")
            .expect("replacement head");
        let WsMessage::Text(request_head) = request_head else {
            panic!("expected replacement request head");
        };
        let ControlMessage::RequestHead { request_id, .. } =
            serde_json::from_str::<ControlMessage>(request_head.as_ref())
                .expect("replacement head json")
        else {
            panic!("expected replacement request head control");
        };
        let _request_end = replacement
            .next()
            .await
            .expect("replacement end")
            .expect("replacement end");
        replacement
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseHead {
                    request_id: request_id.clone(),
                    status: 204,
                    headers: vec![],
                })
                .expect("replacement response head")
                .into(),
            ))
            .await
            .expect("send replacement response head");
        replacement
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::ResponseEnd { request_id })
                    .expect("replacement response end")
                    .into(),
            ))
            .await
            .expect("send replacement response end");
        assert_eq!(
            public_request.await.expect("reconnected task").status(),
            StatusCode::NO_CONTENT
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn handshake_uses_saved_policy_and_pushes_idle_revisions() {
        let directory = tempfile::tempdir().expect("tempdir");
        let database_path = directory.path().join("devices.db");
        let devices = DeviceRegistry::open(&database_path).expect("device registry");
        let policies =
            WorkerPolicyStore::from_writer(devices.database_writer()).expect("policy store");
        let mut policy = policies.current(TunnelService::Mcp);
        policy.start_workers = 6;
        policy.max_idle_workers = 8;
        policy.max_workers = 24;
        policy.burst_warm_workers = 8;
        let policy = policies
            .update(TunnelService::Mcp, policy)
            .expect("save initial policy");
        let (address, server) =
            start_test_server_with_policies(devices.clone(), Registry::default(), policies.clone())
                .await;
        let (signing_key, device_id) = enroll_test_device(address, &devices).await;
        let mut worker = authenticate_worker(address, &signing_key, &device_id).await;

        let hello_ack = worker.next().await.expect("hello ack").expect("hello ack");
        let WsMessage::Text(hello_ack) = hello_ack else {
            panic!("expected hello ack text");
        };
        assert!(matches!(
            serde_json::from_str::<ControlMessage>(hello_ack.as_ref()).expect("hello ack json"),
            ControlMessage::HelloAck { protocol_version: PROTOCOL_VERSION, worker_policy, .. }
                if worker_policy == policy
        ));
        worker
            .send(WsMessage::Text(
                serde_json::to_string(&ControlMessage::Ready)
                    .expect("ready")
                    .into(),
            ))
            .await
            .expect("send ready");

        let mut changed = policy;
        changed.max_workers = 32;
        let changed = policies
            .update(TunnelService::Mcp, changed)
            .expect("save changed policy");
        let update = timeout(Duration::from_secs(1), worker.next())
            .await
            .expect("policy update timeout")
            .expect("policy update frame")
            .expect("policy update");
        let WsMessage::Text(update) = update else {
            panic!("expected policy update text");
        };
        assert_eq!(
            serde_json::from_str::<ControlMessage>(update.as_ref()).expect("policy update json"),
            ControlMessage::PolicyUpdate {
                worker_policy: changed,
            }
        );

        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn route_policy_refuses_workers_above_the_server_maximum() {
        let directory = tempfile::tempdir().expect("tempdir");
        let database_path = directory.path().join("devices.db");
        let devices = DeviceRegistry::open(&database_path).expect("device registry");
        let policies =
            WorkerPolicyStore::from_writer(devices.database_writer()).expect("policy store");
        let mut policy = policies.current(TunnelService::Mcp);
        policy.start_workers = 1;
        policy.min_idle_workers = 1;
        policy.max_idle_workers = 1;
        policy.max_workers = 1;
        policy.max_connecting_workers = 1;
        policy.scale_down_step = 1;
        policy.burst_warm_workers = 1;
        policies
            .update(TunnelService::Mcp, policy)
            .expect("save cap policy");
        let (address, server) =
            start_test_server_with_policies(devices.clone(), Registry::default(), policies).await;
        let (signing_key, device_id) = enroll_test_device(address, &devices).await;

        let mut first = authenticate_worker(address, &signing_key, &device_id).await;
        finish_worker_auth(&mut first).await;
        let mut second = authenticate_worker(address, &signing_key, &device_id).await;
        let rejection = second
            .next()
            .await
            .expect("worker cap frame")
            .expect("worker cap response");
        let WsMessage::Text(rejection) = rejection else {
            panic!("expected worker cap text");
        };
        assert_eq!(
            serde_json::from_str::<ControlMessage>(rejection.as_ref()).expect("worker cap json"),
            ControlMessage::Error {
                request_id: None,
                message: "worker limit reached for this route".into(),
            }
        );

        server.abort();
        let _ = server.await;
    }
}
