import type { ChildProcessWithoutNullStreams } from 'node:child_process';
import type { EventEmitter } from 'node:events';
import type { KeyedMutex, Semaphore } from './runtime.js';
import type { StateStore } from './state.js';
import type { ToolUsageStore } from './toolUsage.js';
import type { WorkerPolicy } from './tunnelPolicy.js';
import type { StartupDiagnostics } from './processStartup.js';
import type { ConversationStore, MutableStringMap } from './conversation.js';

export type JsonObject = Record<string, unknown>;
export type PermissionMode = 'read-only' | 'guarded' | 'trusted' | 'dangerous';
export type ToolProfile = 'advanced' | 'read-only' | 'compat-readonly-all' | 'guarded-core' | 'trusted-core';
export type ToolProfileSetting = ToolProfile | 'core';

export interface SecurityPolicy {
  restrictToolCatalog: boolean;
  enforceCommandAllowlist: boolean;
  requireDangerousConfirmation: boolean;
  requireShellConfirmation: boolean;
  blockNetworkCommands: boolean;
  enforceWorkspaceBoundary: boolean;
  protectRepositoryMetadata: boolean;
  blockSymlinkEscape: boolean;
  protectEnvironmentVariables: boolean;
  enforceHarnessBaseline: boolean;
  requireWriteConfirmation: boolean;
  verifyWriteConflicts: boolean;
  enforceResourceLimits: boolean;
  redactSensitiveOutput: boolean;
  withholdSensitiveSourceOutput: boolean;
  redactTelemetry: boolean;
  redactHistory: boolean;
}

export interface WorkspaceFolder {
  id: string;
  name: string;
  path: string;
}

export interface WorkspaceFolderDocument {
  id?: string;
  name?: string;
  path: string;
}

export interface EditProposalRecord {
  path: string;
  fileSha256: string;
  start: number;
  end: number;
  actualText: string;
  replacement: string;
  createdAt: number;
}

export interface AgentConfig {
  workspaceId?: string;
  workspaceName?: string;
  host: string;
  port: number;
  publicBaseUrl?: string;
  dataDir: string;
  permissionMode: PermissionMode;
  toolProfile: ToolProfileSetting;
  activeToolProfile: ToolProfile;
  securityPolicy: SecurityPolicy;
  securityPolicyCustomized: boolean;
  policy: {
    allowedCommands: string[];
    workspaceLocalEntries: boolean;
    workspaceScriptExtensions: string[];
    maxPatchBytes: number;
  };
  management: {
    enabled: boolean;
  };
  oauth: {
    clientId: string;
    clientSecret?: string;
    password: string;
    tokenSecret: string;
  };
  folders: WorkspaceFolder[];
  limits: {
    blockingConcurrency: number;
    processConcurrency: number;
    globalBlockingConcurrency: number;
    globalProcessConcurrency: number;
    activeSessionLimit: number;
    maxOutputBytes: number;
    commandTimeoutMaxMs: number;
  };
  tunnel?: {
    enabled: boolean;
    publicUrl: string;
    enrollmentUrl?: string;
    stateFile: string;
  };
}

export interface AgentConfigDocument {
  schema_version: 1;
  host?: string;
  port?: number;
  publicBaseUrl?: string;
  dataDir?: string;
  permissionMode?: PermissionMode;
  toolProfile?: ToolProfileSetting | string;
  securityPolicy?: Partial<SecurityPolicy>;
  policy?: {
    allowedCommands?: string[] | string;
    workspaceLocalEntries?: boolean;
    workspaceScriptExtensions?: string[] | string;
    maxPatchBytes?: number;
  };
  management?: Partial<AgentConfig['management']>;
  oauth?: {
    clientId?: string;
  };
  folders?: WorkspaceFolderDocument[];
  limits?: Partial<AgentConfig['limits']>;
  tunnel?: {
    enabled?: boolean;
    publicUrl?: string;
    stateFile?: string;
  };
}

export interface WorkspaceRegistryEntry {
  id: string;
  name: string;
  configPath: string;
}

export interface WorkspaceRegistryDocument {
  schema_version: 1;
  workspaces: WorkspaceRegistryEntry[];
}

export interface AgentSecrets {
  oauthPassword?: string;
  oauthClientSecret?: string;
  oauthTokenSecret?: string;
  tunnelEnrollmentUrl?: string;
}

export interface ToolDefinition {
  name: string;
  title: string;
  description: string;
  inputSchema: JsonObject;
  annotations: {
    title: string;
    readOnlyHint: boolean;
    destructiveHint: boolean;
    idempotentHint: boolean;
    openWorldHint: boolean;
  };
}

export interface PendingOperation {
  resumeId: string;
  name: string;
  args: JsonObject;
  meta: unknown;
  permission: string;
  reason: string;
  folderId: string;
  workspacePath: string;
  defaultCwd: string;
  createdAt: number;
  expiresAt: number;
}

export interface FolderRuntime {
  folderId: string;
  workspacePath: string;
  sessions: Map<string, ProcessSession>;
  operationsByFingerprint: Map<string, string>;
  pendingOperations: Map<string, PendingOperation>;
  editProposals: Map<string, EditProposalRecord>;
  admission: { blocking: Semaphore; process: Semaphore; locks: KeyedMutex };
}

export interface ToolContext {
  config: AgentConfig;
  conversations: ConversationStore;
  workspaceProfileId: string;
  selections: MutableStringMap;
  defaultCwds: MutableStringMap;
  folderRuntimes: Map<string, FolderRuntime>;
  hubAdmission: { blocking: Semaphore; process: Semaphore; locks: KeyedMutex };
  // Single-folder compatibility aliases. Runtime routing uses folderRuntimes.
  sessions: Map<string, ProcessSession>;
  operationsByFingerprint: Map<string, string>;
  pendingOperations: Map<string, PendingOperation>;
  editProposals: Map<string, EditProposalRecord>;
  usage: UsageRecord[];
  usageStore: ToolUsageStore;
  admission: { blocking: Semaphore; process: Semaphore; locks: KeyedMutex };
  state: StateStore;
  tunnelStatus?: TunnelStatus;
}

export interface ProcessOutputEvent {
  sequence: number;
  stream: 'stdout' | 'stderr';
  offset: number;
  data: string;
}

export interface ProcessSession {
  id: string;
  folderId: string;
  workspacePath: string;
  operationId?: string;
  fingerprint: string;
  command: string;
  program: string;
  argv: string[];
  shell: boolean;
  cwd: string;
  startupDiagnostics: StartupDiagnostics;
  startedAt: number;
  firstOutputAt?: number;
  endedAt?: number;
  finalizedAt?: number;
  exitCode?: number | null;
  signal?: string | null;
  stdout: string;
  stderr: string;
  stdoutBytes: number;
  stderrBytes: number;
  stdoutStart: number;
  stderrStart: number;
  sequence: number;
  outputEvents: ProcessOutputEvent[];
  outputEventBytes: number;
  child?: ChildProcessWithoutNullStreams;
  interactive: boolean;
  stdinOpen: boolean;
  timedOut: boolean;
  killed: boolean;
  terminationReason?: string;
  telemetryCommandKind: string;
  telemetryRecorded?: boolean;
  harnessOperations?: Map<string, OperationRecord>;
  harnessOperationRecordedIds?: Set<string>;
  sensitiveOutput: boolean;
  postChecks: JsonObject[];
  postChecksPending: boolean;
  verificationOk?: boolean;
  resourceLockGroup?: string;
  resourceLockTarget?: string;
  operationLockWaitMs: number;
  resourceLockWaitMs: number;
  lockRelease?: () => void;
  timeoutTimer?: NodeJS.Timeout;
  attachmentGeneration: number;
  detachedGeneration: number;
  detachedTimer?: NodeJS.Timeout;
  events: EventEmitter;
}

export interface UsageRecord {
  tool: string;
  startedAt: number;
  durationMs: number;
  ok: boolean;
  queueWaitMs: number;
  lockWaitMs: number;
  responseBytes: number;
}

export type TaskStatus = 'active' | 'paused' | 'verifying' | 'failed' | 'completed' | 'completed_unverified' | 'rolled_back';

export interface ProjectBaseline {
  branch?: string;
  head?: string;
  worktree_fingerprint: string;
  entries: Array<{
    path: string;
    exists: boolean;
    is_binary: boolean;
    sha256: string;
    bytes: number;
  }>;
  captured_at: string;
}

export interface TaskRecord {
  id: string;
  workspace_id: string;
  objective: string;
  status: TaskStatus;
  baseline: ProjectBaseline;
  expected_fingerprint: string;
  completed_steps: string[];
  pending_steps: string[];
  latest_change_id?: string;
  latest_verification_id?: string;
  created_at: string;
  updated_at: string;
}

export interface TaskEvent {
  id: string;
  task_id: string;
  operation_id: string;
  kind: string;
  tool_name?: string;
  input_summary: JsonObject;
  result_summary: JsonObject;
  reason?: { text: string; source: string };
  affected_files: JsonObject[];
  created_at: string;
}

export interface ChangeSet {
  id: string;
  task_id: string;
  objective: string;
  reason: { text: string; source: string };
  files: JsonObject[];
  command_ids: string[];
  verification_ids: string[];
  risks: string[];
  created_at: string;
}

export interface OperationRecord {
  id: string;
  workspace_id: string;
  task_id?: string;
  tool: string;
  kind: string;
  input_summary: JsonObject;
  result_summary: JsonObject;
  reason?: string;
  affected_files: JsonObject[];
  created_at: string;
}

export interface PersistentState {
  tasks: Record<string, TaskRecord>;
  currentTasks: Record<string, string>;
  taskEvents: Record<string, TaskEvent[]>;
  changeSets: Record<string, ChangeSet>;
}

export interface TunnelStatus {
  enabled: boolean;
  state: 'disabled' | 'starting' | 'running' | 'reconnecting' | 'stopped' | 'error';
  publicUrl?: string;
  workers: number;
  connectedWorkers: number;
  connectingWorkers?: number;
  idleWorkers?: number;
  busyWorkers?: number;
  recycledWorkers?: number;
  completedRequests: number;
  policyRevision?: number;
  workerPolicy?: WorkerPolicy;
  lastError?: string;
  lastRequestTimeout?: 'connect' | 'overall';
  lastRequestTimeoutAt?: number;
  startedAt?: number;
}
