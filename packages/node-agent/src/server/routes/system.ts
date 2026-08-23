import type { IncomingMessage, ServerResponse } from 'node:http';
import type { AgentConfig, ToolContext } from '../../types.js';
import { AGENT_VERSION, BUILD_GIT_SHA, CLIENT_COMPAT_VERSION } from '../../version.js';
import { LATEST_MCP_PROTOCOL_VERSION, SUPPORTED_MCP_PROTOCOL_VERSIONS } from '../../mcpTransport.js';
import { sendJson } from '../../oauth.js';

interface ToolCatalogView {
  profile: AgentConfig['activeToolProfile'];
  tools: readonly unknown[];
  names: readonly string[];
  revision: string;
}

interface SystemRouteOptions {
  catalog: ToolCatalogView;
  config: AgentConfig;
  context: ToolContext;
  pathname: string;
  startedAt: number;
}

export function handleSystemRoute(
  req: IncomingMessage,
  res: ServerResponse,
  options: SystemRouteOptions
): boolean {
  const { catalog, config, context, pathname, startedAt } = options;

  if (pathname === '/health' && req.method === 'GET') {
    sendJson(res, 200, {
      ok: true,
      server: 'coding-tools-mcp-node',
      version: AGENT_VERSION,
      buildGitSha: BUILD_GIT_SHA,
      clientCompatVersion: CLIENT_COMPAT_VERSION,
      toolProfile: catalog.profile,
      toolsetRevision: catalog.revision,
      tools: catalog.tools.length,
      tunnel: context.tunnelStatus,
      headless: true,
      management: { enabled: config.management?.enabled === true }
    });
    return true;
  }

  if (pathname === '/mcp/info' && req.method === 'GET') {
    sendJson(res, 200, {
      name: 'coding-tools-mcp-node',
      version: AGENT_VERSION,
      buildGitSha: BUILD_GIT_SHA,
      clientCompatVersion: CLIENT_COMPAT_VERSION,
      protocolVersion: LATEST_MCP_PROTOCOL_VERSION,
      supportedProtocolVersions: SUPPORTED_MCP_PROTOCOL_VERSIONS,
      transport: 'streamable-http',
      toolProfile: catalog.profile,
      toolsetRevision: catalog.revision,
      tools: catalog.names,
      runtimeStartedAtMs: startedAt
    });
    return true;
  }

  return false;
}
