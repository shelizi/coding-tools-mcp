import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { validateUiParity } from '../scripts/check-node-agent-ui-parity.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

async function read(relative) {
  return readFile(path.join(root, relative), 'utf8');
}

test('Node Agent UI parity checklist is complete and executable', async () => {
  const result = await validateUiParity();
  assert.deepEqual(result.errors, []);
  assert.equal(result.version, '0.29.5');
  assert.deepEqual(result.items, ['UI-001', 'UI-002', 'UI-003', 'UI-004', 'UI-005', 'UI-006', 'UI-007']);
  assert.equal(result.checks, 7);
});

test('telemetry and diagnostics keep sensitive runtime fields outside the response contract', async () => {
  const [observability, management] = await Promise.all([
    read('packages/node-agent/src/managementObservability.ts'),
    read('packages/node-agent/src/management.ts')
  ]);
  const allowlist = observability.match(/const TELEMETRY_RECORD_FIELDS = \[([\s\S]*?)\] as const;/)?.[1];
  assert.ok(allowlist);
  for (const forbidden of ['command_preview', 'argument_record', 'resolved_cwd', 'session_id', 'runtime_boot_id', 'arguments_sha256']) {
    assert.doesNotMatch(allowlist, new RegExp(`['"]${forbidden}['"]`));
  }
  assert.match(observability, /content\.replace\(\/\^\\\*\\\*Session key:/);
  assert.doesNotMatch(observability, /fixedProbe\([^\n]*(?:publicBaseUrl|tunnel\.publicUrl)/);
  assert.match(management, /localListenerBaseUrl\(req\)/);
  assert.doesNotMatch(management, /managementHealthPayload\([^\n]*req\.headers\.host/);
  assert.match(observability, /canonicalPath\(folder\.path\)/);
  assert.match(observability, /HISTORY_PATH_OUTSIDE_WORKSPACE/);
  assert.match(observability, /validateManagementHealthPayload/);
  assert.match(observability, /mcpAuthenticationProbe/);
  assert.match(observability, /resource_metadata=/);
  assert.match(management, /action === 'logs'/);
  assert.match(observability, /managementOperationLogPayload/);
  assert.match(observability, /harnessWorkspaceId\(folder\.path\)/);
  assert.match(observability, /redactSensitiveText/);
  assert.match(observability, /'\[WORKSPACE\]'/);
  const operationResponse = observability.match(/function safeOperationGroup[\s\S]*?return \{([\s\S]*?)\r?\n  \};\r?\n\}/)?.[1];
  assert.ok(operationResponse);
  for (const forbidden of ['workspace_id', 'task_id', 'input_summary', 'result_summary', 'affected_files']) {
    assert.doesNotMatch(operationResponse, new RegExp(`\\b${forbidden}\\s*:`));
  }
});

test('workspace UI retains Rust-aligned shared observability surfaces without desktop-only services', async () => {
  const [workspaceView, checklist] = await Promise.all([
    read('packages/node-agent/ui/src/components/WorkspaceView.tsx'),
    read('docs/todo/node-agent-ui-parity/CHECKLIST.md')
  ]);
  assert.match(workspaceView, /'overview' \| 'history' \| 'telemetry' \| 'logs' \| 'health' \| 'settings'/);
  for (const marker of ['HistoryView', 'TelemetryView', 'OperationLogView', 'HealthView', 'OperationalSummary', 'fetchWorkspaceDiagnostics']) {
    assert.match(workspaceView, new RegExp(marker));
  }
  for (const marker of ['role="tablist"', 'role="tabpanel"', "event.key === 'ArrowRight'", "event.key === 'ArrowLeft'", "event.key === 'Home'", "event.key === 'End'"]) {
    assert.ok(workspaceView.includes(marker), `missing accessible Workspace tab marker: ${marker}`);
  }
  assert.match(checklist, /Actions\/OpenAPI/);
  assert.match(checklist, /Live history-session running\/active\/inactive badges/);
  assert.match(checklist, /Raw Desktop per-service stdout\/stderr/);
  assert.match(checklist, /## UI-007 — Structured operation log browser/);
  assert.doesNotMatch(checklist, /- \[ \]/);
});

test('observability views retain complete filters, pagination, and stale-request cancellation', async () => {
  const [telemetry, operationLog, history] = await Promise.all([
    read('packages/node-agent/ui/src/components/TelemetryView.tsx'),
    read('packages/node-agent/ui/src/components/OperationLogView.tsx'),
    read('packages/node-agent/ui/src/components/HistoryView.tsx')
  ]);
  assert.match(telemetry, /value="request_bytes"/);
  assert.match(operationLog, /fetchWorkspaceOperationLogs/);
  assert.match(operationLog, /requestRef\.current\?\.abort\(\)/);
  assert.match(operationLog, /requestRef\.current !== controller/);
  assert.match(operationLog, /nextCursor/);
  assert.match(operationLog, /errorsOnly/);
  assert.match(operationLog, /workspace\.effective\.folders/);
  assert.match(operationLog, /t\('Load older'\)/);
  assert.match(operationLog, /t\('Command result'\)/);
  assert.match(operationLog, /t\('Exit code'\)/);
  assert.match(operationLog, /waitSummary\(operation\.diagnostics\)/);
  assert.match(history, /detailRequest\.current\?\.abort\(\)/);
  assert.match(history, /detailRequest\.current === controller/);
  assert.match(history, /loadHistory\(selectedNumberRef\.current\)/);
  assert.match(history, /aria-live="polite"/);
});

test('operation-log integration omits raw payloads and keeps Rust and Node debugging summaries synchronized', async () => {
  const [managementTest, harnessTest, taskTools, operationSummary, processes, rustDispatch, rustSession] = await Promise.all([
    read('packages/node-agent/test/management.test.mjs'),
    read('packages/node-agent/test/harnessBaseline.test.mjs'),
    read('packages/node-agent/src/taskTools.ts'),
    read('packages/node-agent/src/operationSummary.ts'),
    read('packages/node-agent/src/processes.ts'),
    read('src-tauri/src/tools/dispatch.rs'),
    read('src-tauri/src/tools/session.rs')
  ]);
  for (const marker of [
    'completed-operation-id', 'failed-operation-id', 'incomplete-operation-id',
    'OP_LOG_REASON_MARKER', 'OP_LOG_COMMAND_MARKER', 'OP_LOG_MULTILINE_TAIL',
    'OPERATION_COMMAND_SECRET', 'COMMAND_FAILED', 'operationWorkspaceId',
    'durationMs', 'affectedFileCount', 'diagnostics', 'process_exit_code', 'warning_count', 'nextCursor'
  ]) assert.match(managementTest, new RegExp(marker));
  assert.match(managementTest, /status=failed/);
  assert.match(managementTest, /status=unknown/);
  assert.match(harnessTest, /operation logs persist bounded execution diagnostics without raw process payloads/);
  assert.match(harnessTest, /OPERATION_OUTPUT_MUST_NOT_PERSIST/);
  for (const marker of ['command_ok', 'verification_ok', 'process_exit_code', 'warning_count']) {
    assert.match(operationSummary, new RegExp(marker));
    assert.match(rustDispatch, new RegExp(marker));
  }
  assert.match(operationSummary, /function operationResultSummary/);
  assert.match(rustDispatch, /fn operation_result_summary/);
  for (const marker of ['attach_harness_operation', 'record_harness_operation_finalization', 'harness_operation_recorded']) {
    assert.match(rustSession, new RegExp(marker));
  }
  assert.match(rustDispatch, /deferred_process_operation/);
  assert.match(rustDispatch, /session\.attach_harness_operation/);
  assert.match(rustDispatch, /vec!\["started", "failed"\]/);
  assert.doesNotMatch(operationSummary.match(/function operationResultSummary[\s\S]*?\n\}/)?.[0] ?? '', /\.\.\.result/);
  assert.doesNotMatch(rustDispatch.match(/fn operation_result_summary[\s\S]*?\n\}/)?.[0] ?? '', /\.extend\(/);
  for (const marker of ['attachHarnessOperation', 'deferredProcessOperation', 'result.command_ok === null']) {
    assert.match(taskTools, new RegExp(marker.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));
  }
  for (const marker of ['recordHarnessOperationFinalization', 'harnessOperations', 'harnessOperationRecordedIds']) {
    assert.match(processes, new RegExp(marker));
  }
  assert.match(harnessTest, /yield_time_ms: 0/);
  assert.match(harnessTest, /\['started', 'failed'\]/);
  assert.match(managementTest, /status: 'running'/);
});

test('all settings entry points preserve fine-grained policy and global limits', async () => {
  const [form, quickSetup, testSource] = await Promise.all([
    read('packages/node-agent/ui/src/components/ConfigForm.tsx'),
    read('packages/node-agent/ui/src/components/QuickSetup.tsx'),
    read('packages/node-agent/test/management.test.mjs')
  ]);
  for (const marker of ['allowedCommands', 'workspaceLocalEntries', 'workspaceScriptExtensions', 'maxPatchBytes', 'globalBlockingConcurrency', 'globalProcessConcurrency']) {
    assert.match(form, new RegExp(marker));
    assert.match(testSource, new RegExp(marker));
  }
  assert.match(quickSetup, /policy: saved\.policy/);
  assert.match(quickSetup, /limits: saved\.limits/);
});
