import { randomUUID, timingSafeEqual } from 'node:crypto';
import type { IncomingMessage, ServerResponse } from 'node:http';
import path from 'node:path';
import {
  CURRENT_CONFIG_SCHEMA_VERSION, normalizeConfig, resolveDataDir, validateConfigDocument, writeConfigDocument,
  type LoadedConfig
} from './config.js';
import {
  restoreAgentSecretFiles, snapshotAgentSecretFiles, writeAgentSecrets, type SecretStoreState
} from './secrets.js';
import { sendJson } from './oauth.js';
import {
  configurableToolProfiles, toolNamesForProfile, toolsetRevisionForProfile
} from './catalog.js';
import { AGENT_VERSION } from './version.js';
import { dashboardPayload } from './dashboard.js';
import { allFolderRuntimes } from './folderRuntime.js';
import { handleManagementUiRequest, isManagementUiPath } from './managementUi.js';
import {
  ManagementObservabilityError,
  managementDiagnosticsPayload,
  managementHealthPayload,
  managementHistoryDetailPayload,
  managementHistoryListPayload,
  managementOperationLogPayload,
  managementTelemetryPayload
} from './managementObservability.js';
import { canonicalizeWorkspaceFolders } from './workspace.js';
import { normalizeWorkspacePath } from './wsl.js';
import { parseBuiltinPublicUrl } from './tunnel.js';
import type {
  AgentConfig, AgentConfigDocument, AgentSecrets, JsonObject, PermissionMode, ToolContext, ToolProfileSetting, WorkspaceFolder
} from './types.js';

const permissionModes = new Set<PermissionMode>(['read-only', 'guarded', 'trusted', 'dangerous']);
const environmentKeys = [
  'CTMCP_HOST', 'CTMCP_PORT', 'CTMCP_PUBLIC_BASE_URL', 'CTMCP_DATA_DIR', 'CTMCP_PERMISSION_MODE', 'CTMCP_TOOL_PROFILE',
  'CTMCP_UI_ENABLED', 'CTMCP_OAUTH_CLIENT_ID', 'CTMCP_OAUTH_CLIENT_SECRET', 'CTMCP_OAUTH_PASSWORD',
  'CTMCP_OAUTH_TOKEN_SECRET', 'CTMCP_WORKSPACES', 'CTMCP_BLOCKING_CONCURRENCY',
  'CTMCP_PROCESS_CONCURRENCY', 'CTMCP_GLOBAL_BLOCKING_CONCURRENCY', 'CTMCP_GLOBAL_PROCESS_CONCURRENCY',
  'CTMCP_ACTIVE_SESSION_LIMIT', 'CTMCP_MAX_OUTPUT_BYTES',
  'CTMCP_ALLOWED_COMMANDS', 'CTMCP_WORKSPACE_LOCAL_ENTRIES', 'CTMCP_WORKSPACE_SCRIPT_EXTENSIONS', 'CTMCP_MAX_PATCH_BYTES',
  'CTMCP_BUILTIN_ENABLED', 'CTMCP_BUILTIN_PUBLIC_URL', 'CTMCP_BUILTIN_ENROLLMENT_URL'
] as const;

function record(value: unknown, name: string): JsonObject {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${name} must be an object`);
  return value as JsonObject;
}

function stringValue(value: unknown, name: string, fallback = '', maxLength = 4096): string {
  const output = value === undefined ? fallback : String(value).trim();
  if (output.length > maxLength) throw new Error(`${name} exceeds ${maxLength} characters`);
  return output;
}

function secretValue(value: unknown, name: string, maxLength = 4096): string {
  if (value === undefined || value === null) return '';
  if (typeof value !== 'string') throw new Error(`${name} must be a string`);
  if (value.length > maxLength) throw new Error(`${name} exceeds ${maxLength} characters`);
  return value;
}

function integerValue(value: unknown, name: string, fallback: number, minimum: number, maximum: number): number {
  const output = value === undefined ? fallback : Number(value);
  if (!Number.isInteger(output) || output < minimum || output > maximum) {
    throw new Error(`${name} must be an integer between ${minimum} and ${maximum}`);
  }
  return output;
}

function booleanValue(value: unknown, fallback: boolean): boolean {
  return typeof value === 'boolean' ? value : fallback;
}

function parseFolders(value: unknown): WorkspaceFolder[] {
  if (!Array.isArray(value) || value.length < 1 || value.length > 100) throw new Error('folders must contain 1 to 100 entries');
  const ids = new Set<string>();
  return value.map((entry, index) => {
    const folder = record(entry, `folders[${index}]`);
    const folderPath = stringValue(folder.path, `folders[${index}].path`, '', 4096);
    if (!folderPath) throw new Error(`folders[${index}].path is required`);
    const normalizedPath = normalizeWorkspacePath(folderPath);
    const id = stringValue(folder.id, `folders[${index}].id`, '', 64).trim() || randomUUID().replaceAll('-', '');
    const name = stringValue(folder.name, `folders[${index}].name`, '', 128).trim() || path.basename(normalizedPath);
    if (!/^[A-Za-z0-9._-]+$/.test(id)) throw new Error(`folders[${index}].id contains unsupported characters`);
    if (ids.has(id)) throw new Error(`workspace folder id must be unique: ${id}`);
    ids.add(id);
    return { id, name, path: normalizedPath };
  });
}

function safeConfig(config: AgentConfig, secrets: AgentSecrets, effective: boolean): JsonObject {
  return {
    host: config.host,
    port: config.port,
    publicBaseUrl: config.publicBaseUrl ?? '',
    dataDir: config.dataDir,
    permissionMode: config.permissionMode,
    toolProfile: config.toolProfile,
    activeToolProfile: config.activeToolProfile,
    policy: config.policy,
    management: { enabled: config.management.enabled },
    oauth: {
      clientId: config.oauth.clientId,
      passwordConfigured: effective ? config.oauth.password !== 'change-me' : Boolean(secrets.oauthPassword),
      clientSecretConfigured: effective ? Boolean(config.oauth.clientSecret) : Boolean(secrets.oauthClientSecret),
      tokenSecretSource: process.env.CTMCP_OAUTH_TOKEN_SECRET
        ? 'environment'
        : secrets.oauthTokenSecret
          ? 'secret-store'
          : 'missing'
    },
    folders: config.folders,
    limits: config.limits,
    tunnel: config.tunnel ? {
      enabled: config.tunnel.enabled,
      publicUrl: config.tunnel.publicUrl,
      enrollmentConfigured: effective ? Boolean(config.tunnel.enrollmentUrl) : Boolean(secrets.tunnelEnrollmentUrl)
    } : {
      enabled: false,
      publicUrl: '',
      enrollmentConfigured: false
    }
  };
}

function restartRequired(current: AgentConfig, document: AgentConfigDocument, secrets: AgentSecrets): boolean {
  const desired = normalizeConfig(document, secrets);
  desired.workspaceId = current.workspaceId;
  desired.workspaceName = current.workspaceName;
  return JSON.stringify(current) !== JSON.stringify(desired);
}

export class ConfigStore {
  readonly configPath: string;
  readonly current: AgentConfig;
  private document: AgentConfigDocument;
  private secrets: AgentSecrets;
  private secretStorePath: string;
  private readonly migrationApplied: boolean;
  private readonly migratedFromSchema?: number;

  constructor(loaded: LoadedConfig) {
    this.configPath = loaded.configPath;
    this.current = loaded.config;
    this.document = structuredClone(loaded.document);
    this.secrets = structuredClone(loaded.secrets);
    this.secretStorePath = loaded.secretStorePath;
    this.migrationApplied = loaded.migrationApplied;
    this.migratedFromSchema = loaded.migratedFromSchema;
  }

  snapshot(): JsonObject {
    const saved = validateConfigDocument(this.document);
    return {
      schemaVersion: CURRENT_CONFIG_SCHEMA_VERSION,
      configPath: this.configPath,
      secretStorePath: this.secretStorePath,
      migrationApplied: this.migrationApplied,
      migratedFromSchema: this.migratedFromSchema ?? null,
      restartRequired: restartRequired(this.current, this.document, this.secrets),
      effective: safeConfig(this.current, this.secrets, true),
      saved: safeConfig(saved, this.secrets, false),
      environmentOverrides: environmentKeys.filter(key => process.env[key] !== undefined)
    };
  }

  secret(key: 'oauthPassword'): string {
    if (key === 'oauthPassword' && process.env.CTMCP_OAUTH_PASSWORD !== undefined) {
      return this.current.oauth.password;
    }
    return this.secrets[key] ?? this.current.oauth.password;
  }

  async replaceSecret(key: 'oauthPassword', value: string): Promise<void> {
    if (!value.trim()) throw new Error(`${key} must not be blank`);
    const nextSecrets = { ...this.secrets, [key]: value };
    const targetDataDir = resolveDataDir(this.document);
    const secretState = await writeAgentSecrets(targetDataDir, nextSecrets);
    this.secrets = secretState.secrets;
    this.secretStorePath = secretState.storePath;
  }

  async applyResolvedBuiltinTunnel(publicUrl: string, enrollmentCompleted: boolean): Promise<void> {
    const endpoint = parseBuiltinPublicUrl(publicUrl);
    const document = structuredClone(this.document);
    document.tunnel = {
      ...document.tunnel,
      enabled: true,
      publicUrl: endpoint.publicUrl
    };
    document.publicBaseUrl = endpoint.baseUrl;
    const nextSecrets = structuredClone(this.secrets);
    if (enrollmentCompleted) delete nextSecrets.tunnelEnrollmentUrl;

    validateConfigDocument(document);
    const targetDataDir = resolveDataDir(document);
    const secretSnapshot = await snapshotAgentSecretFiles(targetDataDir);
    let secretState: SecretStoreState;
    try {
      secretState = await writeAgentSecrets(targetDataDir, nextSecrets);
      await writeConfigDocument(this.configPath, document);
    } catch (error) {
      try {
        await restoreAgentSecretFiles(secretSnapshot);
      } catch (rollbackError) {
        throw new AggregateError(
          [error, rollbackError],
          'Resolved tunnel persistence failed and the encrypted secret store rollback also failed.'
        );
      }
      throw error;
    }

    this.document = document;
    this.secrets = secretState.secrets;
    this.secretStorePath = secretState.storePath;
    if (this.current.tunnel) {
      this.current.tunnel.publicUrl = endpoint.publicUrl;
      if (enrollmentCompleted) delete this.current.tunnel.enrollmentUrl;
    }
    this.current.publicBaseUrl = endpoint.baseUrl;
  }

  async save(value: unknown): Promise<JsonObject> {
    const input = record(value, 'config');
    const oauthInput = record(input.oauth ?? {}, 'oauth');
    const limitsInput = record(input.limits ?? {}, 'limits');
    const policyInput = record(input.policy ?? this.current.policy, 'policy');
    const managementInput = record(input.management ?? {}, 'management');
    const tunnelInput = record(input.tunnel ?? {}, 'tunnel');
    const nextSecrets = structuredClone(this.secrets);

    const mode = stringValue(input.permissionMode, 'permissionMode', this.current.permissionMode, 32) as PermissionMode;
    if (!permissionModes.has(mode)) throw new Error('permissionMode is invalid');
    const toolProfile = stringValue(input.toolProfile, 'toolProfile', this.current.toolProfile, 64) as ToolProfileSetting;
    if (!configurableToolProfiles.includes(toolProfile)) throw new Error('toolProfile is invalid');

    const oauth: AgentConfigDocument['oauth'] = {
      clientId: stringValue(oauthInput.clientId, 'oauth.clientId', this.current.oauth.clientId, 256)
    };
    const password = secretValue(oauthInput.password, 'oauth.password');
    if (password) nextSecrets.oauthPassword = password;
    const clientSecret = secretValue(oauthInput.clientSecret, 'oauth.clientSecret');
    if (clientSecret) nextSecrets.oauthClientSecret = clientSecret;
    else if (oauthInput.clearClientSecret === true) delete nextSecrets.oauthClientSecret;

    const tunnelPublicUrl = stringValue(tunnelInput.publicUrl, 'tunnel.publicUrl', '', 2048);
    let tunnel: AgentConfigDocument['tunnel'];
    if (tunnelPublicUrl) {
      let normalizedTunnelPublicUrl: string;
      try {
        normalizedTunnelPublicUrl = parseBuiltinPublicUrl(tunnelPublicUrl).publicUrl;
      } catch (error) {
        throw new Error(`tunnel.publicUrl is invalid: ${error instanceof Error ? error.message : String(error)}`);
      }
      tunnel = {
        enabled: booleanValue(tunnelInput.enabled, true),
        publicUrl: normalizedTunnelPublicUrl
      };
      const enrollmentUrl = secretValue(tunnelInput.enrollmentUrl, 'tunnel.enrollmentUrl');
      if (enrollmentUrl) nextSecrets.tunnelEnrollmentUrl = enrollmentUrl;
      else if (tunnelInput.clearEnrollmentUrl === true) delete nextSecrets.tunnelEnrollmentUrl;
      if (this.document.tunnel?.stateFile) tunnel.stateFile = this.document.tunnel.stateFile;
    } else {
      delete nextSecrets.tunnelEnrollmentUrl;
    }

    const folders = await canonicalizeWorkspaceFolders(parseFolders(input.folders ?? this.current.folders));

    const document: AgentConfigDocument = {
      schema_version: CURRENT_CONFIG_SCHEMA_VERSION,
      host: stringValue(input.host, 'host', this.current.host, 255),
      port: integerValue(input.port, 'port', this.current.port, 1, 65_535),
      dataDir: path.resolve(stringValue(input.dataDir, 'dataDir', this.current.dataDir, 4096)),
      permissionMode: mode,
      toolProfile,
      policy: {
        allowedCommands: Array.isArray(policyInput.allowedCommands)
          ? policyInput.allowedCommands.map(String)
          : String(policyInput.allowedCommands ?? '').split(',').map(value => value.trim()).filter(Boolean),
        workspaceLocalEntries: booleanValue(policyInput.workspaceLocalEntries, this.current.policy.workspaceLocalEntries),
        workspaceScriptExtensions: Array.isArray(policyInput.workspaceScriptExtensions)
          ? policyInput.workspaceScriptExtensions.map(String)
          : String(policyInput.workspaceScriptExtensions ?? '').split(',').map(value => value.trim()).filter(Boolean),
        maxPatchBytes: integerValue(policyInput.maxPatchBytes, 'policy.maxPatchBytes', this.current.policy.maxPatchBytes, 1, 16 * 1024 * 1024)
      },
      management: { enabled: booleanValue(managementInput.enabled, true) },
      oauth,
      folders,
      limits: {
        blockingConcurrency: integerValue(limitsInput.blockingConcurrency, 'limits.blockingConcurrency', this.current.limits.blockingConcurrency, 1, 65_535),
        processConcurrency: integerValue(limitsInput.processConcurrency, 'limits.processConcurrency', this.current.limits.processConcurrency, 1, 65_535),
        globalBlockingConcurrency: integerValue(limitsInput.globalBlockingConcurrency, 'limits.globalBlockingConcurrency', this.current.limits.globalBlockingConcurrency, 1, 65_535),
        globalProcessConcurrency: integerValue(limitsInput.globalProcessConcurrency, 'limits.globalProcessConcurrency', this.current.limits.globalProcessConcurrency, 1, 65_535),
        activeSessionLimit: integerValue(limitsInput.activeSessionLimit, 'limits.activeSessionLimit', this.current.limits.activeSessionLimit, 1, 65_535),
        maxOutputBytes: integerValue(limitsInput.maxOutputBytes, 'limits.maxOutputBytes', this.current.limits.maxOutputBytes, 1_024, 16 * 1024 * 1024)
      },
      ...(tunnel ? { tunnel } : {})
    };
    const publicBaseUrl = stringValue(input.publicBaseUrl, 'publicBaseUrl', '', 2048);
    if (publicBaseUrl) document.publicBaseUrl = publicBaseUrl;

    validateConfigDocument(document);
    const targetDataDir = resolveDataDir(document);
    const secretSnapshot = await snapshotAgentSecretFiles(targetDataDir);
    let secretState: SecretStoreState;
    try {
      secretState = await writeAgentSecrets(targetDataDir, nextSecrets);
      await writeConfigDocument(this.configPath, document);
    } catch (error) {
      try {
        await restoreAgentSecretFiles(secretSnapshot);
      } catch (rollbackError) {
        throw new AggregateError(
          [error, rollbackError],
          'Configuration save failed and the encrypted secret store rollback also failed.'
        );
      }
      throw error;
    }
    this.document = document;
    this.secrets = secretState.secrets;
    this.secretStorePath = secretState.storePath;
    const saved = validateConfigDocument(document);
    const needsRestart = restartRequired(this.current, document, this.secrets);
    return {
      ok: true,
      schemaVersion: CURRENT_CONFIG_SCHEMA_VERSION,
      configPath: this.configPath,
      secretStorePath: this.secretStorePath,
      restartRequired: needsRestart,
      saved: safeConfig(saved, this.secrets, false),
      environmentOverrides: environmentKeys.filter(key => process.env[key] !== undefined),
      warning: needsRestart ? 'The configuration file was saved. Restart the agent to apply it.' : null
    };
  }
}

function isLoopbackAddress(value: string | undefined): boolean {
  const address = (value ?? '').toLowerCase().replace(/^::ffff:/, '');
  return address === '127.0.0.1' || address === '::1';
}

function loopbackHost(value: string | undefined): boolean {
  if (!value || /[\r\n/\\]/.test(value)) return false;
  try {
    const hostname = new URL(`http://${value}`).hostname.replace(/^\[|\]$/g, '').toLowerCase();
    return hostname === '127.0.0.1' || hostname === 'localhost' || hostname === '::1';
  } catch { return false; }
}

function sameOrigin(req: IncomingMessage): boolean {
  const origin = req.headers.origin;
  if (!origin) return true;
  try {
    return new URL(origin).origin === `http://${String(req.headers.host ?? '')}`;
  } catch { return false; }
}

async function requestBody(req: IncomingMessage, limit = 512 * 1024): Promise<unknown> {
  const chunks: Buffer[] = [];
  let bytes = 0;
  for await (const chunk of req) {
    const value = Buffer.from(chunk);
    bytes += value.length;
    if (bytes > limit) throw new Error('management request body is too large');
    chunks.push(value);
  }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}


export interface WorkspaceManagementStore {
  readonly primaryWorkspaceId: string;
  snapshot(): JsonObject;
  saveWorkspace(id: string, value: unknown): Promise<JsonObject>;
  secret(id: string, key: 'oauthPassword'): string;
  regenerateSecret(id: string, key: 'oauthPassword'): Promise<JsonObject>;
}

export interface WorkspaceRuntimeRecord {
  context: ToolContext;
  startedAt: number;
}

export interface ManagementOptions {
  configStore: ConfigStore;
  context: ToolContext;
  startedAt: number;
  adminToken: string;
  requestRestart?: () => void;
  workspaceStore?: WorkspaceManagementStore;
  runtimeRegistry?: Map<string, WorkspaceRuntimeRecord>;
}

function runtimeRecords(options: ManagementOptions): Array<[string, WorkspaceRuntimeRecord]> {
  if (options.runtimeRegistry?.size) return [...options.runtimeRegistry.entries()];
  const id = options.context.config.workspaceId ?? options.context.workspaceProfileId;
  return [[id, { context: options.context, startedAt: options.startedAt }]];
}

function runtimeRecord(options: ManagementOptions, workspaceId: string): WorkspaceRuntimeRecord | undefined {
  const registered = options.runtimeRegistry?.get(workspaceId);
  if (registered) return registered;
  const currentId = options.context.config.workspaceId ?? options.context.workspaceProfileId;
  return workspaceId === currentId ? { context: options.context, startedAt: options.startedAt } : undefined;
}

function statusPayload(options: ManagementOptions): JsonObject {
  const records = runtimeRecords(options);
  const primaryId = options.workspaceStore?.primaryWorkspaceId
    ?? options.context.config.workspaceId
    ?? options.context.workspaceProfileId;
  const primary = options.runtimeRegistry?.get(primaryId) ?? { context: options.context, startedAt: options.startedAt };
  const sessions = records.flatMap(([, record]) => (
    allFolderRuntimes(record.context).flatMap(runtime => [...runtime.sessions.values()])
  ));
  const toolProfile = primary.context.config.activeToolProfile;
  const profileTools = toolNamesForProfile(toolProfile);
  return {
    ok: true,
    version: AGENT_VERSION,
    uptimeMs: Date.now() - Math.min(...records.map(([, record]) => record.startedAt)),
    tools: profileTools.length,
    toolProfile,
    configuredToolProfile: primary.context.config.toolProfile,
    toolsetRevision: toolsetRevisionForProfile(toolProfile),
    workspaces: records.map(([id, record]) => ({
      id,
      name: record.context.config.workspaceName ?? id,
      host: record.context.config.host,
      port: record.context.config.port,
      folders: record.context.config.folders,
      permissionMode: record.context.config.permissionMode,
      toolProfile: record.context.config.activeToolProfile,
      tunnel: record.context.tunnelStatus
    })),
    permissionMode: primary.context.config.permissionMode,
    sessions: {
      total: sessions.length,
      running: sessions.filter(session => !session.endedAt).length,
      finalized: sessions.filter(session => Boolean(session.finalizedAt)).length
    },
    tunnel: primary.context.tunnelStatus,
    restart: {
      supported: Boolean(options.requestRestart),
      mode: options.requestRestart ? 'supervised' : 'unavailable'
    },
    configPath: options.configStore.configPath,
    headless: true
  };
}

function validAdminToken(req: IncomingMessage, expected: string): boolean {
  const supplied = String(req.headers['x-ctmcp-admin-token'] ?? '');
  const left = Buffer.from(supplied);
  const right = Buffer.from(expected);
  return left.length === right.length && timingSafeEqual(left, right);
}

function managementError(res: ServerResponse, error: unknown): void {
  sendJson(res, 400, {
    error: { code: 'CONFIG_INVALID', message: error instanceof Error ? error.message : String(error) }
  });
}

function observabilityError(res: ServerResponse, error: unknown): void {
  if (error instanceof ManagementObservabilityError) {
    sendJson(res, error.status, { error: { code: error.code, message: error.message } });
    return;
  }
  sendJson(res, 500, {
    error: { code: 'OBSERVABILITY_FAILED', message: error instanceof Error ? error.message : String(error) }
  });
}

function localListenerBaseUrl(req: IncomingMessage): string {
  const port = req.socket.localPort;
  if (!port) throw new ManagementObservabilityError(500, 'LOCAL_LISTENER_UNAVAILABLE', 'Local listener address is unavailable.');
  const address = req.socket.localAddress ?? '127.0.0.1';
  const host = address === '0.0.0.0'
    ? '127.0.0.1'
    : address === '::'
      ? '[::1]'
      : address.includes(':')
        ? `[${address}]`
        : address;
  return `http://${host}:${port}`;
}

export async function handleManagementRequest(req: IncomingMessage, res: ServerResponse, pathname: string, options: ManagementOptions): Promise<boolean> {
  const uiPath = isManagementUiPath(pathname);
  const managementPath = uiPath || pathname.startsWith('/admin/api/');
  if (!managementPath || !options.context.config.management.enabled) return false;
  if (!isLoopbackAddress(req.socket.remoteAddress) || !loopbackHost(req.headers.host)) {
    sendJson(res, 403, { error: { code: 'LOCAL_MANAGEMENT_ONLY', message: 'Management UI is available only through a loopback address.' } });
    return true;
  }
  if (uiPath && await handleManagementUiRequest(req, res, pathname, options.adminToken)) return true;
  if (!pathname.startsWith('/admin/api/')) return false;
  if (!validAdminToken(req, options.adminToken) || !sameOrigin(req)) {
    sendJson(res, 403, { error: { code: 'MANAGEMENT_REQUEST_REJECTED', message: 'Management API requires a same-origin UI request.' } });
    return true;
  }
  if (pathname === '/admin/api/status' && req.method === 'GET') {
    sendJson(res, 200, statusPayload(options));
    return true;
  }
  if (pathname === '/admin/api/dashboard' && req.method === 'GET') {
    const requestUrl = new URL(req.url ?? pathname, `http://${req.headers.host ?? '127.0.0.1'}`);
    const workspaceId = requestUrl.searchParams.get('workspaceId')?.trim();
    const record = workspaceId ? options.runtimeRegistry?.get(workspaceId) : undefined;
    if (workspaceId && !record) {
      sendJson(res, 404, { error: { code: 'WORKSPACE_NOT_FOUND', message: `Workspace was not found: ${workspaceId}` } });
      return true;
    }
    const selected = record ?? { context: options.context, startedAt: options.startedAt };
    sendJson(res, 200, await dashboardPayload(selected.context, selected.startedAt));
    return true;
  }

  const observabilityRoute = pathname.match(/^\/admin\/api\/workspaces\/([A-Za-z0-9._-]{1,128})\/(telemetry|logs|history(?:\/([1-9]\d*))?|health|diagnostics)$/);
  if (observabilityRoute) {
    const [, workspaceId, action, historyNumber] = observabilityRoute;
    const selected = runtimeRecord(options, workspaceId);
    if (!selected) {
      sendJson(res, 404, { error: { code: 'WORKSPACE_NOT_FOUND', message: `Workspace was not found: ${workspaceId}` } });
      return true;
    }
    const requestUrl = new URL(req.url ?? pathname, `http://${req.headers.host ?? '127.0.0.1'}`);
    try {
      if (action === 'telemetry' && req.method === 'GET') {
        sendJson(res, 200, await managementTelemetryPayload(selected.context, requestUrl.searchParams));
        return true;
      }
      if (action === 'logs' && req.method === 'GET') {
        sendJson(res, 200, await managementOperationLogPayload(selected.context, requestUrl.searchParams));
        return true;
      }
      if (action === 'history' && req.method === 'GET') {
        sendJson(res, 200, await managementHistoryListPayload(selected.context, requestUrl.searchParams.get('folderId')));
        return true;
      }
      if (historyNumber && req.method === 'GET') {
        sendJson(res, 200, await managementHistoryDetailPayload(
          selected.context,
          Number(historyNumber),
          requestUrl.searchParams.get('folderId')
        ));
        return true;
      }
      if (action === 'health' && req.method === 'POST') {
        sendJson(res, 200, await managementHealthPayload(selected.context, localListenerBaseUrl(req)));
        return true;
      }
      if (action === 'diagnostics' && req.method === 'GET') {
        sendJson(res, 200, await managementDiagnosticsPayload(selected.context, selected.startedAt));
        return true;
      }
    } catch (error) {
      observabilityError(res, error);
      return true;
    }
  }
  if (pathname === '/admin/api/restart' && req.method === 'POST') {
    if (!options.requestRestart) {
      sendJson(res, 409, {
        error: {
          code: 'RESTART_UNAVAILABLE',
          message: 'Restart requires a supervisor. Launch the Agent with start-node-agent.bat.'
        }
      });
      return true;
    }
    sendJson(res, 202, { ok: true, restarting: true });
    setImmediate(() => options.requestRestart?.());
    return true;
  }
  if (pathname === '/admin/api/config' && req.method === 'GET') {
    sendJson(res, 200, options.workspaceStore?.snapshot() ?? options.configStore.snapshot());
    return true;
  }
  if (pathname === '/admin/api/config' && req.method === 'PUT') {
    try {
      const result = await options.configStore.save(await requestBody(req));
      sendJson(res, 200, result);
    } catch (error) {
      managementError(res, error);
    }
    return true;
  }

  const workspaceRoute = pathname.match(/^\/admin\/api\/workspaces\/([A-Za-z0-9._-]{1,128})\/(config|secrets\/oauth-password(?:\/regenerate)?)$/);
  if (workspaceRoute && options.workspaceStore) {
    const [, workspaceId, action] = workspaceRoute;
    try {
      if (action === 'config' && req.method === 'PUT') {
        sendJson(res, 200, await options.workspaceStore.saveWorkspace(workspaceId, await requestBody(req)));
        return true;
      }
      if (action === 'secrets/oauth-password' && req.method === 'GET') {
        sendJson(res, 200, { ok: true, workspaceId, value: options.workspaceStore.secret(workspaceId, 'oauthPassword') });
        return true;
      }
      if (action === 'secrets/oauth-password/regenerate' && req.method === 'POST') {
        sendJson(res, 200, await options.workspaceStore.regenerateSecret(workspaceId, 'oauthPassword'));
        return true;
      }
    } catch (error) {
      managementError(res, error);
      return true;
    }
  }

  sendJson(res, 405, { error: { code: 'METHOD_NOT_ALLOWED', message: 'Method not allowed.' } });
  return true;
}
