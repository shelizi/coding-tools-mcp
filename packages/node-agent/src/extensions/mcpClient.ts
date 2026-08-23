import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { createHash } from 'node:crypto';
import path from 'node:path';
import type { JsonObject, ToolDefinition } from '../types.js';
import type { ExternalMcpTool, McpServerDescriptor } from './types.js';

interface PendingRequest {
  resolve(value: JsonObject): void;
  reject(error: Error): void;
  timer: NodeJS.Timeout;
}

function record(value: unknown): JsonObject {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonObject : {};
}

function sanitized(value: string): string {
  return value.replace(/[^A-Za-z0-9_-]+/g, '_').replace(/^_+|_+$/g, '').slice(0, 40) || 'item';
}

function proxyToolName(server: McpServerDescriptor, toolName: string): string {
  const scope = server.folderId ? sanitized(server.folderId) : server.scope;
  const base = `mcp__${server.provider}__${scope}__${sanitized(server.name)}__${sanitized(toolName)}`;
  if (base.length <= 120) return base;
  return `${base.slice(0, 103)}_${createHash('sha256').update(base).digest('hex').slice(0, 16)}`;
}

function logicalToolName(server: McpServerDescriptor, toolName: string): string {
  return `mcp__${sanitized(server.name)}__${sanitized(toolName)}`;
}

function environment(server: McpServerDescriptor): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, ...server.env };
  for (const name of server.envVars) {
    if (process.env[name] !== undefined) env[name] = process.env[name];
  }
  return env;
}

export class ExternalMcpConnection {
  private process?: ChildProcessWithoutNullStreams;
  private processUsesWindowsShell = false;
  private readonly pending = new Map<number, PendingRequest>();
  private nextId = 1;
  private readBuffer = '';
  private sessionId?: string;
  private initialized = false;
  private tools: ExternalMcpTool[] = [];
  private errorMessage?: string;

  constructor(readonly server: McpServerDescriptor, readonly workspaceRoot?: string) {}

  get connected(): boolean { return this.initialized; }
  get error(): string | undefined { return this.errorMessage; }
  get toolDefinitions(): readonly ExternalMcpTool[] { return this.tools; }

  private async ensureInitialized(): Promise<void> {
    if (this.initialized) return;
    try {
      const initialize = await this.request('initialize', {
        protocolVersion: '2025-11-25',
        capabilities: {},
        clientInfo: { name: 'coding-tools-mcp-node', version: 'extension-proxy' }
      });
      if (!initialize) throw new Error('MCP server returned no initialize result');
      await this.notify('notifications/initialized', {});
      const listed = await this.request('tools/list', {});
      const rawTools = Array.isArray(listed.tools) ? listed.tools : [];
      this.tools = rawTools.flatMap(raw => {
        const tool = record(raw);
        const name = String(tool.name ?? '').trim();
        if (!name) return [];
        const definition: ToolDefinition = {
          name: proxyToolName(this.server, name),
          title: String(tool.title ?? `${this.server.name}: ${name}`),
          description: String(tool.description ?? `Tool ${name} from external MCP server ${this.server.name}.`),
          inputSchema: record(tool.inputSchema ?? { type: 'object', properties: {} }),
          annotations: {
            title: String(record(tool.annotations).title ?? tool.title ?? name),
            readOnlyHint: record(tool.annotations).readOnlyHint === true,
            destructiveHint: record(tool.annotations).destructiveHint !== false,
            idempotentHint: record(tool.annotations).idempotentHint === true,
            openWorldHint: record(tool.annotations).openWorldHint !== false
          }
        };
        return [{ name: definition.name, logicalName: logicalToolName(this.server, name), serverKey: this.server.key, toolName: name, definition }];
      });
      this.initialized = true;
      this.errorMessage = undefined;
    } catch (error) {
      this.errorMessage = error instanceof Error ? error.message : String(error);
      await this.close();
      throw error;
    }
  }

  async refreshTools(): Promise<readonly ExternalMcpTool[]> {
    await this.ensureInitialized();
    return this.tools;
  }

  async call(toolName: string, args: JsonObject): Promise<JsonObject> {
    await this.ensureInitialized();
    return this.request('tools/call', { name: toolName, arguments: args });
  }

  private async request(method: string, params: JsonObject): Promise<JsonObject> {
    const id = this.nextId++;
    if (this.server.transport === 'stdio') return this.stdioRequest(id, method, params);
    if (this.server.transport === 'http') return this.httpRequest(id, method, params);
    throw new Error(`Unsupported MCP transport: ${this.server.transport}`);
  }

  private async notify(method: string, params: JsonObject): Promise<void> {
    const payload = { jsonrpc: '2.0', method, params };
    if (this.server.transport === 'stdio') {
      await this.ensureProcess();
      this.process!.stdin.write(`${JSON.stringify(payload)}\n`);
      return;
    }
    if (this.server.transport === 'http') {
      await this.httpExchange(payload);
      return;
    }
  }

  private async ensureProcess(): Promise<void> {
    if (this.process && !this.process.killed && this.process.exitCode === null) return;
    if (!this.server.command) throw new Error('MCP stdio server is missing command');
    const cwd = this.server.cwd
      ? path.resolve(this.workspaceRoot ?? process.cwd(), this.server.cwd)
      : this.workspaceRoot ?? process.cwd();
    const commandExtension = process.platform === 'win32' ? path.extname(this.server.command).toLowerCase() : '';
    const requiresShell = commandExtension === '.bat' || commandExtension === '.cmd';
    const command = requiresShell ? (process.env.ComSpec || 'cmd.exe') : this.server.command;
    const args = requiresShell
      ? ['/d', '/s', '/c', 'call', this.server.command, ...this.server.args]
      : this.server.args;
    const child = spawn(command, args, {
      cwd,
      env: environment(this.server),
      shell: false,
      stdio: ['pipe', 'pipe', 'pipe'],
      windowsHide: true
    });
    this.process = child;
    this.processUsesWindowsShell = requiresShell;
    child.stdout.setEncoding('utf8');
    child.stdout.on('data', chunk => this.consumeStdout(String(chunk)));
    child.stderr.on('data', () => {});
    child.once('error', error => this.rejectAll(error));
    child.once('exit', code => this.rejectAll(new Error(`MCP server exited with code ${code ?? 'unknown'}`)));
    await new Promise<void>((resolve, reject) => {
      child.once('spawn', resolve);
      child.once('error', reject);
    });
  }

  private consumeStdout(chunk: string): void {
    this.readBuffer += chunk;
    while (true) {
      const newline = this.readBuffer.indexOf('\n');
      if (newline < 0) return;
      const line = this.readBuffer.slice(0, newline).trim();
      this.readBuffer = this.readBuffer.slice(newline + 1);
      if (!line) continue;
      let message: JsonObject;
      try { message = record(JSON.parse(line)); } catch { continue; }
      const id = Number(message.id);
      const pending = this.pending.get(id);
      if (!pending) continue;
      clearTimeout(pending.timer);
      this.pending.delete(id);
      if (message.error) pending.reject(new Error(String(record(message.error).message ?? 'MCP request failed')));
      else pending.resolve(record(message.result));
    }
  }

  private async stdioRequest(id: number, method: string, params: JsonObject): Promise<JsonObject> {
    await this.ensureProcess();
    return new Promise<JsonObject>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`MCP request timed out: ${method}`));
      }, 20_000);
      this.pending.set(id, { resolve, reject, timer });
      this.process!.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id, method, params })}\n`);
    });
  }

  private headers(): Record<string, string> {
    const headers: Record<string, string> = { accept: 'application/json, text/event-stream', 'content-type': 'application/json', ...this.server.headers };
    for (const [header, envName] of Object.entries(this.server.envHeaders)) {
      const value = process.env[envName];
      if (value !== undefined) headers[header] = value;
    }
    if (this.server.bearerTokenEnvVar) {
      const token = process.env[this.server.bearerTokenEnvVar];
      if (token) headers.authorization = `Bearer ${token}`;
    }
    if (this.sessionId) headers['mcp-session-id'] = this.sessionId;
    return headers;
  }

  private async httpRequest(id: number, method: string, params: JsonObject): Promise<JsonObject> {
    return record((await this.httpExchange({ jsonrpc: '2.0', id, method, params })).result);
  }

  private async httpExchange(payload: JsonObject): Promise<JsonObject> {
    if (!this.server.url) throw new Error('MCP HTTP server is missing URL');
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 20_000);
    try {
      const response = await fetch(this.server.url, { method: 'POST', headers: this.headers(), body: JSON.stringify(payload), signal: controller.signal });
      const session = response.headers.get('mcp-session-id');
      if (session) this.sessionId = session;
      if (!response.ok) throw new Error(`MCP HTTP ${response.status}`);
      if (response.status === 202) return {};
      const text = await response.text();
      const contentType = response.headers.get('content-type') ?? '';
      let message: JsonObject;
      if (contentType.includes('text/event-stream')) {
        const data = text.split(/\r?\n/).filter(line => line.startsWith('data:')).map(line => line.slice(5).trim()).filter(Boolean).at(-1);
        message = data ? record(JSON.parse(data)) : {};
      } else {
        message = text.trim() ? record(JSON.parse(text)) : {};
      }
      if (message.error) throw new Error(String(record(message.error).message ?? 'MCP request failed'));
      return message;
    } finally {
      clearTimeout(timer);
    }
  }

  private rejectAll(error: Error): void {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
    this.initialized = false;
  }

  async close(): Promise<void> {
    this.initialized = false;
    this.tools = [];
    this.sessionId = undefined;
    const child = this.process;
    const usesWindowsShell = this.processUsesWindowsShell;
    this.process = undefined;
    this.processUsesWindowsShell = false;
    if (!child || child.exitCode !== null) return;
    if (process.platform === 'win32' && usesWindowsShell && child.pid) {
      await new Promise<void>(resolve => {
        const killer = spawn('taskkill.exe', ['/pid', String(child.pid), '/t', '/f'], {
          stdio: 'ignore',
          windowsHide: true
        });
        let settled = false;
        const finish = () => {
          if (settled) return;
          settled = true;
          resolve();
        };
        killer.once('error', () => {
          child.kill();
          finish();
        });
        killer.once('exit', code => {
          if (code !== 0 && child.exitCode === null) child.kill();
          finish();
        });
        setTimeout(() => {
          if (child.exitCode === null) child.kill();
          finish();
        }, 1_000).unref();
      });
    } else {
      child.kill();
    }
    if (child.exitCode !== null) return;
    await new Promise<void>(resolve => {
      const timer = setTimeout(resolve, 1_000);
      child.once('exit', () => { clearTimeout(timer); resolve(); });
    });
  }
}
