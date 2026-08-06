import { isIP } from 'node:net';
import type { IncomingHttpHeaders, ServerResponse } from 'node:http';
import type { AgentConfig, JsonObject } from './types.js';

export const LATEST_MCP_PROTOCOL_VERSION = '2025-11-25';
export const SUPPORTED_MCP_PROTOCOL_VERSIONS = [
  LATEST_MCP_PROTOCOL_VERSION,
  '2025-06-18',
  '2025-03-26'
] as const;
export const MCP_STREAM_HEARTBEAT_INTERVAL_MS = 10_000;
export const MCP_STREAM_CHANNEL_CAPACITY = 2;

const allowedChatOrigins = new Set(['https://chatgpt.com', 'https://chat.openai.com']);
const supportedProtocolVersions = new Set<string>(SUPPORTED_MCP_PROTOCOL_VERSIONS);

export interface McpTransportIssue {
  status: 400 | 403;
  code: -32700 | -32600 | -32000;
  message: string;
}

export type JsonRpcMessageKind = 'request' | 'notification' | 'response';

export interface ValidatedJsonRpcMessage {
  body: JsonObject;
  kind: JsonRpcMessageKind;
  method?: string;
  id: unknown;
}

interface QueuedStreamChunk {
  payload: string;
  final: boolean;
}


function stripIpv6Brackets(value: string): string {
  return value.startsWith('[') && value.endsWith(']') ? value.slice(1, -1) : value;
}

function canonicalIp(value: string): string | undefined {
  const clean = stripIpv6Brackets(value.trim().toLowerCase());
  const family = isIP(clean);
  if (family === 4) return clean.split('.').map(part => String(Number(part))).join('.');
  if (family === 6) {
    try { return stripIpv6Brackets(new URL(`http://[${clean}]`).hostname); } catch { return undefined; }
  }
  return undefined;
}

function isUnspecifiedIp(value: string): boolean {
  const ip = canonicalIp(value);
  return ip === '0.0.0.0' || ip === '::';
}

function isLoopbackIp(value: string): boolean {
  const ip = canonicalIp(value);
  if (!ip) return false;
  if (ip === '::1') return true;
  const first = ip.split('.')[0];
  return first === '127';
}

export function normalizedMcpOrigin(value: string): string | undefined {
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  try {
    const url = new URL(trimmed);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return undefined;
    return url.origin;
  } catch {
    return undefined;
  }
}

function listenerOriginAllowed(origin: string, config: AgentConfig, listenerPort: number): boolean {
  let url: URL;
  try { url = new URL(origin); } catch { return false; }
  if (url.protocol !== 'http:') return false;
  const effectivePort = url.port ? Number(url.port) : 80;
  if (!Number.isInteger(effectivePort) || effectivePort !== listenerPort) return false;

  const bindIp = canonicalIp(config.host);
  const originHostname = stripIpv6Brackets(url.hostname).toLowerCase();
  if (originHostname === 'localhost') {
    return Boolean(bindIp && (isLoopbackIp(bindIp) || isUnspecifiedIp(bindIp)));
  }

  const originIp = canonicalIp(originHostname);
  if (!originIp || !bindIp) return false;
  return isUnspecifiedIp(bindIp)
    || originIp === bindIp
    || (isLoopbackIp(bindIp) && isLoopbackIp(originIp));
}

export function mcpOriginAllowed(
  originHeader: string | undefined,
  config: AgentConfig,
  listenerPort: number
): boolean {
  if (originHeader === undefined) return true;
  const origin = normalizedMcpOrigin(originHeader);
  if (!origin) return false;
  if (listenerOriginAllowed(origin, config, listenerPort)) return true;
  if (allowedChatOrigins.has(origin)) return true;
  const configuredPublicOrigin = config.publicBaseUrl
    ? normalizedMcpOrigin(config.publicBaseUrl.trim().replace(/\/$/, ''))
    : undefined;
  return configuredPublicOrigin === origin;
}

export function validateMcpConnection(
  headers: IncomingHttpHeaders,
  config: AgentConfig,
  listenerPort: number
): McpTransportIssue | undefined {
  const originValue = headers.origin;
  const origin = Array.isArray(originValue) ? undefined : originValue;
  if (Array.isArray(originValue) || !mcpOriginAllowed(origin, config, listenerPort)) {
    return { status: 403, code: -32000, message: 'Invalid Origin header' };
  }

  const protocolValue = headers['mcp-protocol-version'];
  if (Array.isArray(protocolValue)) {
    return { status: 400, code: -32600, message: 'Invalid MCP-Protocol-Version header' };
  }
  if (protocolValue !== undefined) {
    const version = protocolValue.trim();
    if (!supportedProtocolVersions.has(version)) {
      return {
        status: 400,
        code: -32600,
        message: `Unsupported MCP protocol version: ${version}`
      };
    }
  }
  return undefined;
}

export function validateJsonRpcMessage(value: unknown): ValidatedJsonRpcMessage | McpTransportIssue {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return {
      status: 400,
      code: -32600,
      message: 'The request body must be one JSON-RPC message'
    };
  }
  const body = value as JsonObject;
  if (body.jsonrpc !== '2.0') {
    return { status: 400, code: -32600, message: "jsonrpc must be '2.0'" };
  }

  const hasMethod = typeof body.method === 'string';
  const hasId = Object.hasOwn(body, 'id');
  const isResponse = !hasMethod && hasId && (Object.hasOwn(body, 'result') || Object.hasOwn(body, 'error'));
  if (!hasMethod && !isResponse) {
    return {
      status: 400,
      code: -32600,
      message: 'Invalid JSON-RPC request, notification, or response'
    };
  }

  return {
    body,
    kind: isResponse ? 'response' : hasId ? 'request' : 'notification',
    method: hasMethod ? body.method as string : undefined,
    id: hasId ? body.id : null
  };
}

export function transportErrorBody(issue: McpTransportIssue): JsonObject {
  return {
    jsonrpc: '2.0',
    id: null,
    error: { code: issue.code, message: issue.message }
  };
}

export function sendMcpTransportError(res: ServerResponse, issue: McpTransportIssue): void {
  res.writeHead(issue.status, {
    'content-type': 'application/json',
    'cache-control': 'no-store',
    'x-content-type-options': 'nosniff'
  }).end(JSON.stringify(transportErrorBody(issue)));
}

export function sendMcpAccepted(res: ServerResponse): void {
  res.writeHead(202, { 'cache-control': 'no-store' }).end();
}

export function sendMcpMethodNotAllowed(res: ServerResponse): void {
  res.writeHead(405, { allow: 'POST', 'cache-control': 'no-store' }).end();
}

export class BoundedMcpStreamQueue {
  readonly #chunks: QueuedStreamChunk[] = [];

  constructor(readonly capacity = MCP_STREAM_CHANNEL_CAPACITY) {
    if (!Number.isInteger(capacity) || capacity < 1) throw new Error('MCP stream queue capacity must be positive');
  }

  enqueueHeartbeat(): boolean {
    if (this.#chunks.length >= this.capacity || this.#chunks.some(chunk => chunk.final)) return false;
    this.#chunks.push({ payload: '\n', final: false });
    return true;
  }

  enqueueFinal(payload: string): boolean {
    if (this.#chunks.length >= this.capacity || this.#chunks.some(chunk => chunk.final)) return false;
    this.#chunks.push({ payload, final: true });
    return true;
  }

  shift(): QueuedStreamChunk | undefined {
    return this.#chunks.shift();
  }

  clear(): void {
    this.#chunks.length = 0;
  }

  get length(): number {
    return this.#chunks.length;
  }
}

export class StreamingJsonResponse {
  readonly #queue = new BoundedMcpStreamQueue();
  readonly #res: ServerResponse;
  readonly #timer: NodeJS.Timeout;
  #blocked = false;
  #closed = false;
  #finalWritten = false;
  #pendingFinal: string | undefined;

  constructor(res: ServerResponse, heartbeatIntervalMs = MCP_STREAM_HEARTBEAT_INTERVAL_MS) {
    this.#res = res;
    res.writeHead(200, {
      'content-type': 'application/json',
      'cache-control': 'no-store',
      'x-content-type-options': 'nosniff',
      'x-accel-buffering': 'no',
      'x-coding-tools-streaming': '1'
    });
    res.flushHeaders();
    res.once('close', this.#handleClose);
    this.#timer = setInterval(() => this.#heartbeat(), heartbeatIntervalMs);
    this.#timer.unref();
  }

  finish(value: unknown): void {
    if (this.#closed || this.#finalWritten) return;
    let payload: string;
    try { payload = JSON.stringify(value); } catch {
      payload = JSON.stringify({
        jsonrpc: '2.0',
        id: null,
        error: { code: -32603, message: 'Failed to serialize RPC response' }
      });
    }
    clearInterval(this.#timer);
    if (this.#blocked) {
      if (!this.#queue.enqueueFinal(payload)) this.#pendingFinal = payload;
      return;
    }
    this.#write({ payload, final: true });
  }

  close(): void {
    this.#handleClose();
  }

  #heartbeat(): void {
    if (this.#closed || this.#finalWritten) return;
    if (this.#blocked) {
      this.#queue.enqueueHeartbeat();
      return;
    }
    this.#write({ payload: '\n', final: false });
  }

  #write(chunk: QueuedStreamChunk): void {
    if (this.#closed) return;
    const ready = this.#res.write(chunk.payload);
    if (chunk.final) this.#finalWritten = true;
    if (!ready) {
      this.#blocked = true;
      this.#res.once('drain', this.#drain);
      return;
    }
    if (chunk.final) this.#end();
  }

  #drain = (): void => {
    if (this.#closed) return;
    this.#blocked = false;
    while (!this.#blocked) {
      if (this.#pendingFinal && this.#queue.length < this.#queue.capacity) {
        this.#queue.enqueueFinal(this.#pendingFinal);
        this.#pendingFinal = undefined;
      }
      const chunk = this.#queue.shift();
      if (!chunk) break;
      this.#write(chunk);
      if (chunk.final) return;
    }
    if (this.#pendingFinal && !this.#blocked) {
      const payload = this.#pendingFinal;
      this.#pendingFinal = undefined;
      this.#write({ payload, final: true });
      return;
    }
    if (this.#finalWritten && !this.#blocked) this.#end();
  };

  #handleClose = (): void => {
    if (this.#closed) return;
    this.#closed = true;
    clearInterval(this.#timer);
    this.#queue.clear();
    this.#pendingFinal = undefined;
    this.#res.off('drain', this.#drain);
    this.#res.off('close', this.#handleClose);
  };

  #end(): void {
    if (this.#closed) return;
    clearInterval(this.#timer);
    this.#res.off('drain', this.#drain);
    this.#res.end();
  }
}
