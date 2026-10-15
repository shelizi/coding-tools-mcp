use std::time::Duration;

use coding_tools_tunnel_protocol::{MultiplexConnectionPolicy, WorkerPolicy};

pub(super) const MAX_RECONNECT_DELAY: Duration = Duration::from_secs(120);
pub(super) const INITIAL_RECONNECT_DELAY: Duration = Duration::from_secs(1);
pub(super) const ESTABLISHED_RECONNECT_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PoolCounts {
    pub(super) total: usize,
    pub(super) connecting: usize,
    pub(super) idle: usize,
    pub(super) busy: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PoolAdjustment {
    pub(super) spawn: usize,
    pub(super) retire: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MultiplexPoolSnapshot {
    pub(super) connected_connections: usize,
    pub(super) connecting_connections: usize,
    pub(super) active_streams: usize,
    pub(super) queued_requests: usize,
    pub(super) desired_connections: usize,
    pub(super) idle_connections: usize,
    pub(super) idle_excess_elapsed: bool,
}

pub(super) fn multiplex_pool_adjustment(
    policy: &MultiplexConnectionPolicy,
    snapshot: MultiplexPoolSnapshot,
) -> PoolAdjustment {
    let connected = snapshot.connected_connections;
    let connecting = snapshot.connecting_connections;
    let projected = connected.saturating_add(connecting);
    let maximum = usize::from(policy.max_connections);
    let available_to_spawn = maximum.saturating_sub(projected);

    let minimum = usize::from(policy.min_connections);
    if projected < minimum {
        return PoolAdjustment {
            spawn: minimum.saturating_sub(projected).min(available_to_spawn),
            retire: 0,
        };
    }
    let start = usize::from(policy.start_connections);
    if projected < start && snapshot.active_streams == 0 && snapshot.queued_requests == 0 {
        return PoolAdjustment {
            spawn: start.saturating_sub(projected).min(available_to_spawn),
            retire: 0,
        };
    }

    let projected_capacity = projected
        .max(1)
        .saturating_mul(usize::from(policy.streams_per_connection));
    let utilization_reached = snapshot.active_streams.saturating_mul(100)
        >= projected_capacity.saturating_mul(usize::from(policy.scale_up_threshold_percent));
    let mut spawn = 0;
    if snapshot.queued_requests > 0 || snapshot.desired_connections > projected {
        let demand_gap = snapshot.desired_connections.saturating_sub(projected);
        spawn = usize::from(policy.burst_scale_up_step).max(demand_gap);
    } else if utilization_reached {
        spawn = usize::from(policy.scale_up_step);
    }
    if spawn > 0 {
        return PoolAdjustment {
            spawn: spawn.min(available_to_spawn),
            retire: 0,
        };
    }

    if !snapshot.idle_excess_elapsed || connected <= minimum {
        return PoolAdjustment {
            spawn: 0,
            retire: 0,
        };
    }
    let target = if snapshot.active_streams == 0 {
        minimum
    } else {
        minimum.max(usize::from(policy.max_idle_connections))
    };
    PoolAdjustment {
        spawn: 0,
        retire: snapshot
            .idle_connections
            .min(connected.saturating_sub(target)),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScaleUpBlock {
    ConnectingLimitReached,
    MaxWorkersReached,
}

impl ScaleUpBlock {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::ConnectingLimitReached => "connecting_limit_reached",
            Self::MaxWorkersReached => "max_workers_reached",
        }
    }
}

pub(super) fn configured_max_connecting(policy: &WorkerPolicy) -> usize {
    let maximum = usize::from(policy.max_workers).max(1);
    if policy.max_connecting_workers == 0 {
        maximum.min(4)
    } else {
        usize::from(policy.max_connecting_workers)
            .min(maximum)
            .max(1)
    }
}

pub(super) fn configured_burst_warm_floor(policy: &WorkerPolicy) -> usize {
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
pub(super) fn pool_adjustment(
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

pub(super) fn scale_up_reason(
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

pub(super) fn scale_down_reason(
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
pub(super) fn scale_up_block(
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

pub(super) fn join_worker_indices(indices: &[usize]) -> String {
    indices
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn jittered_limit(base: u64, seed: u64, percent: u8) -> u64 {
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

pub(super) fn worker_should_recycle(
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

pub(super) fn next_reconnect_base(current: Duration, connected: bool) -> Duration {
    if connected {
        ESTABLISHED_RECONNECT_DELAY
    } else {
        (current * 2).min(MAX_RECONNECT_DELAY)
    }
}

pub(super) fn reconnect_delay(base: Duration, worker_index: usize, attempt: u64) -> Duration {
    let mixed = (worker_index as u64 + 1)
        .wrapping_mul(0x9E37_79B9)
        .rotate_left((attempt % 31) as u32)
        ^ attempt.wrapping_mul(0x85EB_CA6B);
    let percent = 50 + mixed % 51;
    let millis = base.as_millis().saturating_mul(u128::from(percent)) / 100;
    Duration::from_millis(millis.max(1).min(MAX_RECONNECT_DELAY.as_millis()) as u64)
}
