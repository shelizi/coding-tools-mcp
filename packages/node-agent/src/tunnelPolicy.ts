export interface WorkerPolicy {
  start_workers: number;
  min_idle_workers: number;
  max_idle_workers: number;
  max_workers: number;
  max_requests_per_worker: number;
  max_lifetime_seconds: number;
  scale_down_delay_seconds: number;
  recycle_jitter_percent: number;
  max_pending_requests: number;
  worker_acquire_timeout_ms: number;
  max_connecting_workers: number;
  connecting_capacity_grace_ms: number;
  scale_down_step: number;
  burst_warm_workers: number;
  burst_warm_seconds: number;
  revision: number;
}

export interface PoolCounts {
  total: number;
  connecting: number;
  idle: number;
  busy: number;
}

export interface PoolAdjustment {
  spawn: number;
  retire: number;
}

const U64_MASK = (1n << 64n) - 1n;

function integer(value: unknown, name: string, minimum: number, maximum: number, fallback?: number): number {
  if (value === undefined && fallback !== undefined) return fallback;
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed) || parsed < minimum || parsed > maximum) {
    throw new Error(`${name} must be an integer between ${minimum} and ${maximum}`);
  }
  return parsed;
}

export function normalizeWorkerPolicy(input: unknown): WorkerPolicy {
  if (!input || typeof input !== 'object' || Array.isArray(input)) throw new Error('worker policy must be an object');
  const value = input as Record<string, unknown>;
  const policy: WorkerPolicy = {
    start_workers: integer(value.start_workers, 'start_workers', 1, 256),
    min_idle_workers: integer(value.min_idle_workers, 'min_idle_workers', 1, 256),
    max_idle_workers: integer(value.max_idle_workers, 'max_idle_workers', 1, 256),
    max_workers: integer(value.max_workers, 'max_workers', 1, 256),
    max_requests_per_worker: integer(value.max_requests_per_worker, 'max_requests_per_worker', 0, 1_000_000),
    max_lifetime_seconds: integer(value.max_lifetime_seconds, 'max_lifetime_seconds', 0, 604_800),
    scale_down_delay_seconds: integer(value.scale_down_delay_seconds, 'scale_down_delay_seconds', 0, 3_600),
    recycle_jitter_percent: integer(value.recycle_jitter_percent, 'recycle_jitter_percent', 0, 50),
    max_pending_requests: integer(value.max_pending_requests, 'max_pending_requests', 1, 4_096, 32),
    worker_acquire_timeout_ms: integer(value.worker_acquire_timeout_ms, 'worker_acquire_timeout_ms', 100, 60_000, 10_000),
    max_connecting_workers: integer(value.max_connecting_workers, 'max_connecting_workers', 0, 256, 0),
    connecting_capacity_grace_ms: integer(value.connecting_capacity_grace_ms, 'connecting_capacity_grace_ms', 0, 30_000, 1_000),
    scale_down_step: integer(value.scale_down_step, 'scale_down_step', 1, 256, 4),
    burst_warm_workers: integer(value.burst_warm_workers, 'burst_warm_workers', 0, 256, 0),
    burst_warm_seconds: integer(value.burst_warm_seconds, 'burst_warm_seconds', 0, 3_600, 120),
    revision: integer(value.revision, 'revision', 1, Number.MAX_SAFE_INTEGER)
  };
  if (!(policy.min_idle_workers <= policy.start_workers
    && policy.start_workers <= policy.max_idle_workers
    && policy.max_idle_workers <= policy.max_workers)) {
    throw new Error('worker counts must satisfy 1 <= min idle <= start <= max idle <= max workers <= 256');
  }
  if (policy.max_lifetime_seconds !== 0 && policy.max_lifetime_seconds < 60) {
    throw new Error('max_lifetime_seconds must be 0 or between 60 and 604800');
  }
  if (policy.max_connecting_workers > policy.max_workers) {
    throw new Error('max_connecting_workers must be 0 or no greater than max_workers');
  }
  if (policy.burst_warm_workers !== 0
    && (policy.burst_warm_workers < policy.max_idle_workers || policy.burst_warm_workers > policy.max_workers)) {
    throw new Error('burst_warm_workers must be 0 or between max_idle_workers and max_workers');
  }
  return policy;
}

export function configuredMaxConnecting(policy: WorkerPolicy): number {
  return policy.max_connecting_workers === 0
    ? Math.min(policy.max_workers, 4)
    : Math.max(1, Math.min(policy.max_connecting_workers, policy.max_workers));
}

export function configuredBurstWarmFloor(policy: WorkerPolicy): number {
  return policy.burst_warm_workers === 0
    ? Math.min(policy.max_workers, Math.max(policy.start_workers, policy.max_idle_workers * 2))
    : Math.min(policy.burst_warm_workers, policy.max_workers);
}

export function poolAdjustment(
  policy: WorkerPolicy,
  counts: PoolCounts,
  effectiveConnecting: number,
  maxConnecting: number,
  desiredWorkers: number,
  idleExcessElapsed: boolean,
  scaleDownFloor: number
): PoolAdjustment {
  if (counts.total !== counts.connecting + counts.idle + counts.busy) throw new Error('invalid pool counts');
  const startupNeeded = Math.max(0, policy.start_workers - counts.total);
  const spareNeeded = Math.max(0, policy.min_idle_workers - (counts.idle + effectiveConnecting));
  const demandNeeded = Math.max(0, desiredWorkers - counts.total);
  const requestedSpawn = Math.max(startupNeeded, spareNeeded, demandNeeded);
  const connectingBudget = Math.max(0, maxConnecting - effectiveConnecting);
  const spawn = Math.min(requestedSpawn, Math.max(0, policy.max_workers - counts.total), connectingBudget);

  const aboveMaximum = Math.min(Math.max(0, counts.total - policy.max_workers), counts.idle);
  const stagedIdleExcess = idleExcessElapsed && aboveMaximum === 0
    ? Math.min(Math.max(0, counts.total - scaleDownFloor), counts.idle, policy.scale_down_step)
    : 0;
  return { spawn, retire: Math.max(aboveMaximum, stagedIdleExcess) };
}

function rotateLeft64(value: bigint, shift: bigint): bigint {
  const bits = Number(shift % 64n);
  if (bits === 0) return value & U64_MASK;
  return ((value << BigInt(bits)) | (value >> BigInt(64 - bits))) & U64_MASK;
}

export function jitteredLimit(base: number, seed: number, percent: number): number {
  if (base === 0 || percent === 0) return base;
  const spread = BigInt(Math.min(percent, 50));
  const seed64 = BigInt(Math.max(0, Math.floor(seed))) & U64_MASK;
  const multiplied = ((seed64 + 1n) * 0x9E37_79B9_7F4A_7C15n) & U64_MASK;
  const mixed = rotateLeft64(multiplied, seed64 % 63n);
  const offset = mixed % (spread * 2n + 1n);
  const factor = 100n - spread + offset;
  return Number((BigInt(base) * factor + 99n) / 100n);
}

export function workerShouldRecycle(
  policy: WorkerPolicy,
  seed: number,
  completedRequests: number,
  connectedForMs: number
): boolean {
  const requestLimit = jitteredLimit(policy.max_requests_per_worker, seed, policy.recycle_jitter_percent);
  const lifetimeLimit = jitteredLimit(policy.max_lifetime_seconds, seed, policy.recycle_jitter_percent);
  return (requestLimit !== 0 && completedRequests >= requestLimit)
    || (lifetimeLimit !== 0 && connectedForMs >= lifetimeLimit * 1000);
}
