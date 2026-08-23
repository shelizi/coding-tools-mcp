import type { JsonObject, ToolDefinition } from '../types.js';

export type ExtensionProvider = 'codex' | 'claude';
export type ExtensionScope = 'workspace' | 'local' | 'user';
export type ExtensionKind = 'hook' | 'mcp';

export interface ExtensionDiagnostic {
  code: string;
  message: string;
  provider?: ExtensionProvider;
  scope?: ExtensionScope;
  path?: string;
  key?: string;
}

export interface HookDescriptor {
  kind: 'hook';
  key: string;
  provider: ExtensionProvider;
  scope: ExtensionScope;
  folderId?: string;
  event: string;
  matcher?: string;
  handlerType: string;
  command?: string;
  args?: string[];
  url?: string;
  timeoutMs: number;
  sourcePath: string;
  sourceEnabled: boolean;
  supported: boolean;
}

export type McpTransport = 'stdio' | 'http' | 'sse' | 'ws' | 'unknown';

export interface McpServerDescriptor {
  kind: 'mcp';
  key: string;
  provider: ExtensionProvider;
  scope: ExtensionScope;
  folderId?: string;
  name: string;
  transport: McpTransport;
  command?: string;
  args: string[];
  env: Record<string, string>;
  envVars: string[];
  cwd?: string;
  url?: string;
  headers: Record<string, string>;
  envHeaders: Record<string, string>;
  bearerTokenEnvVar?: string;
  sourcePath: string;
  sourceEnabled: boolean;
  supported: boolean;
}

export interface HookInventoryItem {
  hook: HookDescriptor;
  selected: boolean;
  enabled: boolean;
}

export interface McpInventoryItem {
  server: McpServerDescriptor;
  selected: boolean;
  enabled: boolean;
  connected: boolean;
  toolCount: number;
  error?: string;
}

export interface ExtensionInventorySnapshot {
  hooks: HookInventoryItem[];
  mcpServers: McpInventoryItem[];
  diagnostics: ExtensionDiagnostic[];
  scannedAtMs: number;
}

export interface ExternalMcpTool {
  name: string;
  logicalName: string;
  serverKey: string;
  toolName: string;
  definition: ToolDefinition;
}

export interface HookPreResult {
  input: JsonObject;
  blocked?: { message: string; hookKey: string };
  context: string[];
}

export interface HookPostResult {
  feedback: string[];
}
