import { createHash } from 'node:crypto';
import { mkdir, readFile, stat } from 'node:fs/promises';
import path from 'node:path';
import type { JsonObject, ToolContext } from './types.js';
import { rootAndCwd } from './workspace.js';
import {
  attachInheritedSummary, checkpointFromArgs, documentTitle, historySummary, inheritedSummary,
  parseCheckpointRecords, redactCheckpointRecord, renderDocument, truncateChars
} from './historyMarkdown.js';
import {
  HistoryError, historySequenceValid, latestHistoryNumber,
  type HistoryDocument, type HistoryIndex, type HistoryIndexEntry, type HistoryScanReport
} from './historyModel.js';
import {
  acquireHistoryLock, canonicalPath, DEFAULT_HISTORY_DIR, historySha256,
  readHistoryDocument, readHistoryIndex, rebuildHistoryIndex, relativeDisplay,
  scanHistory, writeHistoryIndex, writeHistoryMarkdown
} from './historyStorage.js';

const HISTORY_SUMMARY_WINDOW = 12;
const HISTORY_NUMBER_WINDOW = 256;
const MAX_SESSION_SUMMARY_CHARS = 3_000;
const MAX_ALL_HISTORY_SUMMARY_CHARS = 24_000;
const MAX_LATEST_HANDOFF_CHARS = 24_000;
const MAX_INHERITED_SUMMARY_CHARS = 16_000;

interface HistoryLocation {
  root: string;
  dir: string;
  display: string;
}

function hostSessionKey(args: JsonObject): string | undefined {
  const value = typeof args._host_session_key === 'string' ? args._host_session_key.trim() : '';
  return value || undefined;
}

function fallbackSessionKey(args: JsonObject): string | undefined {
  const value = typeof args._fallback_session_key === 'string' ? args._fallback_session_key.trim() : '';
  return value || undefined;
}

function samePath(left: string, right: string): boolean {
  const normalize = (value: string): string => process.platform === 'win32' ? value.toLowerCase() : value;
  return normalize(path.resolve(left)) === normalize(path.resolve(right));
}

function insidePath(root: string, candidate: string): boolean {
  const relative = path.relative(root, candidate);
  return relative === '' || (!relative.startsWith(`..${path.sep}`) && relative !== '..' && !path.isAbsolute(relative));
}

async function ensureSafeCandidate(root: string, candidate: string): Promise<void> {
  const canonicalRoot = await canonicalPath(root);
  let current = candidate;
  for (;;) {
    try {
      const resolved = await canonicalPath(current);
      if (!insidePath(canonicalRoot, resolved)) throw new HistoryError('PATH_OUTSIDE_WORKSPACE', 'History path escapes the workspace.', 'validation');
      return;
    } catch (error) {
      if (error instanceof HistoryError) throw error;
      const code = error && typeof error === 'object' && 'code' in error ? String((error as { code?: unknown }).code ?? '') : '';
      if (code !== 'ENOENT') throw error;
      const parent = path.dirname(current);
      if (parent === current) throw new HistoryError('PATH_OUTSIDE_WORKSPACE', 'History path escapes the workspace.', 'validation');
      current = parent;
    }
  }
}

async function historyLocation(ctx: ToolContext, key: string, args: JsonObject): Promise<HistoryLocation> {
  const { root } = rootAndCwd(ctx, key);
  if (typeof args.workspace_root === 'string') {
    const requestedRaw = args.workspace_root.trim();
    if (!requestedRaw) throw new HistoryError('INVALID_ARGUMENT', 'workspace_root does not exist', 'validation');
    const requested = path.isAbsolute(requestedRaw) ? requestedRaw : path.resolve(root, requestedRaw);
    let canonicalRequested: string;
    try { canonicalRequested = await canonicalPath(requested); }
    catch { throw new HistoryError('INVALID_ARGUMENT', 'workspace_root does not exist', 'validation'); }
    if (!samePath(await canonicalPath(root), canonicalRequested)) {
      throw new HistoryError('PATH_OUTSIDE_WORKSPACE', 'workspace_root is outside the selected workspace.', 'validation');
    }
  }
  const raw = typeof args.history_dir === 'string' ? args.history_dir.trim() : DEFAULT_HISTORY_DIR;
  if (!raw || raw.includes('\0')) throw new HistoryError('PATH_OUTSIDE_WORKSPACE', 'history_dir is outside the workspace.', 'validation');
  const dir = path.resolve(root, raw);
  if (!insidePath(root, dir)) throw new HistoryError('PATH_OUTSIDE_WORKSPACE', 'history_dir is outside the workspace.', 'validation');
  await ensureSafeCandidate(root, dir);
  try {
    const info = await stat(dir);
    if (!info.isDirectory()) throw new HistoryError('NOT_A_DIRECTORY', 'history_dir must be a directory', 'validation');
  } catch (error) {
    if (error instanceof HistoryError) throw error;
    const code = error && typeof error === 'object' && 'code' in error ? String((error as { code?: unknown }).code ?? '') : '';
    if (code !== 'ENOENT') throw error;
  }
  return { root, dir, display: relativeDisplay(root, dir) };
}

function resolveSessionKey(args: JsonObject): { key: string; source: string } {
  const explicit = typeof args.session_key === 'string' ? args.session_key.trim() : '';
  if (explicit) return { key: explicit, source: 'explicit_session_key' };
  const host = hostSessionKey(args);
  if (host) return { key: host, source: 'platform_conversation_id' };
  const fallback = fallbackSessionKey(args);
  if (fallback) return { key: fallback, source: 'stable_runtime_fallback' };
  throw new HistoryError(
    'SESSION_ID_UNAVAILABLE',
    'A stable ChatGPT session identifier is required.',
    'validation',
    false,
    {
      explicit_session_key_present: false,
      host_session_key_present: false,
      fallback_session_key_present: false,
      accepted_identity_sources: ['session_key', 'openai/session', 'x-openai-session', 'stable_runtime_fallback'],
      suggestion: 'Pass an explicit session_key or use a host/runtime that provides a stable conversation identity.'
    }
  );
}

function requiredCheckpointArgument(args: JsonObject, name: string): string {
  const value = typeof args[name] === 'string' ? String(args[name]).trim() : '';
  if (value) return value;
  throw new HistoryError(
    'CHECKPOINT_TARGET_REQUIRED',
    'Pass session_key and expected_path exactly as returned by history_session_bootstrap.',
    'validation',
    false,
    { missing_argument: name }
  );
}

function sessionNotBootstrapped(): HistoryError {
  return new HistoryError('SESSION_NOT_BOOTSTRAPPED', 'The session_key has not been bootstrapped.', 'not_found');
}

function ensureCheckpointTarget(sessionKey: string, expectedPath: string, resolvedPath: string): void {
  if (expectedPath === resolvedPath) return;
  throw new HistoryError(
    'SESSION_TARGET_MISMATCH',
    'The checkpoint target does not match the session initialized by bootstrap.',
    'validation',
    false,
    { expected_path: expectedPath, resolved_path: resolvedPath, session_key: sessionKey }
  );
}

function rejectAmbiguousHistory(report: HistoryScanReport): void {
  if (!report.duplicate_session_keys.length) return;
  throw new HistoryError(
    'HISTORY_INDEX_CONFLICT',
    'Multiple history files declare the same session_key.',
    'validation',
    false,
    { duplicate_session_keys: report.duplicate_session_keys }
  );
}

async function loadIndexedDocument(root: string, historyDir: string, sessionKey: string, entry: HistoryIndexEntry): Promise<HistoryDocument> {
  const document = await readHistoryDocument(root, historyDir, entry);
  if (document.session_key !== sessionKey) {
    throw new HistoryError(
      'HISTORY_INDEX_STALE',
      'History index session_key does not match the indexed Markdown file.',
      'validation',
      true,
      { session_key: sessionKey, path: entry.path, document_session_key: document.session_key ?? null }
    );
  }
  return document;
}

function nowTimestamp(): string {
  return `unix:${Math.floor(Date.now() / 1_000)}`;
}

function buildInheritedSummary(documents: readonly HistoryDocument[], externallyOmitted: number): string {
  const entries: string[] = [];
  let used = 0;
  let omitted = externallyOmitted;
  for (const document of [...documents].reverse()) {
    const compact = truncateChars(historySummary(document.content), MAX_SESSION_SUMMARY_CHARS);
    const entry = `### 会话 ${document.number}（${document.path}）\n\n${compact}`;
    const length = Array.from(entry).length;
    if (used + length > MAX_INHERITED_SUMMARY_CHARS) {
      omitted += 1;
      continue;
    }
    used += length;
    entries.push(entry);
  }
  entries.reverse();
  if (omitted > 0) entries.unshift(`> 另有 ${omitted} 个较早会话未展开，可通过 all_history_summary 读取。`);
  return entries.join('\n\n');
}

function historyDigest(documents: readonly HistoryDocument[]): { digest: string; bytes: number } {
  const hash = createHash('sha256');
  let bytes = 0;
  for (const document of documents) {
    const number = Buffer.alloc(8);
    number.writeBigUInt64LE(BigInt(document.number));
    hash.update(number);
    const content = Buffer.from(document.content);
    hash.update(content);
    bytes += content.length;
  }
  return { digest: hash.digest('hex'), bytes };
}

export async function bootstrapHistory(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const resolvedSession = resolveSessionKey(args);
  const sessionKey = resolvedSession.key;
  const hostMismatch = hostSessionKey(args) !== undefined && hostSessionKey(args) !== sessionKey;
  const fallbackMismatch = fallbackSessionKey(args) !== undefined && fallbackSessionKey(args) !== sessionKey;
  const location = await historyLocation(ctx, key, args);
  await mkdir(location.dir, { recursive: true });
  const historyLock = await acquireHistoryLock(location.dir);
  try {
    const warnings: string[] = [];
    if (hostMismatch) warnings.push('宿主会话标识与显式 session_key 不一致，已使用显式 session_key 保持会话连续。');
    if (fallbackMismatch) warnings.push('运行时回退会话标识与显式 session_key 不一致，已使用显式 session_key 保持会话连续。');
    try { await readFile(path.join(location.dir, 'README.md'), 'utf8'); }
    catch (error) {
      const code = error && typeof error === 'object' && 'code' in error ? String((error as { code?: unknown }).code ?? '') : '';
      if (code === 'ENOENT') warnings.push('docs/history-session/README.md 不存在。');
      else throw new HistoryError('HISTORY_READ_FAILED', String(error), 'filesystem', true, { path: 'docs/history-session/README.md' });
    }

    let index: HistoryIndex;
    let sequenceValid = true;
    let indexRebuilt = false;
    let historyReadMode = 'indexed_recent_summaries_plus_latest_bounded';
    try {
      const existing = await readHistoryIndex(location.dir);
      if (existing) index = existing;
      else {
        warnings.push('历史索引缺失，已根据 Markdown 重建。');
        const report = await scanHistory(location.root, location.dir);
        rejectAmbiguousHistory(report);
        if (report.missing_numbers.length) {
          throw new HistoryError(
            'HISTORY_SEQUENCE_CONFLICT',
            'History numbering contains gaps; run history_session_validate before creating a session.',
            'validation',
            true,
            { missing_numbers: report.missing_numbers }
          );
        }
        sequenceValid = historySequenceValid(report);
        index = rebuildHistoryIndex(report);
        indexRebuilt = true;
        historyReadMode = 'scan_rebuild_recent_summaries_plus_latest_bounded';
      }
    } catch (error) {
      if (error instanceof HistoryError && error.code !== 'HISTORY_INDEX_INVALID') throw error;
      warnings.push('历史索引损坏，已根据 Markdown 重建。');
      const report = await scanHistory(location.root, location.dir);
      rejectAmbiguousHistory(report);
      if (report.missing_numbers.length) {
        throw new HistoryError(
          'HISTORY_SEQUENCE_CONFLICT',
          'History numbering contains gaps; run history_session_validate before creating a session.',
          'validation',
          true,
          { missing_numbers: report.missing_numbers }
        );
      }
      sequenceValid = historySequenceValid(report);
      index = rebuildHistoryIndex(report);
      indexRebuilt = true;
      historyReadMode = 'scan_rebuild_recent_summaries_plus_latest_bounded';
    }

    const prior = Object.entries(index.sessions)
      .filter(([candidate]) => candidate !== sessionKey)
      .sort(([, left], [, right]) => left.number - right.number);
    const historyCount = prior.length;
    const historyNumbersOmittedCount = Math.max(0, historyCount - HISTORY_NUMBER_WINDOW);
    const historyNumbers = prior.slice(-HISTORY_NUMBER_WINDOW).map(([, entry]) => entry.number);
    const historyOmittedCount = Math.max(0, historyCount - HISTORY_SUMMARY_WINDOW);
    const priorDocuments = await Promise.all(prior.slice(-HISTORY_SUMMARY_WINDOW)
      .map(([priorKey, entry]) => loadIndexedDocument(location.root, location.dir, priorKey, entry)));

    const existingEntry = index.sessions[sessionKey];
    let currentNumber: number;
    let currentPath: string;
    let currentContent: string;
    let created: boolean;
    if (existingEntry) {
      const document = await loadIndexedDocument(location.root, location.dir, sessionKey, existingEntry);
      currentNumber = document.number;
      currentPath = document.path;
      currentContent = document.content;
      created = false;
    } else {
      if (args.create_if_missing === false) {
        throw new HistoryError(
          'SESSION_NOT_BOOTSTRAPPED',
          'No history mapping exists for this session_key.',
          'not_found',
          false,
          { session_key_source: resolvedSession.source }
        );
      }
      currentNumber = index.latest_number + 1;
      currentPath = `${location.display}/${currentNumber}.md`;
      const timestamp = nowTimestamp();
      const title = typeof args.title === 'string' ? args.title : '开发会话';
      const inherited = buildInheritedSummary(priorDocuments, historyOmittedCount);
      currentContent = attachInheritedSummary(
        renderDocument(currentNumber, title, sessionKey, timestamp, timestamp, 'active', []),
        inherited
      );
      await writeHistoryMarkdown(path.join(location.dir, `${currentNumber}.md`), currentContent);
      index.latest_number = currentNumber;
      index.sessions[sessionKey] = {
        number: currentNumber, path: currentPath, created_at: timestamp, updated_at: timestamp
      };
      await writeHistoryIndex(location.dir, index);
      created = true;
    }
    if (indexRebuilt && !created) await writeHistoryIndex(location.dir, index);

    const sessionSummaries = priorDocuments.map(document => ({
      number: document.number,
      path: document.path,
      summary: truncateChars(historySummary(document.content), MAX_SESSION_SUMMARY_CHARS)
    }));
    const allHistorySummary = truncateChars(sessionSummaries.map(summary =>
      `会话 ${summary.number}（${summary.path}）：${summary.summary}`).join('\n'), MAX_ALL_HISTORY_SUMMARY_CHARS);
    const latest = priorDocuments.at(-1);
    const latestHandoffTruncated = latest ? Array.from(latest.content).length > MAX_LATEST_HANDOFF_CHARS : false;
    const latestHandoff = latest ? truncateChars(latest.content, MAX_LATEST_HANDOFF_CHARS) : null;
    const digest = historyDigest(priorDocuments);

    return {
      ok: true,
      is_new_session: created,
      session_key: sessionKey,
      session_key_source: resolvedSession.source,
      host_session_key_mismatch: hostMismatch,
      fallback_session_key_mismatch: fallbackMismatch,
      history_numbers: historyNumbers,
      history_numbers_omitted_count: historyNumbersOmittedCount,
      history_number_window: HISTORY_NUMBER_WINDOW,
      history_count: historyCount,
      history_loaded_count: priorDocuments.length,
      history_omitted_count: historyOmittedCount,
      history_summary_window: HISTORY_SUMMARY_WINDOW,
      latest_completed_number: latest?.number ?? null,
      latest_completed_path: latest?.path ?? null,
      current_number: currentNumber,
      current_path: currentPath,
      created,
      resumed: !created,
      sequence_valid: sequenceValid,
      all_history_summary: allHistorySummary,
      inherited_summary: inheritedSummary(currentContent) ?? null,
      session_summaries: sessionSummaries,
      latest_handoff: latestHandoff,
      latest_handoff_truncated: latestHandoffTruncated,
      payload_bounded: historyOmittedCount > 0 || latestHandoffTruncated,
      history_read_mode: historyReadMode,
      history_lock_wait_ms: historyLock.waitMs,
      total_history_bytes: digest.bytes,
      loaded_history_bytes: digest.bytes,
      full_history_included: false,
      history_digest: digest.digest,
      persistence_mode: 'model_mediated_tool_calls',
      assistant_instructions: 'Read all_history_summary, latest_handoff, and inherited_summary before continuing the project. Preserve the session_key and current_path returned by bootstrap, then pass them unchanged as session_key and expected_path to every history_session_checkpoint call. After completing each user-requested task, call history_session_checkpoint before the final response. Only state that progress was saved after checkpoint returns ok=true with the same session_key and path.',
      required_next_actions: [
        'read_all_history_summary', 'read_latest_handoff', 'verify_workspace_state',
        'execute_user_task', 'checkpoint_after_each_completed_task'
      ],
      checkpoint_policy: {
        tool: 'history_session_checkpoint', session_key: sessionKey, expected_path: currentPath,
        stable_target_required: true, required_before_final_response: true,
        applies_after_bootstrap: true, automatic_background_persistence: false
      },
      warnings
    };
  } finally {
    await historyLock.release();
  }
}

export async function checkpointHistory(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const sessionKey = requiredCheckpointArgument(args, 'session_key');
  const expectedPath = requiredCheckpointArgument(args, 'expected_path').replaceAll('\\', '/');
  const hostMismatch = hostSessionKey(args) !== undefined && hostSessionKey(args) !== sessionKey;
  const fallbackMismatch = fallbackSessionKey(args) !== undefined && fallbackSessionKey(args) !== sessionKey;
  const location = await historyLocation(ctx, key, args);
  try {
    const info = await stat(location.dir);
    if (!info.isDirectory()) throw sessionNotBootstrapped();
  } catch (error) {
    if (error instanceof HistoryError) throw error;
    const code = error && typeof error === 'object' && 'code' in error ? String((error as { code?: unknown }).code ?? '') : '';
    if (code === 'ENOENT') throw sessionNotBootstrapped();
    throw error;
  }
  try {
    const index = await readHistoryIndex(location.dir);
    const entry = index?.sessions[sessionKey];
    if (index && !entry) throw sessionNotBootstrapped();
    if (entry) ensureCheckpointTarget(sessionKey, expectedPath, entry.path);
  } catch (error) {
    if (error instanceof HistoryError && error.code !== 'HISTORY_INDEX_INVALID') throw error;
  }

  const historyLock = await acquireHistoryLock(location.dir);
  try {
    let index: HistoryIndex;
    let document: HistoryDocument;
    let historyReadMode: string;
    try {
      const indexed = await readHistoryIndex(location.dir);
      if (!indexed) throw new HistoryError('HISTORY_INDEX_INVALID', 'History index is missing.');
      const entry = indexed.sessions[sessionKey];
      if (!entry) throw sessionNotBootstrapped();
      ensureCheckpointTarget(sessionKey, expectedPath, entry.path);
      document = await loadIndexedDocument(location.root, location.dir, sessionKey, entry);
      index = indexed;
      historyReadMode = 'index_direct';
    } catch (error) {
      if (error instanceof HistoryError && !['HISTORY_INDEX_INVALID'].includes(error.code)) throw error;
      const report = await scanHistory(location.root, location.dir);
      rejectAmbiguousHistory(report);
      const matched = report.documents.find(item => item.session_key === sessionKey);
      if (!matched) throw sessionNotBootstrapped();
      ensureCheckpointTarget(sessionKey, expectedPath, matched.path);
      document = matched;
      index = rebuildHistoryIndex(report);
      historyReadMode = 'scan_rebuild';
    }

    const timestamp = nowTimestamp();
    const built = checkpointFromArgs(args, timestamp);
    const record = built.record;
    const redacted = ctx.config.securityPolicy.redactHistory ? redactCheckpointRecord(record) : false;
    const records = parseCheckpointRecords(document.content);
    const existing = records.find(item => item.turn_id === record.turn_id);
    let duplicateIgnored = false;
    let updated = false;
    if (existing) {
      if (!built.timestampWasExplicit) record.timestamp = existing.timestamp;
      const indexOfExisting = records.indexOf(existing);
      if (JSON.stringify(existing) === JSON.stringify(record)) duplicateIgnored = true;
      else {
        records[indexOfExisting] = record;
        updated = true;
      }
    } else {
      records.push(record);
      updated = true;
    }

    const finalContent = duplicateIgnored ? document.content : attachInheritedSummary(
      renderDocument(
        document.number,
        documentTitle(document.content, document.number),
        sessionKey,
        document.created_at ?? timestamp,
        record.timestamp,
        'active',
        records
      ),
      inheritedSummary(document.content) ?? ''
    );
    if (!duplicateIgnored) await writeHistoryMarkdown(path.join(location.dir, `${document.number}.md`), finalContent);
    index.latest_number = Math.max(index.latest_number, document.number);
    const entry = index.sessions[sessionKey] ?? {
      number: document.number,
      path: document.path,
      created_at: document.created_at ?? timestamp,
      updated_at: record.timestamp
    };
    entry.number = document.number;
    entry.path = document.path;
    if (!entry.created_at) entry.created_at = document.created_at ?? timestamp;
    entry.updated_at = record.timestamp;
    index.sessions[sessionKey] = entry;
    await writeHistoryIndex(location.dir, index);

    const warnings: string[] = [];
    if (redacted) warnings.push('检测到疑似敏感信息，归档内容已脱敏。');
    if (hostMismatch) warnings.push('宿主会话标识已变化；本次仍使用 bootstrap 返回的稳定目标，未切换历史文件。');
    return {
      ok: true,
      session_number: document.number,
      path: document.path,
      session_key: sessionKey,
      expected_path: expectedPath,
      host_session_key_mismatch: hostMismatch,
      fallback_session_key_mismatch: fallbackMismatch,
      turn_id: record.turn_id,
      created: false,
      updated,
      duplicate_ignored: duplicateIgnored,
      content_hash: historySha256(Buffer.from(finalContent)),
      history_read_mode: historyReadMode,
      history_lock_wait_ms: historyLock.waitMs,
      warnings
    };
  } finally {
    await historyLock.release();
  }
}

export async function validateHistory(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const location = await historyLocation(ctx, key, args);
  const repair = args.repair === true;
  if (repair) await mkdir(location.dir, { recursive: true });
  let indexStatus = 'missing';
  try { indexStatus = (await readHistoryIndex(location.dir)) ? 'valid' : 'missing'; }
  catch { indexStatus = 'invalid'; }
  const report = await scanHistory(location.root, location.dir);
  const warnings: string[] = [];
  if (report.duplicate_session_keys.length) warnings.push('存在重复 session_key，相关映射未写入索引。');
  let historyLockWaitMs = 0;
  let repaired = false;
  if (repair) {
    const historyLock = await acquireHistoryLock(location.dir);
    historyLockWaitMs = historyLock.waitMs;
    try {
      const lockedReport = await scanHistory(location.root, location.dir);
      await writeHistoryIndex(location.dir, rebuildHistoryIndex(lockedReport));
      repaired = true;
    } finally {
      await historyLock.release();
    }
  }
  const latestNumber = latestHistoryNumber(report);
  const latestPath = latestNumber === undefined ? null : report.documents.find(document => document.number === latestNumber)?.path ?? null;
  return {
    ok: true,
    sequence_valid: historySequenceValid(report),
    numbers: report.numbers,
    missing_numbers: report.missing_numbers,
    duplicate_session_keys: report.duplicate_session_keys,
    invalid_files: report.invalid_files,
    empty_files: report.empty_files,
    latest_number: latestNumber ?? null,
    latest_path: latestPath,
    index_status: indexStatus,
    repaired,
    history_lock_wait_ms: historyLockWaitMs,
    warnings
  };
}

export { HistoryError } from './historyModel.js';
