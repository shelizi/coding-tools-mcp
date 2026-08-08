import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { EventEmitter } from 'node:events';
import { request as httpRequest } from 'node:http';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  AUTO_DEDUPE_COMPLETED_GRACE_MS,
  DETACHED_SESSION_GRACE_MS,
  disposeProcessSessions,
  FINALIZED_SESSION_RETENTION_MS,
  MAX_RETAINED_FINALIZED_SESSIONS,
  ProcessRequestLifecycle,
  pruneProcessSessions,
  resolvedCommandTimeoutMs,
  startAndYield,
  waitForSession
} from '../dist/processes.js';
import { createAgentRuntime, createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';

const nodeProgram = path.basename(process.execPath);

test('known build and packaging commands inherit the configured long timeout when omitted', () => {
  const max = 30 * 60_000;
  assert.equal(resolvedCommandTimeoutMs({}, 'cmd.exe /d /s /c npm run desktop:portable', max), max);
  assert.equal(resolvedCommandTimeoutMs({}, 'npm --prefix packages/node-agent run build:server', max), max);
  assert.equal(resolvedCommandTimeoutMs({}, 'npm run node-agent:parity:check', max), max);
  assert.equal(resolvedCommandTimeoutMs({}, 'npm --prefix packages/node-agent run sync:rust-contract', max), max);
  assert.equal(resolvedCommandTimeoutMs({}, 'cargo test --manifest-path src-tauri/Cargo.toml', max), max);
  assert.equal(resolvedCommandTimeoutMs({}, 'cargo clippy --manifest-path src-tauri/Cargo.toml', max), max);
  assert.equal(resolvedCommandTimeoutMs({}, 'node scripts/check.mjs', max), 30_000);
  assert.equal(resolvedCommandTimeoutMs({ timeout_ms: 600_000 }, 'npm run desktop:portable', max), 600_000);
});

function config(root, dataDir, maxOutputBytes = 1024 * 1024) {
  return {
    host: '127.0.0.1', port: 0, dataDir, permissionMode: 'trusted',
    management: { enabled: false },
    oauth: { clientId: 'chatgpt', password: 'process-test-password', tokenSecret: 'process-test-token-secret' },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes }
  };
}

async function fixture(t, maxOutputBytes) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-process-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-process-data-'));
  const ctx = await createToolContext(config(root, dataDir, maxOutputBytes));
  const meta = { 'openai/session': `process-${Date.now()}-${Math.random()}` };
  const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  t.after(async () => {
    await disposeProcessSessions(ctx);
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return { root, dataDir, ctx, meta };
}

async function waitFinal(state, sessionId, cursor = 0, outputMode = 'tail') {
  return callTool(state.ctx, 'wait_command', {
    session_id: sessionId,
    cursor,
    timeout_ms: 10_000,
    until: 'finalized',
    output_mode: outputMode,
    max_output_bytes: 1024 * 1024
  }, state.meta);
}

async function waitForValue(read, timeoutMs = 5_000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const value = read();
    if (value) return value;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  throw new Error('Timed out waiting for process lifecycle state');
}

const interactiveEcho = `
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => process.stdout.write(chunk));
process.stdin.on('end', () => process.exit(0));
setInterval(() => {}, 1000);
`;

test('interactive sessions expose stdin state, byte counts, timing and closed-input errors', async t => {
  const state = await fixture(t);
  const started = await callTool(state.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', interactiveEcho],
    tty: true,
    yield_time_ms: 0,
    timeout_ms: 10_000,
    operation_id: 'interactive-stdin'
  }, state.meta);
  assert.equal(started.ok, true, JSON.stringify(started));
  assert.equal(started.startup.attempts, 1);
  assert.equal(started.startup.retry_count, 0);
  assert.ok(started.startup.gate_wait_ms >= 0);
  assert.deepEqual(started.startup.retry_delays_ms, []);
  assert.equal(started.startup.startup_slots, process.platform === 'win32' ? 4 : 0);
  assert.equal(started.startup.start_interval_ms, process.platform === 'win32' ? 75 : 0);
  assert.equal(started.startup.error_dialog_suppressed, false);
  assert.equal(started.interactive, true);
  assert.equal(started.stdin_open, true);
  assert.equal(started.status, 'running');
  assert.equal(started.termination_reason, 'running');
  assert.equal(started.recoverable, false);
  assert.equal(started.first_output_ms, null);
  assert.ok(Array.isArray(started.next_actions));

  const firstInput = await callTool(state.ctx, 'send_input', {
    session_id: started.session_id,
    chars: 'hé'
  }, state.meta);
  assert.equal(firstInput.bytes_written, Buffer.byteLength('hé'));
  assert.equal(firstInput.stdin_closed, false);
  assert.equal(firstInput.stdin_open, true);

  const closed = await callTool(state.ctx, 'send_input', {
    session_id: started.session_id,
    chars: 'llo\n',
    close_stdin: true
  }, state.meta);
  assert.equal(closed.bytes_written, 4);
  assert.equal(closed.stdin_closed, true);
  assert.equal(closed.stdin_open, false);

  const finalized = await waitFinal(state, started.session_id);
  assert.equal(finalized.command_ok, true, JSON.stringify(finalized));
  assert.equal(finalized.termination_reason, 'exited');
  assert.equal(finalized.interactive, true);
  assert.equal(finalized.stdin_open, false);
  assert.match(finalized.stdout, /héllo/);
  assert.equal(typeof finalized.first_output_ms, 'number');
  assert.ok(finalized.elapsed_ms >= finalized.first_output_ms);

  const rejected = await callTool(state.ctx, 'send_input', {
    session_id: started.session_id,
    chars: 'late'
  }, state.meta);
  assert.equal(rejected.ok, false);
  assert.equal(rejected.error.code, 'SESSION_CLOSED');
  assert.equal(rejected.error.retryable, false);
});

test('wait_command ignores the legacy heartbeat interval and uses the full server wait window', async t => {
  const state = await fixture(t);
  const started = await callTool(state.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'setInterval(() => {}, 1000)'],
    yield_time_ms: 0,
    timeout_ms: 10_000
  }, state.meta);
  assert.equal(started.operation_id, null);

  const waited = await callTool(state.ctx, 'wait_command', {
    session_id: started.session_id,
    cursor: started.latest_cursor,
    timeout_ms: 600,
    heartbeat_ms: 50,
    until: 'finalized',
    output_mode: 'delta'
  }, state.meta);
  assert.equal(waited.heartbeat, false, JSON.stringify(waited));
  assert.equal(waited.request_timed_out, true);
  assert.equal(waited.effective_wait_ms, 600);
  assert.ok(waited.actual_wait_ms >= 500, JSON.stringify(waited));
  assert.equal(waited.process_still_running, true);
  assert.ok(Array.isArray(waited.next_actions));
  assert.equal(waited.next_actions[0].arguments.timeout_ms, 600);
  assert.equal(Object.hasOwn(waited.next_actions[0].arguments, 'heartbeat_ms'), false);
  assert.equal(waited.wait_until, 'finalized');
  assert.equal(typeof waited.session_registry_wait_ms, 'number');
  assert.equal(typeof waited.snapshot_ms, 'number');

  const mistaken = await callTool(state.ctx, 'wait_command', {
    session_id: started.output_refs.stdout,
    timeout_ms: 0
  }, state.meta);
  assert.equal(mistaken.ok, false);
  assert.equal(mistaken.error.code, 'OUTPUT_REF_USED_AS_SESSION_ID');
  assert.equal(mistaken.error.retryable, true);
  assert.equal(mistaken.error.details.corrected_session_id, started.session_id);

  const killed = await callTool(state.ctx, 'kill_session', {
    session_id: started.session_id,
    signal: 'KILL',
    wait_ms: 5_000
  }, state.meta);
  assert.equal(killed.evicted, true, JSON.stringify(killed));
});

test('Windows TERM kill_session forcefully terminates the managed tree like Rust', { skip: process.platform !== 'win32' }, async t => {
  const state = await fixture(t);
  const started = await callTool(state.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'require("node:http").createServer((_req, res) => res.end("ok")).listen(0); setInterval(() => {}, 1000)'],
    yield_time_ms: 0,
    timeout_ms: 30_000
  }, state.meta);
  assert.equal(started.process_still_running, true, JSON.stringify(started));

  const killed = await callTool(state.ctx, 'kill_session', {
    session_id: started.session_id,
    signal: 'TERM',
    wait_ms: 5_000
  }, state.meta);
  assert.equal(killed.ok, true, JSON.stringify(killed));
  assert.equal(killed.status, 'killed', JSON.stringify(killed));
  assert.equal(killed.killed, true, JSON.stringify(killed));
  assert.equal(killed.process_still_running, false, JSON.stringify(killed));
  assert.equal(killed.evicted, true, JSON.stringify(killed));
});

test('operation reattachment, conflict and automatic dedupe grace match Rust', async t => {
  const state = await fixture(t);
  const explicitArgs = {
    program: nodeProgram,
    args: ['-e', 'process.stdout.write("explicit")'],
    operation_id: 'explicit-operation',
    lock_group: 'process-fixture',
    timeout_ms: 10_000,
    yield_time_ms: 10_000
  };
  const first = await callTool(state.ctx, 'exec_command', explicitArgs, state.meta);
  assert.equal(first.operation_id, 'explicit-operation');
  assert.equal(first.resource_lock_group, 'process-fixture');
  assert.equal(first.resource_lock_target, state.root);
  assert.equal(typeof first.operation_lock_wait_ms, 'number');
  assert.equal(typeof first.resource_lock_wait_ms, 'number');
  const second = await callTool(state.ctx, 'exec_command', explicitArgs, state.meta);
  assert.equal(second.session_id, first.session_id);
  assert.equal(second.deduplicated, true);
  assert.equal(second.attached_to_session_id, first.session_id);
  assert.equal(typeof second.operation_lock_wait_ms, 'number');

  const resolved = await callTool(state.ctx, 'resolve_operation', {
    operation_id: 'explicit-operation',
    output_mode: 'tail'
  }, state.meta);
  assert.equal(resolved.resolved_by, 'operation_id');
  assert.equal(resolved.deduplicated, true);
  assert.equal(resolved.attached_to_session_id, first.session_id);

  const conflict = await callTool(state.ctx, 'exec_command', {
    ...explicitArgs,
    timeout_ms: 20_000
  }, state.meta);
  assert.equal(conflict.ok, false);
  assert.equal(conflict.error.code, 'OPERATION_ID_CONFLICT');
  assert.equal(conflict.error.category, 'conflict');
  assert.equal(conflict.error.details.existing_session_id, first.session_id);

  const missing = await callTool(state.ctx, 'resolve_operation', {
    operation_id: 'missing-operation'
  }, state.meta);
  assert.equal(missing.ok, false);
  assert.equal(missing.error.code, 'OPERATION_NOT_FOUND');
  assert.equal(missing.error.retryable, false);
  assert.equal(missing.error.details.retention_seconds, FINALIZED_SESSION_RETENTION_MS / 1000);

  const automaticArgs = {
    program: nodeProgram,
    args: ['-e', 'process.stdout.write("automatic")'],
    deduplicate: true,
    timeout_ms: 10_000,
    yield_time_ms: 10_000
  };
  const automaticFirst = await callTool(state.ctx, 'exec_command', automaticArgs, state.meta);
  assert.match(automaticFirst.operation_id, /^auto:[0-9a-f]{32}$/);
  const automaticSecond = await callTool(state.ctx, 'exec_command', automaticArgs, state.meta);
  assert.equal(automaticSecond.session_id, automaticFirst.session_id);
  assert.equal(automaticSecond.deduplicated, true);

  const retained = state.ctx.sessions.get(automaticFirst.session_id);
  assert.ok(retained?.finalizedAt);
  retained.finalizedAt = Date.now() - AUTO_DEDUPE_COMPLETED_GRACE_MS - 1;
  const automaticThird = await callTool(state.ctx, 'exec_command', automaticArgs, state.meta);
  assert.notEqual(automaticThird.session_id, automaticFirst.session_id);
  assert.equal(automaticThird.deduplicated, false);
});

test('Windows startup admission retains sessions even when it consumes the command timeout', {
  skip: process.platform !== 'win32'
}, async t => {
  const state = await fixture(t);
  const started = await Promise.all(Array.from({ length: 8 }, (_, index) => callTool(state.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'setTimeout(() => {}, 10000)'],
    tty: true,
    yield_time_ms: 0,
    timeout_ms: 100,
    operation_id: `startup-timeout-${index}`
  }, state.meta)));

  for (const result of started) {
    assert.equal(typeof result.session_id, 'string', JSON.stringify(result));
  }
  const finalized = await Promise.all(started.map(result => waitFinal(state, result.session_id, 0, 'none')));
  for (const result of finalized) {
    assert.equal(result.command_ok, false, JSON.stringify(result));
    assert.equal(result.termination_reason, 'process_timeout');
    assert.equal(result.status, 'timed_out');
    assert.equal(result.process_timed_out, true);
  }
});

test('timeout and detached grace expose Rust recovery contracts and reattachment cancels cleanup', async t => {
  const state = await fixture(t);
  const timed = await callTool(state.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'setTimeout(() => {}, 10000)'],
    tty: true,
    yield_time_ms: 0,
    timeout_ms: 100,
    operation_id: 'timeout-operation'
  }, state.meta);
  const timedFinal = await waitFinal(state, timed.session_id);
  assert.equal(timedFinal.command_ok, false);
  assert.equal(timedFinal.termination_reason, 'process_timeout');
  assert.equal(timedFinal.status, 'timed_out');
  assert.equal(timedFinal.process_timed_out, true);
  assert.equal(timedFinal.recoverable, true);
  assert.match(timedFinal.suggestion, /timeout_ms/);

  const lifecycle = new ProcessRequestLifecycle(state.ctx, 80);
  const detachedStart = await startAndYield(state.ctx, state.meta['openai/session'], {
    program: nodeProgram,
    args: ['-e', 'setTimeout(() => {}, 10000)'],
    tty: true,
    yield_time_ms: 0,
    timeout_ms: 10_000,
    operation_id: 'detached-operation'
  }, lifecycle);
  lifecycle.abort();
  const detachedSession = state.ctx.sessions.get(detachedStart.session_id);
  assert.ok(detachedSession);
  await waitForSession(detachedSession, detachedSession.sequence, 5_000, 'finalized');
  const detachedFinal = await callTool(state.ctx, 'resolve_operation', {
    operation_id: 'detached-operation',
    output_mode: 'none'
  }, state.meta);
  assert.equal(detachedFinal.termination_reason, 'detached_timeout');
  assert.equal(detachedFinal.recoverable, true);
  assert.equal(detachedFinal.detached, false, 'resolve_operation reattaches and clears detached state');

  const cancelLifecycle = new ProcessRequestLifecycle(state.ctx, 250);
  const cancelStart = await startAndYield(state.ctx, state.meta['openai/session'], {
    program: nodeProgram,
    args: ['-e', 'setTimeout(() => {}, 10000)'],
    tty: true,
    yield_time_ms: 0,
    timeout_ms: 10_000,
    operation_id: 'cancel-detached-operation'
  }, cancelLifecycle);
  cancelLifecycle.abort();
  await new Promise(resolve => setTimeout(resolve, 40));
  const reattached = await callTool(state.ctx, 'resolve_operation', {
    operation_id: 'cancel-detached-operation',
    output_mode: 'none'
  }, state.meta);
  assert.equal(reattached.detached, false);
  await new Promise(resolve => setTimeout(resolve, 300));
  const stillRunning = state.ctx.sessions.get(cancelStart.session_id);
  assert.ok(stillRunning && !stillRunning.endedAt);

  const killed = await callTool(state.ctx, 'kill_session', {
    session_id: cancelStart.session_id,
    signal: 'KILL',
    wait_ms: 5_000
  }, state.meta);
  assert.equal(killed.killed, true);
  assert.equal(killed.evicted, true);
  assert.equal(killed.status, 'killed');
  assert.equal(state.ctx.sessions.has(cancelStart.session_id), false);
});

test('retained output pagination reports expiry, UTF-8 alignment and delta continuation', async t => {
  const clippedState = await fixture(t, 8);
  const clipped = await callTool(clippedState.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'process.stdout.write("A😀BCDEF")'],
    timeout_ms: 10_000,
    yield_time_ms: 10_000
  }, clippedState.meta);
  const expired = await callTool(clippedState.ctx, 'read_output', {
    output_ref: clipped.output_refs.stdout,
    offset: 0,
    limit: 3
  }, clippedState.meta);
  assert.equal(expired.cursor_expired, true);
  assert.ok(expired.retained_start_offset > 0);
  assert.equal(expired.content, 'BCD');
  assert.equal(expired.next_offset, expired.offset + 3);
  assert.equal(expired.truncated, true);
  assert.match(expired.warnings[0], /expired/);
  assert.equal(expired.total_stream_bytes, Buffer.byteLength('A😀BCDEF'));

  const state = await fixture(t);
  const alignedSource = await callTool(state.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'process.stdout.write("A😀B")'],
    timeout_ms: 10_000,
    yield_time_ms: 10_000
  }, state.meta);
  const aligned = await callTool(state.ctx, 'read_output', {
    output_ref: alignedSource.output_refs.stdout,
    offset: 2,
    limit: 8
  }, state.meta);
  assert.equal(aligned.offset, 5);
  assert.equal(aligned.content, 'B');
  assert.equal(aligned.next_offset, null);
  assert.match(aligned.warnings[0], /aligned/);

  const streamed = await callTool(state.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', `
      process.stdout.write('A'.repeat(700));
      process.stdin.once('data', () => {
        process.stdout.write('B'.repeat(700));
        process.exit(0);
      });
      setInterval(() => {}, 1000);
    `],
    tty: true,
    yield_time_ms: 0,
    timeout_ms: 10_000
  }, state.meta);
  const firstOutput = await callTool(state.ctx, 'wait_command', {
    session_id: streamed.session_id,
    cursor: 0,
    timeout_ms: 10_000,
    until: 'output_or_exit',
    output_mode: 'delta',
    max_output_bytes: 1024
  }, state.meta);
  assert.equal(firstOutput.events.length, 1);
  assert.equal(Buffer.byteLength(firstOutput.events[0].data), 700);
  await callTool(state.ctx, 'send_input', {
    session_id: streamed.session_id,
    chars: 'continue',
    close_stdin: true
  }, state.meta);

  const firstPage = await callTool(state.ctx, 'wait_command', {
    session_id: streamed.session_id,
    cursor: 0,
    timeout_ms: 10_000,
    until: 'finalized',
    output_mode: 'delta',
    max_output_bytes: 1024
  }, state.meta);
  assert.equal(firstPage.command_ok, true);
  assert.equal(firstPage.has_more_output, true);
  assert.ok(firstPage.next_cursor < firstPage.latest_cursor);
  assert.equal(firstPage.events.length, 1);
  assert.equal(Buffer.byteLength(firstPage.events[0].data), 700);

  const secondPage = await callTool(state.ctx, 'wait_command', {
    session_id: streamed.session_id,
    cursor: firstPage.next_cursor,
    timeout_ms: 0,
    until: 'finalized',
    output_mode: 'delta',
    max_output_bytes: 1024
  }, state.meta);
  assert.equal(secondPage.events.length, 1);
  assert.equal(Buffer.byteLength(secondPage.events[0].data), 700);
  assert.equal(secondPage.has_more_output, false);
  assert.equal(secondPage.next_cursor, secondPage.latest_cursor);
});

test('closing the Agent finalizes running sessions with server_restart recovery metadata', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-process-server-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-process-server-data-'));
  const runtime = await createAgentRuntime(config(root, dataDir));
  const meta = { 'openai/session': 'process-server-close' };
  const selected = await callTool(runtime.context, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  await new Promise((resolve, reject) => {
    runtime.server.once('error', reject);
    runtime.server.listen(0, '127.0.0.1', resolve);
  });
  t.after(async () => {
    if (runtime.server.listening) await new Promise(resolve => runtime.server.close(() => resolve()));
    await disposeProcessSessions(runtime.context);
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const started = await callTool(runtime.context, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'setTimeout(() => {}, 10000)'],
    tty: true,
    yield_time_ms: 0,
    timeout_ms: 10_000,
    operation_id: 'server-close-operation'
  }, meta);
  const session = runtime.context.sessions.get(started.session_id);
  assert.ok(session && !session.endedAt);

  await new Promise((resolve, reject) => runtime.server.close(error => error ? reject(error) : resolve()));
  await waitForSession(session, session.sequence, 5_000, 'finalized');
  const resolved = await callTool(runtime.context, 'resolve_operation', {
    operation_id: 'server-close-operation',
    output_mode: 'none'
  }, meta);
  assert.equal(resolved.termination_reason, 'server_restart');
  assert.equal(resolved.recoverable, true);
  assert.match(resolved.suggestion, /wait_command|重新执行|恢复/);
  assert.equal(resolved.process_still_running, false);
});

test('aborting a real MCP request starts detached grace and resolve_operation reattaches it', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-process-abort-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-process-abort-data-'));
  const runtime = await createAgentRuntime(config(root, dataDir));
  const meta = { 'openai/session': 'process-http-abort' };
  const selected = await callTool(runtime.context, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  await new Promise((resolve, reject) => {
    runtime.server.once('error', reject);
    runtime.server.listen(0, '127.0.0.1', resolve);
  });
  t.after(async () => {
    if (runtime.server.listening) await new Promise(resolve => runtime.server.close(() => resolve()));
    await disposeProcessSessions(runtime.context);
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const address = runtime.server.address();
  assert.ok(address && typeof address === 'object');
  const base = `http://127.0.0.1:${address.port}`;
  const verifier = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~';
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  const redirectUri = 'https://chatgpt.com/connector_platform_oauth_redirect';
  const authorized = runtime.oauth.authorizeSubmit(new URLSearchParams({
    client_id: 'chatgpt',
    redirect_uri: redirectUri,
    code_challenge: challenge,
    code_challenge_method: 'S256',
    state: 'abort-state',
    password: 'process-test-password'
  }), base);
  const code = new URL(authorized.location).searchParams.get('code');
  assert.ok(code);
  const tokenResponse = runtime.oauth.exchangeToken(new URLSearchParams({
    grant_type: 'authorization_code',
    code,
    redirect_uri: redirectUri,
    code_verifier: verifier,
    client_id: 'chatgpt'
  }), {}, base);
  const token = tokenResponse.body.access_token;
  assert.equal(typeof token, 'string');

  const payload = JSON.stringify({
    jsonrpc: '2.0',
    id: 1,
    method: 'tools/call',
    params: {
      name: 'exec_command',
      arguments: {
        program: nodeProgram,
        args: ['-e', 'setTimeout(() => {}, 10000)'],
        yield_time_ms: 30_000,
        timeout_ms: 10_000,
        operation_id: 'http-abort-operation'
      },
      _meta: meta
    }
  });
  let clientRequest;
  let clientResponse;
  const requestFinished = new Promise(resolve => {
    clientRequest = httpRequest(`${base}/mcp`, {
      method: 'POST',
      headers: {
        authorization: `Bearer ${token}`,
        'content-type': 'application/json',
        'content-length': Buffer.byteLength(payload)
      }
    }, response => {
      clientResponse = response;
      response.resume();
      response.once('end', resolve);
      response.once('error', resolve);
      response.once('close', resolve);
    });
    clientRequest.once('error', resolve);
    clientRequest.end(payload);
  });

  const session = await waitForValue(() => [...runtime.context.sessions.values()]
    .find(candidate => candidate.operationId === 'http-abort-operation'));
  assert.ok(clientResponse, 'streaming response headers should arrive before process completion');
  clientResponse.destroy();
  clientRequest.destroy();
  await requestFinished;
  await waitForValue(() => session.detachedGeneration !== 0 && session);
  assert.notEqual(session.detachedGeneration, 0);

  const reattached = await callTool(runtime.context, 'resolve_operation', {
    operation_id: 'http-abort-operation',
    output_mode: 'none'
  }, meta);
  assert.equal(reattached.detached, false);
  assert.equal(reattached.attached_to_session_id, session.id);

  const killed = await callTool(runtime.context, 'kill_session', {
    session_id: session.id,
    signal: 'KILL',
    wait_ms: 5_000
  }, meta);
  assert.equal(killed.evicted, true);
});

test('finalized session retention expires old records and caps retained summaries', async t => {
  const state = await fixture(t);
  const baseResult = await callTool(state.ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'process.exit(0)'],
    timeout_ms: 10_000,
    yield_time_ms: 10_000,
    operation_id: 'retention-base'
  }, state.meta);
  const base = state.ctx.sessions.get(baseResult.session_id);
  assert.ok(base?.finalizedAt);

  const expired = {
    ...base,
    id: 'expired-session',
    operationId: 'expired-operation',
    fingerprint: 'expired-fingerprint',
    finalizedAt: Date.now() - FINALIZED_SESSION_RETENTION_MS,
    outputEvents: [],
    outputEventBytes: 0,
    postChecks: [],
    events: new EventEmitter(),
    timeoutTimer: undefined,
    detachedTimer: undefined,
    lockRelease: undefined
  };
  state.ctx.sessions.set(expired.id, expired);
  state.ctx.operationsByFingerprint.set(expired.fingerprint, expired.id);
  pruneProcessSessions(state.ctx);
  assert.equal(state.ctx.sessions.has(expired.id), false);
  assert.equal(state.ctx.operationsByFingerprint.has(expired.fingerprint), false);

  for (let index = 0; index < MAX_RETAINED_FINALIZED_SESSIONS + 5; index += 1) {
    const session = {
      ...base,
      id: `finalized-${index}`,
      operationId: `operation-${index}`,
      fingerprint: `fingerprint-${index}`,
      finalizedAt: Date.now() - (MAX_RETAINED_FINALIZED_SESSIONS + 5 - index),
      outputEvents: [],
      outputEventBytes: 0,
      postChecks: [],
      events: new EventEmitter(),
      timeoutTimer: undefined,
      detachedTimer: undefined,
      lockRelease: undefined
    };
    state.ctx.sessions.set(session.id, session);
    state.ctx.operationsByFingerprint.set(session.fingerprint, session.id);
  }
  pruneProcessSessions(state.ctx);
  const finalizedCount = [...state.ctx.sessions.values()].filter(session => session.finalizedAt).length;
  assert.equal(finalizedCount, MAX_RETAINED_FINALIZED_SESSIONS);

  const listed = await callTool(state.ctx, 'list_sessions', { include_finalized: true, limit: 1000 }, state.meta);
  assert.equal(listed.retention_seconds, FINALIZED_SESSION_RETENTION_MS / 1000);
  assert.equal(listed.include_finalized, true);
  assert.equal(listed.finalized_count, MAX_RETAINED_FINALIZED_SESSIONS);
});

test('exec_health_check matches the Rust worker, session, and stream-capture contract', async t => {
  const state = await fixture(t);
  const result = await callTool(state.ctx, 'exec_health_check', {}, state.meta);
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.deepEqual(result.worker, { alive: true });
  assert.equal(result.session_create, true, JSON.stringify(result));
  assert.equal(result.command_run, true, JSON.stringify(result));
  assert.equal(result.stdout_capture, true, JSON.stringify(result));
  assert.equal(result.stderr_capture, true, JSON.stringify(result));
  assert.equal(result.status, 'success');
  assert.deepEqual(result.next_actions, []);
  assert.ok(result.duration_ms >= 0);
  assert.equal(typeof result.probe.session_id, 'string');
  assert.equal(result.probe.status, 'exited');
  assert.equal(result.probe.process_still_running, false);
  assert.equal(Number(result.probe.process_exit_code ?? result.probe.exit_code), 0);
  assert.match(result.probe.stdout, /exec-health/);
  assert.match(result.probe.stderr, /exec-health-stderr/);
});

assert.equal(DETACHED_SESSION_GRACE_MS, 90_000);
