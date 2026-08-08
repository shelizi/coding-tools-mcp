import { access, readFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptPath = fileURLToPath(import.meta.url);
const workspace = resolve(dirname(scriptPath), '..');

const files = {
  checklist: 'docs/todo/node-agent-ui-parity/CHECKLIST.md',
  package: 'packages/node-agent/package.json',
  management: 'packages/node-agent/src/management.ts',
  observability: 'packages/node-agent/src/managementObservability.ts',
  taskTools: 'packages/node-agent/src/taskTools.ts',
  operationSummary: 'packages/node-agent/src/operationSummary.ts',
  processes: 'packages/node-agent/src/processes.ts',
  rustDispatch: 'src-tauri/src/tools/dispatch.rs',
  rustSession: 'src-tauri/src/tools/session.rs',
  workspaceView: 'packages/node-agent/ui/src/components/WorkspaceView.tsx',
  configForm: 'packages/node-agent/ui/src/components/ConfigForm.tsx',
  quickSetup: 'packages/node-agent/ui/src/components/QuickSetup.tsx',
  telemetryView: 'packages/node-agent/ui/src/components/TelemetryView.tsx',
  operationLogView: 'packages/node-agent/ui/src/components/OperationLogView.tsx',
  historyView: 'packages/node-agent/ui/src/components/HistoryView.tsx',
  healthView: 'packages/node-agent/ui/src/components/HealthView.tsx',
  operationalSummary: 'packages/node-agent/ui/src/components/OperationalSummary.tsx',
  managementTest: 'packages/node-agent/test/management.test.mjs',
  harnessTest: 'packages/node-agent/test/harnessBaseline.test.mjs'
};

async function exists(path) {
  try { await access(path); return true; } catch { return false; }
}

function count(text, pattern) {
  return [...text.matchAll(pattern)].length;
}

function requireText(errors, source, marker, label) {
  if (!source.includes(marker)) errors.push(`missing ${label}: ${marker}`);
}

export async function validateUiParity(root = workspace) {
  const errors = [];
  const missing = [];
  for (const [label, relative] of Object.entries(files)) {
    if (!(await exists(resolve(root, relative)))) missing.push(`${label}: ${relative}`);
  }
  if (missing.length) return { errors: missing.map(value => `missing file ${value}`), version: null, items: [], checks: 0 };

  const entries = await Promise.all(Object.entries(files).map(async ([key, relative]) => [key, await readFile(resolve(root, relative), 'utf8')]));
  const source = Object.fromEntries(entries);
  const packageMetadata = JSON.parse(source.package);
  const version = packageMetadata.version;
  const items = ['UI-001', 'UI-002', 'UI-003', 'UI-004', 'UI-005', 'UI-006', 'UI-007'];

  for (const id of items) {
    if (count(source.checklist, new RegExp(`^## ${id}\\b`, 'gm')) !== 1) errors.push(`${id}: checklist heading must appear exactly once`);
  }
  if (source.checklist.includes('- [ ]')) errors.push('checklist contains incomplete acceptance items');
  requireText(errors, source.checklist, `Node Agent \`${version}\``, 'current Node Agent baseline');
  for (const marker of [
    'Actions/OpenAPI', 'FRP and Cloudflare', 'Static Bearer', 'Legacy JSON transport',
    'Per-service runtime supervisor', 'Live history-session running/active/inactive badges',
    'Raw Desktop per-service stdout/stderr'
  ]) requireText(errors, source.checklist, marker, 'intentional exclusion');

  for (const route of [
    '/(telemetry|logs|history', "action === 'telemetry'", "action === 'logs'", "action === 'history'", "action === 'health'", "action === 'diagnostics'"
  ]) requireText(errors, source.management, route, 'Management observability route');

  const allowlist = source.observability.match(/const TELEMETRY_RECORD_FIELDS = \[([\s\S]*?)\] as const;/)?.[1] ?? '';
  if (!allowlist) errors.push('telemetry response allowlist is missing');
  for (const forbidden of ['command_preview', 'argument_record', 'resolved_cwd', 'session_id', 'runtime_boot_id', 'arguments_sha256']) {
    if (allowlist.includes(`'${forbidden}'`)) errors.push(`telemetry allowlist exposes forbidden field ${forbidden}`);
  }
  requireText(errors, source.observability, "content.replace(/^\\*\\*Session key:", 'history session-key redaction');
  for (const path of ['/health', '/mcp/info', '/.well-known/oauth-authorization-server', '/.well-known/oauth-protected-resource/mcp']) {
    requireText(errors, source.observability, path, 'fixed health probe');
  }
  if (/fixedProbe\([^\n]*(?:publicBaseUrl|tunnel\.publicUrl)/.test(source.observability)) {
    errors.push('health diagnostics fetch a configured public URL');
  }
  for (const marker of ['validateManagementHealthPayload', 'mcpAuthenticationProbe', 'resource_metadata=']) {
    requireText(errors, source.observability, marker, 'active health contract validation');
  }
  requireText(errors, source.management, 'localListenerBaseUrl(req)', 'socket-derived local health target');
  if (/managementHealthPayload\([^\n]*req\.headers\.host/.test(source.management)) {
    errors.push('health diagnostics trust the HTTP Host header');
  }
  requireText(errors, source.observability, 'canonicalPath(folder.path)', 'canonical history root');
  requireText(errors, source.observability, 'HISTORY_PATH_OUTSIDE_WORKSPACE', 'history symlink escape rejection');
  for (const marker of ['managementOperationLogPayload', 'harnessWorkspaceId(folder.path)', 'safeOperationGroup', 'redactSensitiveText', "'[WORKSPACE]'", 'taskTracked', 'affectedFileCount']) {
    requireText(errors, source.observability, marker, 'structured operation-log contract');
  }
  for (const marker of ['operationResultSummary', 'command_ok', 'verification_ok', 'process_exit_code', 'warning_count']) {
    requireText(errors, source.operationSummary, marker, 'Node bounded operation result summary');
  }
  for (const marker of ['operation_result_summary', 'command_ok', 'verification_ok', 'process_exit_code', 'warning_count']) {
    requireText(errors, source.rustDispatch, marker, 'Rust bounded operation result summary');
  }
  for (const marker of ['attach_harness_operation', 'record_harness_operation_finalization', 'harness_operation_recorded', 'self.record_harness_operation_finalization()']) {
    requireText(errors, source.rustSession, marker, 'Rust retained process terminal finalization');
  }
  for (const marker of ['deferred_process_operation', 'session.attach_harness_operation', 'vec!["started", "failed"]']) {
    requireText(errors, source.rustDispatch, marker, 'Rust async operation correlation regression');
  }
  if (/operationResultSummary[\s\S]*?\.\.\.result/.test(source.operationSummary)) errors.push('Node operation result summary spreads the raw result');
  if (/operation_result_summary[\s\S]*?\.extend\(/.test(source.rustDispatch)) errors.push('Rust operation result summary extends from a raw result object');
  for (const marker of ['attachHarnessOperation', 'deferredProcessOperation', 'result.command_ok === null']) {
    requireText(errors, source.taskTools, marker, 'deferred process operation binding');
  }
  for (const marker of ['recordHarnessOperationFinalization', 'session.harnessOperations', 'session.harnessOperationRecordedIds', 'await recordHarnessOperationFinalization(ctx, session)']) {
    requireText(errors, source.processes, marker, 'retained process terminal finalization');
  }
  requireText(errors, source.observability, 'operationRecordIsTerminal', 'legacy provisional operation handling');
  const operationResponse = source.observability.match(/function safeOperationGroup[\s\S]*?return \{([\s\S]*?)\r?\n  \};\r?\n\}/)?.[1] ?? '';
  if (!operationResponse) errors.push('operation-log derived response contract is missing');
  for (const forbidden of ['workspace_id', 'task_id', 'input_summary', 'result_summary', 'affected_files']) {
    if (new RegExp(`\\b${forbidden}\\s*:`).test(operationResponse)) errors.push(`operation-log response exposes raw field ${forbidden}`);
  }

  requireText(errors, source.workspaceView, "'overview' | 'history' | 'telemetry' | 'logs' | 'health' | 'settings'", 'six workspace tabs');
  for (const component of ['HistoryView', 'TelemetryView', 'OperationLogView', 'HealthView', 'OperationalSummary', 'fetchWorkspaceDiagnostics']) {
    requireText(errors, source.workspaceView, component, 'workspace observability surface');
  }
  for (const marker of ['role="tablist"', 'role="tabpanel"', "event.key === 'ArrowRight'", "event.key === 'Home'"]) {
    requireText(errors, source.workspaceView, marker, 'accessible workspace tab contract');
  }
  for (const marker of ['allowedCommands', 'workspaceLocalEntries', 'workspaceScriptExtensions', 'maxPatchBytes', 'globalBlockingConcurrency', 'globalProcessConcurrency']) {
    requireText(errors, source.configForm, marker, 'fine-grained policy field');
  }
  requireText(errors, source.quickSetup, 'policy: saved.policy', 'Quick Setup policy preservation');
  requireText(errors, source.quickSetup, 'limits: saved.limits', 'Quick Setup limit preservation');

  for (const [key, marker] of [
    ['telemetryView', 'fetchWorkspaceTelemetry'],
    ['operationLogView', 'fetchWorkspaceOperationLogs'],
    ['historyView', 'fetchWorkspaceHistorySession'],
    ['healthView', 'runWorkspaceHealth'],
    ['operationalSummary', 'permissions.byWorkspace']
  ]) requireText(errors, source[key], marker, `${key} implementation`);
  requireText(errors, source.telemetryView, 'value="request_bytes"', 'complete telemetry sorting');
  for (const marker of ['requestRef.current?.abort()', 'requestRef.current !== controller', 'nextCursor', 'errorsOnly', "t('Load older')", 'workspace.effective.folders', "t('Command result')", "t('Exit code')", 'waitSummary(operation.diagnostics)']) {
    requireText(errors, source.operationLogView, marker, 'race-safe paged operation-log browser');
  }
  for (const marker of ['detailRequest.current?.abort()', 'detailRequest.current === controller', 'loadHistory(selectedNumberRef.current)', 'aria-live="polite"']) {
    requireText(errors, source.historyView, marker, 'race-safe refreshable history browser');
  }

  requireText(errors, source.managementTest, 'management observability routes expose sanitized telemetry, operation logs, history, health and diagnostics', 'observability integration test');
  requireText(errors, source.managementTest, 'management health validators reject incomplete local metadata contracts', 'health contract validation test');
  requireText(errors, source.managementTest, 'mcpChallenge?.status', 'active MCP OAuth challenge regression');
  requireText(errors, source.harnessTest, 'operation logs persist bounded execution diagnostics without raw process payloads', 'persisted operation diagnostics regression');
  for (const marker of ["yield_time_ms: 0", "['failed', 'started']"]) {
    requireText(errors, source.harnessTest, marker, 'async process terminal correlation regression');
  }
  for (const marker of ['TELEMETRY_COMMAND_SECRET', 'OP_LOG_REASON_MARKER', 'OP_LOG_COMMAND_MARKER', 'OP_LOG_MULTILINE_TAIL', 'OPERATION_COMMAND_SECRET', 'COMMAND_FAILED', "status: 'running'", 'process_exit_code', 'warning_count', 'HISTORY_SESSION_KEY_SECRET', 'HISTORY_PATH_OUTSIDE_WORKSPACE', "host: 'localhost:9'", 'globalBlockingConcurrency', 'workspaceLocalEntries']) {
    requireText(errors, source.managementTest, marker, 'security and policy regression coverage');
  }

  return { errors, version, items, checks: items.length };
}

if (process.argv[1] && resolve(process.argv[1]) === scriptPath) {
  const result = await validateUiParity();
  if (result.errors.length) {
    for (const error of result.errors) console.error(`ERROR: ${error}`);
    process.exitCode = 1;
  } else {
    console.log(`Node Agent UI parity: ${result.checks}/${result.items.length} complete; version ${result.version}`);
  }
}
