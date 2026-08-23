import { createHash, randomUUID } from 'node:crypto';
import type { FolderRuntime, JsonObject, ProcessSession, SandboxConfig, ToolContext } from '../types.js';
import { currentFolderRuntime } from '../folderRuntime.js';
import { normalizeCommandGraphCommands, validateCommandGraphStructure } from '../policy.js';
import { sleep } from '../runtime.js';
import { normalizedSandboxConfig } from '../sandbox.js';
import { processResult } from './output.js';

const AUTO_DEDUPE_COMPLETED_GRACE_MS = 30_000;

export const COMMAND_GRAPH_RETENTION_MS = 900_000;
export const MAX_RETAINED_COMMAND_GRAPHS = 128;

interface RetainedCommandGraphRecord {
  id: string;
  createdAt: number;
  completedAt?: number;
}

export function pruneRetainedCommandGraphs<T extends RetainedCommandGraphRecord>(
  graphs: Map<string, T>,
  reserve = 0
): number {
  const cutoff = Date.now() - COMMAND_GRAPH_RETENTION_MS;
  for (const [id, graph] of graphs) {
    if (graph.completedAt !== undefined && graph.completedAt < cutoff) graphs.delete(id);
  }
  let evicted = 0;
  const overflow = Math.max(0, graphs.size + reserve - MAX_RETAINED_COMMAND_GRAPHS);
  if (overflow > 0) {
    const completed = [...graphs.values()]
      .filter(graph => graph.completedAt !== undefined)
      .sort((left, right) => (left.completedAt ?? left.createdAt) - (right.completedAt ?? right.createdAt));
    for (const graph of completed.slice(0, overflow)) {
      if (graphs.delete(graph.id)) evicted += 1;
    }
  }
  return evicted;
}

interface RetainedCommandGraph {
  id: string;
  fingerprint: string;
  commands: Array<JsonObject & { id: string }>;
  requestedMode: string;
  mode: string;
  stopOnError: boolean;
  maxParallel: number;
  results: Map<string, JsonObject>;
  pending: Map<string, JsonObject & { id: string }>;
  running: Map<string, Promise<void>>;
  sessionIds: Map<string, string>;
  ownedSessionIds: Set<string>;
  startedIds: Set<string>;
  createdAt: number;
  completedAt?: number;
  cancelRequestedAt?: number;
  cancelReason?: string;
  schedulerError?: JsonObject;
  abortController: AbortController;
  completion: Promise<void>;
}

const retainedCommandGraphs = new WeakMap<FolderRuntime, Map<string, RetainedCommandGraph>>();
const retainedCommandGraphFingerprints = new WeakMap<FolderRuntime, Map<string, string>>();

function boundedInteger(value: unknown, fallback: number, minimum: number, maximum: number): number {
  const parsed = Number(value);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(minimum, Math.min(Math.trunc(parsed), maximum));
}

function stableValue(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stableValue);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([key, nested]) => [key, stableValue(nested)]));
  }
  return value;
}

function sandboxFingerprintMaterial(config: SandboxConfig | undefined): unknown {
  const normalized = normalizedSandboxConfig(config);
  if (!normalized.enabled) return null;
  return stableValue({
    backend: normalized.backend,
    external_paths: normalized.externalPaths,
    options: normalized.options
  });
}

interface StartedGraphProcess {
  session: ProcessSession;
  deduplicated: boolean;
  attachedToSessionId: string | null;
  operationLockWaitMs: number;
}

interface NormalizedProcessError {
  code: string;
  message: string;
  category: string;
  retryable: boolean;
  details: JsonObject;
}

export interface CommandGraphProcessDependencies {
  startProcess(ctx: ToolContext, key: string, args: JsonObject, signal?: AbortSignal): Promise<StartedGraphProcess>;
  waitForSession(session: ProcessSession, cursor: number, timeoutMs: number, until: string): Promise<unknown>;
  killProcessTree(session: ProcessSession, signal?: 'TERM' | 'KILL' | 'INT', reason?: string): Promise<void>;
  normalizeError(error: unknown): NormalizedProcessError;
  error(code: string, message: string, category?: string, retryable?: boolean, details?: JsonObject): Error;
}

export function abortRetainedCommandGraphs(runtimes: FolderRuntime[], reason = 'server restart'): Array<Promise<void>> {
  const graphs = runtimes.flatMap(runtime => [...(retainedCommandGraphs.get(runtime)?.values() ?? [])]);
  for (const graph of graphs) {
    if (!graph.completedAt && !graph.abortController.signal.aborted) graph.abortController.abort(new Error(reason));
  }
  return graphs.map(graph => graph.completion);
}

function retainedCommandGraphMap(runtime: FolderRuntime): Map<string, RetainedCommandGraph> {
  let fingerprints = retainedCommandGraphFingerprints.get(runtime);
  if (!fingerprints) {
    fingerprints = new Map();
    retainedCommandGraphFingerprints.set(runtime, fingerprints);
  }
  let graphs = retainedCommandGraphs.get(runtime);
  if (!graphs) {
    graphs = new Map();
    retainedCommandGraphs.set(runtime, graphs);
  }
  pruneRetainedCommandGraphs(graphs);
  for (const [fingerprint, graphId] of fingerprints) {
    if (!graphs.has(graphId)) fingerprints.delete(fingerprint);
  }
  return graphs;
}

function commandGraphFingerprint(
  commands: Array<JsonObject & { id: string }>,
  mode: string,
  maxParallel: number,
  stopOnError: boolean,
  sandboxConfig?: SandboxConfig
): string {
  const normalizedCommands = commands.map(command => {
    const normalized = { ...command };
    delete normalized.yield_time_ms;
    delete normalized.output_mode;
    delete normalized.max_output_bytes;
    delete normalized.reason;
    return normalized;
  });
  return createHash('sha256').update(JSON.stringify(stableValue({
    commands: normalizedCommands,
    mode,
    max_parallel: maxParallel,
    stop_on_error: stopOnError,
    sandbox: sandboxFingerprintMaterial(sandboxConfig)
  }))).digest('hex');
}

function commandGraphConfiguration(ctx: ToolContext, commands: Array<JsonObject & { id: string }>, args: JsonObject) {
  const requestedMode = String(args.mode ?? 'auto');
  const hasDependencies = commands.some(command => Array.isArray(command.depends_on) && command.depends_on.length > 0);
  const mode = requestedMode === 'auto' ? (hasDependencies ? 'dag' : 'sequential') : requestedMode;
  const stopOnError = args.stop_on_error !== false;
  const defaultParallel = mode === 'sequential' ? 1 : Math.min(8, ctx.config.limits.processConcurrency);
  const maxParallel = Math.max(1, Math.min(Number(args.max_parallel ?? defaultParallel), 256));
  return { requestedMode, mode, stopOnError, maxParallel };
}

function skipPendingGraphCommands(graph: RetainedCommandGraph, reason: string): void {
  for (const [id] of graph.pending) {
    graph.results.set(id, { id, ok: false, skipped: true, skip_reason: reason });
  }
  graph.pending.clear();
}

function commandGraphExplicitlyDeduplicable(commands: Array<JsonObject & { id: string }>): boolean {
  return commands.every(command => Boolean(String(command.operation_id ?? '').trim()) || command.deduplicate === true);
}

async function waitForGraphSession(dependencies: CommandGraphProcessDependencies, graph: RetainedCommandGraph, session: ProcessSession): Promise<void> {
  while (!session.finalizedAt) {
    if (graph.abortController.signal.aborted) {
      if (!graph.ownedSessionIds.has(session.id)) return;
      await dependencies.waitForSession(session, session.sequence, 5_000, 'finalized');
      continue;
    }
    let removeAbortListener = () => {};
    const aborted = new Promise<void>(resolve => {
      const onAbort = () => resolve();
      graph.abortController.signal.addEventListener('abort', onAbort, { once: true });
      removeAbortListener = () => graph.abortController.signal.removeEventListener('abort', onAbort);
    });
    try {
      await Promise.race([
        dependencies.waitForSession(session, session.sequence, 30_000, 'finalized').then(() => undefined),
        aborted
      ]);
    } finally {
      removeAbortListener();
    }
  }
}

async function scheduleCommandGraph(dependencies: CommandGraphProcessDependencies, ctx: ToolContext, key: string, graph: RetainedCommandGraph): Promise<void> {
  const launch = (id: string, command: JsonObject & { id: string }) => {
    graph.pending.delete(id);
    graph.startedIds.add(id);
    const task = (async () => {
      try {
        const started = await dependencies.startProcess(ctx, key, command, graph.abortController.signal);
        const session = started.session;
        graph.sessionIds.set(id, session.id);
        if (!started.deduplicated) graph.ownedSessionIds.add(session.id);
        if (graph.abortController.signal.aborted && graph.ownedSessionIds.has(session.id) && !session.finalizedAt) {
          await dependencies.killProcessTree(session, 'KILL', 'graph_cancelled');
        }
        await waitForGraphSession(dependencies, graph, session);
        if (graph.abortController.signal.aborted && !graph.ownedSessionIds.has(session.id) && !session.finalizedAt) {
          graph.results.set(id, {
            id,
            ok: false,
            command_ok: false,
            skipped: true,
            skip_reason: 'graph_cancelled_shared_session_preserved',
            session_id: session.id,
            shared_session_preserved: true,
            deduplicated: started.deduplicated,
            attached_to_session_id: started.attachedToSessionId
          });
          return;
        }
        graph.results.set(id, {
          id,
          ...processResult(session, {
            ...command,
            deduplicated: started.deduplicated,
            attached_to_session_id: started.attachedToSessionId,
            operation_lock_wait_ms: started.operationLockWaitMs
          })
        });
      } catch (error) {
        const structured = dependencies.normalizeError(error);
        graph.results.set(id, {
          id,
          ok: false,
          command_ok: false,
          error: {
            code: structured.code,
            message: structured.message,
            category: structured.category,
            retryable: structured.retryable,
            details: structured.details
          }
        });
      }
    })().finally(() => graph.running.delete(id));
    graph.running.set(id, task);
  };

  try {
    while (graph.pending.size || graph.running.size) {
      if (graph.abortController.signal.aborted) {
        skipPendingGraphCommands(graph, 'graph_cancelled');
        if (graph.running.size) await Promise.allSettled([...graph.running.values()]);
        break;
      }

      let launched = false;
      for (const [id, command] of graph.pending) {
        if (graph.running.size >= graph.maxParallel) break;
        const dependencies = Array.isArray(command.depends_on) ? command.depends_on.map(String) : [];
        const unresolved = dependencies.some(dependency => !graph.results.has(dependency));
        if (unresolved) continue;
        const failedDependency = dependencies.some(dependency => {
          const dependencyResult = graph.results.get(dependency);
          return dependencyResult?.command_ok === false || dependencyResult?.ok === false;
        });
        if (failedDependency) {
          graph.results.set(id, { id, ok: false, skipped: true, skip_reason: 'dependency_failed' });
          graph.pending.delete(id);
          launched = true;
          continue;
        }
        if (graph.mode === 'sequential' && graph.running.size) break;
        launch(id, command);
        launched = true;
        if (graph.mode === 'sequential') break;
      }

      if (!launched && !graph.running.size && graph.pending.size) {
        skipPendingGraphCommands(graph, 'dependency_cycle_or_missing_dependency');
        break;
      }
      if (graph.running.size) await Promise.race([...graph.running.values()]); else await sleep(1);

      if (graph.stopOnError && [...graph.results.values()].some(result =>
        (result.command_ok === false || result.ok === false) && !result.skipped)) {
        skipPendingGraphCommands(graph, 'stopped_after_failure');
        if (graph.running.size) await Promise.allSettled([...graph.running.values()]);
        break;
      }
    }
  } catch (error) {
    graph.schedulerError = {
      code: 'COMMAND_GRAPH_SCHEDULER_FAILED',
      message: error instanceof Error ? error.message : String(error)
    };
    skipPendingGraphCommands(graph, 'graph_scheduler_failed');
    if (graph.running.size) await Promise.allSettled([...graph.running.values()]);
  } finally {
    graph.completedAt = Date.now();
  }
}

async function waitForCommandGraph(graph: RetainedCommandGraph, requestedYieldMs: unknown): Promise<{ yieldMs: number; waitMs: number }> {
  const yieldMs = boundedInteger(requestedYieldMs, 30_000, 0, 30_000);
  if (graph.completedAt !== undefined || yieldMs <= 0) return { yieldMs, waitMs: 0 };
  const startedAt = Date.now();
  await Promise.race([graph.completion, sleep(yieldMs)]);
  return { yieldMs, waitMs: Date.now() - startedAt };
}

async function cancelCommandGraph(dependencies: CommandGraphProcessDependencies, runtime: FolderRuntime, graph: RetainedCommandGraph, reason: string): Promise<{ accepted: boolean }> {
  if (graph.completedAt !== undefined) return { accepted: false };
  if (graph.cancelRequestedAt === undefined) {
    graph.cancelRequestedAt = Date.now();
    graph.cancelReason = reason || 'cancelled_by_user';
    graph.abortController.abort(new Error(graph.cancelReason));
  }
  skipPendingGraphCommands(graph, 'graph_cancelled');
  const sessions = [...graph.ownedSessionIds]
    .map(sessionId => runtime.sessions.get(sessionId))
    .filter((session): session is ProcessSession => Boolean(session && !session.finalizedAt));
  await Promise.all(sessions.map(session => dependencies.killProcessTree(session, 'KILL', 'graph_cancelled')));
  return { accepted: true };
}

function commandGraphResultMode(dependencies: CommandGraphProcessDependencies, args: JsonObject, graphAction: string): 'full' | 'summary' | 'none' {
  const requested = String(args.result_mode ?? '').trim().toLowerCase();
  if (requested) {
    if (!['full', 'summary', 'none'].includes(requested)) {
      throw dependencies.error('INVALID_ARGUMENT', 'exec_many result_mode must be full, summary, or none.', 'invalid_argument', false);
    }
    return requested as 'full' | 'summary' | 'none';
  }
  return graphAction === 'run' ? 'full' : 'summary';
}

function compactCommandGraphResult(result: JsonObject): JsonObject {
  const error = result.error && typeof result.error === 'object' && !Array.isArray(result.error)
    ? result.error as JsonObject
    : undefined;
  return {
    id: String(result.id ?? ''),
    ok: result.ok ?? true,
    command_ok: result.command_ok ?? null,
    status: result.status ?? null,
    in_progress: result.in_progress === true,
    pending: result.pending === true,
    skipped: result.skipped === true,
    skip_reason: result.skip_reason ?? null,
    shared_session_preserved: result.shared_session_preserved === true,
    deduplicated: result.deduplicated === true,
    attached_to_session_id: result.attached_to_session_id ?? null,
    session_id: result.session_id ?? null,
    exit_code: result.exit_code ?? result.process_exit_code ?? null,
    termination_reason: result.termination_reason ?? null,
    process_still_running: result.process_still_running ?? false,
    elapsed_ms: result.elapsed_ms ?? null,
    stdout_bytes: result.stdout_bytes ?? 0,
    stderr_bytes: result.stderr_bytes ?? 0,
    error_code: error?.code ?? null,
    error_message: error?.message ?? null
  };
}

function commandGraphSnapshot(
  runtime: FolderRuntime,
  graph: RetainedCommandGraph,
  reattached: boolean,
  yieldMs: number,
  waitMs: number,
  graphAction = 'run',
  resultMode: 'full' | 'summary' | 'none' = 'full'
): JsonObject {
  const ordered = graph.commands.map(command => {
    const id = String(command.id);
    const completed = graph.results.get(id);
    if (completed) return completed;
    if (graph.running.has(id)) {
      const sessionId = graph.sessionIds.get(id);
      const session = sessionId ? runtime.sessions.get(sessionId) : undefined;
      if (session) return {
        id,
        in_progress: true,
        ...processResult(session, {
          output_mode: String(command.output_mode ?? 'tail'),
          max_output_bytes: Number(command.max_output_bytes ?? 65_536)
        })
      };
      return { id, ok: true, command_ok: null, status: 'starting', in_progress: true };
    }
    if (graph.pending.has(id)) {
      return {
        id,
        ok: true,
        command_ok: null,
        status: 'pending',
        pending: true,
        depends_on: Array.isArray(command.depends_on) ? command.depends_on.map(String) : []
      };
    }
    return { id, ok: false, skipped: true, skip_reason: 'graph_state_unavailable' };
  });
  const completedResults = [...graph.results.values()];
  const failed = completedResults.filter(result => (result.command_ok === false || result.ok === false) && !result.skipped);
  const skipped = completedResults.filter(result => result.skipped);
  const failedCommandIds = failed.map(result => String(result.id));
  const skippedCommandIds = skipped.map(result => String(result.id));
  const runningCommandIds = graph.commands.map(command => String(command.id)).filter(id => graph.running.has(id));
  const pendingCommandIds = graph.commands.map(command => String(command.id)).filter(id => graph.pending.has(id));
  const completedCommandIds = graph.commands.map(command => String(command.id)).filter(id => graph.results.has(id));
  const firstFailure = failed[0];
  const firstFailureError = firstFailure?.error && typeof firstFailure.error === 'object' && !Array.isArray(firstFailure.error)
    ? firstFailure.error as JsonObject
    : undefined;
  const firstFailureDetails = firstFailureError?.details && typeof firstFailureError.details === 'object' && !Array.isArray(firstFailureError.details)
    ? firstFailureError.details as JsonObject
    : undefined;
  const graphCompleted = graph.completedAt !== undefined;
  const cancelRequested = graph.cancelRequestedAt !== undefined;
  const graphStatus = graphCompleted ? (cancelRequested ? 'cancelled' : 'completed') : (cancelRequested ? 'cancelling' : 'running');
  const recoveryActions: JsonObject[] = [];
  if (graphCompleted && !cancelRequested && failedCommandIds.length) recoveryActions.push({
    action: 'retry_failed_commands',
    tool: 'exec_many',
    command_ids: failedCommandIds,
    required_arguments: ['commands'],
    suggestion: 'Correct the first failure, then retry only the failed command definitions instead of rerunning successful commands.'
  });
  if (graphCompleted && !cancelRequested && skippedCommandIds.length) recoveryActions.push({
    action: failedCommandIds.length ? 'retry_affected_subgraph' : 'fix_command_dependencies',
    tool: 'exec_many',
    command_ids: skippedCommandIds,
    failed_command_ids: failedCommandIds,
    required_arguments: ['commands'],
    suggestion: failedCommandIds.length
      ? 'After fixing the failed commands, retry the skipped downstream commands whose dependencies were not satisfied.'
      : 'Fix missing or cyclic dependencies before retrying the skipped commands.'
  });
  const nextActions: JsonObject[] = graphCompleted ? [] : [{
    tool: 'exec_many',
    arguments: {
      operation_id: graph.id,
      yield_time_ms: 30_000,
      result_mode: 'summary'
    }
  }];
  const results = resultMode === 'none' ? [] : resultMode === 'summary' ? ordered.map(compactCommandGraphResult) : ordered;
  const retentionExpiresAt = graph.completedAt === undefined ? null : graph.completedAt + COMMAND_GRAPH_RETENTION_MS;
  const graphProgressOk = failed.length === 0 && !graph.schedulerError;
  const graphExecutionOk = graphCompleted ? graphProgressOk && skipped.length === 0 && !cancelRequested : null;
  const controlOk = graphAction !== 'run';
  return {
    ok: controlOk ? true : (graphExecutionOk ?? graphProgressOk),
    control_ok: controlOk ? true : null,
    graph_execution_ok: graphExecutionOk,
    graph_progress_ok: graphProgressOk,
    operation_id: graph.id,
    graph_operation_id: graph.id,
    graph_action: graphAction,
    graph_status: graphStatus,
    graph_completed: graphCompleted,
    terminal: graphCompleted,
    detached: !graphCompleted,
    reattached,
    cancel_requested: cancelRequested,
    cancel_requested_at_ms: graph.cancelRequestedAt ?? null,
    cancel_reason: graph.cancelReason ?? null,
    graph_created_ts_ms: graph.createdAt,
    graph_completed_ts_ms: graph.completedAt ?? null,
    retention_seconds: COMMAND_GRAPH_RETENTION_MS / 1000,
    retention_expires_ts_ms: retentionExpiresAt,
    retention_remaining_ms: retentionExpiresAt === null ? null : Math.max(0, retentionExpiresAt - Date.now()),
    result_mode: resultMode,
    results_included: resultMode !== 'none',
    result_output_included: resultMode === 'full',
    results_omitted_count: resultMode === 'none' ? graph.commands.length : 0,
    graph_yield_ms: yieldMs,
    graph_wait_ms: waitMs,
    requested_mode: graph.requestedMode,
    mode: graph.mode,
    auto_selected: graph.requestedMode === 'auto',
    max_parallel: graph.maxParallel,
    commands_requested: graph.commands.length,
    commands_executed: graph.startedIds.size,
    completed_command_count: completedCommandIds.length,
    running_command_count: runningCommandIds.length,
    pending_command_count: pendingCommandIds.length,
    failed_command_count: failed.length,
    skipped_command_count: skipped.length,
    completed_command_ids: completedCommandIds,
    running_command_ids: runningCommandIds,
    pending_command_ids: pendingCommandIds,
    failed_command_ids: failedCommandIds,
    skipped_command_ids: skippedCommandIds,
    first_failure: firstFailure ? {
      id: String(firstFailure.id),
      command_ok: firstFailure.command_ok ?? false,
      exit_code: firstFailure.exit_code ?? null,
      termination_reason: firstFailure.termination_reason ?? firstFailureDetails?.termination_reason ?? null,
      error_code: firstFailureError?.code ?? null,
      error_message: firstFailureError?.message ?? null
    } : null,
    scheduler_error: graph.schedulerError ?? null,
    recovery_actions: recoveryActions,
    next_actions: nextActions,
    results
  };
}

export async function runCommandGraph(
  dependencies: CommandGraphProcessDependencies,
  ctx: ToolContext,
  key: string,
  args: JsonObject,
  signal?: AbortSignal
): Promise<JsonObject> {
  if (signal?.aborted) {
    throw dependencies.error('COMMAND_GRAPH_START_CANCELLED', 'Command graph request was cancelled before it could be retained.', 'runtime', true);
  }
  const runtime = currentFolderRuntime(ctx, key);
  const graphs = retainedCommandGraphMap(runtime);
  const requestedOperationId = String(args.operation_id ?? '').trim();
  const graphAction = String(args.action ?? 'run').trim().toLowerCase();
  if (!['run', 'status', 'cancel', 'forget'].includes(graphAction)) {
    throw dependencies.error('INVALID_ARGUMENT', 'exec_many action must be run, status, cancel, or forget.', 'invalid_argument', false);
  }
  const resultMode = commandGraphResultMode(dependencies, args, graphAction);
  let commands: Array<JsonObject & { id: string }> = [];
  if (args.commands !== undefined) {
    try {
      commands = normalizeCommandGraphCommands(args.commands);
      const hasDependencies = commands.some(command => Array.isArray(command.depends_on) && command.depends_on.length > 0);
      validateCommandGraphStructure(commands, hasDependencies);
    } catch (error) {
      throw dependencies.error('INVALID_ARGUMENT', error instanceof Error ? error.message : String(error), 'invalid_argument', false);
    }
  }

  if (requestedOperationId) {
    const existing = graphs.get(requestedOperationId);
    if (existing) {
      if (graphAction !== 'run' && commands.length) {
        throw dependencies.error('INVALID_ARGUMENT', `exec_many action=${graphAction} does not accept commands.`, 'invalid_argument', false);
      }
      if (graphAction === 'status') {
        return commandGraphSnapshot(runtime, existing, true, 0, 0, graphAction, resultMode);
      }
      if (graphAction === 'cancel') {
        const cancelled = await cancelCommandGraph(dependencies, runtime, existing, String(args.reason ?? '').trim());
        const waited = await waitForCommandGraph(existing, args.yield_time_ms);
        const cancelledSessionCount = [...existing.ownedSessionIds]
          .map(sessionId => runtime.sessions.get(sessionId))
          .filter(session => session?.terminationReason === 'graph_cancelled').length;
        return {
          ...commandGraphSnapshot(runtime, existing, true, waited.yieldMs, waited.waitMs, graphAction, resultMode),
          cancel_accepted: cancelled.accepted,
          cancelled_session_count: cancelledSessionCount
        };
      }
      if (graphAction === 'forget') {
        if (existing.completedAt === undefined) {
          throw dependencies.error('COMMAND_GRAPH_STILL_RUNNING', 'Running exec_many graphs must be cancelled or completed before they can be forgotten.', 'conflict', true, {
            operation_id: requestedOperationId,
            suggestion: 'Use exec_many with action=cancel, or wait for graph completion before action=forget.'
          });
        }
        graphs.delete(existing.id);
        const graphFingerprints = retainedCommandGraphFingerprints.get(runtime);
        if (graphFingerprints?.get(existing.fingerprint) === existing.id) graphFingerprints.delete(existing.fingerprint);
        return {
          ok: true,
          operation_id: existing.id,
          graph_operation_id: existing.id,
          graph_action: graphAction,
          graph_status: 'forgotten',
          graph_completed: true,
          terminal: true,
          forgotten: true,
          retained_graph_count: graphs.size,
          next_actions: []
        };
      }
      if (commands.length) {
        const configuration = commandGraphConfiguration(ctx, commands, args);
        const requestedFingerprint = commandGraphFingerprint(commands, configuration.mode, configuration.maxParallel, configuration.stopOnError, ctx.config.sandbox);
        if (requestedFingerprint !== existing.fingerprint) {
          throw dependencies.error('OPERATION_ID_CONFLICT', 'operation_id is already bound to a different exec_many graph.', 'conflict', false, {
            operation_id: requestedOperationId,
            operation_scope: 'exec_many_graph',
            existing_graph_fingerprint: existing.fingerprint,
            requested_graph_fingerprint: requestedFingerprint
          });
        }
      }
      const waited = await waitForCommandGraph(existing, args.yield_time_ms);
      return commandGraphSnapshot(runtime, existing, true, waited.yieldMs, waited.waitMs, graphAction, resultMode);
    }
    if (graphAction !== 'run' || !commands.length) {
      throw dependencies.error('COMMAND_GRAPH_OPERATION_NOT_FOUND', 'Retained exec_many graph operation was not found or expired.', 'not_found', false, {
        operation_id: requestedOperationId,
        retention_seconds: COMMAND_GRAPH_RETENTION_MS / 1000,
        suggestion: 'Start a new exec_many graph by providing commands with a new operation_id.'
      });
    }
  }

  if (graphAction !== 'run') {
    throw dependencies.error('INVALID_ARGUMENT', `exec_many action=${graphAction} requires a retained operation_id.`, 'invalid_argument', false);
  }
  if (!commands.length) {
    throw dependencies.error('INVALID_ARGUMENT', 'commands or a retained operation_id is required.', 'invalid_argument', false);
  }
  const capacityEvictedGraphCount = pruneRetainedCommandGraphs(graphs, 1);
  if (graphs.size >= MAX_RETAINED_COMMAND_GRAPHS) {
    throw dependencies.error('COMMAND_GRAPH_LIMIT_REACHED', 'Too many retained exec_many graph operations are active or awaiting expiry.', 'resource', true, {
      retained_graph_count: graphs.size,
      retained_graph_limit: MAX_RETAINED_COMMAND_GRAPHS,
      active_graph_count: [...graphs.values()].filter(graph => graph.completedAt === undefined).length,
      completed_graph_count: [...graphs.values()].filter(graph => graph.completedAt !== undefined).length,
      capacity_evicted_graph_count: capacityEvictedGraphCount,
      retention_seconds: COMMAND_GRAPH_RETENTION_MS / 1000
    });
  }

  const configuration = commandGraphConfiguration(ctx, commands, args);
  const fingerprintValue = commandGraphFingerprint(commands, configuration.mode, configuration.maxParallel, configuration.stopOnError, ctx.config.sandbox);
  const graphDeduplicable = commandGraphExplicitlyDeduplicable(commands);
  const graphFingerprints = retainedCommandGraphFingerprints.get(runtime) ?? new Map<string, string>();
  retainedCommandGraphFingerprints.set(runtime, graphFingerprints);
  if (!requestedOperationId && graphDeduplicable) {
    const existingGraphId = graphFingerprints.get(fingerprintValue);
    const existingGraph = existingGraphId ? graphs.get(existingGraphId) : undefined;
    const reusable = existingGraph && (
      existingGraph.completedAt === undefined
      || Date.now() - existingGraph.completedAt <= AUTO_DEDUPE_COMPLETED_GRACE_MS
    );
    if (reusable) {
      const waited = await waitForCommandGraph(existingGraph, args.yield_time_ms);
      return {
        ...commandGraphSnapshot(runtime, existingGraph, true, waited.yieldMs, waited.waitMs, graphAction, resultMode),
        graph_deduplicated: true,
        retained_graph_count: graphs.size
      };
    }
    if (existingGraphId) graphFingerprints.delete(fingerprintValue);
  }
  let operationId = requestedOperationId;
  while (!operationId || graphs.has(operationId)) operationId = randomUUID();
  const graph: RetainedCommandGraph = {
    id: operationId,
    fingerprint: fingerprintValue,
    commands,
    requestedMode: configuration.requestedMode,
    mode: configuration.mode,
    stopOnError: configuration.stopOnError,
    maxParallel: configuration.maxParallel,
    results: new Map(),
    pending: new Map(commands.map(command => [String(command.id), command])),
    running: new Map(),
    sessionIds: new Map(),
    ownedSessionIds: new Set(),
    startedIds: new Set(),
    createdAt: Date.now(),
    abortController: new AbortController(),
    completion: Promise.resolve()
  };
  graphs.set(operationId, graph);
  if (!requestedOperationId && graphDeduplicable) graphFingerprints.set(fingerprintValue, operationId);
  graph.completion = scheduleCommandGraph(dependencies, ctx, key, graph);
  const waited = await waitForCommandGraph(graph, args.yield_time_ms);
  return {
    ...commandGraphSnapshot(runtime, graph, false, waited.yieldMs, waited.waitMs, graphAction, resultMode),
    capacity_evicted_graph_count: capacityEvictedGraphCount,
    retained_graph_count: graphs.size
  };
}
