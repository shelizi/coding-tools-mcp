import { FINALIZED_SESSION_RETENTION_MS } from './processes.js';
import { wrapMcpToolResult } from './toolContract.js';
import type { JsonObject, ToolContext } from './types.js';

export const MCP_TASKS_EXTENSION = 'io.modelcontextprotocol/tasks';
export const MCP_TASK_POLL_INTERVAL_MS = 1_000;

type ProcessTaskStatus = 'working' | 'completed' | 'cancelled';

export interface McpProcessTaskRecord {
  readonly taskId: string;
  readonly sessionId: string;
  readonly toolName: string;
  readonly argumentsValue: JsonObject;
  readonly conversationKey: string;
  readonly createdAtMs: number;
  readonly expiresAtMs: number;
  status: ProcessTaskStatus;
  statusMessage?: string;
  lastUpdatedAtMs: number;
  cancelRequested: boolean;
  finalResult?: JsonObject;
}

const processTasks = new WeakMap<ToolContext, Map<string, McpProcessTaskRecord>>();

function objectValue(value: unknown): JsonObject | undefined {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonObject : undefined;
}

function finiteTimestamp(value: unknown, fallback: number): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function isoTimestamp(value: number): string {
  return new Date(Math.max(0, value)).toISOString();
}

function taskMap(context: ToolContext): Map<string, McpProcessTaskRecord> {
  let tasks = processTasks.get(context);
  if (!tasks) {
    tasks = new Map();
    processTasks.set(context, tasks);
  }
  const now = Date.now();
  for (const [taskId, task] of tasks) {
    if (task.expiresAtMs <= now) tasks.delete(taskId);
  }
  return tasks;
}

export function clientSupportsMcpTasks(params: JsonObject): boolean {
  const meta = objectValue(params._meta);
  const capabilities = objectValue(meta?.['io.modelcontextprotocol/clientCapabilities']);
  const extensions = objectValue(capabilities?.extensions);
  return objectValue(extensions?.[MCP_TASKS_EXTENSION]) !== undefined;
}

function taskBase(task: McpProcessTaskRecord): JsonObject {
  return {
    taskId: task.taskId,
    status: task.status,
    ...(task.statusMessage ? { statusMessage: task.statusMessage } : {}),
    createdAt: isoTimestamp(task.createdAtMs),
    lastUpdatedAt: isoTimestamp(task.lastUpdatedAtMs),
    ttlMs: FINALIZED_SESSION_RETENTION_MS,
    pollIntervalMs: MCP_TASK_POLL_INTERVAL_MS
  };
}

export function createProcessTask(
  context: ToolContext,
  conversationKey: string,
  toolName: string,
  argumentsValue: JsonObject,
  structured: JsonObject
): JsonObject | undefined {
  const sessionId = typeof structured.session_id === 'string' ? structured.session_id.trim() : '';
  const pending = structured.process_still_running === true
    || structured.post_checks_pending === true
    || structured.command_ok === null;
  if (toolName !== 'exec_command' || !sessionId || !pending) return undefined;

  const now = Date.now();
  const createdAtMs = finiteTimestamp(structured.started_ts_ms, now);
  const taskId = `exec:${sessionId}`;
  const record: McpProcessTaskRecord = {
    taskId,
    sessionId,
    toolName,
    argumentsValue: structuredClone(argumentsValue),
    conversationKey,
    createdAtMs,
    expiresAtMs: createdAtMs + FINALIZED_SESSION_RETENTION_MS,
    status: 'working',
    statusMessage: structured.post_checks_pending === true
      ? 'Process exited; verification is still pending.'
      : 'Process is still running.',
    lastUpdatedAtMs: now,
    cancelRequested: false
  };
  taskMap(context).set(taskId, record);
  return { resultType: 'task', ...taskBase(record) };
}

export function requireProcessTask(
  context: ToolContext,
  conversationKey: string,
  taskIdValue: unknown
): McpProcessTaskRecord {
  const taskId = typeof taskIdValue === 'string' ? taskIdValue.trim() : '';
  if (!taskId) throw new Error('taskId is required');
  const task = taskMap(context).get(taskId);
  if (!task || task.conversationKey !== conversationKey) throw new Error(`Task not found: ${taskId}`);
  return task;
}

export function detailedProcessTask(task: McpProcessTaskRecord): JsonObject {
  return {
    resultType: 'complete',
    ...taskBase(task),
    ...(task.status === 'completed' && task.finalResult ? { result: task.finalResult } : {})
  };
}

export function updateProcessTaskFromSnapshot(
  task: McpProcessTaskRecord,
  structured: JsonObject
): JsonObject {
  const now = Date.now();
  task.lastUpdatedAtMs = now;
  const stillWorking = structured.process_still_running === true
    || structured.post_checks_pending === true
    || structured.command_ok === null;
  if (stillWorking) {
    task.status = 'working';
    task.statusMessage = task.cancelRequested
      ? 'Cancellation requested; process termination is still pending.'
      : structured.post_checks_pending === true
        ? 'Process exited; verification is still pending.'
        : 'Process is still running.';
    return detailedProcessTask(task);
  }

  const reason = typeof structured.termination_reason === 'string' ? structured.termination_reason : '';
  if (task.cancelRequested || reason === 'killed' || reason === 'graph_cancelled' || reason === 'detached_timeout') {
    task.status = 'cancelled';
    task.statusMessage = 'Task was cancelled.';
    task.finalResult = undefined;
    return detailedProcessTask(task);
  }

  task.status = 'completed';
  task.statusMessage = undefined;
  task.finalResult = {
    ...wrapMcpToolResult(task.toolName, task.argumentsValue, structured),
    resultType: 'complete'
  };
  return detailedProcessTask(task);
}

export function markProcessTaskCancellationRequested(task: McpProcessTaskRecord, structured?: JsonObject): void {
  task.cancelRequested = true;
  task.lastUpdatedAtMs = Date.now();
  const stillWorking = structured?.process_still_running === true || structured?.post_checks_pending === true;
  if (structured && !stillWorking) {
    task.status = 'cancelled';
    task.statusMessage = 'Task was cancelled.';
    task.finalResult = undefined;
  } else {
    task.status = 'working';
    task.statusMessage = 'Cancellation requested; process termination is still pending.';
  }
}

export function markMissingCancelledProcessTask(task: McpProcessTaskRecord): JsonObject | undefined {
  if (!task.cancelRequested) return undefined;
  task.status = 'cancelled';
  task.statusMessage = 'Task was cancelled.';
  task.lastUpdatedAtMs = Date.now();
  task.finalResult = undefined;
  return detailedProcessTask(task);
}
