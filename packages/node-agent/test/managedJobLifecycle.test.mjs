import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';
import { disposeProcessSessions } from '../dist/processes.js';
import { runtimeForFolderId } from '../dist/folderRuntime.js';
import { syncDurableSession } from '../dist/processes/durableSession.js';

const nodeProgram = path.basename(process.execPath);
const sleeper = ['-e', "console.log('job-start'); setTimeout(() => console.log('job-end'), 60000)"];

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-job-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-job-data-'));
  const ctx = await createToolContext({
    host: '127.0.0.1', port: 0, dataDir, permissionMode: 'trusted',
    management: { enabled: false },
    oauth: { clientId: 'chatgpt', password: 'job-test-password', tokenSecret: 'job-test-token-secret' },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024, commandTimeoutMaxMs: 100 }
  });
  const meta = { 'openai/session': `job-${Date.now()}-${Math.random()}` };
  assert.equal((await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta)).ok, true);
  t.after(async () => {
    await disposeProcessSessions(ctx);
    // On Windows, the durable worker can briefly retain cwd/log handles after
    // finalization. Retry transient EBUSY locks instead of racing its cleanup.
    await rm(root, { recursive: true, force: true, maxRetries: 40, retryDelay: 250 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 40, retryDelay: 250 });
  });
  return { ctx, meta, invoke: (name, args) => callTool(ctx, name, args, meta) };
}

async function waitFinal(invoke, sessionId) {
  // On a heavily loaded Windows runner, taskkill and durable-worker finalization
  // can exceed a single kill_session wait. A fixed overall cap still catches leaks.
  const deadline = Date.now() + (process.platform === 'win32' ? 60_000 : 15_000);
  let result;
  do {
    result = await invoke('wait_command', { session_id: sessionId, until: 'finalized', timeout_ms: 500, output_mode: 'delta' });
    if (result.process_completed === true) return result;
  } while (Date.now() < deadline);
  assert.fail(`session did not finalize: ${JSON.stringify(result)}`);
}

test('durable monitor waits for a late result file before declaring a dead worker crashed', async t => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-durable-race-data-'));
  t.after(() => rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const folderId = 'repo';
  const sessionId = 'late-result';
  const folderKey = `${encodeURIComponent(folderId)}-${createHash('sha256').update(folderId).digest('hex').slice(0, 8)}`;
  const durableDirectory = path.join(dataDir, 'process-sessions', folderKey, sessionId);
  await mkdir(durableDirectory, { recursive: true });
  await Promise.all([
    writeFile(path.join(durableDirectory, 'stdout.log'), ''),
    writeFile(path.join(durableDirectory, 'stderr.log'), '')
  ]);
  const ctx = { config: { dataDir, limits: { maxOutputBytes: 1024 * 1024 } } };
  const session = {
    id: sessionId,
    folderId,
    durableDirectory,
    durableWorkerPid: 0x7fffffff,
    stdoutBytes: 0,
    stderrBytes: 0
  };

  const first = await syncDurableSession(ctx, session);
  assert.equal(first, undefined);
  assert.ok(Number.isFinite(session.durableWorkerExitObservedAt));

  const expected = {
    version: 1,
    exitCode: 0,
    signal: null,
    terminationReason: 'exited',
    error: null,
    endedAt: Date.now()
  };
  await writeFile(path.join(durableDirectory, 'result.json'), `${JSON.stringify(expected)}\n`);
  const recovered = await syncDurableSession(ctx, session);
  assert.deepEqual(recovered, expected);
  assert.equal(session.durableWorkerExitObservedAt, undefined);

  await rm(path.join(durableDirectory, 'result.json'), { force: true });
  session.durableWorkerExitObservedAt = Date.now() - 6_000;
  const crashed = await syncDurableSession(ctx, session);
  assert.equal(crashed.terminationReason, 'crashed');
  assert.match(crashed.error, /retry grace period/i);
});

test('managed job survives ordinary command cap and reattaches without renewing its deadline', async t => {
  const { invoke } = await fixture(t);
  const args = { program: nodeProgram, args: sleeper, operation_id: 'long-one', job_timeout_ms: 5000, yield_time_ms: 0 };
  const started = await invoke('exec_command', args);
  assert.equal(started.ok, true, JSON.stringify(started));
  assert.equal(started.execution_mode, 'job');
  assert.equal(started.effective_process_timeout_ms, 5000);
  assert.equal(started.process_deadline_ts_ms - started.started_ts_ms, 5000);
  assert.equal(started.polling_extends_process_deadline, false);
  assert.equal(started.next_actions[0].tool, 'resolve_operation');
  assert.deepEqual(started.next_actions[0].arguments, {
    operation_id: 'long-one',
    cursor: started.latest_cursor,
    output_mode: 'delta',
    event_detail: 'compact'
  });
  const snapshotStartedAt = Date.now();
  const snapshot = await invoke('resolve_operation', started.next_actions[0].arguments);
  assert.ok(Date.now() - snapshotStartedAt < 500, JSON.stringify(snapshot));
  assert.equal(snapshot.process_still_running, true, JSON.stringify(snapshot));
  assert.equal(snapshot.process_deadline_ts_ms, started.process_deadline_ts_ms);
  assert.equal(snapshot.next_actions[0].tool, 'resolve_operation');
  assert.equal(snapshot.next_actions[0].arguments.operation_id, 'long-one');
  const waited = await invoke('wait_command', { session_id: started.session_id, until: 'finalized', timeout_ms: 300 });
  assert.equal(waited.request_timed_out, true);
  assert.equal(waited.process_still_running, true);
  assert.ok(waited.elapsed_ms > 100, JSON.stringify(waited));
  const retry = await invoke('exec_command', args);
  assert.equal(retry.session_id, started.session_id);
  assert.equal(retry.process_id, started.process_id);
  assert.equal(retry.deduplicated, true);
  assert.equal(retry.process_deadline_ts_ms, started.process_deadline_ts_ms);
  const resolved = await invoke('resolve_operation', { operation_id: 'long-one' });
  assert.equal(resolved.session_id, started.session_id);
  assert.equal(resolved.process_deadline_ts_ms, started.process_deadline_ts_ms);
  const changed = await invoke('exec_command', { ...args, job_timeout_ms: 4000 });
  assert.equal(changed.ok, false);
  assert.equal(changed.error.code, 'OPERATION_ID_CONFLICT');
  const stopped = await invoke('kill_session', { session_id: started.session_id, wait_ms: 15_000 });
  assert.equal(stopped.ok, true, JSON.stringify(stopped));
  if (!stopped.evicted) {
    const final = await waitFinal(invoke, started.session_id);
    assert.equal(final.process_still_running, false, JSON.stringify(final));
    const evicted = await invoke('kill_session', { session_id: started.session_id, wait_ms: 0 });
    assert.equal(evicted.evicted, true, JSON.stringify(evicted));
  }
  assert.equal(stopped.process_timeout_remaining_ms, 0);
});

test('managed job reaches fixed deadline even while it is repeatedly polled', async t => {
  const { invoke } = await fixture(t);
  const started = await invoke('exec_command', { program: nodeProgram, args: sleeper, operation_id: 'fixed-deadline', job_timeout_ms: 1200, yield_time_ms: 0 });
  assert.equal(started.ok, true, JSON.stringify(started));
  let stopped;
  for (let i = 0; i < 8; i += 1) {
    stopped = await invoke('wait_command', { session_id: started.session_id, until: 'finalized', timeout_ms: 250 });
    if (!stopped.process_still_running) break;
  }
  if (stopped.process_still_running) stopped = await waitFinal(invoke, started.session_id);
  assert.equal(stopped.process_timed_out, true);
  assert.equal(stopped.termination_reason, 'process_timeout');
  assert.equal(stopped.process_deadline_ts_ms, started.process_deadline_ts_ms);
});

test('invalid exec_many child budget is rejected before any process starts', async t => {
  const { ctx, invoke } = await fixture(t);
  const result = await invoke('exec_many', { operation_id: 'invalid-batch', commands: [
    { id: 'valid', program: nodeProgram, args: ['-e', 'setTimeout(()=>{},5000)'], job_timeout_ms: 2000, operation_id: 'first' },
    { id: 'invalid', program: nodeProgram, args: ['-e', '0'], timeout_ms: 101 }
  ] });
  assert.equal(result.ok, false, JSON.stringify(result));
  assert.equal(runtimeForFolderId(ctx, 'repo').sessions.size, 0);
});

test('child env cannot override a host-disabled long-job policy', async t => {
  const previous = process.env.CTMCP_JOB_TIMEOUT_MAX_MS;
  process.env.CTMCP_JOB_TIMEOUT_MAX_MS = '0';
  try {
    const { ctx, invoke } = await fixture(t);
    const result = await invoke('exec_command', {
      program: nodeProgram,
      args: ['-e', 'setTimeout(()=>{},5000)'],
      operation_id: 'host-policy-wins',
      job_timeout_ms: 2000,
      env: { CTMCP_JOB_TIMEOUT_MAX_MS: '86400000' }
    });
    assert.equal(result.ok, false, JSON.stringify(result));
    assert.equal(runtimeForFolderId(ctx, 'repo').sessions.size, 0);
  } finally {
    if (previous === undefined) delete process.env.CTMCP_JOB_TIMEOUT_MAX_MS;
    else process.env.CTMCP_JOB_TIMEOUT_MAX_MS = previous;
  }
});

test('exec_many summary retains managed-job deadline metadata', async t => {
  const { invoke } = await fixture(t);
  const graph = await invoke('exec_many', {
    operation_id: 'job-summary-graph',
    result_mode: 'summary',
    yield_time_ms: 0,
    commands: [{
      id: 'job', program: nodeProgram, args: sleeper,
      operation_id: 'job-summary-child', job_timeout_ms: 5000, yield_time_ms: 0
    }]
  });
  assert.equal(graph.ok, true, JSON.stringify(graph));
  let snapshot = graph;
  // Durable worker startup can exceed 500 ms on a loaded Windows runner.
  const readyDeadline = Date.now() + 20_000;
  while (snapshot.results[0]?.execution_mode !== 'job' && Date.now() < readyDeadline) {
    if (!snapshot.ok || snapshot.terminal) break;
    await new Promise(resolve => setTimeout(resolve, 50));
    snapshot = await invoke('exec_many', { operation_id: 'job-summary-graph', action: 'status', result_mode: 'summary' });
  }
  assert.equal(snapshot.results[0].execution_mode, 'job', JSON.stringify(snapshot));
  assert.equal(snapshot.results[0].effective_process_timeout_ms, 5000);
  assert.equal(snapshot.results[0].polling_extends_process_deadline, false);
  assert.equal(snapshot.results[0].timeout_scope, 'process');
  const sessionId = snapshot.results[0].session_id;
  if (sessionId) {
    const killed = await invoke('kill_session', { session_id: sessionId, wait_ms: 15_000 });
    assert.equal(killed.ok, true, JSON.stringify(killed));
    if (!killed.evicted) {
      const final = await waitFinal(invoke, sessionId);
      assert.equal(final.process_still_running, false, JSON.stringify(final));
      const evicted = await invoke('kill_session', { session_id: sessionId, wait_ms: 0 });
      assert.equal(evicted.evicted, true, JSON.stringify(evicted));
    }
  }
});
