import type { JsonObject } from './types.js';
import { containsSensitivePath, redactSensitiveText } from './redaction.js';

// A two-minute window made ordinary tool-driven work appear idle between
// commands. Keep "running" strictly tied to in-flight requests/processes;
// Five minutes applies only to the "active recently" label. Longer stale
// windows hide truly idle projects from the Overview.
export const ACTIVE_WINDOW_MS = 5 * 60_000;
const MAX_ACTION_CHARS = 120;
const MAX_TRACKED_SESSIONS = 4_096;
const MAX_RECENT_ACTIONS = 16;

export interface SessionRecentAction {
  sequence: number;
  tool: string;
  action: string;
  startedAtMs: number;
  updatedAtMs: number;
  status: string;
}

interface ActiveRequest {
  tool: string;
  action: string;
}

interface SessionActivityState {
  lastActivityAtMs: number;
  lastTool: string;
  lastAction: string;
  lastOutcome: string;
  nextSequence: number;
  requests: Map<number, ActiveRequest>;
  backgroundJobs: Map<string, ActiveRequest>;
  recentActions: SessionRecentAction[];
}

export interface SessionActivitySnapshot {
  status: 'running' | 'active' | 'inactive';
  tool: string;
  action: string;
  lastActivityAtMs: number;
  activeRequestCount: number;
  lastOutcome: string;
}

export interface BackgroundJobStatus {
  running: boolean;
  endedAtMs?: number;
  outcome?: string;
}

/** Only the authenticated history backend should consume process IDs here. */
export function sessionBackgroundProcessRefs(
  owner: object,
  sessionKey: string | undefined,
): ReadonlyArray<{ id: string; tool: string; action: string }> {
  if (!sessionKey) return [];
  const state = stores.get(owner)?.get(sessionKey);
  if (!state) return [];
  return [...state.backgroundJobs].slice(0, 64).map(([id, request]) => ({
    id, tool: request.tool, action: request.action,
  }));
}

/** The observable tool timeline is not an AI-generated session summary. */
export function sessionRecentActions(owner: object, sessionKey: string | undefined): SessionRecentAction[] {
  const entries = sessionKey ? stores.get(owner)?.get(sessionKey)?.recentActions : undefined;
  return entries
    ? [...entries].sort((left, right) =>
      right.updatedAtMs - left.updatedAtMs || right.sequence - left.sequence)
      .map(entry => ({ ...entry }))
    : [];
}

// Each Node workspace runtime owns a distinct conversation store. Keep activity
// scoped to it, so matching session keys in other workspaces cannot collide.
const stores = new WeakMap<object, Map<string, SessionActivityState>>();

function safeActionDetail(value: string): string {
  if (containsSensitivePath(value)) return '[sensitive path]';
  return Array.from(redactSensitiveText(value).value.trim()).slice(0, MAX_ACTION_CHARS).join('');
}

function actionSummary(tool: string, args: JsonObject): string {
  const detail = tool === 'exec_many' && Array.isArray(args.commands)
    ? `${args.commands.length} commands`
    : ['path', 'query', 'pattern', 'folder_id', 'cmd', 'script',
      'program', 'session_id', 'output_ref', 'workdir']
      .map(key => args[key])
      .find((value): value is string => typeof value === 'string' && value.trim().length > 0);
  const label = typeof detail === 'string' ? safeActionDetail(detail) : '';
  return Array.from(label ? `${tool} · ${label}` : tool).slice(0, MAX_ACTION_CHARS).join('');
}

function storeFor(owner: object): Map<string, SessionActivityState> {
  let store = stores.get(owner);
  if (!store) {
    store = new Map();
    stores.set(owner, store);
  }
  return store;
}

export function beginSessionActivity(
  owner: object,
  sessionKey: string | undefined,
  tool: string,
  argumentsValue: JsonObject,
  nowMs = Date.now(),
): { complete(outcome: string, finishedMs?: number): void } | undefined {
  const key = sessionKey?.trim();
  if (!key || !tool.trim()) return undefined;
  const store = storeFor(owner);
  let state = store.get(key);
  if (!state) {
    if (store.size >= MAX_TRACKED_SESSIONS) {
      const evict = [...store].find(([, candidate]) =>
        candidate.requests.size === 0 && candidate.backgroundJobs.size === 0);
      if (evict) store.delete(evict[0]);
    }
    state = {
      lastActivityAtMs: 0, lastTool: '', lastAction: '', lastOutcome: '',
      nextSequence: 0, requests: new Map(), backgroundJobs: new Map(), recentActions: [],
    };
    store.set(key, state);
  }
  const sequence = ++state.nextSequence;
  const action = actionSummary(tool, argumentsValue);
  state.lastActivityAtMs = Math.max(state.lastActivityAtMs, nowMs);
  state.lastTool = tool;
  state.lastAction = action;
  state.requests.set(sequence, { tool, action });
  state.recentActions.push({
    sequence, tool, action, startedAtMs: nowMs, updatedAtMs: nowMs, status: 'running',
  });
  if (state.recentActions.length > MAX_RECENT_ACTIONS) state.recentActions.shift();
  let completed = false;
  return {
    complete(outcome: string, finishedMs = Date.now()) {
      if (completed) return;
      completed = true;
      const request = state.requests.get(sequence);
      if (!request) return;
      state.requests.delete(sequence);
      state.lastTool = request.tool;
      state.lastAction = request.action;
      state.lastActivityAtMs = Math.max(state.lastActivityAtMs, finishedMs);
      state.lastOutcome = outcome;
      const entry = state.recentActions.find(item => item.sequence === sequence);
      if (entry) {
        entry.status = outcome;
        entry.updatedAtMs = finishedMs;
      }
    },
  };
}

export function sessionActivitySnapshot(
  owner: object,
  sessionKey: string | undefined,
  nowMs = Date.now(),
  lookupJob?: (id: string) => BackgroundJobStatus | undefined,
): SessionActivitySnapshot | undefined {
  const state = sessionKey ? stores.get(owner)?.get(sessionKey) : undefined;
  if (!state) return undefined;
  let runningJob: ActiveRequest | undefined;
  let runningJobs = 0;
  if (lookupJob) {
    for (const [id, request] of state.backgroundJobs) {
      const job = lookupJob(id);
      if (job?.running) {
        runningJob = request;
        runningJobs++;
      } else {
        state.backgroundJobs.delete(id);
        if (job?.endedAtMs !== undefined) {
          state.lastActivityAtMs = Math.max(state.lastActivityAtMs, job.endedAtMs);
          if (job.outcome) state.lastOutcome = job.outcome;
        }
      }
    }
  }
  const active = [...state.requests.values()].at(-1);
  return {
    status: active || runningJob ? 'running'
      : Math.max(0, nowMs - state.lastActivityAtMs) <= ACTIVE_WINDOW_MS ? 'active' : 'inactive',
    tool: active?.tool ?? runningJob?.tool ?? state.lastTool,
    action: active?.action ?? runningJob?.action ?? state.lastAction,
    lastActivityAtMs: state.lastActivityAtMs,
    activeRequestCount: state.requests.size + runningJobs,
    lastOutcome: state.lastOutcome,
  };
}

function managedProcessIds(tool: string, response: unknown): string[] {
  if (!['exec_command', 'exec_many', 'exec_health_check'].includes(tool)) return [];
  if (!response || typeof response !== 'object') return [];
  const content = (response as Record<string, unknown>).structuredContent;
  if (!content || typeof content !== 'object' || Array.isArray(content)) return [];
  const root = content as Record<string, unknown>;
  const snapshots: unknown[] = [root];
  if (tool === 'exec_many' && Array.isArray(root.results)) {
    for (const item of root.results.slice(0, 256)) {
      if (!item || typeof item !== 'object') continue;
      const nested = (item as Record<string, unknown>).result;
      snapshots.push(nested && typeof nested === 'object' ? nested : item);
    }
  }
  return snapshots.flatMap(snapshot => {
    if (!snapshot || typeof snapshot !== 'object' || Array.isArray(snapshot)) return [];
    const data = snapshot as Record<string, unknown>;
    return typeof data.session_id === 'string' && data.session_id.length > 0
      && (data.status === 'running' || data.status === 'verifying')
      ? [data.session_id] : [];
  });
}

export function observeManagedProcesses(
  owner: object,
  sessionKey: string | undefined,
  tool: string,
  response: unknown,
): void {
  const state = sessionKey ? stores.get(owner)?.get(sessionKey) : undefined;
  if (!state) return;
  for (const id of managedProcessIds(tool, response)) {
    if (!state.backgroundJobs.has(id) && state.backgroundJobs.size >= 64) {
      const oldest = state.backgroundJobs.keys().next().value;
      if (oldest) state.backgroundJobs.delete(oldest);
    }
    state.backgroundJobs.set(id, { tool, action: state.lastAction });
  }
}

export async function trackMcpToolActivity<T>(
  owner: object,
  sessionKey: string | undefined,
  tool: string,
  args: JsonObject,
  action: () => Promise<T>,
  onChange?: () => void,
): Promise<T> {
  const activity = beginSessionActivity(owner, sessionKey, tool, args);
  const notify = () => {
    if (!activity) return;
    try { onChange?.(); }
    catch { /* Best-effort persistence must not break user tools. */ }
  };
  notify();
  try {
    const result = await action();
    observeManagedProcesses(owner, sessionKey, tool, result);
    const data = result && typeof result === 'object' ? result as Record<string, unknown> : {};
    activity?.complete(data.isError === true || data.ok === false || data.error ? 'failed' : 'success');
    notify();
    return result;
  } catch (error) {
    activity?.complete('failed');
    notify();
    throw error;
  }
}
