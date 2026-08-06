import { randomBytes } from 'node:crypto';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import type { AgentConfig, JsonObject, ToolContext } from './types.js';
import {
  resolveToolProfile, toolNamesForProfile, toolsForProfile, toolsetRevisionForProfile
} from './catalog.js';
import { callTool } from './tools.js';
import {
  authorizationMetadata, externalBase, OAuthRuntime,
  protectedResourceMetadataUrl, resourceMetadata, sendJson
} from './oauth.js';
import { KeyedMutex, Semaphore } from './runtime.js';
import { StateStore } from './state.js';
import {
  ConfigStore, handleManagementRequest,
  type WorkspaceManagementStore, type WorkspaceRuntimeRecord
} from './management.js';
import { AGENT_VERSION, CLIENT_COMPAT_VERSION } from './version.js';
import { defaultPolicy } from './policy.js';
import { disposeProcessSessions, ProcessRequestLifecycle } from './processes.js';
import { ToolUsageStore } from './toolUsage.js';
import { createFolderRuntime } from './folderRuntime.js';
import { canonicalizeWorkspaceFolders } from './workspace.js';
import { ConversationStore, deriveWorkspaceProfileId, markMcpConversationMetadata } from './conversation.js';
import { wrapMcpToolResult } from './toolContract.js';
import {
  LATEST_MCP_PROTOCOL_VERSION,
  MCP_STREAM_HEARTBEAT_INTERVAL_MS,
  SUPPORTED_MCP_PROTOCOL_VERSIONS,
  StreamingJsonResponse,
  sendMcpAccepted,
  sendMcpMethodNotAllowed,
  sendMcpTransportError,
  validateJsonRpcMessage,
  validateMcpConnection,
  type McpTransportIssue
} from './mcpTransport.js';

const supportedProtocols = new Set<string>(SUPPORTED_MCP_PROTOCOL_VERSIONS);

async function body(req: IncomingMessage, limit = 1024 * 1024): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    const value = Buffer.from(chunk);
    size += value.length;
    if (size > limit) throw new Error('request body too large');
    chunks.push(value);
  }
  return Buffer.concat(chunks);
}

function text(res: ServerResponse, status: number, value: string, type = 'text/plain; charset=utf-8'): void {
  res.writeHead(status, { 'content-type': type, 'cache-control': 'no-store', 'x-content-type-options': 'nosniff' }).end(value);
}

function routePrefix(config: AgentConfig): string {
  if (!config.publicBaseUrl) return '';
  try {
    const pathname = new URL(config.publicBaseUrl).pathname.replace(/\/$/, '');
    return pathname === '/' ? '' : pathname;
  } catch { return ''; }
}

function localPath(pathname: string, prefix: string): string {
  if (!prefix) return pathname;
  if (pathname === prefix) return '/';
  if (pathname.startsWith(`${prefix}/`)) return pathname.slice(prefix.length);
  return pathname;
}

function isAuthorizationMetadata(pathname: string, prefix: string): boolean {
  return pathname === '/.well-known/oauth-authorization-server'
    || (prefix !== '' && pathname === `/.well-known/oauth-authorization-server${prefix}`);
}

function isResourceMetadata(pathname: string, prefix: string): boolean {
  return pathname === '/.well-known/oauth-protected-resource'
    || pathname === '/.well-known/oauth-protected-resource/mcp'
    || (prefix !== '' && pathname === `/.well-known/oauth-protected-resource${prefix}/mcp`);
}

export async function createToolContext(config: AgentConfig): Promise<ToolContext> {
  config.policy ??= defaultPolicy();
  config.toolProfile ??= 'advanced';
  config.activeToolProfile ??= resolveToolProfile(config.toolProfile, config.permissionMode);
  const folders = await canonicalizeWorkspaceFolders(config.folders);
  config = { ...config, folders };
  const state = new StateStore(config.dataDir);
  await state.load();
  const usageStore = new ToolUsageStore(config.dataDir);
  const folderRuntimes = new Map(config.folders.map(folder => [folder.id, createFolderRuntime(config, folder)]));
  const firstRuntime = folderRuntimes.values().next().value;
  if (!firstRuntime) throw new Error('at least one workspace folder is required');
  const hubAdmission = {
    blocking: new Semaphore(config.limits.globalBlockingConcurrency ?? 1_024),
    process: new Semaphore(config.limits.globalProcessConcurrency ?? 512),
    locks: new KeyedMutex()
  };
  const conversations = new ConversationStore();
  return {
    config,
    conversations,
    workspaceProfileId: config.workspaceId ?? deriveWorkspaceProfileId(config.folders),
    selections: conversations.selectionMap,
    defaultCwds: conversations.cwdMap,
    folderRuntimes,
    hubAdmission,
    sessions: firstRuntime.sessions,
    operationsByFingerprint: firstRuntime.operationsByFingerprint,
    pendingOperations: firstRuntime.pendingOperations,
    editProposals: firstRuntime.editProposals,
    usage: [],
    usageStore,
    admission: firstRuntime.admission,
    state,
    tunnelStatus: config.tunnel?.enabled
      ? { enabled: true, state: 'stopped', publicUrl: config.tunnel.publicUrl, workers: 1, connectedWorkers: 0, completedRequests: 0 }
      : { enabled: false, state: 'disabled', workers: 0, connectedWorkers: 0, completedRequests: 0 }
  };
}

export interface AgentRuntime {
  server: Server;
  context: ToolContext;
  oauth: OAuthRuntime;
}

export interface AgentRuntimeOptions {
  configStore?: ConfigStore;
  mcpHeartbeatIntervalMs?: number;
  requestRestart?: () => void;
  workspaceStore?: WorkspaceManagementStore;
  runtimeRegistry?: Map<string, WorkspaceRuntimeRecord>;
}

function currentListenerPort(server: Server, fallback: number): number {
  const address = server.address();
  return address && typeof address === 'object' ? address.port : fallback;
}

function rpcErrorResponse(requestId: unknown, error: unknown): JsonObject {
  const code = typeof error === 'object' && error && 'rpcCode' in error
    ? Number((error as { rpcCode: number }).rpcCode)
    : -32603;
  const data = typeof error === 'object' && error && 'rpcData' in error
    ? (error as { rpcData: JsonObject }).rpcData
    : undefined;
  return {
    jsonrpc: '2.0',
    id: requestId,
    error: {
      code,
      message: error instanceof Error ? error.message : String(error),
      ...(data ? { data } : {})
    }
  };
}

export async function createAgentRuntime(config: AgentConfig, options: AgentRuntimeOptions = {}): Promise<AgentRuntime> {
  const context = await createToolContext(config);
  const oauth = new OAuthRuntime(config.oauth);
  const startedAt = Date.now();
  const workspaceId = config.workspaceId ?? context.workspaceProfileId;
  options.runtimeRegistry?.set(workspaceId, { context, startedAt });
  const adminToken = options.configStore ? randomBytes(32).toString('base64url') : '';
  const activeToolProfile = config.activeToolProfile;
  const exposedTools = toolsForProfile(activeToolProfile);
  const exposedToolNames = toolNamesForProfile(activeToolProfile);
  const exposedToolsetRevision = toolsetRevisionForProfile(activeToolProfile);
  const server = createServer(async (req, res) => {
    let requestId: unknown = null;
    let processLifecycle: ProcessRequestLifecycle | undefined;
    let stream: StreamingJsonResponse | undefined;
    let abortProcessLifecycle: (() => void) | undefined;
    let closeProcessLifecycle: (() => void) | undefined;
    const clearProcessLifecycleListeners = () => {
      if (abortProcessLifecycle) req.off('aborted', abortProcessLifecycle);
      if (closeProcessLifecycle) res.off('close', closeProcessLifecycle);
    };
    try {
      const base = externalBase(req.headers, config);
      const prefix = routePrefix(config);
      const url = new URL(req.url ?? '/', `http://${req.headers.host ?? `${config.host}:${config.port}`}`);
      const pathname = localPath(url.pathname, prefix);

      if (options.configStore && await handleManagementRequest(req, res, url.pathname, {
        configStore: options.configStore,
        context,
        startedAt,
        adminToken,
        requestRestart: options.requestRestart,
        workspaceStore: options.workspaceStore,
        runtimeRegistry: options.runtimeRegistry
      })) return;

      if (req.method === 'GET' && isAuthorizationMetadata(url.pathname, prefix)) return sendJson(res, 200, authorizationMetadata(base, oauth));
      if (req.method === 'GET' && isResourceMetadata(url.pathname, prefix)) return sendJson(res, 200, resourceMetadata(base));
      if (pathname === '/health' && req.method === 'GET') return sendJson(res, 200, { ok: true, server: 'coding-tools-mcp-node', version: AGENT_VERSION, clientCompatVersion: CLIENT_COMPAT_VERSION, toolProfile: activeToolProfile, toolsetRevision: exposedToolsetRevision, tools: exposedTools.length, tunnel: context.tunnelStatus, headless: true, management: { enabled: config.management?.enabled === true } });

      if (pathname === '/oauth/authorize' && req.method === 'GET') {
        const scoped = new URL(url.toString());
        scoped.pathname = '/oauth/authorize';
        const output = oauth.authorizePage(scoped);
        return text(res, output.status, output.body, 'text/html; charset=utf-8');
      }
      if (pathname === '/oauth/authorize' && req.method === 'POST') {
        const form = new URLSearchParams((await body(req, 8192)).toString());
        const output = oauth.authorizeSubmit(form, base);
        if (output.location) { res.writeHead(output.status, { location: output.location, 'cache-control': 'no-store' }).end(); return; }
        return text(res, output.status, output.body ?? 'Authorization failed', 'text/html; charset=utf-8');
      }
      if (pathname === '/oauth/token' && req.method === 'POST') {
        const output = oauth.exchangeToken(new URLSearchParams((await body(req, 8192)).toString()), req.headers, base);
        return sendJson(res, output.status, output.body);
      }
      if (pathname === '/mcp/info' && req.method === 'GET') {
        return sendJson(res, 200, {
          name: 'coding-tools-mcp-node',
          version: AGENT_VERSION,
          clientCompatVersion: CLIENT_COMPAT_VERSION,
          protocolVersion: LATEST_MCP_PROTOCOL_VERSION,
          supportedProtocolVersions: SUPPORTED_MCP_PROTOCOL_VERSIONS,
          transport: 'streamable-http',
          toolProfile: activeToolProfile,
          toolsetRevision: exposedToolsetRevision,
          tools: exposedToolNames
        });
      }
      if (pathname !== '/mcp') return text(res, 404, 'Not found');

      const connectionIssue = validateMcpConnection(
        req.headers,
        config,
        currentListenerPort(server, config.port)
      );
      if (connectionIssue) return sendMcpTransportError(res, connectionIssue);
      if (!oauth.verifyBearer(req.headers, base)) {
        res.writeHead(401, {
          'www-authenticate': `Bearer resource_metadata="${protectedResourceMetadataUrl(base)}"`,
          'cache-control': 'no-store'
        }).end('Unauthorized');
        return;
      }
      if (req.method !== 'POST') return sendMcpMethodNotAllowed(res);

      let parsed: unknown;
      try {
        parsed = JSON.parse((await body(req)).toString());
      } catch (error) {
        const tooLarge = error instanceof Error && error.message === 'request body too large';
        const issue: McpTransportIssue = tooLarge
          ? { status: 400, code: -32600, message: 'request body too large' }
          : { status: 400, code: -32700, message: 'Parse error' };
        return sendMcpTransportError(res, issue);
      }
      const validated = validateJsonRpcMessage(parsed);
      if ('status' in validated) return sendMcpTransportError(res, validated);
      const request = validated.body;
      requestId = validated.id;
      const method = validated.method ?? '';
      if (validated.kind === 'response') return sendMcpAccepted(res);

      const fastPath = method === 'initialize'
        || method === 'ping'
        || method === 'tools/list'
        || method.startsWith('notifications/');
      if (validated.kind === 'request' && !fastPath) {
        stream = new StreamingJsonResponse(
          res,
          options.mcpHeartbeatIntervalMs ?? MCP_STREAM_HEARTBEAT_INTERVAL_MS
        );
      }

      let rpcResponse: JsonObject;
      try {
        let result: unknown;
        if (method === 'initialize') {
          const params = (request.params ?? {}) as JsonObject;
          const requested = String(params.protocolVersion ?? '');
          result = {
            protocolVersion: supportedProtocols.has(requested)
              ? requested
              : LATEST_MCP_PROTOCOL_VERSION,
            capabilities: { tools: { listChanged: false }, logging: {} },
            serverInfo: { name: 'coding-tools-mcp-node', title: 'Coding Tools MCP Node Agent', version: AGENT_VERSION, toolsetRevision: exposedToolsetRevision },
            instructions: 'Call list_workspace_folders, switch_workspace_folder, then history_session_bootstrap before project tools. FRP and Cloudflare transports are intentionally unsupported.'
          };
        } else if (method === 'ping') result = {};
        else if (method === 'tools/list') result = { tools: exposedTools, toolsetRevision: exposedToolsetRevision };
        else if (method === 'tools/call') {
          const params = (request.params ?? {}) as JsonObject;
          const name = String(params.name ?? '');
          if (!exposedToolNames.includes(name)) throw Object.assign(new Error(`Unknown tool: ${name}`), {
            rpcCode: -32602,
            rpcData: {
              reason: 'unknown_tool',
              error_code: 'UNKNOWN_TOOL',
              error_category: 'catalog',
              retryable: true,
              suggestion: 'Refresh tools/list and retry with the current tool catalog.',
              toolset_revision: exposedToolsetRevision,
              available_tools: exposedToolNames
            }
          });
          processLifecycle = new ProcessRequestLifecycle(context);
          abortProcessLifecycle = () => processLifecycle?.abort();
          closeProcessLifecycle = () => { if (!res.writableEnded) processLifecycle?.abort(); };
          req.once('aborted', abortProcessLifecycle);
          res.once('close', closeProcessLifecycle);
          const structured = await callTool(
            context,
            name,
            (params.arguments ?? {}) as JsonObject,
            markMcpConversationMetadata(params._meta),
            false,
            processLifecycle
          );
          result = wrapMcpToolResult(name, (params.arguments ?? {}) as JsonObject, structured);
        } else throw Object.assign(new Error(`Method not found: ${method}`), { rpcCode: -32601 });
        rpcResponse = { jsonrpc: '2.0', id: requestId, result };
        processLifecycle?.complete();
      } catch (error) {
        processLifecycle?.abort();
        rpcResponse = rpcErrorResponse(requestId, error);
      } finally {
        clearProcessLifecycleListeners();
      }

      if (validated.kind === 'notification') return sendMcpAccepted(res);
      if (stream) {
        stream.finish(rpcResponse);
        return;
      }
      return sendJson(res, 200, rpcResponse);
    } catch (error) {
      processLifecycle?.abort();
      clearProcessLifecycleListeners();
      const response = rpcErrorResponse(requestId, error);
      if (stream) {
        stream.finish(response);
        return;
      }
      if (!res.headersSent) return sendJson(res, 200, response);
      res.destroy();
    }
  });
  server.once('close', () => {
    if (options.runtimeRegistry?.get(workspaceId)?.context === context) options.runtimeRegistry.delete(workspaceId);
    oauth.dispose();
    void (async () => {
      await disposeProcessSessions(context);
      await context.usageStore.flush();
    })();
  });
  return { server, context, oauth };
}

export async function createAgentServer(config: AgentConfig, options: AgentRuntimeOptions = {}): Promise<Server> {
  return (await createAgentRuntime(config, options)).server;
}
