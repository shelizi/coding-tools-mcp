use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use coding_tools_tunnel_protocol::{MultiplexConnectionPolicy, WorkerPolicy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinTunnelSnapshot {
    pub configured_workers: usize,
    pub connected_workers: usize,
    pub idle_workers: usize,
    pub busy_workers: usize,
    pub recycled_workers: u64,
    pub policy_revision: u64,
    pub last_error: Option<String>,
    pub transport_mode: Option<String>,
    pub server_id: Option<String>,
    pub device_id: Option<String>,
    pub control_connected: bool,
    pub session_id: Option<String>,
    pub route_id: Option<u32>,
    pub stream_limit: Option<usize>,
    pub streams_per_connection: Option<usize>,
    pub active_streams: usize,
    pub queued_requests: usize,
    pub desired_data_connections: usize,
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

pub(super) struct BuiltinTunnelMetrics {
    configured_workers: AtomicUsize,
    connected_workers: AtomicUsize,
    idle_workers: AtomicUsize,
    busy_workers: AtomicUsize,
    recycled_workers: AtomicU64,
    policy_revision: AtomicU64,
    last_error: Mutex<Option<String>>,
    transport_mode: Mutex<Option<String>>,
    server_id: Mutex<Option<String>>,
    device_id: Mutex<Option<String>>,
    control_connected: AtomicBool,
    session_id: Mutex<Option<String>>,
    route_id: AtomicU64,
    stream_limit: AtomicUsize,
    streams_per_connection: AtomicUsize,
    active_streams: AtomicUsize,
    queued_requests: AtomicUsize,
    desired_data_connections: AtomicUsize,
}

impl BuiltinTunnelMetrics {
    pub(super) fn new(configured_workers: usize) -> Self {
        Self {
            configured_workers: AtomicUsize::new(configured_workers),
            connected_workers: AtomicUsize::new(0),
            idle_workers: AtomicUsize::new(0),
            busy_workers: AtomicUsize::new(0),
            recycled_workers: AtomicU64::new(0),
            policy_revision: AtomicU64::new(0),
            last_error: Mutex::new(None),
            transport_mode: Mutex::new(None),
            server_id: Mutex::new(None),
            device_id: Mutex::new(None),
            control_connected: AtomicBool::new(false),
            session_id: Mutex::new(None),
            route_id: AtomicU64::new(0),
            stream_limit: AtomicUsize::new(0),
            streams_per_connection: AtomicUsize::new(0),
            active_streams: AtomicUsize::new(0),
            queued_requests: AtomicUsize::new(0),
            desired_data_connections: AtomicUsize::new(0),
        }
    }

    pub(super) fn set_policy(&self, policy: &WorkerPolicy) {
        self.configured_workers
            .store(usize::from(policy.max_workers), Ordering::Release);
        self.policy_revision
            .store(policy.revision, Ordering::Release);
    }

    pub(super) fn set_multiplex_policy(&self, policy: &MultiplexConnectionPolicy) {
        self.configured_workers
            .store(usize::from(policy.max_connections), Ordering::Release);
        self.policy_revision
            .store(policy.revision, Ordering::Release);
    }

    pub(super) fn set_peer_identity(&self, server_id: &str, device_id: &str) {
        *self
            .server_id
            .lock()
            .unwrap_or_else(|guard| guard.into_inner()) = Some(server_id.to_string());
        *self
            .device_id
            .lock()
            .unwrap_or_else(|guard| guard.into_inner()) = Some(device_id.to_string());
    }

    pub(super) fn set_legacy_mode(&self) {
        *self
            .transport_mode
            .lock()
            .unwrap_or_else(|guard| guard.into_inner()) = Some("legacy-v4".into());
        self.control_connected.store(false, Ordering::Release);
        *self
            .session_id
            .lock()
            .unwrap_or_else(|guard| guard.into_inner()) = None;
        self.route_id.store(0, Ordering::Release);
        self.stream_limit.store(0, Ordering::Release);
        self.streams_per_connection.store(0, Ordering::Release);
        self.active_streams.store(0, Ordering::Release);
        self.queued_requests.store(0, Ordering::Release);
        self.desired_data_connections.store(0, Ordering::Release);
    }

    pub(super) fn set_split_session(
        &self,
        session_id: &str,
        route_id: u32,
        stream_limit: u16,
        policy: &MultiplexConnectionPolicy,
    ) {
        *self
            .transport_mode
            .lock()
            .unwrap_or_else(|guard| guard.into_inner()) = Some("split-v5".into());
        self.control_connected.store(true, Ordering::Release);
        *self
            .session_id
            .lock()
            .unwrap_or_else(|guard| guard.into_inner()) = Some(session_id.to_string());
        self.route_id.store(u64::from(route_id), Ordering::Release);
        self.stream_limit
            .store(usize::from(stream_limit), Ordering::Release);
        self.streams_per_connection.store(
            usize::from(policy.streams_per_connection),
            Ordering::Release,
        );
    }

    pub(super) fn set_split_demand(
        &self,
        active_streams: usize,
        queued_requests: usize,
        desired_connections: usize,
    ) {
        self.active_streams.store(active_streams, Ordering::Release);
        self.queued_requests
            .store(queued_requests, Ordering::Release);
        self.desired_data_connections
            .store(desired_connections, Ordering::Release);
    }

    pub(super) fn set_pool_counts(&self, idle: usize, busy: usize) {
        self.idle_workers.store(idle, Ordering::Release);
        self.busy_workers.store(busy, Ordering::Release);
    }

    pub(super) fn record_recycle(&self) {
        self.recycled_workers.fetch_add(1, Ordering::AcqRel);
    }

    pub(super) fn set_last_error(&self, error: Option<String>) {
        *self
            .last_error
            .lock()
            .unwrap_or_else(|guard| guard.into_inner()) = error;
    }

    pub(super) fn snapshot(&self) -> BuiltinTunnelSnapshot {
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
            transport_mode: self
                .transport_mode
                .lock()
                .unwrap_or_else(|guard| guard.into_inner())
                .clone(),
            server_id: self
                .server_id
                .lock()
                .unwrap_or_else(|guard| guard.into_inner())
                .clone(),
            device_id: self
                .device_id
                .lock()
                .unwrap_or_else(|guard| guard.into_inner())
                .clone(),
            control_connected: self.control_connected.load(Ordering::Acquire),
            session_id: self
                .session_id
                .lock()
                .unwrap_or_else(|guard| guard.into_inner())
                .clone(),
            route_id: u32::try_from(self.route_id.load(Ordering::Acquire))
                .ok()
                .filter(|value| *value != 0),
            stream_limit: Some(self.stream_limit.load(Ordering::Acquire))
                .filter(|value| *value != 0),
            streams_per_connection: Some(self.streams_per_connection.load(Ordering::Acquire))
                .filter(|value| *value != 0),
            active_streams: self.active_streams.load(Ordering::Acquire),
            queued_requests: self.queued_requests.load(Ordering::Acquire),
            desired_data_connections: self.desired_data_connections.load(Ordering::Acquire),
        }
    }
}

pub(super) struct ConnectedWorkerGuard {
    metrics: Arc<BuiltinTunnelMetrics>,
}

impl ConnectedWorkerGuard {
    pub(super) fn new(metrics: Arc<BuiltinTunnelMetrics>) -> Self {
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
