use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use coding_tools_tunnel_protocol::WorkerPolicy;

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

pub(super) struct BuiltinTunnelMetrics {
    configured_workers: AtomicUsize,
    connected_workers: AtomicUsize,
    idle_workers: AtomicUsize,
    busy_workers: AtomicUsize,
    recycled_workers: AtomicU64,
    policy_revision: AtomicU64,
    last_error: Mutex<Option<String>>,
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
        }
    }

    pub(super) fn set_policy(&self, policy: &WorkerPolicy) {
        self.configured_workers
            .store(usize::from(policy.max_workers), Ordering::Release);
        self.policy_revision
            .store(policy.revision, Ordering::Release);
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
