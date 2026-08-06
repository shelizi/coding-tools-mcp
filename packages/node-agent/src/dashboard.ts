import path from 'node:path';
import { processStatus } from './processes.js';
import { AGENT_VERSION } from './version.js';
import type { AgentConfig, JsonObject, OperationRecord, ProcessSession, ToolContext, UsageRecord } from './types.js';
import { allFolderRuntimes } from './folderRuntime.js';

const RECENT_USAGE_LIMIT = 100;
const USAGE_AGGREGATE_WINDOW = 1_000;
const SESSION_LIMIT = 50;
const ACTIVITY_LIMIT = 50;

function workspaceLocation(config: AgentConfig, cwd: string): JsonObject {
  const folders = [...config.folders].sort((left, right) => right.path.length - left.path.length);
  for (const folder of folders) {
    const relative = path.relative(folder.path, cwd);
    if (relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative))) {
      return {
        workspaceId: folder.id,
        workspaceName: folder.name,
        cwd: relative ? relative.replaceAll('\\', '/') : '.'
      };
    }
  }
  return { workspaceId: null, workspaceName: null, cwd: 'outside-configured-workspace' };
}

function sessionSummary(config: AgentConfig, session: ProcessSession, now: number): JsonObject {
  const status = processStatus(session);
  const completedAt = session.finalizedAt ?? session.endedAt ?? now;
  return {
    id: session.id,
    ...workspaceLocation(config, session.cwd),
    status,
    pid: session.child?.pid ?? null,
    startedAt: session.startedAt,
    endedAt: session.endedAt ?? null,
    finalizedAt: session.finalizedAt ?? null,
    durationMs: Math.max(0, completedAt - session.startedAt),
    exitCode: session.exitCode ?? null,
    timedOut: session.timedOut,
    killed: session.killed,
    verificationOk: session.verificationOk ?? null,
    stdoutBytes: session.stdoutBytes,
    stderrBytes: session.stderrBytes
  };
}

function percentile(values: number[], fraction: number): number {
  if (!values.length) return 0;
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * fraction) - 1));
  return sorted[index];
}

function usageAggregate(rows: UsageRecord[]): JsonObject[] {
  const groups = new Map<string, UsageRecord[]>();
  for (const row of rows) {
    const group = groups.get(row.tool) ?? [];
    group.push(row);
    groups.set(row.tool, group);
  }
  return [...groups.entries()]
    .map(([tool, records]) => {
      const durations = records.map(record => record.durationMs);
      const queueWait = records.reduce((sum, record) => sum + record.queueWaitMs, 0);
      const lockWait = records.reduce((sum, record) => sum + record.lockWaitMs, 0);
      return {
        tool,
        calls: records.length,
        errors: records.filter(record => !record.ok).length,
        averageDurationMs: Math.round(durations.reduce((sum, value) => sum + value, 0) / records.length),
        p95DurationMs: percentile(durations, 0.95),
        maxDurationMs: Math.max(...durations),
        averageQueueWaitMs: Math.round(queueWait / records.length),
        averageLockWaitMs: Math.round(lockWait / records.length),
        responseBytes: records.reduce((sum, record) => sum + record.responseBytes, 0)
      };
    })
    .sort((left, right) => Number(right.calls) - Number(left.calls) || String(left.tool).localeCompare(String(right.tool)));
}

function recentUsage(rows: UsageRecord[]): JsonObject[] {
  return rows.slice(-RECENT_USAGE_LIMIT).reverse().map(row => ({
    tool: row.tool,
    startedAt: row.startedAt,
    durationMs: row.durationMs,
    ok: row.ok,
    queueWaitMs: row.queueWaitMs,
    lockWaitMs: row.lockWaitMs,
    responseBytes: row.responseBytes
  }));
}

function recentActivity(rows: OperationRecord[]): JsonObject[] {
  return rows.slice(-ACTIVITY_LIMIT).reverse().map(row => ({
    tool: row.tool,
    workspaceId: row.workspace_id,
    startedAt: Number(row.created_at) || 0,
    durationMs: Number(row.result_summary.duration_ms ?? 0) || 0,
    ok: row.result_summary.ok === true
  }));
}

function taskCounts(ctx: ToolContext): JsonObject {
  const counts: Record<string, number> = {};
  const tasks = Object.values(ctx.state.tasks());
  for (const task of tasks) counts[task.status] = (counts[task.status] ?? 0) + 1;
  return { total: tasks.length, byStatus: counts };
}

function tunnelSummary(ctx: ToolContext): JsonObject {
  const tunnel = ctx.tunnelStatus;
  if (!tunnel) return { enabled: false, state: 'disabled' };
  return {
    enabled: tunnel.enabled,
    state: tunnel.state,
    workers: tunnel.workers,
    connectedWorkers: tunnel.connectedWorkers,
    connectingWorkers: tunnel.connectingWorkers ?? 0,
    idleWorkers: tunnel.idleWorkers ?? 0,
    busyWorkers: tunnel.busyWorkers ?? 0,
    recycledWorkers: tunnel.recycledWorkers ?? 0,
    completedRequests: tunnel.completedRequests,
    policyRevision: tunnel.policyRevision ?? null,
    lastRequestTimeout: tunnel.lastRequestTimeout ?? null,
    lastRequestTimeoutAt: tunnel.lastRequestTimeoutAt ?? null
  };
}

export async function dashboardPayload(ctx: ToolContext, startedAt: number): Promise<JsonObject> {
  const now = Date.now();
  const runtimes = allFolderRuntimes(ctx);
  const sessions = runtimes.flatMap(runtime => [...runtime.sessions.values()])
    .sort((left, right) => right.startedAt - left.startedAt);
  const aggregateRows = ctx.usage.slice(-USAGE_AGGREGATE_WINDOW);
  const recentRows = ctx.usage.slice(-RECENT_USAGE_LIMIT);
  const recentErrors = recentRows.filter(row => !row.ok).length;
  const errorRate = recentRows.length ? recentErrors / recentRows.length : 0;
  const tunnel = tunnelSummary(ctx);
  const blocking = {
    limit: runtimes.reduce((sum, runtime) => sum + runtime.admission.blocking.limit, 0),
    active: runtimes.reduce((sum, runtime) => sum + runtime.admission.blocking.active, 0),
    queued: runtimes.reduce((sum, runtime) => sum + runtime.admission.blocking.queued, 0)
  };
  const processLane = {
    limit: runtimes.reduce((sum, runtime) => sum + runtime.admission.process.limit, 0),
    active: runtimes.reduce((sum, runtime) => sum + runtime.admission.process.active, 0),
    queued: runtimes.reduce((sum, runtime) => sum + runtime.admission.process.queued, 0)
  };
  const queueDepth = blocking.queued + processLane.queued;
  const tunnelState = String(tunnel.state ?? 'disabled');
  const degraded = Boolean(tunnel.enabled) && ['error', 'reconnecting'].includes(tunnelState)
    || recentRows.length >= 10 && errorRate >= 0.25;
  const healthState = degraded ? 'degraded' : queueDepth > 0 ? 'busy' : 'healthy';
  const memory = process.memoryUsage();
  const operations = ctx.state.operations(undefined, ACTIVITY_LIMIT);
  const lastActivityAt = Math.max(
    0,
    ...recentRows.map(row => row.startedAt),
    ...operations.map(row => Number(row.created_at) || 0)
  );
  const persistentUsage = await ctx.usageStore.dashboardSummary().catch(error => ({
    enabled: true,
    error: error instanceof Error ? error.message : String(error)
  }));

  return {
    ok: true,
    generatedAt: now,
    health: {
      state: healthState,
      uptimeMs: Math.max(0, now - startedAt),
      lastActivityAt: lastActivityAt || null,
      recentCalls: recentRows.length,
      recentErrors,
      recentErrorRate: Number(errorRate.toFixed(4))
    },
    runtime: {
      version: AGENT_VERSION,
      nodeVersion: process.version,
      platform: process.platform,
      arch: process.arch,
      pid: process.pid,
      memory: {
        rssBytes: memory.rss,
        heapUsedBytes: memory.heapUsed,
        heapTotalBytes: memory.heapTotal,
        externalBytes: memory.external,
        arrayBuffersBytes: memory.arrayBuffers
      }
    },
    admission: {
      blocking: { limit: blocking.limit, active: blocking.active, queued: blocking.queued },
      process: { limit: processLane.limit, active: processLane.active, queued: processLane.queued }
    },
    sessions: {
      total: sessions.length,
      running: sessions.filter(session => processStatus(session) === 'running').length,
      verifying: sessions.filter(session => processStatus(session) === 'verifying').length,
      finalized: sessions.filter(session => Boolean(session.finalizedAt)).length,
      items: sessions.slice(0, SESSION_LIMIT).map(session => sessionSummary(ctx.config, session, now))
    },
    permissions: {
      pending: runtimes.reduce((sum, runtime) => sum + runtime.pendingOperations.size, 0),
      byWorkspace: runtimes.map(runtime => ({
        workspaceFolderId: runtime.folderId,
        pending: runtime.pendingOperations.size
      }))
    },
    tasks: taskCounts(ctx),
    tunnel,
    usage: {
      windowSize: aggregateRows.length,
      aggregate: usageAggregate(aggregateRows),
      recent: recentUsage(ctx.usage),
      persistent: persistentUsage
    },
    activity: recentActivity(operations),
    limits: {
      recentUsage: RECENT_USAGE_LIMIT,
      aggregateWindow: USAGE_AGGREGATE_WINDOW,
      sessions: SESSION_LIMIT,
      activity: ACTIVITY_LIMIT
    }
  };
}
