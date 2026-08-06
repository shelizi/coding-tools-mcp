import { createHash, randomUUID } from 'node:crypto';
import {
  mkdir, open, readFile, readdir, realpath, rename, rm, stat, writeFile
} from 'node:fs/promises';
import path from 'node:path';
import {
  emptyHistoryIndex, HistoryError, latestHistoryNumber,
  type HistoryDocument, type HistoryIndex, type HistoryIndexEntry, type HistoryScanReport
} from './historyModel.js';
import { metadata } from './historyMarkdown.js';

export const DEFAULT_HISTORY_DIR = 'docs/history-session';
export const HISTORY_INDEX_FILE = 'index.json';
export const HISTORY_LOCK_DIR = '.history.lock.d';
export const HISTORY_LOCK_TIMEOUT_MS = 5_000;
export const HISTORY_LOCK_RETRY_MS = 10;
export const HISTORY_LOCK_STALE_MS = 30_000;

const HISTORY_LOCK_OWNER_FILE = 'owner.json';
const HISTORY_TEMP_PREFIX = '.history-tmp-';

function errorCode(error: unknown): string | undefined {
  return error && typeof error === 'object' && 'code' in error ? String((error as { code?: unknown }).code ?? '') : undefined;
}

function delay(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

export function relativeDisplay(root: string, value: string): string {
  return path.relative(root, value).replaceAll('\\', '/');
}

function sortedSessions(sessions: Record<string, HistoryIndexEntry>): Record<string, HistoryIndexEntry> {
  return Object.fromEntries(Object.entries(sessions).sort(([left], [right]) => left.localeCompare(right)));
}

function normalizedIndex(value: unknown): HistoryIndex {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('index must be an object');
  const input = value as Record<string, unknown>;
  if (input.version !== 1) throw new Error('index version must be 1');
  if (!Number.isSafeInteger(input.latest_number) || Number(input.latest_number) < 0) throw new Error('latest_number is invalid');
  if (!input.sessions || typeof input.sessions !== 'object' || Array.isArray(input.sessions)) throw new Error('sessions must be an object');
  const sessions: Record<string, HistoryIndexEntry> = {};
  for (const [sessionKey, raw] of Object.entries(input.sessions as Record<string, unknown>)) {
    if (!sessionKey || !raw || typeof raw !== 'object' || Array.isArray(raw)) throw new Error('session entry is invalid');
    const entry = raw as Record<string, unknown>;
    if (!Number.isSafeInteger(entry.number) || Number(entry.number) < 1) throw new Error('session number is invalid');
    if (typeof entry.path !== 'string' || !entry.path) throw new Error('session path is invalid');
    if (typeof entry.created_at !== 'string' || typeof entry.updated_at !== 'string') throw new Error('session timestamps are invalid');
    sessions[sessionKey] = {
      number: Number(entry.number), path: entry.path,
      created_at: entry.created_at, updated_at: entry.updated_at
    };
  }
  return { version: 1, latest_number: Number(input.latest_number), sessions: sortedSessions(sessions) };
}

export async function readHistoryIndex(historyDir: string): Promise<HistoryIndex | undefined> {
  const file = path.join(historyDir, HISTORY_INDEX_FILE);
  try {
    const content = await readFile(file, 'utf8');
    return normalizedIndex(JSON.parse(content));
  } catch (error) {
    if (errorCode(error) === 'ENOENT') return undefined;
    if (error instanceof HistoryError) throw error;
    throw new HistoryError(
      'HISTORY_INDEX_INVALID',
      'History index is not valid JSON.',
      'validation',
      true,
      { error: error instanceof Error ? error.message : String(error) }
    );
  }
}

async function atomicWrite(target: string, content: Uint8Array): Promise<void> {
  const parent = path.dirname(target);
  await mkdir(parent, { recursive: true });
  const temporary = path.join(parent, `${HISTORY_TEMP_PREFIX}${randomUUID()}`);
  let handle;
  try {
    handle = await open(temporary, 'wx', 0o600);
    await handle.writeFile(content);
    await handle.sync();
    await handle.close();
    handle = undefined;
    await rename(temporary, target);
    try {
      const directory = await open(parent, 'r');
      await directory.sync().catch(() => undefined);
      await directory.close();
    } catch {
      // Directory fsync is not available on every platform.
    }
  } catch (error) {
    await handle?.close().catch(() => undefined);
    await rm(temporary, { force: true }).catch(() => undefined);
    throw new HistoryError(
      'HISTORY_WRITE_FAILED',
      error instanceof Error ? error.message : String(error),
      'filesystem',
      true,
      { kind: errorCode(error) ?? 'unknown' }
    );
  }
}

export async function writeHistoryIndex(historyDir: string, index: HistoryIndex): Promise<void> {
  const normalized: HistoryIndex = {
    version: 1,
    latest_number: index.latest_number,
    sessions: sortedSessions(index.sessions)
  };
  await atomicWrite(path.join(historyDir, HISTORY_INDEX_FILE), Buffer.from(JSON.stringify(normalized, null, 2)));
}

export async function writeHistoryMarkdown(file: string, content: string): Promise<void> {
  await atomicWrite(file, Buffer.from(content));
}

export function historySha256(content: Uint8Array): string {
  return createHash('sha256').update(content).digest('hex');
}

async function decodeUtf8(file: string, displayPath: string): Promise<string> {
  let bytes: Buffer;
  try {
    bytes = await readFile(file);
  } catch (error) {
    throw new HistoryError(
      'HISTORY_READ_FAILED',
      error instanceof Error ? error.message : String(error),
      'filesystem',
      true,
      { path: displayPath, kind: errorCode(error) ?? 'unknown' }
    );
  }
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch (error) {
    throw new HistoryError(
      'HISTORY_INVALID_UTF8',
      'History Markdown must be UTF-8.',
      'validation',
      false,
      { file: displayPath, error: error instanceof Error ? error.message : String(error) }
    );
  }
}

export async function readHistoryDocument(root: string, historyDir: string, entry: HistoryIndexEntry): Promise<HistoryDocument> {
  const file = path.join(historyDir, `${entry.number}.md`);
  const resolvedPath = relativeDisplay(root, file);
  if (resolvedPath !== entry.path) {
    throw new HistoryError(
      'HISTORY_INDEX_STALE',
      'History index path does not match its numbered Markdown file.',
      'validation',
      true,
      { indexed_path: entry.path, resolved_path: resolvedPath, number: entry.number }
    );
  }
  const content = await decodeUtf8(file, entry.path);
  return {
    number: entry.number,
    path: entry.path,
    content,
    session_key: metadata(content, 'Session key'),
    created_at: metadata(content, 'Created'),
    updated_at: metadata(content, 'Updated')
  };
}

export async function scanHistory(root: string, historyDir: string): Promise<HistoryScanReport> {
  const report: HistoryScanReport = {
    documents: [], numbers: [], missing_numbers: [], duplicate_session_keys: [], invalid_files: [], empty_files: []
  };
  let entries;
  try {
    entries = await readdir(historyDir, { withFileTypes: true });
  } catch (error) {
    if (errorCode(error) === 'ENOENT') return report;
    throw new HistoryError('HISTORY_READ_FAILED', String(error), 'filesystem', true, { kind: errorCode(error) ?? 'unknown' });
  }
  for (const entry of entries) {
    if (!entry.isFile()) continue;
    const name = entry.name;
    if (name === 'README.md' || name === HISTORY_INDEX_FILE || name === '.history.lock' || name.startsWith(HISTORY_TEMP_PREFIX)) continue;
    const match = /^([1-9]\d*)\.md$/.exec(name);
    if (!match || String(Number(match[1])) !== match[1]) {
      report.invalid_files.push(name);
      continue;
    }
    const number = Number(match[1]);
    const file = path.join(historyDir, name);
    const displayPath = relativeDisplay(root, file);
    const content = await decodeUtf8(file, displayPath);
    if (!content.trim()) report.empty_files.push(name);
    report.documents.push({
      number,
      path: displayPath,
      content,
      session_key: metadata(content, 'Session key'),
      created_at: metadata(content, 'Created'),
      updated_at: metadata(content, 'Updated')
    });
  }
  report.documents.sort((left, right) => left.number - right.number);
  report.invalid_files.sort();
  report.empty_files.sort();
  report.numbers = report.documents.map(document => document.number);
  const latest = latestHistoryNumber(report) ?? 0;
  const present = new Set(report.numbers);
  for (let number = 1; number <= latest; number += 1) if (!present.has(number)) report.missing_numbers.push(number);
  const keyCounts = new Map<string, number>();
  for (const document of report.documents) {
    if (document.session_key) keyCounts.set(document.session_key, (keyCounts.get(document.session_key) ?? 0) + 1);
  }
  report.duplicate_session_keys = [...keyCounts.entries()]
    .filter(([, count]) => count > 1)
    .map(([key]) => key)
    .sort();
  return report;
}

export function rebuildHistoryIndex(report: HistoryScanReport): HistoryIndex {
  const duplicates = new Set(report.duplicate_session_keys);
  const index = emptyHistoryIndex();
  index.latest_number = latestHistoryNumber(report) ?? 0;
  for (const document of report.documents) {
    if (!document.session_key || duplicates.has(document.session_key)) continue;
    index.sessions[document.session_key] = {
      number: document.number,
      path: document.path,
      created_at: document.created_at ?? '',
      updated_at: document.updated_at ?? ''
    };
  }
  index.sessions = sortedSessions(index.sessions);
  return index;
}

export interface HistoryLock {
  waitMs: number;
  release(): Promise<void>;
}

interface HistoryLockOptions {
  timeoutMs?: number;
  retryMs?: number;
  staleMs?: number;
}

async function lockIsStale(lockDir: string, staleMs: number): Promise<boolean> {
  try {
    const info = await stat(lockDir);
    return Date.now() - info.mtimeMs >= staleMs;
  } catch (error) {
    return errorCode(error) === 'ENOENT';
  }
}

async function releaseOwnedLock(lockDir: string, token: string): Promise<void> {
  try {
    const owner = JSON.parse(await readFile(path.join(lockDir, HISTORY_LOCK_OWNER_FILE), 'utf8')) as { token?: unknown };
    if (owner.token !== token) return;
  } catch {
    return;
  }
  await rm(lockDir, { recursive: true, force: true }).catch(() => undefined);
}

export async function acquireHistoryLock(historyDir: string, options: HistoryLockOptions = {}): Promise<HistoryLock> {
  const timeoutMs = Math.max(1, options.timeoutMs ?? HISTORY_LOCK_TIMEOUT_MS);
  const retryMs = Math.max(1, options.retryMs ?? HISTORY_LOCK_RETRY_MS);
  const staleMs = Math.max(timeoutMs + 1, options.staleMs ?? HISTORY_LOCK_STALE_MS);
  await mkdir(historyDir, { recursive: true });
  const lockDir = path.join(historyDir, HISTORY_LOCK_DIR);
  const token = randomUUID();
  const started = Date.now();
  for (;;) {
    try {
      await mkdir(lockDir);
      try {
        await writeFile(path.join(lockDir, HISTORY_LOCK_OWNER_FILE), JSON.stringify({
          version: 1, token, pid: process.pid, created_at_ms: Date.now()
        }), { flag: 'wx', mode: 0o600 });
      } catch (error) {
        await rm(lockDir, { recursive: true, force: true }).catch(() => undefined);
        throw error;
      }
      let released = false;
      return {
        waitMs: Date.now() - started,
        async release() {
          if (released) return;
          released = true;
          await releaseOwnedLock(lockDir, token);
        }
      };
    } catch (error) {
      if (errorCode(error) !== 'EEXIST') {
        throw new HistoryError(
          'HISTORY_LOCK_FAILED',
          error instanceof Error ? error.message : String(error),
          'filesystem',
          true,
          { kind: errorCode(error) ?? 'unknown' }
        );
      }
      if (await lockIsStale(lockDir, staleMs)) {
        await rm(lockDir, { recursive: true, force: true }).catch(() => undefined);
        continue;
      }
      const waitMs = Date.now() - started;
      if (waitMs >= timeoutMs) {
        throw new HistoryError(
          'HISTORY_LOCK_TIMEOUT',
          'Timed out waiting for the history archive lock.',
          'runtime',
          true,
          { history_lock_wait_ms: waitMs, timeout_ms: timeoutMs, suggestion: 'Retry after the current history write completes' }
        );
      }
      await delay(Math.min(retryMs, timeoutMs - waitMs));
    }
  }
}

export async function canonicalPath(value: string): Promise<string> {
  return realpath(value);
}
