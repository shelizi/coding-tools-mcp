import { createHash } from 'node:crypto';
import { lstat, readFile, realpath } from 'node:fs/promises';
import { homedir } from 'node:os';
import path from 'node:path';
import { parse as parseToml } from 'smol-toml';
import type { JsonObject, WorkspaceFolder } from '../types.js';
import type {
  ExtensionDiagnostic,
  ExtensionProvider,
  ExtensionScope,
  HookDescriptor,
  McpServerDescriptor,
  McpTransport
} from './types.js';

const MAX_EXTENSION_CONFIG_BYTES = 1024 * 1024;
const SUPPORTED_HOOK_EVENTS = new Set([
  'SessionStart',
  'SessionEnd',
  'PreToolUse',
  'PostToolUse',
  'PostToolUseFailure'
]);

export interface ExtensionDiscoveryOptions {
  folders: readonly WorkspaceFolder[];
  homeDir?: string | null;
}

export interface DiscoveredExtensions {
  hooks: HookDescriptor[];
  mcpServers: McpServerDescriptor[];
  diagnostics: ExtensionDiagnostic[];
  scannedAtMs: number;
}

function object(value: unknown): JsonObject {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonObject : {};
}

function array(value: unknown): unknown[] {
  return Array.isArray(value) ? value : [];
}

function stringArray(value: unknown): string[] {
  return array(value).map(String).map(item => item.trim()).filter(Boolean);
}

function stringRecord(value: unknown): Record<string, string> {
  return Object.fromEntries(Object.entries(object(value)).map(([key, raw]) => [key, String(raw)]));
}

function inside(root: string, candidate: string): boolean {
  const relative = path.relative(root, candidate);
  return relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative));
}

function displayPath(actual: string, scope: ExtensionScope, base: string): string {
  if (scope === 'user') {
    const relative = path.relative(base, actual).split(path.sep).join('/');
    return relative ? `~/${relative}` : '~';
  }
  return path.relative(base, actual).split(path.sep).join('/');
}

function key(parts: readonly unknown[]): string {
  const prefix = parts
    .slice(0, 4)
    .map(value => String(value ?? '').replace(/[^A-Za-z0-9._-]+/g, '_'))
    .join(':')
    .slice(0, 80);
  return `${prefix}:${createHash('sha256').update(JSON.stringify(parts)).digest('hex').slice(0, 16)}`;
}

async function safeRead(
  file: string,
  containmentRoot: string,
  display: string,
  diagnostics: ExtensionDiagnostic[],
  provider: ExtensionProvider,
  scope: ExtensionScope
): Promise<string | undefined> {
  try {
    const info = await lstat(file);
    if (!info.isFile() || info.isSymbolicLink()) {
      diagnostics.push({ code: 'EXTENSION_CONFIG_SKIPPED', message: 'Extension config must be a regular non-symlink file.', provider, scope, path: display });
      return undefined;
    }
    if (info.size > MAX_EXTENSION_CONFIG_BYTES) {
      diagnostics.push({ code: 'EXTENSION_CONFIG_TOO_LARGE', message: `Extension config exceeds ${MAX_EXTENSION_CONFIG_BYTES} bytes.`, provider, scope, path: display });
      return undefined;
    }
    const resolved = await realpath(file);
    if (!inside(containmentRoot, resolved)) {
      diagnostics.push({ code: 'EXTENSION_CONFIG_OUTSIDE_SCOPE', message: 'Resolved extension config escapes its allowed scope.', provider, scope, path: display });
      return undefined;
    }
    return readFile(resolved, 'utf8');
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    const code = (error as NodeJS.ErrnoException).code ?? 'UNKNOWN';
    diagnostics.push({ code: 'EXTENSION_CONFIG_READ_FAILED', message: `Failed to read extension configuration (${code}).`, provider, scope, path: display });
    return undefined;
  }
}

async function readJson(
  file: string,
  containmentRoot: string,
  display: string,
  diagnostics: ExtensionDiagnostic[],
  provider: ExtensionProvider,
  scope: ExtensionScope
): Promise<JsonObject | undefined> {
  const content = await safeRead(file, containmentRoot, display, diagnostics, provider, scope);
  if (content === undefined) return undefined;
  try { return object(JSON.parse(content)); }
  catch (error) {
    diagnostics.push({ code: 'EXTENSION_CONFIG_INVALID_JSON', message: 'Invalid JSON extension configuration.', provider, scope, path: display });
    return undefined;
  }
}

async function readToml(
  file: string,
  containmentRoot: string,
  display: string,
  diagnostics: ExtensionDiagnostic[],
  provider: ExtensionProvider,
  scope: ExtensionScope
): Promise<JsonObject | undefined> {
  const content = await safeRead(file, containmentRoot, display, diagnostics, provider, scope);
  if (content === undefined) return undefined;
  try { return object(parseToml(content)); }
  catch (error) {
    diagnostics.push({ code: 'EXTENSION_CONFIG_INVALID_TOML', message: 'Invalid TOML extension configuration.', provider, scope, path: display });
    return undefined;
  }
}

function timeoutMs(handler: JsonObject): number {
  const explicit = Number(handler.timeout_ms ?? handler.timeoutMs);
  if (Number.isFinite(explicit) && explicit > 0) return Math.min(Math.round(explicit), 120_000);
  const seconds = Number(handler.timeout);
  return Number.isFinite(seconds) && seconds > 0 ? Math.min(Math.round(seconds * 1000), 120_000) : 10_000;
}

function hookHandler(
  provider: ExtensionProvider,
  scope: ExtensionScope,
  folderId: string | undefined,
  sourcePath: string,
  sourceEnabled: boolean,
  event: string,
  matcher: string | undefined,
  handler: JsonObject,
  groupIndex: number,
  handlerIndex: number
): HookDescriptor {
  const handlerType = String(handler.type ?? 'command').trim().toLowerCase();
  const command = process.platform === 'win32' && provider === 'codex'
    ? String(handler.commandWindows ?? handler.command ?? '').trim()
    : String(handler.command ?? '').trim();
  const url = String(handler.url ?? '').trim() || undefined;
  const handlerSupported = handlerType === 'command' ? Boolean(command) : handlerType === 'http' ? Boolean(url) : false;
  const supported = handlerSupported && SUPPORTED_HOOK_EVENTS.has(event);
  return {
    kind: 'hook',
    key: key(['hook', provider, scope, folderId ?? '', sourcePath, event, groupIndex, handlerIndex, matcher ?? '', handlerType, command, url]),
    provider, scope, folderId, event, matcher, handlerType,
    command: command || undefined,
    args: stringArray(handler.args),
    url,
    timeoutMs: timeoutMs(handler),
    sourcePath,
    sourceEnabled,
    supported
  };
}

function hooksFromDocument(
  document: JsonObject,
  provider: ExtensionProvider,
  scope: ExtensionScope,
  folderId: string | undefined,
  sourcePath: string,
  sourceEnabled: boolean,
  diagnostics: ExtensionDiagnostic[]
): HookDescriptor[] {
  const result: HookDescriptor[] = [];
  const hooks = object(document.hooks);
  for (const [event, rawGroups] of Object.entries(hooks)) {
    const groups = Array.isArray(rawGroups) ? rawGroups : [rawGroups];
    groups.forEach((rawGroup, groupIndex) => {
      const group = object(rawGroup);
      const matcher = String(group.matcher ?? '').trim() || undefined;
      const handlers = Array.isArray(group.hooks) ? group.hooks : group.command || group.type ? [group] : [];
      handlers.forEach((rawHandler, handlerIndex) => {
        const descriptor = hookHandler(provider, scope, folderId, sourcePath, sourceEnabled, event, matcher, object(rawHandler), groupIndex, handlerIndex);
        if (!SUPPORTED_HOOK_EVENTS.has(event)) diagnostics.push({
          code: 'HOOK_EVENT_UNSUPPORTED',
          message: `Hook event ${event || 'unknown'} is discoverable but not executable by Node Agent.`,
          provider, scope, path: sourcePath, key: descriptor.key
        });
        else if (!descriptor.supported) diagnostics.push({
          code: 'HOOK_HANDLER_UNSUPPORTED',
          message: `Hook handler type ${descriptor.handlerType || 'unknown'} is discoverable but not executable by Node Agent.`,
          provider, scope, path: sourcePath, key: descriptor.key
        });
        result.push(descriptor);
      });
    });
  }
  return result;
}

function expandEnv(value: string): string {
  return value.replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}/g, (_match, name: string, fallback: string | undefined) => process.env[name] ?? fallback ?? '');
}

function mcpTransport(config: JsonObject): McpTransport {
  const explicit = String(config.type ?? '').trim().toLowerCase();
  if (explicit === 'stdio' || explicit === 'http' || explicit === 'sse' || explicit === 'ws') return explicit;
  if (config.command) return 'stdio';
  if (config.url) return 'http';
  return 'unknown';
}

function mcpDescriptor(
  provider: ExtensionProvider,
  scope: ExtensionScope,
  folderId: string,
  sourcePath: string,
  name: string,
  raw: unknown,
  sourceEnabled: boolean
): McpServerDescriptor {
  const config = object(raw);
  const transport = mcpTransport(config);
  const command = String(config.command ?? '').trim() || undefined;
  const url = String(config.url ?? '').trim();
  const env = Object.fromEntries(Object.entries(stringRecord(config.env)).map(([envName, value]) => [envName, expandEnv(value)]));
  const headers = Object.fromEntries(Object.entries(stringRecord(config.http_headers ?? config.headers)).map(([header, value]) => [header, expandEnv(value)]));
  return {
    kind: 'mcp',
    key: key(['mcp', provider, scope, folderId, sourcePath, name]),
    provider, scope, folderId, name,
    transport,
    command,
    args: stringArray(config.args).map(expandEnv),
    env,
    envVars: stringArray(config.env_vars ?? config.envVars),
    cwd: String(config.cwd ?? '').trim() || undefined,
    url: url ? expandEnv(url) : undefined,
    headers,
    envHeaders: stringRecord(config.env_http_headers ?? config.envHeaders),
    bearerTokenEnvVar: String(config.bearer_token_env_var ?? '').trim() || undefined,
    sourcePath,
    sourceEnabled: sourceEnabled && config.enabled !== false,
    supported: transport === 'stdio' ? Boolean(command) : transport === 'http' ? Boolean(url) : false
  };
}

function collectMcp(
  target: McpServerDescriptor[],
  provider: ExtensionProvider,
  scope: ExtensionScope,
  folderId: string,
  sourcePath: string,
  servers: unknown,
  sourceEnabled: boolean,
  diagnostics: ExtensionDiagnostic[]
): void {
  for (const [name, raw] of Object.entries(object(servers))) {
    const server = mcpDescriptor(provider, scope, folderId, sourcePath, name, raw, sourceEnabled);
    if (!server.supported) diagnostics.push({
      code: 'MCP_TRANSPORT_UNSUPPORTED',
      message: `MCP server ${name} uses unsupported or incomplete transport ${server.transport}. Node Agent currently proxies stdio and streamable HTTP.`,
      provider, scope, path: sourcePath, key: server.key
    });
    target.push(server);
  }
}

function samePath(left: string, right: string): boolean {
  const a = path.resolve(left);
  const b = path.resolve(right);
  return process.platform === 'win32' ? a.toLowerCase() === b.toLowerCase() : a === b;
}

function precedence(scope: ExtensionScope): number {
  return scope === 'local' ? 0 : scope === 'workspace' ? 10 : 20;
}

function dedupeMcp(candidates: McpServerDescriptor[], diagnostics: ExtensionDiagnostic[]): McpServerDescriptor[] {
  const sorted = [...candidates].sort((left, right) => precedence(left.scope) - precedence(right.scope) || left.sourcePath.localeCompare(right.sourcePath));
  const selected = new Map<string, McpServerDescriptor>();
  for (const server of sorted) {
    const identity = `${server.provider}:${server.folderId ?? ''}:${server.name.toLocaleLowerCase('en-US')}`;
    const existing = selected.get(identity);
    if (!existing) selected.set(identity, server);
    else diagnostics.push({
      code: 'MCP_SERVER_SHADOWED',
      message: `${server.sourcePath} server ${server.name} is shadowed by ${existing.sourcePath}.`,
      provider: server.provider, scope: server.scope, path: server.sourcePath, key: server.key
    });
  }
  return [...selected.values()].sort((left, right) => left.name.localeCompare(right.name) || left.key.localeCompare(right.key));
}

export async function discoverExtensions(options: ExtensionDiscoveryOptions): Promise<DiscoveredExtensions> {
  const diagnostics: ExtensionDiagnostic[] = [];
  const hooks: HookDescriptor[] = [];
  const mcpCandidates: McpServerDescriptor[] = [];
  const home = options.homeDir === null ? undefined : path.resolve(options.homeDir ?? homedir());
  let homeReal: string | undefined;
  if (home) {
    try { homeReal = await realpath(home); }
    catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') diagnostics.push({ code: 'EXTENSION_USER_HOME_FAILED', message: 'Failed to resolve the user home for extension discovery.', scope: 'user', path: '~' });
    }
  }

  let claudeUserSettings: JsonObject = {};
  let claudeUserRoot: JsonObject = {};
  let codexUserConfig: JsonObject = {};
  if (home && homeReal) {
    const claudeSettingsPath = path.join(home, '.claude', 'settings.json');
    claudeUserSettings = await readJson(claudeSettingsPath, homeReal, '~/.claude/settings.json', diagnostics, 'claude', 'user') ?? {};
    hooks.push(...hooksFromDocument(claudeUserSettings, 'claude', 'user', undefined, '~/.claude/settings.json', claudeUserSettings.disableAllHooks !== true, diagnostics));
    const claudeRootPath = path.join(home, '.claude.json');
    claudeUserRoot = await readJson(claudeRootPath, homeReal, '~/.claude.json', diagnostics, 'claude', 'user') ?? {};

    const codexHooksPath = path.join(home, '.codex', 'hooks.json');
    const codexHooks = await readJson(codexHooksPath, homeReal, '~/.codex/hooks.json', diagnostics, 'codex', 'user') ?? {};
    const codexConfigPath = path.join(home, '.codex', 'config.toml');
    codexUserConfig = await readToml(codexConfigPath, homeReal, '~/.codex/config.toml', diagnostics, 'codex', 'user') ?? {};
    const codexHooksEnabled = object(codexUserConfig.features).hooks !== false;
    hooks.push(...hooksFromDocument(codexHooks, 'codex', 'user', undefined, '~/.codex/hooks.json', codexHooksEnabled, diagnostics));
    hooks.push(...hooksFromDocument(codexUserConfig, 'codex', 'user', undefined, '~/.codex/config.toml', codexHooksEnabled, diagnostics));
  }

  for (const folder of options.folders) {
    let workspaceReal: string;
    try { workspaceReal = await realpath(folder.path); }
    catch (error) {
      diagnostics.push({ code: 'EXTENSION_WORKSPACE_FAILED', message: error instanceof Error ? error.message : String(error), scope: 'workspace', path: folder.path });
      continue;
    }

    if (home && homeReal) {
      collectMcp(mcpCandidates, 'claude', 'user', folder.id, '~/.claude.json', claudeUserRoot.mcpServers, true, diagnostics);
      const projects = object(claudeUserRoot.projects);
      const localProject = Object.entries(projects).find(([projectPath]) => samePath(projectPath, workspaceReal));
      if (localProject) {
        collectMcp(mcpCandidates, 'claude', 'local', folder.id, '~/.claude.json (project-local)', object(localProject[1]).mcpServers, true, diagnostics);
      }
      collectMcp(mcpCandidates, 'codex', 'user', folder.id, '~/.codex/config.toml', codexUserConfig.mcp_servers, true, diagnostics);
    }

    const claudeProjectSettingsPath = path.join(folder.path, '.claude', 'settings.json');
    const claudeLocalSettingsPath = path.join(folder.path, '.claude', 'settings.local.json');
    const claudeProjectSettings = await readJson(claudeProjectSettingsPath, workspaceReal, displayPath(claudeProjectSettingsPath, 'workspace', folder.path), diagnostics, 'claude', 'workspace') ?? {};
    const claudeLocalSettings = await readJson(claudeLocalSettingsPath, workspaceReal, displayPath(claudeLocalSettingsPath, 'local', folder.path), diagnostics, 'claude', 'local') ?? {};
    hooks.push(...hooksFromDocument(claudeProjectSettings, 'claude', 'workspace', folder.id, '.claude/settings.json', claudeProjectSettings.disableAllHooks !== true && claudeUserSettings.disableAllHooks !== true, diagnostics));
    hooks.push(...hooksFromDocument(claudeLocalSettings, 'claude', 'local', folder.id, '.claude/settings.local.json', claudeLocalSettings.disableAllHooks !== true && claudeUserSettings.disableAllHooks !== true, diagnostics));

    const disabledProjectServers = new Set([
      ...stringArray(claudeUserSettings.disabledMcpjsonServers),
      ...stringArray(claudeProjectSettings.disabledMcpjsonServers),
      ...stringArray(claudeLocalSettings.disabledMcpjsonServers)
    ].map(value => value.toLocaleLowerCase('en-US')));
    const mcpJsonPath = path.join(folder.path, '.mcp.json');
    const mcpJson = await readJson(mcpJsonPath, workspaceReal, '.mcp.json', diagnostics, 'claude', 'workspace') ?? {};
    for (const [name, raw] of Object.entries(object(mcpJson.mcpServers))) {
      collectMcp(mcpCandidates, 'claude', 'workspace', folder.id, '.mcp.json', { [name]: raw }, !disabledProjectServers.has(name.toLocaleLowerCase('en-US')), diagnostics);
    }

    const codexHooksPath = path.join(folder.path, '.codex', 'hooks.json');
    const codexHooks = await readJson(codexHooksPath, workspaceReal, '.codex/hooks.json', diagnostics, 'codex', 'workspace') ?? {};
    const codexConfigPath = path.join(folder.path, '.codex', 'config.toml');
    const codexProjectConfig = await readToml(codexConfigPath, workspaceReal, '.codex/config.toml', diagnostics, 'codex', 'workspace') ?? {};
    const codexHooksEnabled = object(codexProjectConfig.features).hooks !== false && object(codexUserConfig.features).hooks !== false;
    hooks.push(...hooksFromDocument(codexHooks, 'codex', 'workspace', folder.id, '.codex/hooks.json', codexHooksEnabled, diagnostics));
    hooks.push(...hooksFromDocument(codexProjectConfig, 'codex', 'workspace', folder.id, '.codex/config.toml', codexHooksEnabled, diagnostics));
    collectMcp(mcpCandidates, 'codex', 'workspace', folder.id, '.codex/config.toml', codexProjectConfig.mcp_servers, true, diagnostics);
  }

  return {
    hooks: hooks.sort((left, right) => left.event.localeCompare(right.event) || left.key.localeCompare(right.key)),
    mcpServers: dedupeMcp(mcpCandidates, diagnostics),
    diagnostics,
    scannedAtMs: Date.now()
  };
}
