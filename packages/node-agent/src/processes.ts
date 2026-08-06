import { createHash, randomUUID } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import path from 'node:path';
import type { FolderRuntime, JsonObject, OperationRecord, ProcessOutputEvent, ProcessSession, ToolContext } from './types.js';
import { relativeInside, resolveExistingDirectory, rootAndCwd } from './workspace.js';
import { sleep } from './runtime.js';
import { argumentsReferenceSensitiveSource, processRedactionWarning, REDACTED } from './redaction.js';
import { resolveCommandSpec } from './policy.js';
import { classifyCommandKind } from './toolUsage.js';
import { operationResultSummary } from './operationSummary.js';
import { parseWslUncPath, wslInvocationForPath } from './wsl.js';
import { allFolderRuntimes, currentFolderRuntime, runtimeForFolderId } from './folderRuntime.js';
import {
  ProcessStartupError, processStartupController, startupDiagnosticsJson,
  WINDOWS_DLL_INIT_FAILED_SIGNED
} from './processStartup.js';

const SESSION_EVENT_BYTES = 1_048_576;
export const MAX_RETAINED_FINALIZED_SESSIONS = 128;
export const FINALIZED_SESSION_RETENTION_MS = 900_000;
export const DETACHED_SESSION_GRACE_MS = 90_000;
export const AUTO_DEDUPE_COMPLETED_GRACE_MS = 30_000;

interface CommandSpec { program: string; argv: string[]; display: string; shell: boolean }
interface ProcessViewOptions {
  cursor?: number;
  max_output_bytes?: number;
  output_mode?: string;
  tail_lines?: number;
  request_timed_out?: boolean;
  deduplicated?: boolean;
  attached_to_session_id?: string | null;
  operation_lock_wait_ms?: number;
}

interface StartedProcess {
  session: ProcessSession;
  deduplicated: boolean;
  attachedToSessionId: string | null;
  operationLockWaitMs: number;
}

export class ProcessToolError extends Error {
  readonly code: string;
  readonly category: string;
  readonly retryable: boolean;
  readonly details: JsonObject;

  constructor(code: string, message: string, category = 'runtime', retryable = false, details: JsonObject = {}) {
    super(message);
    this.name = 'ProcessToolError';
    this.code = code;
    this.category = category;
    this.retryable = retryable;
    this.details = details;
  }
}

export class ProcessRequestLifecycle {
  #session?: ProcessSession;
  #aborted = false;
  #completed = false;
  #abortController = new AbortController();

  constructor(private readonly ctx: ToolContext, private readonly graceMs = DETACHED_SESSION_GRACE_MS) {}

  attach(session: ProcessSession): void {
    this.#session = session;
    if (this.#aborted && !this.#completed) markSessionDetached(this.ctx, session, this.graceMs);
  }

  abort(): void {
    if (this.#completed || this.#aborted) return;
    this.#aborted = true;
    this.#abortController.abort(new Error('request aborted'));
    if (this.#session) markSessionDetached(this.ctx, this.#session, this.graceMs);
  }

  complete(): void {
    if (this.#completed) return;
    this.#completed = true;
    if (this.#session && !this.#aborted) touchSessionAttachment(this.#session);
  }

  get aborted(): boolean { return this.#aborted; }
  get signal(): AbortSignal { return this.#abortController.signal; }
}

function boundedInteger(value: unknown, fallback: number, minimum: number, maximum: number): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? Math.max(minimum, Math.min(maximum, Math.trunc(parsed))) : fallback;
}

function commandEnvironment(args: JsonObject): NodeJS.ProcessEnv {
  const environment: NodeJS.ProcessEnv = { ...process.env };
  for (const name of Array.isArray(args.remove_env) ? args.remove_env.map(String) : []) delete environment[name];
  for (const [name, value] of Object.entries((args.env as Record<string, unknown> | undefined) ?? {})) environment[name] = String(value);
  return environment;
}

function explicitEnvironment(args: JsonObject): Array<[string, string]> {
  return Object.entries((args.env as Record<string, unknown> | undefined) ?? {})
    .map(([name, value]) => [name, String(value)]);
}

function removedEnvironment(args: JsonObject): string[] {
  return Array.isArray(args.remove_env) ? args.remove_env.map(String) : [];
}

function startupToolError(error: unknown): ProcessToolError {
  if (!(error instanceof ProcessStartupError)) {
    return new ProcessToolError(
      'COMMAND_SPAWN_FAILED',
      `Failed to start command: ${error instanceof Error ? error.message : String(error)}`,
      'runtime',
      true,
      {
        termination_reason: 'spawn_failed',
        recoverable: true,
        suggestion: '检查命令路径、权限和运行时环境后重试'
      }
    );
  }
  const startup = startupDiagnosticsJson(error.diagnostics);
  if (error.kind === 'loader_initialization') {
    return new ProcessToolError(
      'COMMAND_START_TRANSIENT_FAILURE',
      'Windows could not initialize the child process after controlled retries.',
      'runtime',
      true,
      {
        message: error.message,
        termination_reason: 'loader_initialization_failed',
        recoverable: true,
        process_exit_code: error.exitCode ?? WINDOWS_DLL_INIT_FAILED_SIGNED,
        ntstatus: '0xc0000142',
        startup
      }
    );
  }
  if (error.kind === 'cancelled') {
    return new ProcessToolError(
      'COMMAND_START_CANCELLED',
      'Command startup was cancelled before a process session was retained.',
      'runtime',
      true,
      {
        termination_reason: 'request_cancelled',
        recoverable: true,
        startup
      }
    );
  }
  if (error.kind === 'timeout') {
    return new ProcessToolError(
      'COMMAND_START_TIMEOUT',
      'Command startup exhausted the configured timeout before a process session was retained.',
      'runtime',
      true,
      {
        termination_reason: 'process_timeout',
        recoverable: true,
        startup
      }
    );
  }
  return new ProcessToolError(
    'COMMAND_SPAWN_FAILED',
    `Failed to start command: ${error.message}`,
    'runtime',
    true,
    {
      termination_reason: 'spawn_failed',
      recoverable: true,
      suggestion: '检查命令路径、权限和运行时环境后重试',
      startup
    }
  );
}

async function terminateUntrackedChild(child: ChildProcessWithoutNullStreams): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32' && child.pid) {
    await new Promise<void>(resolve => {
      const killer = spawn('taskkill.exe', ['/pid', String(child.pid), '/t', '/f'], {
        windowsHide: true,
        stdio: 'ignore'
      });
      killer.once('exit', () => resolve());
      killer.once('error', () => {
        try { child.kill('SIGKILL'); } catch { /* best effort */ }
        resolve();
      });
    });
    return;
  }
  try {
    if (child.pid) process.kill(-child.pid, 'SIGKILL');
    else child.kill('SIGKILL');
  } catch {
    try { child.kill('SIGKILL'); } catch { /* best effort */ }
  }
}

function waitForReadableEnd(stream: ChildProcessWithoutNullStreams['stdout']): Promise<void> {
  if (stream.readableEnded || stream.destroyed) return Promise.resolve();
  return new Promise(resolve => {
    const done = () => {
      stream.off('end', done);
      stream.off('close', done);
      stream.off('error', done);
      resolve();
    };
    stream.once('end', done);
    stream.once('close', done);
    stream.once('error', done);
    if (stream.readableEnded || stream.destroyed) done();
  });
}

async function waitForChildStreams(child: ChildProcessWithoutNullStreams): Promise<void> {
  await Promise.all([waitForReadableEnd(child.stdout), waitForReadableEnd(child.stderr)]);
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

function fingerprint(cwd: string, spec: CommandSpec, args: JsonObject): string {
  const env = Object.fromEntries(Object.entries((args.env as Record<string, unknown> | undefined) ?? {})
    .sort(([left], [right]) => left.localeCompare(right))
    .map(([name, value]) => [name, String(value)]));
  const removeEnv = Array.isArray(args.remove_env) ? args.remove_env.map(String).sort() : [];
  const stdin = typeof args.stdin === 'string' ? args.stdin : '';
  const material = stableValue({
    cwd,
    program: spec.program,
    argv: spec.argv,
    shell: spec.shell,
    env,
    remove_env: removeEnv,
    timeout_ms: boundedInteger(args.timeout_ms, 30_000, 1, 600_000),
    tty: args.tty === true,
    stdin_sha256: createHash('sha256').update(stdin).digest('hex'),
    post_checks: Array.isArray(args.post_checks) ? args.post_checks.slice(0, 16) : [],
    resource_lock_group: String(args.lock_group ?? '').trim() || null
  });
  return createHash('sha256').update(JSON.stringify(material)).digest('hex');
}

function safeAutomaticDedup(spec: CommandSpec): boolean {
  const executable = (spec.program.split(/[\\/]/).at(-1) ?? '').toLowerCase().replace(/\.exe$/, '').replace(/\.cmd$/, '');
  if (executable !== 'cargo') return false;
  return ['check', 'test', 'build', 'fmt', 'clippy'].includes(spec.argv[0] ?? '');
}

function safeUtf8Tail(buffer: Buffer, maxBytes: number): Buffer {
  if (buffer.length <= maxBytes) return buffer;
  let start = buffer.length - maxBytes;
  while (start < buffer.length && (buffer[start] & 0xc0) === 0x80) start += 1;
  return buffer.subarray(start);
}

function safeUtf8Prefix(buffer: Buffer): Buffer {
  if (!buffer.length) return buffer;
  const decoder = new TextDecoder('utf-8', { fatal: true });
  for (let trim = 0; trim <= Math.min(3, buffer.length); trim += 1) {
    const candidate = trim === 0 ? buffer : buffer.subarray(0, buffer.length - trim);
    try { decoder.decode(candidate); return candidate; } catch { /* trim incomplete suffix */ }
  }
  return Buffer.alloc(0);
}

function retainOutput(current: string, chunk: Buffer, maxBytes: number): string {
  return safeUtf8Tail(Buffer.concat([Buffer.from(current), chunk]), maxBytes).toString('utf8');
}

function appendOutput(ctx: ToolContext, session: ProcessSession, stream: 'stdout' | 'stderr', chunk: Buffer): void {
  const offset = stream === 'stdout' ? session.stdoutBytes : session.stderrBytes;
  if (session.firstOutputAt === undefined) session.firstOutputAt = Date.now();
  const text = chunk.toString('utf8');
  session.sequence += 1;
  const outputEvent: ProcessOutputEvent = { sequence: session.sequence, stream, offset, data: text };
  session.outputEvents.push(outputEvent);
  session.outputEventBytes += chunk.length;
  while (session.outputEventBytes > SESSION_EVENT_BYTES && session.outputEvents.length) {
    const removed = session.outputEvents.shift();
    if (removed) session.outputEventBytes = Math.max(0, session.outputEventBytes - Buffer.byteLength(removed.data));
  }
  if (stream === 'stdout') {
    session.stdoutBytes += chunk.length;
    session.stdout = retainOutput(session.stdout, chunk, ctx.config.limits.maxOutputBytes);
    session.stdoutStart = Math.max(0, session.stdoutBytes - Buffer.byteLength(session.stdout));
  } else {
    session.stderrBytes += chunk.length;
    session.stderr = retainOutput(session.stderr, chunk, ctx.config.limits.maxOutputBytes);
    session.stderrStart = Math.max(0, session.stderrBytes - Buffer.byteLength(session.stderr));
  }
  session.events.emit('change');
}

function terminationReason(session: ProcessSession): string {
  return session.terminationReason ?? (session.endedAt ? 'exited' : 'running');
}

function recoverySuggestion(reason: string): string {
  switch (reason) {
    case 'process_timeout': return '读取保留输出，调整 timeout_ms 后重试';
    case 'detached_timeout': return '连接失联超过宽限时间；确认没有可恢复 session 后再使用新的 operation_id 重试';
    case 'killed': return '确认终止原因后重新执行命令';
    case 'exited': return '检查 process_exit_code、stderr 与 post_checks';
    case 'crashed': return '检查 stderr 后重试或恢复工作区';
    default: return '使用 wait_command 等待新输出或进程结束';
  }
}

function recoverable(reason: string): boolean {
  return ['process_timeout', 'killed', 'spawn_failed', 'server_restart', 'detached_timeout'].includes(reason);
}

export function processStatus(session: ProcessSession): 'running' | 'verifying' | 'exited' | 'timed_out' | 'killed' {
  if (!session.endedAt) return 'running';
  if (session.postChecksPending || !session.finalizedAt) return 'verifying';
  const reason = terminationReason(session);
  if (reason === 'process_timeout') return 'timed_out';
  if (reason === 'killed' || reason === 'detached_timeout' || reason === 'server_restart') return 'killed';
  return 'exited';
}

function capTail(value: string, maxBytes: number): { content: string; truncated: boolean } {
  const buffer = Buffer.from(value);
  if (buffer.length <= maxBytes) return { content: value, truncated: false };
  return { content: safeUtf8Tail(buffer, maxBytes).toString('utf8'), truncated: true };
}

function summarizeStream(value: string, maxBytes: number, tailLineCount: number): { content: string; truncated: boolean } {
  const collapsed: string[] = [];
  for (const line of value.trimEnd().split(/\r?\n/)) {
    if (!line.trim()) continue;
    if (collapsed.at(-1) !== line) collapsed.push(line);
  }
  return capTail(collapsed.slice(-tailLineCount).join('\n'), maxBytes);
}

function outputView(session: ProcessSession, options: ProcessViewOptions): {
  mode: string;
  stdout: string;
  stderr: string;
  events: JsonObject[];
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
  cursorExpired: boolean;
  nextCursor: number;
  hasMore: boolean;
} {
  const requestedMode = String(options.output_mode ?? 'tail');
  const mode = ['delta', 'tail', 'all', 'none', 'summary'].includes(requestedMode) ? requestedMode : 'tail';
  const cursor = boundedInteger(options.cursor, 0, 0, Number.MAX_SAFE_INTEGER);
  const maxBytes = boundedInteger(options.max_output_bytes, 65_536, 1, 1_048_576);
  const lines = boundedInteger(options.tail_lines, 100, 1, 10_000);
  const latestCursor = session.sequence;
  const oldest = session.outputEvents[0]?.sequence;
  const cursorExpired = oldest !== undefined && cursor + 1 < oldest;
  const effectiveCursor = cursorExpired ? oldest! - 1 : cursor;

  if (mode === 'none') {
    return { mode, stdout: '', stderr: '', events: [], stdoutTruncated: false, stderrTruncated: false, cursorExpired: false, nextCursor: latestCursor, hasMore: false };
  }
  if (mode === 'delta') {
    const selected: ProcessOutputEvent[] = [];
    let selectedBytes = 0;
    for (const event of session.outputEvents) {
      if (event.sequence <= effectiveCursor) continue;
      const eventBytes = Buffer.byteLength(event.data);
      if (selected.length && selectedBytes + eventBytes > maxBytes) break;
      selected.push(event);
      selectedBytes += eventBytes;
    }
    const nextCursor = selected.at(-1)?.sequence ?? Math.min(effectiveCursor, latestCursor);
    const events = selected.map(event => ({
      sequence: event.sequence,
      stream: event.stream,
      stream_offset: event.offset,
      decoded_offset: event.offset,
      offset: event.offset,
      encoding: 'utf-8',
      data: event.data
    }));
    return {
      mode,
      stdout: selected.filter(event => event.stream === 'stdout').map(event => event.data).join(''),
      stderr: selected.filter(event => event.stream === 'stderr').map(event => event.data).join(''),
      events,
      stdoutTruncated: false,
      stderrTruncated: false,
      cursorExpired,
      nextCursor,
      hasMore: nextCursor < latestCursor
    };
  }
  if (mode === 'summary') {
    const stdout = summarizeStream(session.stdout, maxBytes, lines);
    const stderr = summarizeStream(session.stderr, maxBytes, lines);
    return { mode, stdout: stdout.content, stderr: stderr.content, events: [], stdoutTruncated: stdout.truncated, stderrTruncated: stderr.truncated, cursorExpired: false, nextCursor: latestCursor, hasMore: false };
  }
  const stdout = capTail(session.stdout, maxBytes);
  const stderr = capTail(session.stderr, maxBytes);
  return { mode, stdout: stdout.content, stderr: stderr.content, events: [], stdoutTruncated: stdout.truncated, stderrTruncated: stderr.truncated, cursorExpired: false, nextCursor: latestCursor, hasMore: false };
}

export function processResult(session: ProcessSession, options: ProcessViewOptions = {}): JsonObject {
  const view = outputView(session, options);
  const status = processStatus(session);
  const reason = terminationReason(session);
  const verificationOk = session.postChecksPending ? null : (session.verificationOk ?? true);
  const executionOk = session.endedAt ? reason === 'exited' && session.exitCode === 0 : null;
  const commandOk = executionOk === null || verificationOk === null ? null : executionOk && verificationOk;
  let stdout = view.stdout;
  let stderr = view.stderr;
  let redactionCount = 0;
  const events = view.events.map(event => ({ ...event }));
  if (session.sensitiveOutput) {
    if (stdout) { stdout = REDACTED; redactionCount += 1; }
    if (stderr) { stderr = REDACTED; redactionCount += 1; }
    for (const event of events) {
      if (typeof event.data === 'string' && event.data) {
        event.data = REDACTED;
        redactionCount += 1;
      }
    }
  }
  return {
    ok: commandOk !== false,
    command_ok: commandOk,
    execution_ok: executionOk,
    verification_ok: verificationOk,
    transport_ok: true,
    session_id: session.id,
    startup: startupDiagnosticsJson(session.startupDiagnostics),
    workspace_folder_id: session.folderId,
    workspace_path: session.workspacePath,
    operation_id: session.operationId ?? null,
    command_fingerprint: session.fingerprint,
    command: session.command,
    program: session.program,
    args: session.argv,
    shell: session.shell ? 'shell' : 'none',
    cwd: session.cwd,
    interactive: session.interactive,
    stdin_open: session.stdinOpen,
    status,
    termination_reason: reason,
    recoverable: recoverable(reason),
    suggestion: recoverySuggestion(reason),
    request_timed_out: options.request_timed_out === true,
    process_timed_out: reason === 'process_timeout',
    process_still_running: !session.endedAt,
    process_id: session.child?.pid ?? null,
    exit_code: session.exitCode ?? null,
    process_exit_code: session.exitCode ?? null,
    signal: session.signal ?? null,
    output_mode: view.mode,
    stdout,
    stderr,
    stdout_bytes: session.stdoutBytes,
    stderr_bytes: session.stderrBytes,
    stdout_retained_from: session.stdoutStart,
    stderr_retained_from: session.stderrStart,
    stdout_truncated: view.stdoutTruncated,
    stderr_truncated: view.stderrTruncated,
    events,
    cursor: boundedInteger(options.cursor, 0, 0, Number.MAX_SAFE_INTEGER),
    latest_cursor: session.sequence,
    next_cursor: view.nextCursor,
    cursor_expired: view.cursorExpired,
    has_more_output: view.hasMore,
    elapsed_ms: Math.max(0, Date.now() - session.startedAt),
    first_output_ms: session.firstOutputAt === undefined ? null : Math.max(0, session.firstOutputAt - session.startedAt),
    started_ts_ms: session.startedAt,
    ended_ts_ms: session.endedAt ?? null,
    finalized_ts_ms: session.finalizedAt ?? null,
    killed: session.killed,
    process_tree_contained: process.platform !== 'win32',
    process_tree_control: process.platform === 'win32' ? 'taskkill_tree' : 'process_group',
    post_checks: session.postChecks,
    post_checks_pending: session.postChecksPending,
    resource_lock_group: session.resourceLockGroup ?? null,
    resource_lock_target: session.resourceLockTarget ?? null,
    operation_lock_wait_ms: options.operation_lock_wait_ms ?? session.operationLockWaitMs,
    resource_lock_wait_ms: session.resourceLockWaitMs,
    deduplicated: options.deduplicated === true,
    attached_to_session_id: options.attached_to_session_id ?? null,
    detached: session.detachedGeneration !== 0,
    sensitive_data_redacted: session.sensitiveOutput,
    redaction_count: redactionCount,
    warnings: session.sensitiveOutput ? [processRedactionWarning()] : [],
    output_refs: {
      stdout: `output://${session.id}/stdout`,
      stderr: `output://${session.id}/stderr`
    }
  };
}

async function recordHarnessOperationFinalization(ctx: ToolContext, session: ProcessSession): Promise<void> {
  if (!session.finalizedAt || !session.harnessOperations?.size) return;
  const recordedIds = session.harnessOperationRecordedIds ??= new Set<string>();
  for (const operation of session.harnessOperations.values()) {
    if (recordedIds.has(operation.id)) continue;
    recordedIds.add(operation.id);
    const summary = operationResultSummary(operation.tool, processResult(session, { output_mode: 'none' }));
    const completed: OperationRecord = {
      ...operation,
      kind: summary.command_ok === true ? 'completed' : 'failed',
      result_summary: summary,
      created_at: String(session.finalizedAt)
    };
    try {
      await ctx.state.addOperation(operation.workspace_id, completed);
    } catch {
      recordedIds.delete(operation.id);
    }
  }
}

export async function attachHarnessOperation(
  ctx: ToolContext,
  sessionId: string,
  operation: OperationRecord
): Promise<boolean> {
  const session = allFolderRuntimes(ctx)
    .map(runtime => runtime.sessions.get(sessionId))
    .find((candidate): candidate is ProcessSession => Boolean(candidate));
  if (!session) return false;
  const operations = session.harnessOperations ??= new Map<string, OperationRecord>();
  operations.set(operation.id, operation);
  if (session.finalizedAt) await recordHarnessOperationFinalization(ctx, session);
  return true;
}

async function finalizeSession(ctx: ToolContext, session: ProcessSession, verificationOk: boolean): Promise<void> {
  if (session.finalizedAt) return;
  session.postChecksPending = false;
  session.verificationOk = verificationOk;
  session.finalizedAt = Date.now();
  if (!session.telemetryRecorded) {
    session.telemetryRecorded = true;
    ctx.usageStore.recordAsyncSession({
      sessionId: session.id,
      commandKind: session.telemetryCommandKind,
      startedTsMs: session.startedAt,
      completedTsMs: session.finalizedAt,
      childProcessTotalMs: Math.max(0, session.finalizedAt - session.startedAt),
      firstOutputMs: session.firstOutputAt === undefined ? null : Math.max(0, session.firstOutputAt - session.startedAt),
      exitCode: session.exitCode ?? null,
      terminationReason: terminationReason(session),
      stdoutBytes: session.stdoutBytes,
      stderrBytes: session.stderrBytes
    });
  }
  await recordHarnessOperationFinalization(ctx, session);
  if (session.timeoutTimer) clearTimeout(session.timeoutTimer);
  if (session.detachedTimer) clearTimeout(session.detachedTimer);
  session.lockRelease?.();
  session.lockRelease = undefined;
  session.events.emit('change');
  session.events.emit('finalized');
}

async function runPostChecks(ctx: ToolContext, key: string, session: ProcessSession, checks: JsonObject[], cwd: string): Promise<void> {
  if (!checks.length || session.exitCode !== 0 || terminationReason(session) !== 'exited') {
    await finalizeSession(ctx, session, session.exitCode === 0 && terminationReason(session) === 'exited');
    return;
  }
  session.postChecksPending = true;
  session.events.emit('change');
  let verificationOk = true;
  for (let index = 0; index < checks.length; index += 1) {
    const check = checks[index];
    const spec = await resolveCommandSpec(ctx, key, { ...check, workdir: relativeInside(rootAndCwd(ctx, key).root, cwd) });
    const expected = Number(check.expected_exit_code ?? 0);
    const timeoutMs = boundedInteger(check.timeout_ms, 30_000, 1, 600_000);
    const maxOutput = boundedInteger(check.max_output_bytes, 16_384, 1, 1_048_576);
    const wslWorkspace = Boolean(parseWslUncPath(cwd));
    const result = await runBuffered(
      spec.program, spec.argv, cwd, undefined, timeoutMs,
      wslWorkspace ? process.env : commandEnvironment(check),
      { routeWsl: true, environment: explicitEnvironment(check), removeEnvironment: removedEnvironment(check) }
    );
    const passed = result.code === expected;
    verificationOk &&= passed;
    session.postChecks.push({
      index,
      name: String(check.name ?? `post-check-${index + 1}`),
      command: spec.display,
      expected_exit_code: expected,
      exit_code: result.code,
      ok: passed,
      stdout: capTail(result.stdout, maxOutput).content,
      stderr: capTail(result.stderr, maxOutput).content
    });
  }
  await finalizeSession(ctx, session, verificationOk);
}

function processRuntime(value: ToolContext | FolderRuntime): FolderRuntime {
  if ('folderId' in value) return value;
  const runtime = value.folderRuntimes.values().next().value;
  if (!runtime) throw new Error('WORKSPACE_FOLDER_NOT_FOUND');
  return runtime;
}

export function removeProcessSession(value: ToolContext | FolderRuntime, sessionId: string): boolean {
  const runtime = processRuntime(value);
  const session = runtime.sessions.get(sessionId);
  if (!session) return false;
  if (session.timeoutTimer) clearTimeout(session.timeoutTimer);
  if (session.detachedTimer) clearTimeout(session.detachedTimer);
  session.lockRelease?.();
  runtime.sessions.delete(sessionId);
  for (const [fingerprintValue, indexedSessionId] of runtime.operationsByFingerprint) {
    if (indexedSessionId === sessionId) runtime.operationsByFingerprint.delete(fingerprintValue);
  }
  return true;
}

export function pruneProcessSessions(value: ToolContext | FolderRuntime, now = Date.now()): void {
  const runtime = processRuntime(value);
  for (const session of [...runtime.sessions.values()]) {
    if (session.finalizedAt && now - session.finalizedAt >= FINALIZED_SESSION_RETENTION_MS) removeProcessSession(runtime, session.id);
  }
  const finalized = [...runtime.sessions.values()]
    .filter(session => session.finalizedAt !== undefined)
    .sort((left, right) => (left.finalizedAt ?? 0) - (right.finalizedAt ?? 0));
  for (const session of finalized.slice(0, Math.max(0, finalized.length - MAX_RETAINED_FINALIZED_SESSIONS))) {
    removeProcessSession(runtime, session.id);
  }
}

export function touchSessionAttachment(session: ProcessSession): void {
  session.attachmentGeneration += 1;
  session.detachedGeneration = 0;
  if (session.detachedTimer) clearTimeout(session.detachedTimer);
  session.detachedTimer = undefined;
}

export function markSessionDetached(ctx: ToolContext, session: ProcessSession, graceMs = DETACHED_SESSION_GRACE_MS): number {
  const generation = session.attachmentGeneration + 1;
  session.attachmentGeneration = generation;
  session.detachedGeneration = generation;
  if (session.detachedTimer) clearTimeout(session.detachedTimer);
  session.detachedTimer = setTimeout(() => {
    void (async () => {
      if (session.detachedGeneration !== generation || session.attachmentGeneration !== generation || session.finalizedAt) return;
      session.terminationReason = 'detached_timeout';
      if (!session.endedAt) {
        await killProcessTree(session, 'KILL', 'detached_timeout');
        await waitForSession(session, session.sequence, 5_000, 'exit');
      }
      if (!session.finalizedAt) await finalizeSession(ctx, session, false);
      pruneProcessSessions(runtimeForFolderId(ctx, session.folderId));
    })();
  }, Math.max(1, graceMs));
  session.detachedTimer.unref();
  session.events.emit('change');
  return generation;
}

export function requireProcessSession(value: ToolContext | FolderRuntime, rawSessionId: unknown, touch = true): ProcessSession {
  const runtime = processRuntime(value);
  pruneProcessSessions(runtime);
  const sessionId = String(rawSessionId ?? '').trim();
  const outputReference = /^output:\/\/([^/]+)\/(stdout|stderr)$/.exec(sessionId);
  if (outputReference) {
    throw new ProcessToolError('OUTPUT_REF_USED_AS_SESSION_ID', 'output_ref cannot be used as session_id', 'runtime', true, {
      received: sessionId,
      corrected_session_id: outputReference[1],
      suggestion: 'Use the corrected_session_id with wait_command, send_input, or kill_session.'
    });
  }
  const session = runtime.sessions.get(sessionId);
  if (!session) throw new ProcessToolError('SESSION_NOT_FOUND', `Session not found: ${sessionId}`, 'not_found', false);
  if (touch) touchSessionAttachment(session);
  return session;
}

export function findProcessOperation(value: ToolContext | FolderRuntime, operationId: string, fingerprintValue: string): { session?: ProcessSession; resolvedBy?: string } {
  const runtime = processRuntime(value);
  pruneProcessSessions(runtime);
  if (operationId) {
    const session = [...runtime.sessions.values()].find(candidate => candidate.operationId === operationId);
    if (session) return { session, resolvedBy: 'operation_id' };
  }
  if (fingerprintValue) {
    const sessionId = runtime.operationsByFingerprint.get(fingerprintValue);
    const session = sessionId ? runtime.sessions.get(sessionId) : undefined;
    if (session) return { session, resolvedBy: 'fingerprint' };
  }
  return {};
}

export async function startProcess(
  ctx: ToolContext,
  key: string,
  args: JsonObject,
  signal?: AbortSignal
): Promise<StartedProcess> {
  const runtime = currentFolderRuntime(ctx, key);
  pruneProcessSessions(runtime);
  const { folder, root, cwd: defaultCwd } = rootAndCwd(ctx, key);
  const resolvedCwd = await resolveExistingDirectory(
    root,
    String(args.workdir ?? args.cwd ?? relativeInside(root, defaultCwd)),
    'Command workdir must be a directory'
  );
  const cwd = resolvedCwd.full;
  const spec = await resolveCommandSpec(ctx, key, args);
  const commandFingerprint = fingerprint(cwd, spec, args);
  const explicitOperationId = String(args.operation_id ?? '').trim();
  const deduplicate = Boolean(explicitOperationId) || args.deduplicate === true || (args.deduplicate === undefined && safeAutomaticDedup(spec));
  const operationId = explicitOperationId || (deduplicate ? `auto:${commandFingerprint.slice(0, 32)}` : undefined);
  const operationLockStarted = Date.now();
  const operationRelease = operationId ? await runtime.admission.locks.acquire([`exec-operation:${operationId}`]) : undefined;
  const operationLockWaitMs = Date.now() - operationLockStarted;

  try {
    if (operationId) {
      const existing = [...runtime.sessions.values()].find(session => session.operationId === operationId);
      if (existing) {
        if (explicitOperationId && existing.fingerprint !== commandFingerprint) {
          throw new ProcessToolError('OPERATION_ID_CONFLICT', `operation_id already belongs to a different command: ${operationId}`, 'conflict', false, {
            operation_id: operationId,
            existing_session_id: existing.id,
            existing_fingerprint: existing.fingerprint,
            requested_fingerprint: commandFingerprint
          });
        }
        const reusable = explicitOperationId || !existing.finalizedAt || Date.now() - existing.finalizedAt <= AUTO_DEDUPE_COMPLETED_GRACE_MS;
        if (reusable) {
          touchSessionAttachment(existing);
          return { session: existing, deduplicated: true, attachedToSessionId: existing.id, operationLockWaitMs };
        }
        removeProcessSession(runtime, existing.id);
      }
    }
    if (deduplicate && !explicitOperationId) {
      const existingId = runtime.operationsByFingerprint.get(commandFingerprint);
      const existing = existingId ? runtime.sessions.get(existingId) : undefined;
      if (existing) {
        const reusable = !existing.finalizedAt || Date.now() - existing.finalizedAt <= AUTO_DEDUPE_COMPLETED_GRACE_MS;
        if (reusable) {
          touchSessionAttachment(existing);
          return { session: existing, deduplicated: true, attachedToSessionId: existing.id, operationLockWaitMs };
        }
        removeProcessSession(runtime, existing.id);
      }
    }
    const activeSessions = [...runtime.sessions.values()].filter(session => !session.finalizedAt).length;
    if (activeSessions >= ctx.config.limits.activeSessionLimit) {
      throw new ProcessToolError('SESSION_LIMIT_REACHED', `Active command session limit reached (${ctx.config.limits.activeSessionLimit}).`, 'runtime', true, {
        stage: 'session_admission',
        active_session_limit: ctx.config.limits.activeSessionLimit,
        active_session_slots_available: 0,
        suggestion: '等待现有命令完成，或使用 kill_session 终止不再需要的长任务'
      });
    }

    const lockGroup = String(args.lock_group ?? '').trim();
    const resourceLockStarted = Date.now();
    const lockRelease = lockGroup ? await runtime.admission.locks.acquire([`process:${lockGroup}`]) : undefined;
    const resourceLockWaitMs = Date.now() - resourceLockStarted;
    const id = randomUUID();
    const timeoutMs = boundedInteger(args.timeout_ms, 30_000, 1, 600_000);
    const processStartedAt = Date.now();
    const deadlineMs = processStartedAt + timeoutMs;
    const wsl = wslInvocationForPath(cwd, spec.program, spec.argv, explicitEnvironment(args), removedEnvironment(args));
    const launchProgram = wsl?.program ?? spec.program;
    const launchArgs = wsl?.args ?? spec.argv;
    const startupOutput = new WeakMap<ChildProcessWithoutNullStreams, {
      stdout: Buffer[];
      stderr: Buffer[];
      onStdout: (chunk: Buffer) => void;
      onStderr: (chunk: Buffer) => void;
    }>();
    let controlled;
    try {
      controlled = await processStartupController.start(() => {
        const launched = spawn(launchProgram, launchArgs, {
          cwd: wsl ? undefined : cwd,
          env: wsl ? process.env : commandEnvironment(args),
          windowsHide: true,
          detached: process.platform !== 'win32',
          shell: spec.shell,
          stdio: 'pipe'
        });
        const captured = {
          stdout: [] as Buffer[],
          stderr: [] as Buffer[],
          onStdout: (chunk: Buffer) => captured.stdout.push(Buffer.from(chunk)),
          onStderr: (chunk: Buffer) => captured.stderr.push(Buffer.from(chunk))
        };
        launched.stdout.on('data', captured.onStdout);
        launched.stderr.on('data', captured.onStderr);
        startupOutput.set(launched, captured);
        return launched;
      }, {
        signal,
        // Match Rust retained-session semantics: startup admission and the
        // Windows loader probe may consume the command budget, but they must
        // not fail before a session is registered. The original deadline is
        // enforced immediately by the session timeout timer after retention.
        terminate: terminateUntrackedChild
      });
    } catch (error) {
      lockRelease?.();
      throw startupToolError(error);
    }
    const child = controlled.child;
    const interactive = args.tty === true;
    const session: ProcessSession = {
      id,
      folderId: folder.id,
      workspacePath: folder.path,
      operationId,
      fingerprint: commandFingerprint,
      command: spec.display,
      program: spec.program,
      argv: [...spec.argv],
      shell: spec.shell,
      cwd,
      startupDiagnostics: controlled.diagnostics,
      startedAt: processStartedAt,
      stdout: '',
      stderr: '',
      stdoutBytes: 0,
      stderrBytes: 0,
      stdoutStart: 0,
      stderrStart: 0,
      sequence: 0,
      outputEvents: [],
      outputEventBytes: 0,
      child,
      interactive,
      stdinOpen: true,
      timedOut: false,
      killed: false,
      telemetryCommandKind: classifyCommandKind(args),
      sensitiveOutput: argumentsReferenceSensitiveSource(args),
      postChecks: [],
      postChecksPending: false,
      resourceLockGroup: lockGroup || undefined,
      resourceLockTarget: lockGroup ? cwd : undefined,
      operationLockWaitMs,
      resourceLockWaitMs,
      lockRelease,
      attachmentGeneration: 1,
      detachedGeneration: 0,
      events: new EventEmitter()
    };
    runtime.sessions.set(id, session);
    runtime.operationsByFingerprint.set(commandFingerprint, id);

    child.stdout.on('data', (chunk: Buffer) => appendOutput(ctx, session, 'stdout', chunk));
    child.stderr.on('data', (chunk: Buffer) => appendOutput(ctx, session, 'stderr', chunk));
    const capturedStartupOutput = startupOutput.get(child);
    if (capturedStartupOutput) {
      child.stdout.off('data', capturedStartupOutput.onStdout);
      child.stderr.off('data', capturedStartupOutput.onStderr);
      for (const chunk of capturedStartupOutput.stdout) appendOutput(ctx, session, 'stdout', chunk);
      for (const chunk of capturedStartupOutput.stderr) appendOutput(ctx, session, 'stderr', chunk);
      startupOutput.delete(child);
    }
    const onChildError = (error: Error) => {
      if (!session.terminationReason) session.terminationReason = 'crashed';
      appendOutput(ctx, session, 'stderr', Buffer.from(`${error.message}\n`));
    };
    child.on('error', onChildError);
    let closed = false;
    const closeChild = (code: number | null, childSignal: NodeJS.Signals | null) => {
      if (closed) return;
      closed = true;
      child.off('error', onChildError);
      session.exitCode = code;
      session.signal = childSignal;
      session.endedAt = Date.now();
      session.stdinOpen = false;
      session.child = undefined;
      session.terminationReason ??= 'exited';
      session.events.emit('change');
      session.events.emit('exit');
      const checks = Array.isArray(args.post_checks) ? args.post_checks as JsonObject[] : [];
      void runPostChecks(ctx, key, session, checks.slice(0, 16), cwd).catch(async error => {
        session.postChecks.push({ ok: false, error: error instanceof Error ? error.message : String(error) });
        await finalizeSession(ctx, session, false);
      });
    };
    child.once('close', closeChild);
    for (const startupError of controlled.handoff()) onChildError(startupError);

    const stdin = typeof args.stdin === 'string' ? args.stdin : '';
    if (controlled.earlyExit) {
      session.stdinOpen = false;
      try { child.stdin.end(); } catch { /* child already exited */ }
    } else if (!interactive && stdin) {
      child.stdin.write(stdin);
      child.stdin.end();
      session.stdinOpen = false;
    }
    if (controlled.earlyExit) {
      await waitForChildStreams(child);
      closeChild(child.exitCode, child.signalCode);
    } else {
      queueMicrotask(() => {
        if (closed || (child.exitCode === null && child.signalCode === null)) return;
        void waitForChildStreams(child).then(() => closeChild(child.exitCode, child.signalCode));
      });
    }

    if (!controlled.earlyExit) {
      const remainingTimeoutMs = Math.max(1, deadlineMs - Date.now());
      session.timeoutTimer = setTimeout(() => {
        if (!session.endedAt) {
          session.timedOut = true;
          session.terminationReason = 'process_timeout';
          void killProcessTree(session, 'KILL', 'process_timeout');
        }
      }, remainingTimeoutMs);
      session.timeoutTimer.unref();
      session.events.once('exit', () => { if (session.timeoutTimer) clearTimeout(session.timeoutTimer); });
    }
    return { session, deduplicated: false, attachedToSessionId: null, operationLockWaitMs };
  } finally {
    operationRelease?.();
  }
}

function retainedNextActions(session: ProcessSession): JsonObject[] {
  if (session.finalizedAt) return [];
  return [{
    tool: 'wait_command',
    arguments: {
      session_id: session.id,
      cursor: session.sequence,
      timeout_ms: 30_000,
      heartbeat_ms: 10_000,
      until: 'finalized',
      output_mode: 'delta'
    }
  }];
}

export async function startAndYield(ctx: ToolContext, key: string, args: JsonObject, lifecycle?: ProcessRequestLifecycle): Promise<JsonObject> {
  const started = await startProcess(ctx, key, args, lifecycle?.signal);
  const session = started.session;
  lifecycle?.attach(session);
  const yieldMs = boundedInteger(args.yield_time_ms, 1_000, 0, 30_000);
  if (!session.endedAt && yieldMs > 0 && !session.interactive) await waitForSession(session, session.sequence, yieldMs, 'output_or_exit');
  const result = processResult(session, {
    ...args,
    deduplicated: started.deduplicated,
    attached_to_session_id: started.attachedToSessionId,
    operation_lock_wait_ms: started.operationLockWaitMs
  });
  const nextActions = retainedNextActions(session);
  if (nextActions.length) result.next_actions = nextActions;
  return result;
}

export async function waitForSession(
  session: ProcessSession,
  cursor: number,
  timeoutMs: number,
  until: string,
  heartbeatMs = 0
): Promise<{ heartbeat: boolean; changed: boolean; actualWaitMs: number; effectiveWaitMs: number }> {
  const startedAt = Date.now();
  const satisfied = () => {
    if (until === 'finalized') return Boolean(session.finalizedAt);
    if (until === 'exit') return Boolean(session.endedAt);
    return Boolean(session.endedAt) || session.sequence > cursor || (cursor === 0 && session.firstOutputAt !== undefined);
  };
  const boundedTimeout = boundedInteger(timeoutMs, 30_000, 0, 120_000);
  const boundedHeartbeat = boundedInteger(heartbeatMs, 0, 0, 30_000);
  const effectiveWaitMs = boundedHeartbeat > 0 ? Math.min(boundedTimeout, Math.max(1_000, boundedHeartbeat)) : boundedTimeout;
  if (satisfied() || effectiveWaitMs <= 0) return { heartbeat: false, changed: satisfied(), actualWaitMs: 0, effectiveWaitMs };
  return new Promise(resolve => {
    let changed = false;
    const done = () => {
      cleanup();
      resolve({
        heartbeat: !changed && boundedHeartbeat > 0 && !satisfied(),
        changed,
        actualWaitMs: Date.now() - startedAt,
        effectiveWaitMs
      });
    };
    const onChange = () => { if (satisfied()) { changed = true; done(); } };
    const timer = setTimeout(done, effectiveWaitMs);
    const cleanup = () => { clearTimeout(timer); session.events.off('change', onChange); };
    session.events.on('change', onChange);
    if (satisfied()) { changed = true; done(); }
  });
}

export async function killProcessTree(session: ProcessSession, signal: 'TERM' | 'KILL' | 'INT' = 'TERM', reason = 'killed'): Promise<void> {
  const child = session.child;
  if (!child?.pid || session.endedAt) return;
  session.terminationReason = reason;
  session.killed = reason === 'killed';
  session.events.emit('change');
  if (process.platform === 'win32') {
    const args = ['/pid', String(child.pid), '/t', ...(signal === 'KILL' ? ['/f'] : [])];
    await new Promise<void>(resolve => {
      const killer = spawn('taskkill.exe', args, { windowsHide: true, stdio: 'ignore' });
      killer.once('exit', () => resolve());
      killer.once('error', () => { child.kill(signal === 'INT' ? 'SIGINT' : signal === 'KILL' ? 'SIGKILL' : 'SIGTERM'); resolve(); });
    });
  } else {
    try { process.kill(-child.pid, signal === 'INT' ? 'SIGINT' : signal === 'KILL' ? 'SIGKILL' : 'SIGTERM'); }
    catch { child.kill(signal === 'INT' ? 'SIGINT' : signal === 'KILL' ? 'SIGKILL' : 'SIGTERM'); }
  }
}

export async function disposeProcessSessions(ctx: ToolContext): Promise<void> {
  const sessions = allFolderRuntimes(ctx).flatMap(runtime => [...runtime.sessions.values()]);
  await Promise.all(sessions.map(async session => {
    if (session.finalizedAt) return;
    session.terminationReason = 'server_restart';
    if (!session.endedAt) {
      await killProcessTree(session, 'KILL', 'server_restart');
      await waitForSession(session, session.sequence, 5_000, 'exit');
    }
    if (!session.finalizedAt) await finalizeSession(ctx, session, false);
  }));
}

export interface BufferedRunRouting {
  routeWsl?: boolean;
  environment?: Array<[string, string]>;
  removeEnvironment?: string[];
  platform?: NodeJS.Platform;
}

export async function runBuffered(
  program: string,
  args: string[],
  cwd: string,
  input?: string,
  timeoutMs = 30_000,
  environment?: NodeJS.ProcessEnv,
  routing: BufferedRunRouting = {}
): Promise<{ code: number | null; stdout: string; stderr: string }> {
  const wsl = routing.routeWsl
    ? wslInvocationForPath(cwd, program, args, routing.environment ?? [], routing.removeEnvironment ?? [], routing.platform)
    : undefined;
  const startedAt = Date.now();
  const deadlineMs = startedAt + Math.max(1, timeoutMs);
  const startupOutput = new WeakMap<ChildProcessWithoutNullStreams, {
    stdout: Buffer[];
    stderr: Buffer[];
    onStdout: (chunk: Buffer) => void;
    onStderr: (chunk: Buffer) => void;
  }>();
  let controlled;
  try {
    controlled = await processStartupController.start(() => {
      const launched = spawn(wsl?.program ?? program, wsl?.args ?? args, {
        cwd: wsl ? undefined : cwd,
        env: environment ?? process.env,
        windowsHide: true,
        detached: process.platform !== 'win32',
        stdio: 'pipe'
      });
      const captured = {
        stdout: [] as Buffer[],
        stderr: [] as Buffer[],
        onStdout: (chunk: Buffer) => captured.stdout.push(Buffer.from(chunk)),
        onStderr: (chunk: Buffer) => captured.stderr.push(Buffer.from(chunk))
      };
      launched.stdout.on('data', captured.onStdout);
      launched.stderr.on('data', captured.onStderr);
      startupOutput.set(launched, captured);
      return launched;
    }, {
      deadlineMs,
      terminate: terminateUntrackedChild
    });
  } catch (error) {
    throw startupToolError(error);
  }

  const child = controlled.child;
  const capturedStartupOutput = startupOutput.get(child);
  if (capturedStartupOutput) {
    child.stdout.off('data', capturedStartupOutput.onStdout);
    child.stderr.off('data', capturedStartupOutput.onStderr);
    startupOutput.delete(child);
  }
  return new Promise((resolve, reject) => {
    let stdout = capturedStartupOutput ? Buffer.concat(capturedStartupOutput.stdout).toString() : '';
    let stderr = capturedStartupOutput ? Buffer.concat(capturedStartupOutput.stderr).toString() : '';
    let settled = false;
    let timer: NodeJS.Timeout | undefined;
    const cleanup = () => {
      if (timer) clearTimeout(timer);
      child.off('error', onError);
      child.off('close', onClose);
    };
    const finish = (error?: Error, code: number | null = child.exitCode) => {
      if (settled) return;
      settled = true;
      cleanup();
      if (error) reject(error); else resolve({ code, stdout, stderr });
    };
    const onError = (error: Error) => finish(error);
    const onClose = (code: number | null) => finish(undefined, code);
    child.stdout.on('data', (data: Buffer) => { stdout += data.toString(); });
    child.stderr.on('data', (data: Buffer) => { stderr += data.toString(); });
    child.on('error', onError);
    child.once('close', onClose);
    if (!controlled.earlyExit) {
      timer = setTimeout(() => {
        void terminateUntrackedChild(child);
      }, Math.max(1, timeoutMs));
      timer.unref();
    }
    for (const startupError of controlled.handoff()) onError(startupError);
    try { child.stdin.end(input); } catch { /* child already exited */ }
    queueMicrotask(() => {
      if (settled || (child.exitCode === null && child.signalCode === null)) return;
      void waitForChildStreams(child).then(() => finish(undefined, child.exitCode));
    });
  });
}

export function readSessionOutput(session: ProcessSession, stream: 'stdout' | 'stderr', offset: number, limit: number): JsonObject {
  const outputReference = `output://${session.id}/${stream}`;
  const retained = session.sensitiveOutput ? REDACTED : stream === 'stdout' ? session.stdout : session.stderr;
  const retainedStart = session.sensitiveOutput ? 0 : stream === 'stdout' ? session.stdoutStart : session.stderrStart;
  const total = session.sensitiveOutput ? Buffer.byteLength(REDACTED) : stream === 'stdout' ? session.stdoutBytes : session.stderrBytes;
  const requested = Math.max(0, Math.trunc(offset));
  let effective = Math.max(retainedStart, Math.min(requested, total));
  const source = Buffer.from(retained);
  let relative = Math.max(0, effective - retainedStart);
  const originalRelative = relative;
  while (relative < source.length && (source[relative] & 0xc0) === 0x80) relative += 1;
  effective += relative - originalRelative;
  const selected = safeUtf8Prefix(source.subarray(relative, relative + boundedInteger(limit, 4_096, 1, 1_048_576)));
  const content = selected.toString('utf8');
  const bytes = selected.length;
  const nextOffsetValue = effective + bytes;
  const hasMore = nextOffsetValue < total;
  const cursorExpired = requested < retainedStart;
  const aligned = effective !== Math.max(retainedStart, Math.min(requested, total));
  const warnings: string[] = [];
  if (cursorExpired) warnings.push('requested offset expired; response starts at the oldest retained byte');
  if (aligned) warnings.push('requested offset was aligned to the start of a complete character');
  return {
    ok: true,
    output_ref: outputReference,
    stream_output_ref: outputReference,
    stream,
    offset: effective,
    requested_offset: requested,
    retained_start_offset: retainedStart,
    retained_from: retainedStart,
    cursor_expired: cursorExpired,
    limit: boundedInteger(limit, 4_096, 1, 1_048_576),
    encoding: 'utf-8',
    content,
    next_offset: hasMore ? nextOffsetValue : null,
    total_retained_bytes: source.length,
    total_stream_bytes: total,
    total_bytes: total,
    truncated: hasMore,
    has_more: hasMore,
    warnings
  };
}

export async function runCommandGraph(
  ctx: ToolContext,
  key: string,
  args: JsonObject,
  signal?: AbortSignal
): Promise<JsonObject> {
  const commands: Array<JsonObject & { id: string }> = Array.isArray(args.commands)
    ? args.commands.map((value, index) => ({ ...(value as JsonObject), id: String((value as JsonObject).id ?? `command-${index + 1}`) }))
    : [];
  if (!commands.length) throw new Error('commands are required');
  const requestedMode = String(args.mode ?? 'auto');
  const hasDependencies = commands.some(command => Array.isArray(command.depends_on) && command.depends_on.length > 0);
  const mode = requestedMode === 'auto' ? (hasDependencies ? 'dag' : 'sequential') : requestedMode;
  const stopOnError = args.stop_on_error !== false;
  const defaultParallel = mode === 'sequential' ? 1 : Math.min(8, ctx.config.limits.processConcurrency);
  const maxParallel = Math.max(1, Math.min(Number(args.max_parallel ?? defaultParallel), 256));
  const results = new Map<string, JsonObject>();
  const pending = new Map(commands.map(command => [String(command.id), command]));
  const running = new Map<string, Promise<void>>();

  const launch = (id: string, command: JsonObject) => {
    const task = (async () => {
      const started = await startProcess(ctx, key, command, signal);
      const session = started.session;
      while (!session.finalizedAt) await waitForSession(session, session.sequence, 30_000, 'finalized');
      results.set(id, {
        id,
        ...processResult(session, {
          ...command,
          deduplicated: started.deduplicated,
          attached_to_session_id: started.attachedToSessionId,
          operation_lock_wait_ms: started.operationLockWaitMs
        })
      });
      pending.delete(id);
    })().finally(() => running.delete(id));
    running.set(id, task);
  };

  while (pending.size || running.size) {
    let launched = false;
    for (const [id, command] of pending) {
      if (running.size >= maxParallel) break;
      const dependencies = Array.isArray(command.depends_on) ? command.depends_on.map(String) : [];
      const unresolved = dependencies.some(dependency => !results.has(dependency));
      if (unresolved) continue;
      const failedDependency = dependencies.some(dependency => results.get(dependency)?.command_ok === false || results.get(dependency)?.ok === false);
      if (failedDependency) {
        results.set(id, { id, ok: false, skipped: true, skip_reason: 'dependency_failed' });
        pending.delete(id);
        launched = true;
        continue;
      }
      if (mode === 'sequential' && running.size) break;
      launch(id, command);
      launched = true;
      if (mode === 'sequential') break;
    }
    if (!launched && !running.size && pending.size) {
      for (const [id] of pending) results.set(id, { id, ok: false, skipped: true, skip_reason: 'dependency_cycle_or_missing_dependency' });
      pending.clear();
      break;
    }
    if (running.size) await Promise.race(running.values()); else await sleep(1);
    if (stopOnError && [...results.values()].some(result => (result.command_ok === false || result.ok === false) && !result.skipped)) {
      for (const [id] of pending) results.set(id, { id, ok: false, skipped: true, skip_reason: 'stopped_after_failure' });
      pending.clear();
      await Promise.allSettled(running.values());
      break;
    }
  }

  const ordered = commands.map(command => results.get(String(command.id)) ?? { id: command.id, ok: false });
  return {
    ok: ordered.every(result => result.command_ok !== false && (result.ok !== false || result.skipped === true)),
    requested_mode: requestedMode,
    mode,
    auto_selected: requestedMode === 'auto',
    max_parallel: maxParallel,
    commands_requested: commands.length,
    commands_executed: ordered.filter(result => !result.skipped).length,
    failed_command_count: ordered.filter(result => (result.command_ok === false || result.ok === false) && !result.skipped).length,
    skipped_command_count: ordered.filter(result => result.skipped).length,
    results: ordered
  };
}
