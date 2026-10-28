import { stat } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import path from 'node:path';
import type { ToolContext } from './types.js';
import {
  acquireHistoryLock, canonicalPath, DEFAULT_HISTORY_DIR, readHistoryDocument,
  readHistoryIndex, writeHistoryMarkdown,
} from './historyStorage.js';
import { mergeAutoActivity, type AutoActivityEvent } from './historyAutoMarkdown.js';
import { sessionRecentActions, sessionBackgroundProcessRefs } from './sessionActivity.js';
import { processStatus } from './processes.js';

const FLUSH_DELAY_MS = 5_000;
const MAX_PENDING_EVENTS = 96;
// Running jobs are checked every 10 seconds. Persist at most one verified
// heartbeat per minute so other runtimes can still see long-running projects
// after their five-minute recent-activity window, without growing the archive.
export const BACKGROUND_HEARTBEAT_MS = 60_000;
const MAX_OBSERVED_JOB_EVENTS = 256;

export function backgroundObservationId(
  pid: number,
  digest: string,
  running: boolean,
  atMs: number,
): string {
  return running
    ? `node:${pid}:${digest}:${Math.floor(atMs / BACKGROUND_HEARTBEAT_MS)}:background_running`
    : `node:${pid}:${digest}:background_ended`;
}

interface Pending {
  folderId: string;
  sessionKey: string;
  events: Map<string, AutoActivityEvent>;
  timer?: ReturnType<typeof setTimeout>;
  flushing: boolean;
  retries: number;
  observedJobs: Set<string>;
  hasRunningJob: boolean;
}
const pendingByContext = new WeakMap<ToolContext, Map<string, Pending>>();

function pendingBackgroundEvents(ctx: ToolContext, job: Pending): AutoActivityEvent[] {
  const results: AutoActivityEvent[] = [];
  job.hasRunningJob = false;
  const sessions = ctx.folderRuntimes?.get(job.folderId)?.sessions;
  if (!sessions) return results;
  for (const reference of sessionBackgroundProcessRefs(ctx.conversations, job.sessionKey)) {
    const managed = sessions.get(reference.id);
    if (!managed) continue;
    const status = processStatus(managed);
    const running = status === 'running' || status === 'verifying';
    job.hasRunningJob ||= running;
    const kind = running ? 'background_running' : 'background_ended';
    const digest = createHash('sha256').update(reference.id).digest('hex').slice(0, 16);
    const observedAtMs = Date.now();
    const id = backgroundObservationId(process.pid, digest, running, observedAtMs);
    if (job.observedJobs.has(id)) continue;
    job.observedJobs.add(id);
    if (job.observedJobs.size > MAX_OBSERVED_JOB_EVENTS) {
      job.observedJobs.delete(job.observedJobs.values().next().value!);
    }
    results.push({
      id,
      atMs: running ? observedAtMs : managed.endedAt ?? observedAtMs,
      tool: reference.tool,
      action: reference.action,
      status: kind,
    });
  }
  return results;
}

function inside(root: string, candidate: string): boolean {
  const relative = path.relative(root, candidate);
  return relative === '' ||
    (relative !== '..' && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
}

export async function persistAutoActivity(
  ctx: ToolContext,
  folderId: string,
  sessionKey: string,
  events: readonly AutoActivityEvent[],
): Promise<boolean> {
  if (!events.length) return false;
  const folder = ctx.config.folders.find(candidate => candidate.id === folderId);
  if (!folder) return false;
  let root: string;
  let dir: string;
  try {
    root = await canonicalPath(folder.path);
    const candidate = path.join(root, DEFAULT_HISTORY_DIR);
    if (!(await stat(candidate)).isDirectory()) return false;
    dir = await canonicalPath(candidate);
    if (!inside(root, dir)) return false;
  } catch {
    // No durable history was bootstrapped yet, or path has become invalid.
    return false;
  }

  // Both Node and Rust use .history.lock.d, so a concurrent semantic
  // checkpoint cannot overwrite new auto activity or vice versa.
  const lock = await acquireHistoryLock(dir);
  try {
    const index = await readHistoryIndex(dir);
    const entry = index?.sessions[sessionKey];
    if (!entry) return false;
    const document = await readHistoryDocument(root, dir, entry);
    if (document.session_key !== sessionKey) return false;
    if (/^completed$/i.test((document.content.match(/^\*\*Status:\*\*\s*(.*)$/m)?.[1] ?? '').trim())) return false;
    const next = mergeAutoActivity(document.content, events);
    if (next === document.content) return false;
    await writeHistoryMarkdown(path.join(dir, `${entry.number}.md`), next);
    return true;
  } finally {
    await lock.release();
  }
}

function scheduleFlush(ctx: ToolContext, key: string, job: Pending): void {
  if (job.timer || job.flushing) return;
  job.timer = setTimeout(() => {
    job.timer = undefined;
    job.flushing = true;
    const background = pendingBackgroundEvents(ctx, job);
    const events = [...job.events.values(), ...background];
    job.events.clear();
    void persistAutoActivity(ctx, job.folderId, job.sessionKey, events)
      .catch(() => {
        // Best effort: tool calls must never fail because a history disk write
        // was denied, busy or unavailable; retry pending activity once later.
        job.retries++;
        if (job.retries <= 2) {
          for (const event of events) job.events.set(event.id, event);
        }
      })
      .then(() => {
        if (!job.events.size) job.retries = 0;
      })
      .finally(() => {
        job.flushing = false;
        if (job.events.size || job.hasRunningJob) scheduleFlush(ctx, key, job);
        else pendingByContext.get(ctx)?.delete(key);
      });
  }, job.hasRunningJob ? 10_000 : FLUSH_DELAY_MS);
  job.timer.unref?.();
}

/** Non-blocking, per-folder debounce. Never creates an unbootstrapped archive. */
export function scheduleAutoActivity(ctx: ToolContext, sessionKey: string | undefined): void {
  if (!sessionKey) return;
  const selectedFolderId = ctx.selections.get(sessionKey);
  const folder = selectedFolderId
    ? ctx.config.folders.find(item => item.id === selectedFolderId)
    : ctx.config.folders.length === 1 ? ctx.config.folders[0] : undefined;
  if (!folder) return;
  const actions = sessionRecentActions(ctx.conversations, sessionKey);
  if (!actions.length) return;
  let byKey = pendingByContext.get(ctx);
  if (!byKey) {
    byKey = new Map();
    pendingByContext.set(ctx, byKey);
  }
  const key = `${folder.id}\0${sessionKey}`;
  let pending = byKey.get(key);
  if (!pending) {
    pending = {
      folderId: folder.id, sessionKey, events: new Map(), flushing: false,
      retries: 0, observedJobs: new Set(), hasRunningJob: false,
    };
    byKey.set(key, pending);
  }
  for (const item of actions) {
    const id = `node:${process.pid}:${item.startedAtMs}:${item.sequence}:${item.status}`;
    pending.events.set(id, {
      id, atMs: item.updatedAtMs, tool: item.tool, action: item.action, status: item.status,
    });
  }
  while (pending.events.size > MAX_PENDING_EVENTS) pending.events.delete(pending.events.keys().next().value!);
  scheduleFlush(ctx, key, pending);
}
