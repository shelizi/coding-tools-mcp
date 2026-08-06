export type PermissionMode = 'read-only' | 'guarded' | 'trusted' | 'dangerous';
export type ToolProfile = 'advanced' | 'read-only' | 'compat-readonly-all' | 'guarded-core' | 'trusted-core';
export type ToolProfileSetting = ToolProfile | 'core';
export type HealthState = 'healthy' | 'busy' | 'degraded';
export type SessionState = 'running' | 'verifying' | 'exited' | 'killed' | 'timed_out' | string;

export interface WorkspaceFolder {
  id: string;
  name: string;
  path: string;
}

export interface WorkspaceFolderInput {
  id?: string;
  name?: string;
  path: string;
}

export interface SafeConfig {
  host: string;
  port: number;
  publicBaseUrl: string;
  dataDir: string;
  permissionMode: PermissionMode;
  toolProfile: ToolProfileSetting;
  activeToolProfile: ToolProfile;
  management: { enabled: boolean };
  oauth: {
    clientId: string;
    passwordConfigured: boolean;
    clientSecretConfigured: boolean;
    tokenSecretSource: string;
  };
  policy: {
    allowedCommands: string[];
    workspaceLocalEntries: boolean;
    workspaceScriptExtensions: string[];
    maxPatchBytes: number;
  };
  folders: WorkspaceFolder[];
  limits: {
    blockingConcurrency: number;
    processConcurrency: number;
    globalBlockingConcurrency: number;
    globalProcessConcurrency: number;
    activeSessionLimit: number;
    maxOutputBytes: number;
  };
  tunnel: {
    enabled: boolean;
    publicUrl: string;
    enrollmentConfigured: boolean;
  };
}

export interface WorkspaceConfigSnapshot {
  id: string;
  name: string;
  schemaVersion: number;
  configPath: string;
  secretStorePath: string;
  migrationApplied: boolean;
  migratedFromSchema: number | null;
  restartRequired: boolean;
  effective: SafeConfig;
  saved: SafeConfig;
  environmentOverrides: string[];
}

export interface ConfigSnapshot {
  schemaVersion: number;
  registryPath: string;
  primaryWorkspaceId: string;
  workspaces: WorkspaceConfigSnapshot[];
}

export interface ConfigUpdatePayload {
  name: string;
  host: string;
  port: number;
  publicBaseUrl: string;
  dataDir: string;
  permissionMode: PermissionMode;
  toolProfile: ToolProfileSetting;
  management: { enabled: boolean };
  oauth: {
    clientId: string;
    password: string;
    clientSecret: string;
    clearClientSecret: boolean;
  };
  policy: SafeConfig['policy'];
  folders: WorkspaceFolderInput[];
  limits: SafeConfig['limits'];
  tunnel: {
    enabled: boolean;
    publicUrl: string;
    enrollmentUrl: string;
    clearEnrollmentUrl: boolean;
  };
}

export interface ConfigSaveResult {
  ok: true;
  id: string;
  name: string;
  schemaVersion: number;
  configPath: string;
  secretStorePath: string;
  restartRequired: boolean;
  saved: SafeConfig;
  environmentOverrides: string[];
  warning: string | null;
}

export interface SecretResult {
  ok: true;
  workspaceId: string;
  value: string;
  key?: 'oauthPassword';
  restartRequired?: boolean;
}

export interface RestartResult {
  ok: true;
  restarting: true;
}

export interface WorkspaceStatus {
  id: string;
  name: string;
  host: string;
  port: number;
  folders: WorkspaceFolder[];
  permissionMode: PermissionMode;
  toolProfile: ToolProfile;
  tunnel: {
    enabled?: boolean;
    state?: string;
    publicUrl?: string;
    workers?: number;
    connectedWorkers?: number;
    connectingWorkers?: number;
    idleWorkers?: number;
    busyWorkers?: number;
    recycledWorkers?: number;
    completedRequests?: number;
    policyRevision?: number | null;
    lastError?: string;
    lastRequestTimeout?: 'connect' | 'overall';
    lastRequestTimeoutAt?: number;
  } | null;
}

export interface ManagementStatus {
  ok: true;
  version: string;
  uptimeMs: number;
  tools: number;
  toolProfile: ToolProfile;
  configuredToolProfile: ToolProfileSetting;
  toolsetRevision: string;
  workspaces: WorkspaceStatus[];
  permissionMode: PermissionMode;
  sessions: { total: number; running: number; finalized: number };
  tunnel: WorkspaceStatus['tunnel'];
  restart: {
    supported: boolean;
    mode: 'supervised' | 'unavailable';
  };
  configPath: string;
  headless: boolean;
}


export interface SessionSummary {
  id: string;
  workspaceId: string | null;
  workspaceName: string | null;
  cwd: string;
  status: SessionState;
  pid: number | null;
  startedAt: number;
  endedAt: number | null;
  finalizedAt: number | null;
  durationMs: number;
  exitCode: number | null;
  timedOut: boolean;
  killed: boolean;
  verificationOk: boolean | null;
  stdoutBytes: number;
  stderrBytes: number;
}

export interface UsageAggregate {
  tool: string;
  calls: number;
  errors: number;
  averageDurationMs: number;
  p95DurationMs: number;
  maxDurationMs: number;
  averageQueueWaitMs: number;
  averageLockWaitMs: number;
  responseBytes: number;
}

export interface ActivityRecord {
  tool: string;
  workspaceId: string | null;
  startedAt: number;
  durationMs: number;
  ok: boolean;
}

export interface DashboardPayload {
  ok: true;
  generatedAt: number;
  health: {
    state: HealthState;
    uptimeMs: number;
    lastActivityAt: number | null;
    recentCalls: number;
    recentErrors: number;
    recentErrorRate: number;
  };
  runtime: {
    version: string;
    nodeVersion: string;
    platform: string;
    arch: string;
    pid: number;
    memory: {
      rssBytes: number;
      heapUsedBytes: number;
      heapTotalBytes: number;
      externalBytes: number;
      arrayBuffersBytes: number;
    };
  };
  admission: {
    blocking: { limit: number; active: number; queued: number };
    process: { limit: number; active: number; queued: number };
  };
  sessions: {
    total: number;
    running: number;
    verifying: number;
    finalized: number;
    items: SessionSummary[];
  };
  permissions: {
    pending: number;
    byWorkspace: Array<{ workspaceFolderId: string; pending: number }>;
  };
  tasks: { total: number; byStatus: Record<string, number> };
  tunnel: {
    enabled: boolean;
    state: string;
    workers?: number;
    connectedWorkers?: number;
    connectingWorkers?: number;
    idleWorkers?: number;
    busyWorkers?: number;
    recycledWorkers?: number;
    completedRequests?: number;
    policyRevision?: number | null;
    lastRequestTimeout?: 'connect' | 'overall' | null;
    lastRequestTimeoutAt?: number | null;
  };
  usage: {
    windowSize: number;
    aggregate: UsageAggregate[];
    recent: Array<{
      tool: string;
      startedAt: number;
      durationMs: number;
      ok: boolean;
      queueWaitMs: number;
      lockWaitMs: number;
      responseBytes: number;
    }>;
    persistent: {
      enabled: boolean;
      workspaceId?: string;
      runtimeBootId?: string;
      serverVersion?: string;
      scannedLines?: number;
      matchedLines?: number;
      matchedAsyncSessionEvents?: number;
      invalidCompleteLines?: number;
      aggregate?: Record<string, unknown> | null;
      optimization?: Record<string, unknown> | null;
      formatting?: Record<string, unknown> | null;
      parallelism?: Record<string, unknown> | null;
      performance?: Record<string, unknown> | null;
      warnings?: string[];
      error?: string;
    };
  };
  activity: ActivityRecord[];
  limits: {
    recentUsage: number;
    aggregateWindow: number;
    sessions: number;
    activity: number;
  };
}

export type TelemetryScope = 'current_runtime' | 'current_version' | 'all';
export type TelemetrySort = 'calls' | 'errors' | 'duration_ms' | 'p95_ms' | 'response_bytes' | 'request_bytes' | 'queue_wait_ms';

export interface TelemetryFilters {
  scope: TelemetryScope;
  errorsOnly: boolean;
  limit: number;
  minDurationMs: number;
  sortBy: TelemetrySort;
}

export interface TelemetryRecord {
  event?: string;
  started_ts_ms?: number;
  completed_ts_ms?: number;
  tool?: string;
  duration_ms?: number;
  outcome?: string;
  outcome_class?: string;
  is_error?: boolean;
  error_code?: string;
  warning_count?: number;
  queue_wait_ms?: number;
  response_json_bytes?: number;
  command_kind?: string;
  [key: string]: unknown;
}

export interface TelemetryToolAggregate {
  tool: string;
  calls: number;
  errors: number;
  warnings: number;
  duration_ms: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  max_ms: number;
  queue_wait_ms: number;
  request_bytes: number;
  response_bytes: number;
  [key: string]: unknown;
}

export interface TelemetryAggregate {
  calls: number;
  errors: number;
  warnings: number;
  duration_ms: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  max_ms: number;
  queue_wait_ms: number;
  request_bytes: number;
  response_bytes: number;
  outcomes: Record<string, number>;
  errors_by_code: Record<string, number>;
  tools: TelemetryToolAggregate[];
  [key: string]: unknown;
}

export interface TelemetryPayload {
  ok: true;
  generated_at: number;
  scope: TelemetryScope;
  scanned_lines: number;
  matched_lines: number;
  matched_async_session_events: number;
  invalid_complete_lines: number;
  records: TelemetryRecord[];
  slowest: TelemetryRecord[];
  largest: TelemetryRecord[];
  aggregate: TelemetryAggregate | null;
  optimization: Record<string, unknown> | null;
  formatting: Record<string, unknown> | null;
  parallelism: Record<string, unknown> | null;
  performance: Record<string, unknown> | null;
  warnings: string[];
}

export type OperationLogStatus = 'all' | 'completed' | 'failed' | 'incomplete';

export interface OperationLogFilters {
  folderId: string;
  status: OperationLogStatus;
  tool: string;
  errorsOnly: boolean;
  limit: number;
}

export interface OperationLogEvent {
  kind: string;
  createdAt: number;
  ok: boolean;
}

export interface OperationLogDiagnostics {
  commandOk: boolean | null;
  transportOk: boolean | null;
  executionOk: boolean | null;
  verificationOk: boolean | null;
  errorCode: string | null;
  errorCategory: string | null;
  retryable: boolean | null;
  runtimeStatus: string | null;
  terminationReason: string | null;
  executionLane: string | null;
  outcomeClass: string | null;
  exitCode: number | null;
  processTimedOut: boolean | null;
  requestTimedOut: boolean | null;
  recoverable: boolean | null;
  truncated: boolean | null;
  stdoutTruncated: boolean | null;
  stderrTruncated: boolean | null;
  cursorExpired: boolean | null;
  postChecksPending: boolean | null;
  detached: boolean | null;
  deduplicated: boolean | null;
  elapsedMs: number | null;
  actualWaitMs: number | null;
  firstOutputMs: number | null;
  stdoutBytes: number | null;
  stderrBytes: number | null;
  warningCount: number | null;
  waitMs: {
    blocking: number | null;
    workspaceAdmission: number | null;
    globalAdmission: number | null;
    admissionQueue: number | null;
    workspaceLock: number | null;
    operationLock: number | null;
    resourceLock: number | null;
    historyLock: number | null;
    sessionRegistry: number | null;
  };
}

export interface OperationLogItem {
  id: string;
  tool: string;
  status: Exclude<OperationLogStatus, 'all'>;
  startedAt: number | null;
  finishedAt: number | null;
  durationMs: number | null;
  taskTracked: boolean;
  affectedFileCount: number;
  diagnostics: OperationLogDiagnostics;
  reason?: string;
  events: OperationLogEvent[];
}

export interface OperationLogPayload {
  ok: true;
  generatedAt: number;
  folder: { id: string; name: string };
  source: 'operation_log';
  cursor: number;
  limit: number;
  nextCursor: number | null;
  matched: number;
  summary: {
    total: number;
    completed: number;
    failed: number;
    incomplete: number;
  };
  operations: OperationLogItem[];
}

export interface HistorySessionSummary {
  number: number;
  title: string;
  status: string;
  createdAt: string | null;
  updatedAt: string | null;
  checkpointCount: number;
  summary: string;
  path: string;
}

export interface HistoryListPayload {
  ok: true;
  folder: { id: string; name: string };
  sessions: HistorySessionSummary[];
  integrity: {
    missingNumbers: number[];
    invalidFiles: string[];
    emptyFiles: string[];
    duplicateSessionKeyCount: number;
  };
}

export interface HistoryCheckpoint {
  turnId: string;
  timestamp: string;
  userIntent: string;
  findings: string[];
  decisions: string[];
  filesChanged: string[];
  tests: string[];
  runtimeState: string[];
  remainingIssues: string[];
  nextActions: string[];
  notes: string;
}

export interface HistoryDetailPayload {
  ok: true;
  folder: { id: string; name: string };
  number: number;
  title: string;
  status: string;
  createdAt: string | null;
  updatedAt: string | null;
  path: string;
  records: HistoryCheckpoint[];
  content: string;
}

export interface HealthCheckItem {
  id: string;
  label: string;
  ok: boolean;
  required: boolean;
  detail: string;
  hint?: string;
  status?: number;
  durationMs?: number;
}

export interface HealthCheckPayload {
  ok: boolean;
  generatedAt: number;
  items: HealthCheckItem[];
}

export interface DiagnosticsPayload {
  schemaVersion: number;
  generatedAt: number;
  agent: Record<string, unknown>;
  workspace: Record<string, unknown>;
  runtime: Record<string, unknown>;
  telemetry: TelemetryPayload;
}
