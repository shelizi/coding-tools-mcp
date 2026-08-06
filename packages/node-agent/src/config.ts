import { randomBytes, randomUUID } from 'node:crypto';
import { homedir } from 'node:os';
import { chmod, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { ensureAgentSecrets } from './secrets.js';
import { defaultPolicy, mergeAllowedCommands, normalizeScriptExtensions } from './policy.js';
import { configuredToolProfile, resolveToolProfile } from './catalog.js';
import type {
  AgentConfig, AgentConfigDocument, AgentSecrets, PermissionMode, WorkspaceFolder, WorkspaceFolderDocument
} from './types.js';
import { normalizeWorkspacePath, workspaceBasename } from './wsl.js';
import { canonicalizeWorkspaceFolders, validateUniqueWorkspaceFolders } from './workspace.js';

export const CURRENT_CONFIG_SCHEMA_VERSION = 1 as const;

export function createOAuthClientId(): string {
  return `chatgpt-client-${randomUUID().slice(0, 12)}`;
}

export function createWorkspaceFolderId(): string {
  return randomUUID().replaceAll('-', '');
}

export function normalizeWorkspaceFolderDocuments(
  folders: readonly WorkspaceFolderDocument[]
): WorkspaceFolder[] {
  const ids = new Set<string>();
  return folders.map((folder, index) => {
    const folderPath = normalizeWorkspacePath(folder.path);
    const id = folder.id?.trim() || createWorkspaceFolderId();
    const name = folder.name?.trim() || workspaceBasename(folderPath);
    if (!/^[A-Za-z0-9._-]+$/.test(id)) throw new Error(`folders[${index}].id contains unsupported characters`);
    if (ids.has(id)) throw new Error(`workspace folder id must be unique: ${id}`);
    ids.add(id);
    return { id, name, path: folderPath };
  });
}

export const defaultDataDir = process.platform === 'win32'
  ? path.join(process.env.LOCALAPPDATA ?? homedir(), 'CodingToolsMCPNode')
  : path.join(homedir(), '.coding-tools-mcp-node');

export interface LoadedConfig {
  config: AgentConfig;
  configPath: string;
  document: AgentConfigDocument;
  secrets: AgentSecrets;
  secretStorePath: string;
  secretKeyPath: string;
  migrationApplied: boolean;
  migratedFromSchema?: number;
}

interface MigrationResult {
  document: AgentConfigDocument;
  secrets: AgentSecrets;
  changed: boolean;
  fromSchema: number;
}

function parseFolders(value: string | undefined): WorkspaceFolder[] {
  const raw = value?.trim();
  if (!raw) return [{ id: 'default', name: path.basename(process.cwd()), path: process.cwd() }];
  return raw.split(path.delimiter).filter(Boolean).map((folderPath, index) => ({
    id: `folder-${index + 1}`,
    name: workspaceBasename(folderPath),
    path: normalizeWorkspacePath(folderPath)
  }));
}

function positiveInt(value: unknown, fallback: number, max = Number.MAX_SAFE_INTEGER): number {
  const parsed = Number(value);
  return Number.isInteger(parsed) && parsed > 0 ? Math.min(parsed, max) : fallback;
}

function permissionMode(value: unknown): PermissionMode {
  return ['read-only', 'guarded', 'trusted', 'dangerous'].includes(String(value))
    ? String(value) as PermissionMode
    : 'trusted';
}

function enabled(value: unknown, fallback: boolean): boolean {
  if (typeof value === 'boolean') return value;
  if (typeof value === 'string') return !['0', 'false', 'no', 'off'].includes(value.toLowerCase());
  return fallback;
}

function record(value: unknown, name: string): Record<string, unknown> {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error(`${name} must be an object`);
  return value as Record<string, unknown>;
}

function optionalRecord(value: unknown, name: string): Record<string, unknown> {
  return value === undefined ? {} : record(value, name);
}

function legacySecret(value: unknown, name: string): string | undefined {
  if (value === undefined || value === null || value === '') return undefined;
  if (typeof value !== 'string') throw new Error(`${name} must be a string`);
  if (value.length > 4096) throw new Error(`${name} exceeds 4096 characters`);
  return value;
}

export function resolveConfigPath(file?: string): string {
  const configuredDataDir = path.resolve(process.env.CTMCP_DATA_DIR ?? defaultDataDir);
  return path.resolve(file ?? process.env.CTMCP_CONFIG_FILE ?? path.join(configuredDataDir, 'agent.json'));
}

export function resolveDataDir(document: AgentConfigDocument, environment: NodeJS.ProcessEnv = process.env): string {
  return path.resolve(environment.CTMCP_DATA_DIR ?? document.dataDir ?? defaultDataDir);
}

export function normalizeConfig(
  input: AgentConfigDocument,
  secrets: AgentSecrets = {},
  environment: NodeJS.ProcessEnv = process.env
): AgentConfig {
  const dataDir = resolveDataDir(input, environment);
  const publicUrl = environment.CTMCP_BUILTIN_PUBLIC_URL ?? input.tunnel?.publicUrl ?? '';
  const tokenSecret = environment.CTMCP_OAUTH_TOKEN_SECRET ?? secrets.oauthTokenSecret ?? '';
  const environmentFolders = environment.CTMCP_WORKSPACES;
  const effectivePermissionMode = permissionMode(environment.CTMCP_PERMISSION_MODE ?? input.permissionMode);
  const toolProfile = configuredToolProfile(environment.CTMCP_TOOL_PROFILE ?? input.toolProfile);
  return {
    host: environment.CTMCP_HOST ?? input.host ?? '127.0.0.1',
    port: positiveInt(environment.CTMCP_PORT ?? input.port, 3789, 65_535),
    publicBaseUrl: environment.CTMCP_PUBLIC_BASE_URL ?? input.publicBaseUrl ?? derivePublicBase(publicUrl),
    dataDir,
    permissionMode: effectivePermissionMode,
    toolProfile,
    activeToolProfile: resolveToolProfile(toolProfile, effectivePermissionMode),
    policy: {
      allowedCommands: mergeAllowedCommands(environment.CTMCP_ALLOWED_COMMANDS ?? input.policy?.allowedCommands),
      workspaceLocalEntries: enabled(environment.CTMCP_WORKSPACE_LOCAL_ENTRIES ?? input.policy?.workspaceLocalEntries, true),
      workspaceScriptExtensions: normalizeScriptExtensions(environment.CTMCP_WORKSPACE_SCRIPT_EXTENSIONS ?? input.policy?.workspaceScriptExtensions),
      maxPatchBytes: positiveInt(environment.CTMCP_MAX_PATCH_BYTES ?? input.policy?.maxPatchBytes, defaultPolicy().maxPatchBytes, 16 * 1024 * 1024)
    },
    management: {
      enabled: enabled(environment.CTMCP_UI_ENABLED ?? input.management?.enabled, true)
    },
    oauth: {
      clientId: environment.CTMCP_OAUTH_CLIENT_ID ?? input.oauth?.clientId ?? createOAuthClientId(),
      clientSecret: environment.CTMCP_OAUTH_CLIENT_SECRET ?? secrets.oauthClientSecret,
      password: environment.CTMCP_OAUTH_PASSWORD ?? secrets.oauthPassword ?? 'change-me',
      tokenSecret
    },
    folders: environmentFolders !== undefined
      ? parseFolders(environmentFolders)
      : input.folders?.length
        ? normalizeWorkspaceFolderDocuments(input.folders)
        : parseFolders(undefined),
    limits: {
      blockingConcurrency: positiveInt(environment.CTMCP_BLOCKING_CONCURRENCY ?? input.limits?.blockingConcurrency, 128, 65_535),
      processConcurrency: positiveInt(environment.CTMCP_PROCESS_CONCURRENCY ?? input.limits?.processConcurrency, 64, 65_535),
      globalBlockingConcurrency: positiveInt(environment.CTMCP_GLOBAL_BLOCKING_CONCURRENCY ?? input.limits?.globalBlockingConcurrency, 1_024, 65_535),
      globalProcessConcurrency: positiveInt(environment.CTMCP_GLOBAL_PROCESS_CONCURRENCY ?? input.limits?.globalProcessConcurrency, 512, 65_535),
      activeSessionLimit: positiveInt(environment.CTMCP_ACTIVE_SESSION_LIMIT ?? input.limits?.activeSessionLimit, 512, 65_535),
      maxOutputBytes: positiveInt(environment.CTMCP_MAX_OUTPUT_BYTES ?? input.limits?.maxOutputBytes, 1024 * 1024, 16 * 1024 * 1024)
    },
    tunnel: publicUrl ? {
      enabled: enabled(environment.CTMCP_BUILTIN_ENABLED ?? input.tunnel?.enabled, true),
      publicUrl,
      enrollmentUrl: environment.CTMCP_BUILTIN_ENROLLMENT_URL ?? secrets.tunnelEnrollmentUrl,
      stateFile: path.resolve(input.tunnel?.stateFile ?? path.join(dataDir, 'builtin-tunnel-identity.enc.json'))
    } : undefined
  };
}

function derivePublicBase(publicUrl: string): string | undefined {
  if (!publicUrl) return undefined;
  try {
    const url = new URL(publicUrl);
    url.pathname = url.pathname.replace(/\/mcp\/?$/, '');
    url.search = '';
    url.hash = '';
    return url.toString().replace(/\/$/, '');
  } catch { return undefined; }
}

function migrateConfigDocument(value: unknown): MigrationResult {
  const source = record(value, 'config');
  const rawVersion = source.schema_version;
  if (rawVersion !== undefined
    && typeof rawVersion !== 'number'
    && (typeof rawVersion !== 'string' || !/^\d+$/.test(rawVersion.trim()))) {
    throw new Error('schema_version must be a non-negative integer');
  }
  const fromSchema = rawVersion === undefined ? 0 : Number(rawVersion);
  if (!Number.isInteger(fromSchema) || fromSchema < 0) throw new Error('schema_version must be a non-negative integer');
  if (fromSchema > CURRENT_CONFIG_SCHEMA_VERSION) {
    throw new Error(`Unsupported config schema_version ${fromSchema}; this Agent supports ${CURRENT_CONFIG_SCHEMA_VERSION}`);
  }
  if (fromSchema !== 0 && fromSchema !== CURRENT_CONFIG_SCHEMA_VERSION) {
    throw new Error(`Unsupported config schema_version ${fromSchema}`);
  }

  const document = structuredClone(source) as unknown as AgentConfigDocument;
  const oauth = optionalRecord(source.oauth, 'oauth');
  const tunnel = optionalRecord(source.tunnel, 'tunnel');
  const secrets: AgentSecrets = {
    oauthPassword: legacySecret(oauth.password, 'oauth.password'),
    oauthClientSecret: legacySecret(oauth.clientSecret, 'oauth.clientSecret'),
    oauthTokenSecret: legacySecret(oauth.tokenSecret, 'oauth.tokenSecret'),
    tunnelEnrollmentUrl: legacySecret(tunnel.enrollmentUrl, 'tunnel.enrollmentUrl')
  };

  document.schema_version = CURRENT_CONFIG_SCHEMA_VERSION;
  if (document.oauth) {
    delete (document.oauth as Record<string, unknown>).password;
    delete (document.oauth as Record<string, unknown>).clientSecret;
    delete (document.oauth as Record<string, unknown>).tokenSecret;
  }
  if (document.tunnel) delete (document.tunnel as Record<string, unknown>).enrollmentUrl;

  const containsPlaintextSecrets = ['password', 'clientSecret', 'tokenSecret'].some(key => key in oauth)
    || 'enrollmentUrl' in tunnel;
  return {
    document,
    secrets,
    changed: rawVersion !== CURRENT_CONFIG_SCHEMA_VERSION || containsPlaintextSecrets,
    fromSchema
  };
}

async function readConfigInput(configPath: string): Promise<{ value: unknown; exists: boolean }> {
  try {
    return { value: JSON.parse(await readFile(configPath, 'utf8')) as unknown, exists: true };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') {
      return { value: { schema_version: CURRENT_CONFIG_SCHEMA_VERSION }, exists: false };
    }
    throw error;
  }
}

async function readLegacyTokenSecret(dataDir: string): Promise<{ filePath: string; value?: string }> {
  const filePath = path.join(dataDir, 'oauth-token-secret');
  try {
    const value = (await readFile(filePath, 'utf8')).trim();
    if (!value) throw new Error(`Persisted OAuth token secret is invalid: ${filePath}`);
    return { filePath, value };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return { filePath };
    throw error;
  }
}

export function validateConfig(config: AgentConfig): void {
  if (!config.folders.length) throw new Error('at least one workspace folder is required');
  if (!config.host.trim()) throw new Error('host is required');
  const ids = new Set<string>();
  for (const folder of config.folders) {
    if (!folder.id || ids.has(folder.id)) throw new Error(`workspace folder id must be unique: ${folder.id}`);
    if (!folder.path.trim()) throw new Error(`workspace folder path is required: ${folder.id}`);
    ids.add(folder.id);
  }
  validateUniqueWorkspaceFolders(config.folders);
  if (!config.oauth.clientId.trim()) throw new Error('OAuth client ID is required');
  if (!config.oauth.password.trim()) throw new Error('OAuth password is required');
  if (!config.oauth.tokenSecret.trim()) throw new Error('OAuth token secret is not configured');
  if (!config.toolProfile.trim() || !config.activeToolProfile.trim()) throw new Error('tool profile is required');
  if (!config.policy.allowedCommands.length) throw new Error('at least one allowed command is required');
  if (!config.policy.workspaceScriptExtensions.length) throw new Error('at least one workspace script extension is required');
  if (config.policy.maxPatchBytes < 1) throw new Error('maxPatchBytes must be positive');
  if (config.tunnel?.enabled && !config.tunnel.publicUrl.startsWith('https://')) throw new Error('Built-in tunnel public URL must use HTTPS');
}

export function validateConfigDocument(document: AgentConfigDocument): AgentConfig {
  if (document.schema_version !== CURRENT_CONFIG_SCHEMA_VERSION) {
    throw new Error(`schema_version must be ${CURRENT_CONFIG_SCHEMA_VERSION}`);
  }
  const normalized = normalizeConfig(document, {
    oauthTokenSecret: 'managed-token-secret-placeholder-value'
  }, {});
  validateConfig(normalized);
  return normalized;
}

export async function writeConfigDocument(configPath: string, document: AgentConfigDocument): Promise<void> {
  validateConfigDocument(document);
  await mkdir(path.dirname(configPath), { recursive: true, mode: 0o700 });
  const temporary = `${configPath}.${process.pid}.${randomBytes(6).toString('hex')}.tmp`;
  const content = `${JSON.stringify(document, null, 2)}\n`;
  await writeFile(temporary, content, { flag: 'wx', mode: 0o600 });
  try {
    await rename(temporary, configPath);
  } catch (error) {
    await rm(temporary, { force: true }).catch(() => undefined);
    throw error;
  }
  await chmod(configPath, 0o600).catch(() => undefined);
}

export async function loadConfigBundle(file?: string): Promise<LoadedConfig> {
  const configPath = resolveConfigPath(file);
  const input = await readConfigInput(configPath);
  const migration = migrateConfigDocument(input.value);
  if (!migration.document.oauth?.clientId?.trim()) {
    migration.document.oauth = { ...migration.document.oauth, clientId: createOAuthClientId() };
    migration.changed = true;
  }
  if (migration.document.folders?.length) {
    const normalizedFolders = normalizeWorkspaceFolderDocuments(migration.document.folders);
    if (JSON.stringify(normalizedFolders) !== JSON.stringify(migration.document.folders)) {
      migration.document.folders = normalizedFolders;
      migration.changed = true;
    }
  }
  const dataDir = resolveDataDir(migration.document);
  const legacyToken = await readLegacyTokenSecret(dataDir);
  const seed: AgentSecrets = {
    ...migration.secrets,
    ...(!migration.secrets.oauthTokenSecret && legacyToken.value
      ? { oauthTokenSecret: legacyToken.value }
      : {})
  };
  const secretState = await ensureAgentSecrets(
    dataDir,
    seed,
    process.env.CTMCP_OAUTH_TOKEN_SECRET === undefined
  );
  let config = normalizeConfig(migration.document, secretState.secrets);
  validateConfig(config);
  config = { ...config, folders: await canonicalizeWorkspaceFolders(config.folders) };

  if (!input.exists || migration.changed) await writeConfigDocument(configPath, migration.document);
  if (legacyToken.value && secretState.secrets.oauthTokenSecret) {
    await rm(legacyToken.filePath, { force: true });
  }

  return {
    config,
    configPath,
    document: migration.document,
    secrets: secretState.secrets,
    secretStorePath: secretState.storePath,
    secretKeyPath: secretState.keyPath,
    migrationApplied: input.exists && migration.changed,
    ...(migration.changed ? { migratedFromSchema: migration.fromSchema } : {})
  };
}

export async function loadConfig(file?: string): Promise<AgentConfig> {
  return (await loadConfigBundle(file)).config;
}
