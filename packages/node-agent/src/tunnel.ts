import { createPublicKey, randomUUID, sign, verify } from 'node:crypto';
import {
  request as httpRequest, type ClientRequest, type IncomingMessage,
  type OutgoingHttpHeaders, type RequestOptions
} from 'node:http';
import WebSocket from 'ws';
import type { AgentConfig, JsonObject, ToolContext } from './types.js';
import { AsyncQueue } from './runtime.js';
import {
  authSigningPayload, builtinEndpointForClient, parseBuiltinPublicUrl,
  serverAckSigningPayload, serverChallengeSigningPayload,
  TUNNEL_PROTOCOL_VERSION, TUNNEL_SUBPROTOCOL, TUNNEL_WS_PATH,
  type TunnelEndpoint
} from './tunnel/endpoint.js';
import { identityPrivateKey, loadOrEnroll, type DeviceIdentity } from './tunnel/identity.js';
import {
  MULTIPLEX_TUNNEL_PROTOCOL_VERSION, MULTIPLEX_TUNNEL_SUBPROTOCOL,
  MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL, MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL,
  MULTIPLEX_CONNECTION_ID_HEADER, MULTIPLEX_SESSION_ID_HEADER,
  MultiplexBinaryFrameKind, deviceSessionAuthSigningPayload, multiplexServerChallengeSigningPayload,
  multiplexServerAckSigningPayload, splitSessionAuthSigningPayload, splitServerAckSigningPayload,
  encodeMultiplexBinaryFrame, decodeMultiplexBinaryFrame, normalizeMultiplexConnectionPolicy,
  multiplexPoolAdjustment, type MultiplexConnectionPolicy, type MultiplexConnectionRole
} from './tunnel/multiplex.js';
import {
  configuredBurstWarmFloor, configuredMaxConnecting, normalizeWorkerPolicy,
  poolAdjustment, workerShouldRecycle, type PoolCounts, type WorkerPolicy
} from './tunnelPolicy.js';

export {
  authSigningPayload, builtinEndpointForClient, parseBuiltinPublicUrl,
  serverAckSigningPayload, serverChallengeSigningPayload,
  TUNNEL_ENROLL_PATH, TUNNEL_PROTOCOL_VERSION, TUNNEL_SUBPROTOCOL, TUNNEL_WS_PATH
} from './tunnel/endpoint.js';
export type { TunnelEndpoint } from './tunnel/endpoint.js';
export {
  MULTIPLEX_TUNNEL_PROTOCOL_VERSION, MULTIPLEX_TUNNEL_SUBPROTOCOL,
  MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL, MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL,
  MULTIPLEX_CONNECTION_ID_HEADER, MULTIPLEX_SESSION_ID_HEADER,
  MultiplexBinaryFrameKind, deviceSessionAuthSigningPayload, multiplexServerChallengeSigningPayload,
  multiplexServerAckSigningPayload, splitSessionAuthSigningPayload, splitServerAckSigningPayload,
  encodeMultiplexBinaryFrame, decodeMultiplexBinaryFrame
} from './tunnel/multiplex.js';
export type { MultiplexConnectionPolicy, MultiplexConnectionRole } from './tunnel/multiplex.js';
export const MAX_TUNNEL_REQUEST_BYTES = 8 * 1024 * 1024;
export const BUILTIN_TUNNEL_DEMAND_TTL_MS = 3_000;
export const BUILTIN_TUNNEL_LOCAL_CONNECT_TIMEOUT_MS = 10_000;
export const BUILTIN_TUNNEL_LOCAL_REQUEST_TIMEOUT_MS = 5 * 60_000;

export const BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS = 30_000;
export const BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS = 90_000;
export const BUILTIN_TUNNEL_RECONNECT_INITIAL_MS = 1_000;
export const BUILTIN_TUNNEL_RECONNECT_MAX_MS = 120_000;

export function builtinTunnelReconnectDelayMs(attempt: number, connected: boolean, random: () => number = Math.random): number {
  const boundedAttempt = Math.max(0, Math.min(Math.trunc(attempt), 16));
  const base = connected
    ? BUILTIN_TUNNEL_RECONNECT_INITIAL_MS
    : Math.min(BUILTIN_TUNNEL_RECONNECT_MAX_MS, BUILTIN_TUNNEL_RECONNECT_INITIAL_MS * 2 ** boundedAttempt);
  const sample = Math.max(0, Math.min(1, random()));
  return Math.max(1, Math.floor(base * (0.5 + sample * 0.5)));
}

const HOP_BY_HOP = new Set([
  'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization', 'te',
  'trailer', 'transfer-encoding', 'upgrade', 'host', 'content-length'
]);

interface TunnelMessage {
  binary: boolean;
  data: Buffer;
}

interface WorkerDemand {
  queued_requests: number;
  oldest_queue_wait_ms: number;
  desired_workers: number;
}

interface RequestHead {
  kind: 'request_head';
  request_id: string;
  method: string;
  path_and_query: string;
  headers: Array<{ name: string; value: string }>;
  demand?: WorkerDemand;
}

type WorkerState = 'connecting' | 'idle' | 'busy' | 'retiring';

interface ManagedWorker {
  index: number;
  state: WorkerState;
  connectingSince: number;
  retire: boolean;
  completedRequests: number;
  connectedAt?: number;
  multiplex?: boolean;
  multiplexActiveStreams: number;
  socket?: WebSocket;
  task?: Promise<void>;
}

interface SharedSplitRoute {
  manager: BuiltinTunnelManager;
  routeId: number;
  streamLimit: number;
}

type SplitControlCommand =
  | {
      kind: 'add'; manager: BuiltinTunnelManager; begin: () => boolean;
      resolve: (route: SharedSplitRoute) => void; reject: (error: Error) => void;
    }
  | { kind: 'remove'; manager: BuiltinTunnelManager; resolve: () => void; reject: (error: Error) => void };

export interface ResolvedBuiltinTunnelEndpoint {
  publicUrl: string;
  publicBaseUrl: string;
  enrollmentCompleted: boolean;
}

export interface BuiltinTunnelManagerOptions {
  enrollmentFetch?: typeof fetch;
  websocketUrlOverride?: string;
  reconcileIntervalMs?: number;
  demandTtlMs?: number;
  demandNow?: () => number;
  localOriginOverride?: string;
  localConnectTimeoutMs?: number;
  localRequestTimeoutMs?: number;
  localLookup?: RequestOptions['lookup'];
  onUpdateOffer?: (offer: JsonObject, sendStatus: (status: JsonObject) => Promise<void>) => Promise<void>;
  clientStatus?: JsonObject;
  now?: () => number;
  onEndpointResolved?: (endpoint: ResolvedBuiltinTunnelEndpoint) => Promise<void> | void;
  sharedDeviceIdentities?: Map<string, DeviceIdentity>;
  sharedSplitHubs?: Map<string, BuiltinTunnelManager>;
}

function send(ws: WebSocket, data: string | Buffer): Promise<void> {
  return new Promise((resolve, reject) => ws.send(data, error => error ? reject(error) : resolve()));
}

function sendControl(ws: WebSocket, value: JsonObject): Promise<void> {
  return send(ws, JSON.stringify(value));
}

function messageQueue(ws: WebSocket, touch: () => void): AsyncQueue<TunnelMessage> {
  const queue = new AsyncQueue<TunnelMessage>();
  ws.on('message', (data, isBinary) => {
    touch();
    queue.push({ binary: isBinary, data: Buffer.isBuffer(data) ? data : Buffer.from(data as ArrayBuffer) });
  });
  ws.on('ping', touch);
  ws.on('pong', touch);
  ws.once('close', () => queue.close(new Error('websocket closed')));
  ws.once('error', error => queue.close(error));
  return queue;
}

async function nextControl(
  queue: AsyncQueue<TunnelMessage>, timeoutMs = 30_000, signal?: AbortSignal
): Promise<JsonObject> {
  const message = await queue.shift(timeoutMs, signal);
  if (message.binary) throw new Error('unexpected binary frame');
  return JSON.parse(message.data.toString('utf8')) as JsonObject;
}

async function openWebSocket(
  websocketUrl: string,
  endpoint: TunnelEndpoint,
  identity: DeviceIdentity,
  workerId: string,
  connectionId: string,
  touch: () => void
): Promise<{ socket: WebSocket; queue: AsyncQueue<TunnelMessage> }> {
  const socket = new WebSocket(websocketUrl, [MULTIPLEX_TUNNEL_SUBPROTOCOL, TUNNEL_SUBPROTOCOL], {
    handshakeTimeout: 15_000,
    headers: {
      'x-coding-tools-client-id': endpoint.clientId,
      'x-coding-tools-service': 'mcp',
      'x-coding-tools-device-id': identity.deviceId,
      'x-coding-tools-worker-id': workerId,
      [MULTIPLEX_CONNECTION_ID_HEADER]: connectionId
    }
  });
  const queue = messageQueue(socket, touch);
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => { socket.terminate(); reject(new Error('websocket connect timeout')); }, 20_000);
    timer.unref();
    socket.once('open', () => { clearTimeout(timer); resolve(); });
    socket.once('error', error => { clearTimeout(timer); reject(error); });
  });
  if (socket.protocol !== MULTIPLEX_TUNNEL_SUBPROTOCOL && socket.protocol !== TUNNEL_SUBPROTOCOL) {
    socket.close();
    throw new Error(`server did not accept ${MULTIPLEX_TUNNEL_SUBPROTOCOL} or ${TUNNEL_SUBPROTOCOL}`);
  }
  return { socket, queue };
}

async function openSplitWebSocket(
  websocketUrl: string,
  endpoint: TunnelEndpoint,
  identity: DeviceIdentity,
  role: MultiplexConnectionRole,
  connectionId: string,
  sessionId: string | undefined,
  touch: () => void
): Promise<{ socket: WebSocket; queue: AsyncQueue<TunnelMessage> }> {
  const protocol = role === 'control' ? MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL : MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL;
  const protocols = role === 'control'
    ? [MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL, MULTIPLEX_TUNNEL_SUBPROTOCOL, TUNNEL_SUBPROTOCOL]
    : [MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL];
  const headers: Record<string, string> = {
    'x-coding-tools-client-id': endpoint.clientId,
    'x-coding-tools-service': 'mcp',
    'x-coding-tools-device-id': identity.deviceId,
    'x-coding-tools-worker-id': connectionId,
    [MULTIPLEX_CONNECTION_ID_HEADER]: connectionId
  };
  if (sessionId) headers[MULTIPLEX_SESSION_ID_HEADER] = sessionId;
  const socket = new WebSocket(websocketUrl, protocols, { handshakeTimeout: 15_000, headers });
  const queue = messageQueue(socket, touch);
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => { socket.terminate(); reject(new Error('websocket connect timeout')); }, 20_000);
    timer.unref();
    socket.once('open', () => { clearTimeout(timer); resolve(); });
    socket.once('error', error => { clearTimeout(timer); reject(error); });
  });
  const accepted = role === 'control'
    ? [MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL, MULTIPLEX_TUNNEL_SUBPROTOCOL, TUNNEL_SUBPROTOCOL].includes(socket.protocol)
    : socket.protocol === MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL;
  if (!accepted) {
    socket.close();
    throw new Error(`server did not accept a supported ${role} tunnel protocol`);
  }
  return { socket, queue };
}

function localOrigin(config: AgentConfig): string {
  const host = ['0.0.0.0', '::', '[::]'].includes(config.host) ? '127.0.0.1' : config.host;
  return `http://${host.includes(':') && !host.startsWith('[') ? `[${host}]` : host}:${config.port}`;
}

function localRequestOrigin(config: AgentConfig, override?: string): string {
  const url = new URL(override ?? localOrigin(config));
  if (url.protocol !== 'http:' || url.username || url.password || url.pathname !== '/' || url.search || url.hash) {
    throw new Error('local tunnel origin must be a clean HTTP origin');
  }
  return url.origin;
}

export function forwardedTunnelRequestHeaders(pairs: Array<{ name: string; value: string }>): OutgoingHttpHeaders {
  const headers: OutgoingHttpHeaders = Object.create(null);
  for (const pair of pairs) {
    const name = String(pair.name).toLowerCase();
    if (!HOP_BY_HOP.has(name)) {
      const existing = headers[name];
      if (existing === undefined) headers[name] = pair.value;
      else if (Array.isArray(existing)) existing.push(pair.value);
      else headers[name] = [String(existing), pair.value];
    }
  }
  return headers;
}

function responseHeaders(response: IncomingMessage): Array<{ name: string; value: string }> {
  const output: Array<{ name: string; value: string }> = [];
  for (let index = 0; index + 1 < response.rawHeaders.length; index += 2) {
    const name = response.rawHeaders[index];
    const value = response.rawHeaders[index + 1];
    if (!HOP_BY_HOP.has(name.toLowerCase())) output.push({ name, value });
  }
  return output;
}

export type BuiltinTunnelTimeoutReason = 'connect' | 'overall';

class LocalTunnelTimeoutError extends Error {
  readonly timeoutReason: BuiltinTunnelTimeoutReason;

  constructor(timeoutReason: BuiltinTunnelTimeoutReason) {
    super(timeoutReason === 'connect'
      ? 'local tunnel connection timed out'
      : 'local tunnel request timed out');
    this.name = 'LocalTunnelTimeoutError';
    this.timeoutReason = timeoutReason;
  }
}

interface LocalRequestHandle {
  response: Promise<IncomingMessage>;
  abort(error: Error): void;
  terminalError(): Error | undefined;
}

function asError(value: unknown, fallback: string): Error {
  if (value instanceof Error) return value;
  return new Error(value === undefined ? fallback : String(value));
}

function boundedTimeout(value: number | undefined, fallback: number): number {
  return Number.isFinite(value) && Number(value) > 0 ? Math.max(1, Math.floor(Number(value))) : fallback;
}

function startLocalRequest(
  url: URL,
  method: string,
  headerPairs: Array<{ name: string; value: string }>,
  body: Buffer,
  connectTimeoutMs: number,
  lookup?: RequestOptions['lookup']
): LocalRequestHandle {
  let request: ClientRequest | undefined;
  let incoming: IncomingMessage | undefined;
  let connectTimer: NodeJS.Timeout | undefined;
  let finalError: Error | undefined;
  const clearConnectTimer = () => {
    if (connectTimer) clearTimeout(connectTimer);
    connectTimer = undefined;
  };
  const abort = (error: Error) => {
    finalError ??= error;
    clearConnectTimer();
    incoming?.destroy(finalError);
    request?.destroy(finalError);
  };
  const headers = forwardedTunnelRequestHeaders(headerPairs);
  if (body.length > 0) headers['content-length'] = body.length;
  const response = new Promise<IncomingMessage>((resolve, reject) => {
    request = httpRequest(url, { method, headers, lookup, agent: false }, value => {
      incoming = value;
      clearConnectTimer();
      resolve(value);
    });
    request.once('socket', socket => {
      if (!socket.connecting) return;
      connectTimer = setTimeout(
        () => abort(new LocalTunnelTimeoutError('connect')),
        connectTimeoutMs
      );
      connectTimer.unref();
      socket.once('connect', clearConnectTimer);
      socket.once('error', clearConnectTimer);
      socket.once('close', clearConnectTimer);
    });
    request.once('error', error => {
      clearConnectTimer();
      reject(finalError ?? error);
    });
    if (body.length > 0) request.write(body);
    request.end();
  });
  return { response, abort, terminalError: () => finalError };
}

function parseDemand(value: unknown): WorkerDemand | undefined {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return undefined;
  const input = value as Record<string, unknown>;
  const queued = Number(input.queued_requests);
  const oldest = Number(input.oldest_queue_wait_ms);
  const desired = Number(input.desired_workers);
  if (![queued, oldest, desired].every(Number.isSafeInteger) || queued < 0 || oldest < 0 || desired < 0) return undefined;
  return { queued_requests: Math.min(queued, 65_535), oldest_queue_wait_ms: oldest, desired_workers: Math.min(desired, 65_535) };
}

async function receiveRequest(
  queue: AsyncQueue<TunnelMessage>,
  head: RequestHead,
  applyPolicy: (value: unknown) => void
): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let bytes = 0;
  while (true) {
    const message = await queue.shift(120_000);
    if (message.binary) {
      bytes += message.data.length;
      if (bytes > MAX_TUNNEL_REQUEST_BYTES) throw new Error('request body exceeds built-in tunnel limit');
      chunks.push(message.data);
      continue;
    }
    const control = JSON.parse(message.data.toString('utf8')) as JsonObject;
    if (control.kind === 'request_end' && control.request_id === head.request_id) return Buffer.concat(chunks);
    if (control.kind === 'cancel' && control.request_id === head.request_id) throw new Error('request cancelled');
    if (control.kind === 'policy_update') { applyPolicy(control.worker_policy); continue; }
    if (control.kind === 'error') throw new Error(String(control.message ?? 'tunnel server error'));
    throw new Error(`unexpected control message while receiving request: ${control.kind}`);
  }
}

export function tunnelPathAllowed(config: AgentConfig, pathname: string): boolean {
  const publicBase = config.publicBaseUrl
    ?? (config.tunnel ? config.tunnel.publicUrl.replace(/\/mcp\/?$/, '') : undefined);
  if (!publicBase) return false;
  let prefix: string;
  try {
    prefix = new URL(publicBase).pathname.replace(/\/$/, '');
  } catch { return false; }
  const scoped = (suffix: string) => `${prefix}${suffix}` || suffix;
  return new Set([
    scoped('/mcp'),
    scoped('/mcp/info'),
    scoped('/oauth/authorize'),
    scoped('/oauth/token'),
    `/.well-known/oauth-authorization-server${prefix}`,
    `/.well-known/oauth-protected-resource${prefix}/mcp`
  ]).has(pathname);
}

async function monitorRequestControls(
  queue: AsyncQueue<TunnelMessage>,
  requestId: string,
  phase: 'waiting for local response' | 'streaming local response',
  signal: AbortSignal
): Promise<void> {
  while (true) {
    const message = await queue.shift(0, signal);
    if (message.binary) throw new Error(`unexpected binary frame while ${phase}`);
    const control = JSON.parse(message.data.toString('utf8')) as JsonObject;
    if (control.kind === 'cancel' && control.request_id === requestId) return;
    if (control.kind === 'error') throw new Error(String(control.message ?? 'tunnel server error'));
    throw new Error(`unexpected control message while ${phase}: ${control.kind}`);
  }
}

async function forwardRequest(
  ws: WebSocket,
  config: AgentConfig,
  head: RequestHead,
  requestBody: Buffer,
  queue: AsyncQueue<TunnelMessage>,
  stopSignal: AbortSignal,
  options: BuiltinTunnelManagerOptions
): Promise<void> {
  const method = head.method.toUpperCase();
  if (!head.path_and_query.startsWith('/') || head.path_and_query.startsWith('//')) throw new Error('tunnel request path must be origin-relative');
  const origin = localRequestOrigin(config, options.localOriginOverride);
  const url = new URL(head.path_and_query, origin);
  if (url.origin !== origin) throw new Error('tunnel request attempted to leave the local origin');
  if (!tunnelPathAllowed(config, url.pathname)) throw new Error('tunnel request path is not an allowed MCP or OAuth route');
  const local = startLocalRequest(
    url,
    method,
    head.headers,
    method === 'GET' || method === 'HEAD' ? Buffer.alloc(0) : requestBody,
    boundedTimeout(options.localConnectTimeoutMs, BUILTIN_TUNNEL_LOCAL_CONNECT_TIMEOUT_MS),
    options.localLookup
  );
  const stopLocalRequest = () => local.abort(asError(stopSignal.reason, 'tunnel stopped'));
  if (stopSignal.aborted) stopLocalRequest();
  else stopSignal.addEventListener('abort', stopLocalRequest, { once: true });
  const timeout = setTimeout(
    () => local.abort(new LocalTunnelTimeoutError('overall')),
    boundedTimeout(options.localRequestTimeoutMs, BUILTIN_TUNNEL_LOCAL_REQUEST_TIMEOUT_MS)
  );
  timeout.unref();
  let response: IncomingMessage;
  const fetchControls = new AbortController();
  const fetchOutcome = local.response.then(
    value => ({ kind: 'response' as const, value }),
    error => ({ kind: 'fetch_error' as const, error })
  );
  const fetchControlOutcome = monitorRequestControls(
    queue, head.request_id, 'waiting for local response', fetchControls.signal
  ).then(
    () => ({ kind: 'cancel' as const }),
    error => ({ kind: 'control_error' as const, error })
  );
  try {
    const outcome = await Promise.race([fetchOutcome, fetchControlOutcome]);
    if (outcome.kind === 'response') {
      response = outcome.value;
      fetchControls.abort();
    } else {
      local.abort(outcome.kind === 'cancel'
        ? new Error('request cancelled')
        : asError(outcome.error, 'local tunnel request failed'));
      fetchControls.abort();
      await local.response.catch(() => undefined);
      if (outcome.kind === 'cancel') return;
      throw local.terminalError() ?? outcome.error;
    }
    await sendControl(ws, {
      kind: 'response_head', request_id: head.request_id,
      status: response.statusCode ?? 502, headers: responseHeaders(response)
    });
    {
      const streamControls = new AbortController();
      const streamPromise = (async () => {
        for await (const chunk of response) {
          const value = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
          if (value.length) await send(ws, value);
        }
      })();
      const streamOutcome = streamPromise.then(
        () => ({ kind: 'complete' as const }),
        error => ({ kind: 'stream_error' as const, error })
      );
      const streamControlOutcome = monitorRequestControls(
        queue, head.request_id, 'streaming local response', streamControls.signal
      ).then(
        () => ({ kind: 'cancel' as const }),
        error => ({ kind: 'control_error' as const, error })
      );
      try {
        const outcome = await Promise.race([streamOutcome, streamControlOutcome]);
        if (outcome.kind === 'complete') {
          streamControls.abort();
        } else {
          local.abort(outcome.kind === 'cancel'
            ? new Error('request cancelled')
            : asError(outcome.error, 'local tunnel response forwarding failed'));
          streamControls.abort();
          await streamPromise.catch(() => undefined);
          if (outcome.kind === 'cancel') return;
          throw local.terminalError() ?? outcome.error;
        }
      } finally {
        streamControls.abort();
      }
    }
    await sendControl(ws, { kind: 'response_end', request_id: head.request_id });
  } finally {
    clearTimeout(timeout);
    fetchControls.abort();
    stopSignal.removeEventListener('abort', stopLocalRequest);
  }
}

async function authenticate(ws: WebSocket, queue: AsyncQueue<TunnelMessage>, identity: DeviceIdentity, workerId: string): Promise<WorkerPolicy> {
  const challenge = await nextControl(queue);
  if (challenge.kind === 'error') throw new Error(String(challenge.message ?? 'tunnel server error'));
  if (challenge.kind !== 'challenge') throw new Error('server did not issue a tunnel authentication challenge');
  if (Date.now() > Number(challenge.expires_at_unix_ms)) throw new Error('tunnel authentication challenge expired');
  if (!identity.serverId || !identity.serverPublicKeyRaw || String(challenge.server_id) !== identity.serverId) {
    throw new Error('tunnel server identity does not match the enrolled server');
  }
  const serverSpki = Buffer.concat([
    Buffer.from('302a300506032b6570032100', 'hex'),
    Buffer.from(identity.serverPublicKeyRaw, 'base64url')
  ]);
  const serverPublicKey = createPublicKey({ key: serverSpki, format: 'der', type: 'spki' });
  const challengeValid = verify(
    null,
    serverChallengeSigningPayload(
      String(challenge.nonce),
      Number(challenge.expires_at_unix_ms),
      identity.serverId,
      identity.deviceId,
      identity.clientId,
      workerId
    ),
    serverPublicKey,
    Buffer.from(String(challenge.server_signature || ''), 'base64url')
  );
  if (!challengeValid) throw new Error('tunnel server challenge signature is invalid');
  const hello = { protocol_version: TUNNEL_PROTOCOL_VERSION, client_id: identity.clientId, service: 'mcp', worker_id: workerId };
  const signature = sign(null, authSigningPayload(String(challenge.nonce), identity.deviceId, identity.clientId, workerId), identityPrivateKey(identity)).toString('base64url');
  await sendControl(ws, { kind: 'authenticate', hello, device_id: identity.deviceId, signature });
  const acknowledgment = await nextControl(queue);
  if (acknowledgment.kind === 'error') throw new Error(String(acknowledgment.message ?? 'tunnel authentication failed'));
  if (acknowledgment.kind !== 'hello_ack' || Number(acknowledgment.protocol_version) !== TUNNEL_PROTOCOL_VERSION) {
    throw new Error('server did not acknowledge tunnel authentication');
  }
  if (String(acknowledgment.server_id) !== identity.serverId) {
    throw new Error('tunnel server acknowledgement identity changed during authentication');
  }
  const ackValid = verify(
    null,
    serverAckSigningPayload(
      String(challenge.nonce),
      identity.serverId,
      identity.deviceId,
      identity.clientId,
      workerId,
      acknowledgment.worker_policy
    ),
    serverPublicKey,
    Buffer.from(String(acknowledgment.server_signature || ''), 'base64url')
  );
  if (!ackValid) throw new Error('tunnel server acknowledgement signature is invalid');
  return normalizeWorkerPolicy(acknowledgment.worker_policy);
}

interface MultiplexRequestHead {
  kind: 'request_head';
  route_id: number;
  stream_id: number;
  method: string;
  path_and_query: string;
  headers: Array<{ name: string; value: string }>;
  demand?: {
    queued_requests?: number;
    oldest_queue_wait_ms?: number;
    active_streams?: number;
    available_stream_slots?: number;
    desired_connections?: number;
  };
}

class MultiplexWriter {
  #tail = Promise.resolve();
  readonly socket: WebSocket;

  constructor(socket: WebSocket) { this.socket = socket; }

  send(data: string | Buffer): Promise<void> {
    const next = this.#tail.then(() => send(this.socket, data));
    this.#tail = next.catch(() => undefined);
    return next;
  }

  control(value: JsonObject): Promise<void> {
    return this.send(JSON.stringify(value));
  }

  binary(kind: 1 | 2, routeId: number, streamId: bigint, payload: Buffer): Promise<void> {
    return this.send(encodeMultiplexBinaryFrame(kind, routeId, streamId, payload));
  }
}

function multiplexServerPublicKey(identity: DeviceIdentity) {
  if (!identity.serverId || !identity.serverPublicKeyRaw) throw new Error('tunnel server identity is unavailable');
  const serverSpki = Buffer.concat([
    Buffer.from('302a300506032b6570032100', 'hex'),
    Buffer.from(identity.serverPublicKeyRaw, 'base64url')
  ]);
  return createPublicKey({ key: serverSpki, format: 'der', type: 'spki' });
}

async function authenticateMultiplex(
  ws: WebSocket,
  queue: AsyncQueue<TunnelMessage>,
  identity: DeviceIdentity,
  connectionId: string
): Promise<MultiplexConnectionPolicy> {
  const challenge = await nextControl(queue);
  if (challenge.kind === 'error') throw new Error(String(challenge.message ?? 'tunnel server error'));
  if (challenge.kind !== 'challenge') throw new Error('server did not issue a multiplex authentication challenge');
  if (Date.now() > Number(challenge.expires_at_unix_ms)) throw new Error('tunnel authentication challenge expired');
  if (!identity.serverId || String(challenge.server_id) !== identity.serverId) {
    throw new Error('tunnel server identity does not match the enrolled server');
  }
  const serverPublicKey = multiplexServerPublicKey(identity);
  const challengeValid = verify(
    null,
    multiplexServerChallengeSigningPayload(
      String(challenge.nonce), Number(challenge.expires_at_unix_ms), identity.serverId,
      identity.deviceId, connectionId
    ),
    serverPublicKey,
    Buffer.from(String(challenge.server_signature || ''), 'base64url')
  );
  if (!challengeValid) throw new Error('tunnel server multiplex challenge signature is invalid');

  const hello = { protocol_version: MULTIPLEX_TUNNEL_PROTOCOL_VERSION, connection_id: connectionId };
  const signature = sign(
    null,
    deviceSessionAuthSigningPayload(String(challenge.nonce), identity.deviceId, connectionId),
    identityPrivateKey(identity)
  ).toString('base64url');
  await sendControl(ws, { kind: 'authenticate', hello, device_id: identity.deviceId, signature });
  const acknowledgment = await nextControl(queue);
  if (acknowledgment.kind === 'error') throw new Error(String(acknowledgment.message ?? 'tunnel authentication failed'));
  if (acknowledgment.kind !== 'hello_ack' || Number(acknowledgment.protocol_version) !== MULTIPLEX_TUNNEL_PROTOCOL_VERSION) {
    throw new Error('server did not acknowledge multiplex tunnel authentication');
  }
  if (String(acknowledgment.server_id) !== identity.serverId) {
    throw new Error('tunnel server acknowledgement identity changed during multiplex authentication');
  }
  const policy = normalizeMultiplexConnectionPolicy(acknowledgment.connection_policy);
  const ackValid = verify(
    null,
    multiplexServerAckSigningPayload(
      String(challenge.nonce), identity.serverId, identity.deviceId, connectionId, policy
    ),
    serverPublicKey,
    Buffer.from(String(acknowledgment.server_signature || ''), 'base64url')
  );
  if (!ackValid) throw new Error('tunnel server multiplex acknowledgement signature is invalid');
  return policy;
}

async function authenticateSplitSession(
  ws: WebSocket,
  queue: AsyncQueue<TunnelMessage>,
  identity: DeviceIdentity,
  connectionId: string,
  role: MultiplexConnectionRole,
  sessionId?: string
): Promise<{ sessionId: string; policy: MultiplexConnectionPolicy }> {
  const challenge = await nextControl(queue);
  if (challenge.kind === 'error') throw new Error(String(challenge.message ?? 'tunnel server error'));
  if (challenge.kind !== 'challenge') throw new Error('server did not issue a split tunnel authentication challenge');
  if (Date.now() > Number(challenge.expires_at_unix_ms)) throw new Error('tunnel authentication challenge expired');
  if (!identity.serverId || String(challenge.server_id) !== identity.serverId) {
    throw new Error('tunnel server identity does not match the enrolled server');
  }
  const serverPublicKey = multiplexServerPublicKey(identity);
  const challengeValid = verify(
    null,
    multiplexServerChallengeSigningPayload(
      String(challenge.nonce), Number(challenge.expires_at_unix_ms), identity.serverId,
      identity.deviceId, connectionId
    ),
    serverPublicKey,
    Buffer.from(String(challenge.server_signature || ''), 'base64url')
  );
  if (!challengeValid) throw new Error('tunnel server split challenge signature is invalid');
  const hello: JsonObject = {
    protocol_version: MULTIPLEX_TUNNEL_PROTOCOL_VERSION, connection_role: role, connection_id: connectionId
  };
  if (sessionId) hello.session_id = sessionId;
  const signature = sign(
    null, splitSessionAuthSigningPayload(String(challenge.nonce), role, identity.deviceId, connectionId, sessionId),
    identityPrivateKey(identity)
  ).toString('base64url');
  await sendControl(ws, { kind: 'session_authenticate', hello, device_id: identity.deviceId, signature });
  const acknowledgment = await nextControl(queue);
  if (acknowledgment.kind === 'error') throw new Error(String(acknowledgment.message ?? 'split tunnel authentication failed'));
  if (acknowledgment.kind !== 'session_ready' || Number(acknowledgment.protocol_version) !== MULTIPLEX_TUNNEL_PROTOCOL_VERSION) {
    throw new Error('server did not acknowledge split tunnel authentication');
  }
  if (String(acknowledgment.connection_role) !== role || String(acknowledgment.server_id) !== identity.serverId) {
    throw new Error('split tunnel acknowledgement identity or role changed during authentication');
  }
  const acceptedSessionId = String(acknowledgment.session_id || '');
  if (!acceptedSessionId || (sessionId && acceptedSessionId !== sessionId)) {
    throw new Error('split tunnel acknowledgement returned an unexpected session id');
  }
  const policy = normalizeMultiplexConnectionPolicy(acknowledgment.connection_policy);
  const ackValid = verify(
    null,
    splitServerAckSigningPayload(
      String(challenge.nonce), identity.serverId, role, identity.deviceId, connectionId, acceptedSessionId, policy
    ),
    serverPublicKey,
    Buffer.from(String(acknowledgment.server_signature || ''), 'base64url')
  );
  if (!ackValid) throw new Error('tunnel server split acknowledgement signature is invalid');
  return { sessionId: acceptedSessionId, policy };
}

async function registerMultiplexRoute(
  writer: MultiplexWriter,
  queue: AsyncQueue<TunnelMessage>,
  clientId: string,
  onCapacity?: (demand: unknown) => void
): Promise<{ routeId: number; streamLimit: number }> {
  const registrationId = randomUUID();
  await writer.control({ kind: 'route_register', registration_id: registrationId, client_id: clientId, service: 'mcp' });
  while (true) {
    const control = await nextControl(queue);
    if (control.kind === 'error') throw new Error(String(control.message ?? 'multiplex route registration failed'));
    if (control.kind === 'capacity_update') { onCapacity?.(control.demand); continue; }
    if (control.kind === 'route_registered' && control.registration_id === registrationId) {
      const routeId = Number(control.route_id);
      const streamLimit = Number(control.stream_limit);
      if (!Number.isInteger(routeId) || routeId <= 0) throw new Error('server returned an invalid multiplex route id');
      if (!Number.isInteger(streamLimit) || streamLimit <= 0 || streamLimit > 64) throw new Error('server returned an invalid multiplex stream limit');
      return { routeId, streamLimit };
    }
    throw new Error(`unexpected control message while registering multiplex route: ${control.kind}`);
  }
}

async function forwardMultiplexRequest(
  writer: MultiplexWriter,
  config: AgentConfig,
  routeId: number,
  streamId: bigint,
  head: MultiplexRequestHead,
  requestBody: Buffer,
  signal: AbortSignal,
  stopSignal: AbortSignal,
  options: BuiltinTunnelManagerOptions
): Promise<'complete' | 'cancelled'> {
  const method = head.method.toUpperCase();
  if (!head.path_and_query.startsWith('/') || head.path_and_query.startsWith('//')) throw new Error('tunnel request path must be origin-relative');
  const origin = localRequestOrigin(config, options.localOriginOverride);
  const url = new URL(head.path_and_query, origin);
  if (url.origin !== origin) throw new Error('tunnel request attempted to leave the local origin');
  if (!tunnelPathAllowed(config, url.pathname)) throw new Error('tunnel request path is not an allowed MCP or OAuth route');
  const local = startLocalRequest(
    url,
    method,
    head.headers,
    method === 'GET' || method === 'HEAD' ? Buffer.alloc(0) : requestBody,
    boundedTimeout(options.localConnectTimeoutMs, BUILTIN_TUNNEL_LOCAL_CONNECT_TIMEOUT_MS),
    options.localLookup
  );
  const abortLocal = () => local.abort(asError(signal.reason, 'request cancelled'));
  const stopLocal = () => local.abort(asError(stopSignal.reason, 'tunnel stopped'));
  if (signal.aborted) abortLocal(); else signal.addEventListener('abort', abortLocal, { once: true });
  if (stopSignal.aborted) stopLocal(); else stopSignal.addEventListener('abort', stopLocal, { once: true });
  const overallTimer = setTimeout(
    () => local.abort(new LocalTunnelTimeoutError('overall')),
    boundedTimeout(options.localRequestTimeoutMs, BUILTIN_TUNNEL_LOCAL_REQUEST_TIMEOUT_MS)
  );
  overallTimer.unref();
  try {
    let response: IncomingMessage;
    try {
      response = await local.response;
    } catch (error) {
      if (signal.aborted || stopSignal.aborted) return 'cancelled';
      throw local.terminalError() ?? error;
    }
    if (signal.aborted || stopSignal.aborted) return 'cancelled';
    await writer.control({
      kind: 'response_head', route_id: routeId, stream_id: Number(streamId),
      status: response.statusCode ?? 502, headers: responseHeaders(response)
    });
    for await (const chunk of response) {
      if (signal.aborted || stopSignal.aborted) {
        local.abort(new Error('request cancelled'));
        return 'cancelled';
      }
      const value = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      for (let offset = 0; offset < value.length; offset += 64 * 1024) {
        await writer.binary(
          MultiplexBinaryFrameKind.ResponseBody,
          routeId,
          streamId,
          value.subarray(offset, offset + 64 * 1024)
        );
      }
    }
    if (signal.aborted || stopSignal.aborted) return 'cancelled';
    await writer.control({ kind: 'response_end', route_id: routeId, stream_id: Number(streamId) });
    return 'complete';
  } finally {
    clearTimeout(overallTimer);
    signal.removeEventListener('abort', abortLocal);
    stopSignal.removeEventListener('abort', stopLocal);
  }
}

export function publicTunnelSecurityError(_config: AgentConfig): string | undefined {
  return undefined;
}

export class BuiltinTunnelManager {
  readonly config: AgentConfig;
  readonly context: ToolContext;
  readonly options: BuiltinTunnelManagerOptions;
  #stopped = true;
  #workers = new Map<number, ManagedWorker>();
  #nextWorkerIndex = 0;
  #identity?: DeviceIdentity;
  #endpoint?: TunnelEndpoint;
  #abort = new AbortController();
  #policy?: WorkerPolicy;
  #demandTarget = 0;
  #demandSeenAt = 0;
  #lastPressureAt = 0;
  #idleExcessSince = 0;
  #reconcileTimer?: NodeJS.Timeout;
  #recycledWorkers = 0;
  #multiplexActive = false;
  #multiplexPolicy?: MultiplexConnectionPolicy;
  #multiplexActiveStreams = 0;
  #multiplexReportedActiveStreams = 0;
  #multiplexQueuedRequests = 0;
  #multiplexDesiredConnections = 1;
  #multiplexDemandSeenAt = 0;
  #multiplexIdleExcessSince = 0;
  #splitMode = false;
  #splitControlSocket?: WebSocket;
  #splitControlTask?: Promise<void>;
  #splitSessionId?: string;
  #splitRouteId?: number;
  #splitStreamLimit?: number;
  #sharedHubKey?: string;
  #sharedOwner?: BuiltinTunnelManager;
  #splitRoutes = new Map<number, SharedSplitRoute>();
  #splitRoutesByManager = new Map<BuiltinTunnelManager, SharedSplitRoute>();
  #splitPendingRemovals = new Set<BuiltinTunnelManager>();
  #splitControlCommands = new AsyncQueue<SplitControlCommand>();
  #preopenedWorker?: {
    socket: WebSocket; queue: AsyncQueue<TunnelMessage>; connectionId: string; workerId: string; lastActivity: number;
  };

  constructor(config: AgentConfig, context: ToolContext, options: BuiltinTunnelManagerOptions = {}) {
    this.config = config;
    this.context = context;
    this.options = options;
  }

  async start(): Promise<void> {
    if (!this.config.tunnel?.enabled || !this.config.tunnel.publicUrl || !this.#stopped) return;
    const securityError = publicTunnelSecurityError(this.config);
    if (securityError) {
      this.context.tunnelStatus = {
        enabled: true, state: 'error', publicUrl: this.config.tunnel.publicUrl,
        workers: 0, connectedWorkers: 0, connectingWorkers: 0, idleWorkers: 0,
        busyWorkers: 0, recycledWorkers: 0, completedRequests: 0,
        startedAt: this.#now(), lastError: securityError
      };
      throw new Error(securityError);
    }
    this.#stopped = false;
    this.#abort = new AbortController();
    this.context.tunnelStatus = {
      enabled: true, state: 'starting', publicUrl: this.config.tunnel.publicUrl,
      workers: 1, connectedWorkers: 0, connectingWorkers: 0, idleWorkers: 0,
      busyWorkers: 0, recycledWorkers: 0, completedRequests: 0, startedAt: this.#now()
    };
    try {
      const configuredEndpoint = parseBuiltinPublicUrl(this.config.tunnel.publicUrl);
      const serverOrigin = new URL(configuredEndpoint.publicUrl).origin;
      const preferredIdentity = this.options.sharedDeviceIdentities?.get(serverOrigin);
      const loaded = await loadOrEnroll(
        this.config, configuredEndpoint, this.options.enrollmentFetch ?? fetch, preferredIdentity
      );
      this.#identity = loaded.identity;
      if (!this.options.sharedDeviceIdentities?.has(serverOrigin)) {
        this.options.sharedDeviceIdentities?.set(serverOrigin, loaded.identity);
      }
      this.#sharedHubKey = `${serverOrigin}|${loaded.identity.deviceId}|${loaded.identity.serverId ?? ''}`;
      this.#endpoint = builtinEndpointForClient(configuredEndpoint.publicUrl, this.#identity.clientId);
      const endpointChanged = this.#endpoint.publicUrl !== configuredEndpoint.publicUrl;
      this.config.tunnel.publicUrl = this.#endpoint.publicUrl;
      this.config.publicBaseUrl = this.#endpoint.baseUrl;
      this.context.config.tunnel!.publicUrl = this.#endpoint.publicUrl;
      this.context.config.publicBaseUrl = this.#endpoint.baseUrl;
      if (loaded.enrollmentCompleted) {
        delete this.config.tunnel.enrollmentUrl;
        delete this.context.config.tunnel!.enrollmentUrl;
      }
      this.context.tunnelStatus.publicUrl = this.#endpoint.publicUrl;
      Object.assign(this.context.tunnelStatus, {
        serverOrigin,
        websocketUrl: this.#endpoint.websocketUrl,
        serverId: loaded.identity.serverId,
        deviceId: loaded.identity.deviceId
      });
      if (endpointChanged || loaded.enrollmentCompleted) {
        await this.options.onEndpointResolved?.({
          publicUrl: this.#endpoint.publicUrl,
          publicBaseUrl: this.#endpoint.baseUrl,
          enrollmentCompleted: loaded.enrollmentCompleted
        });
      }
    } catch (error) {
      this.#stopped = true;
      this.#abort.abort();
      this.context.tunnelStatus.state = 'error';
      this.context.tunnelStatus.workers = 0;
      this.context.tunnelStatus.lastError = error instanceof Error ? error.message : String(error);
      throw error;
    }
    if (await this.#tryJoinSharedSplitHub()) return;
    const splitStarted = await this.#tryStartSplitControl();
    if (splitStarted && this.#sharedHubKey) this.options.sharedSplitHubs?.set(this.#sharedHubKey, this);
    if (!splitStarted) this.#spawnWorker();
    else {
      const startConnections = Math.max(1, this.#multiplexPolicy?.start_connections ?? 1);
      for (let index = 0; index < startConnections; index += 1) this.#spawnWorker();
    }
    this.#reconcileTimer = setInterval(() => this.#reconcile(), Math.max(20, this.options.reconcileIntervalMs ?? 1_000));
    this.#reconcileTimer.unref();
  }

  async stop(): Promise<void> {
    if (this.#stopped) return;
    if (this.#sharedOwner) {
      const owner = this.#sharedOwner;
      this.#stopped = true;
      try { if (!owner.#stopped) await owner.#requestSharedRoute({ kind: 'remove', manager: this }); } catch { /* owner is stopping */ }
      this.#sharedOwner = undefined;
      this.#splitMode = false;
      this.#multiplexActive = false;
      this.#splitSessionId = undefined;
      this.#splitRouteId = undefined;
      this.#splitStreamLimit = undefined;
      if (this.context.tunnelStatus) Object.assign(this.context.tunnelStatus, {
        state: 'stopped', workers: 0, connectedWorkers: 0, connectingWorkers: 0, idleWorkers: 0, busyWorkers: 0
      });
      return;
    }
    if (this.#splitMode && this.#splitRoutesByManager.size > 1) await this.#handoffSharedSplitHub();
    this.#stopped = true;
    if (this.#sharedHubKey && this.options.sharedSplitHubs?.get(this.#sharedHubKey) === this) {
      this.options.sharedSplitHubs.delete(this.#sharedHubKey);
    }
    this.#abort.abort();
    this.#splitMode = false;
    this.#splitControlSocket?.terminate();
    if (this.#reconcileTimer) clearInterval(this.#reconcileTimer);
    this.#reconcileTimer = undefined;
    const tasks: Promise<void>[] = [];
    for (const worker of this.#workers.values()) {
      worker.retire = true;
      worker.state = 'retiring';
      worker.socket?.terminate();
      if (worker.task) tasks.push(worker.task);
    }
    await Promise.allSettled(tasks);
    if (this.#splitControlTask) await Promise.allSettled([this.#splitControlTask]);
    this.#splitControlTask = undefined;
    this.#splitControlSocket = undefined;
    this.#splitSessionId = undefined;
    this.#splitRouteId = undefined;
    this.#splitStreamLimit = undefined;
    this.#splitMode = false;
    this.#preopenedWorker = undefined;
    this.#workers.clear();
    if (this.context.tunnelStatus) {
      Object.assign(this.context.tunnelStatus, {
        state: 'stopped', workers: 0, connectedWorkers: 0,
        connectingWorkers: 0, idleWorkers: 0, busyWorkers: 0
      });
    }
  }

  async enforceSecurity(): Promise<void> {
    const securityError = publicTunnelSecurityError(this.config);
    if (!securityError) return;
    await this.stop();
    if (this.context.tunnelStatus) {
      this.context.tunnelStatus.state = 'error';
      this.context.tunnelStatus.lastError = securityError;
    }
  }

  async reconfigure(tunnel: AgentConfig['tunnel'], publicBaseUrl?: string): Promise<void> {
    const previousTunnel = this.config.tunnel ? structuredClone(this.config.tunnel) : undefined;
    const previousPublicBaseUrl = this.config.publicBaseUrl;
    await this.stop();
    this.#identity = undefined;
    this.#endpoint = undefined;
    this.#policy = undefined;
    this.#demandTarget = 0;
    this.#demandSeenAt = 0;
    this.#lastPressureAt = 0;
    this.#idleExcessSince = 0;
    this.#recycledWorkers = 0;
    this.#multiplexActive = false;
    this.#multiplexPolicy = undefined;
    this.#multiplexActiveStreams = 0;
    this.#multiplexReportedActiveStreams = 0;
    this.#multiplexQueuedRequests = 0;
    this.#multiplexDesiredConnections = 1;
    this.#multiplexDemandSeenAt = 0;
    this.#multiplexIdleExcessSince = 0;
    this.#splitMode = false;
    this.#splitControlSocket = undefined;
    this.#splitControlTask = undefined;
    this.#splitSessionId = undefined;
    this.#splitRouteId = undefined;
    this.#splitStreamLimit = undefined;
    this.#preopenedWorker = undefined;
    if (tunnel) {
      this.config.tunnel = structuredClone(tunnel);
      this.context.config.tunnel = structuredClone(tunnel);
    } else {
      delete this.config.tunnel;
      delete this.context.config.tunnel;
    }
    if (publicBaseUrl) {
      this.config.publicBaseUrl = publicBaseUrl;
      this.context.config.publicBaseUrl = publicBaseUrl;
    } else {
      delete this.config.publicBaseUrl;
      delete this.context.config.publicBaseUrl;
    }
    if (!tunnel?.enabled) {
      this.context.tunnelStatus = {
        enabled: false, state: 'disabled', workers: 0, connectedWorkers: 0, completedRequests: 0
      };
      return;
    }
    try {
      await this.start();
    } catch (error) {
      await this.stop();
      if (previousTunnel) {
        this.config.tunnel = structuredClone(previousTunnel);
        this.context.config.tunnel = structuredClone(previousTunnel);
      } else {
        delete this.config.tunnel;
        delete this.context.config.tunnel;
      }
      if (previousPublicBaseUrl) {
        this.config.publicBaseUrl = previousPublicBaseUrl;
        this.context.config.publicBaseUrl = previousPublicBaseUrl;
      } else {
        delete this.config.publicBaseUrl;
        delete this.context.config.publicBaseUrl;
      }
      if (previousTunnel?.enabled) {
        try {
          await this.start();
        } catch (rollbackError) {
          throw new AggregateError([error, rollbackError], 'Tunnel reconfiguration failed and the previous tunnel could not be restored.');
        }
      } else {
        this.context.tunnelStatus = {
          enabled: false, state: 'disabled', workers: 0, connectedWorkers: 0, completedRequests: 0
        };
      }
      throw error;
    }
  }

  #now(): number {
    return this.options.now?.() ?? Date.now();
  }

  #demandNow(): number {
    return this.options.demandNow?.() ?? this.#now();
  }

  #spawnWorker(): void {
    if (this.#stopped) return;
    const worker: ManagedWorker = {
      index: this.#nextWorkerIndex++, state: 'connecting', connectingSince: this.#now(),
      retire: false, completedRequests: 0, multiplexActiveStreams: 0
    };
    this.#workers.set(worker.index, worker);
    worker.task = this.#workerLoop(worker).finally(() => {
      this.#workers.delete(worker.index);
      this.#updateStatus();
      if (!this.#stopped) queueMicrotask(() => this.#reconcile());
    });
    this.#updateStatus();
  }

  #setWorkerState(worker: ManagedWorker, state: WorkerState): void {
    if (worker.state === state) return;
    const previous = worker.state;
    worker.state = state;
    if (state === 'connecting') worker.connectingSince = this.#now();
    if (state === 'busy' || (previous === 'busy' && state === 'idle')) this.#lastPressureAt = this.#now();
    this.#updateStatus();
    this.#reconcile();
  }

  #applyPolicy(value: unknown): void {
    const policy = normalizeWorkerPolicy(value);
    if (this.#policy && policy.revision < this.#policy.revision) return;
    this.#policy = policy;
    if (this.context.tunnelStatus) {
      this.context.tunnelStatus.workerPolicy = policy;
      this.context.tunnelStatus.policyRevision = policy.revision;
      this.context.tunnelStatus.workers = policy.max_workers;
    }
    this.#reconcile();
  }

  #applyDemand(value: unknown): void {
    const demand = parseDemand(value);
    if (!demand) return;
    this.#demandTarget = demand.desired_workers;
    this.#demandSeenAt = this.#demandNow();
    if (demand.queued_requests > 0) this.#lastPressureAt = this.#now();
    this.#reconcile();
  }

  #counts(): PoolCounts {
    let connecting = 0;
    let idle = 0;
    let busy = 0;
    for (const worker of this.#workers.values()) {
      if (worker.state === 'connecting') connecting += 1;
      else if (worker.state === 'idle') idle += 1;
      else if (worker.state === 'busy') busy += 1;
    }
    return { total: connecting + idle + busy, connecting, idle, busy };
  }

  #effectiveConnecting(policy: WorkerPolicy): number {
    const now = this.#now();
    let count = 0;
    for (const worker of this.#workers.values()) {
      if (worker.state === 'connecting' && now - worker.connectingSince <= policy.connecting_capacity_grace_ms) count += 1;
    }
    return count;
  }

  #reconcile(): void {
    if (this.#multiplexActive && this.#multiplexPolicy) {
      const policy = this.#multiplexPolicy;
      const now = this.#now();
      const workers = [...this.#workers.values()].filter(worker => !worker.retire);
      const connected = workers.filter(worker => worker.multiplex && worker.socket?.readyState === WebSocket.OPEN).length;
      const connecting = workers.filter(worker => worker.state === 'connecting').length;
      const idle = workers.filter(worker => worker.multiplex && worker.socket?.readyState === WebSocket.OPEN && worker.multiplexActiveStreams === 0).length;
      const demandActive = this.#multiplexDemandSeenAt > 0
        && this.#demandNow() - this.#multiplexDemandSeenAt <= (this.options.demandTtlMs ?? BUILTIN_TUNNEL_DEMAND_TTL_MS);
      const queuedRequests = demandActive ? this.#multiplexQueuedRequests : 0;
      const desiredConnections = demandActive ? this.#multiplexDesiredConnections : policy.min_connections;
      const activeStreams = demandActive
        ? Math.max(this.#multiplexActiveStreams, this.#multiplexReportedActiveStreams)
        : this.#multiplexActiveStreams;
      if (connected > policy.min_connections && idle > 0 && activeStreams === 0 && queuedRequests === 0) {
        if (this.#multiplexIdleExcessSince === 0) this.#multiplexIdleExcessSince = now;
      } else {
        this.#multiplexIdleExcessSince = 0;
      }
      const adjustment = multiplexPoolAdjustment(policy, {
        connectedConnections: connected,
        connectingConnections: connecting,
        activeStreams,
        queuedRequests,
        desiredConnections,
        idleConnections: idle,
        idleExcessElapsed: this.#multiplexIdleExcessSince > 0
          && now - this.#multiplexIdleExcessSince >= policy.scale_down_delay_seconds * 1_000
      });
      let retire = adjustment.retire;
      const idleWorkers = workers
        .filter(worker => worker.multiplex && worker.socket?.readyState === WebSocket.OPEN && worker.multiplexActiveStreams === 0)
        .sort((left, right) => right.index - left.index);
      for (const worker of idleWorkers) {
        if (retire <= 0) break;
        worker.retire = true;
        worker.state = 'retiring';
        worker.socket?.close(1000, 'multiplex-scale-down');
        retire -= 1;
      }
      for (let index = 0; index < adjustment.spawn; index += 1) this.#spawnWorker();
      if (adjustment.retire > 0) this.#multiplexIdleExcessSince = 0;
      this.#updateStatus();
      return;
    }
    if (this.#stopped || !this.#policy) { this.#updateStatus(); return; }
    const policy = this.#policy;
    const now = this.#now();
    const counts = this.#counts();
    const demandActive = this.#demandSeenAt > 0
      && this.#demandNow() - this.#demandSeenAt <= (this.options.demandTtlMs ?? BUILTIN_TUNNEL_DEMAND_TTL_MS);
    const desiredWorkers = demandActive ? Math.min(this.#demandTarget, policy.max_workers) : 0;
    const effectiveConnecting = this.#effectiveConnecting(policy);
    const warmActive = policy.burst_warm_seconds > 0
      && this.#lastPressureAt > 0
      && now - this.#lastPressureAt < policy.burst_warm_seconds * 1_000;
    const scaleDownFloor = warmActive ? configuredBurstWarmFloor(policy) : policy.max_idle_workers;
    if (counts.total > scaleDownFloor && counts.idle > 0) {
      if (this.#idleExcessSince === 0) this.#idleExcessSince = now;
    } else this.#idleExcessSince = 0;
    const idleExcessElapsed = this.#idleExcessSince > 0
      && now - this.#idleExcessSince >= policy.scale_down_delay_seconds * 1_000;
    const adjustment = poolAdjustment(
      policy, counts, effectiveConnecting, configuredMaxConnecting(policy),
      desiredWorkers, idleExcessElapsed, scaleDownFloor
    );

    let retire = adjustment.retire;
    const idleWorkers = [...this.#workers.values()]
      .filter(worker => worker.state === 'idle' && !worker.retire)
      .sort((left, right) => right.index - left.index);
    for (const worker of idleWorkers) {
      if (retire <= 0) break;
      worker.retire = true;
      worker.state = 'retiring';
      worker.socket?.close(1000, 'scale-down');
      retire -= 1;
    }
    for (let index = 0; index < adjustment.spawn; index += 1) this.#spawnWorker();
    if (adjustment.retire > 0 && counts.total - adjustment.retire <= scaleDownFloor) this.#idleExcessSince = 0;
    this.#updateStatus();
  }

  #updateStatus(): void {
    const status = this.context.tunnelStatus;
    if (!status) return;
    const counts = this.#counts();
    if (this.#multiplexActive) {
      const multiplexWorkers = [...this.#workers.values()].filter(worker => worker.multiplex && worker.socket?.readyState === WebSocket.OPEN);
      status.connectedWorkers = multiplexWorkers.length;
      status.connectingWorkers = [...this.#workers.values()].filter(worker => worker.state === 'connecting').length;
      status.idleWorkers = multiplexWorkers.filter(worker => worker.multiplexActiveStreams === 0).length;
      status.busyWorkers = multiplexWorkers.filter(worker => worker.multiplexActiveStreams > 0).length;
      status.workers = this.#multiplexPolicy?.max_connections ?? Math.max(1, counts.total);
      status.policyRevision = this.#multiplexPolicy?.revision;
      const streamsPerConnection = this.#multiplexPolicy?.streams_per_connection ?? this.#splitStreamLimit ?? 1;
      const streamCapacity = multiplexWorkers.length * streamsPerConnection;
      Object.assign(status, {
        transportMode: this.#splitMode ? 'split-v5' : 'multiplex-v5',
        controlConnected: this.#splitMode ? this.#splitControlSocket?.readyState === WebSocket.OPEN : undefined,
        sharedSession: Boolean(this.#sharedOwner) || this.#splitRoutesByManager.size > 1,
        sessionId: this.#splitSessionId,
        routeId: this.#splitRouteId,
        streamLimit: this.#splitStreamLimit,
        dataConnections: multiplexWorkers.length,
        maxDataConnections: this.#multiplexPolicy?.max_connections,
        streamsPerConnection,
        activeStreams: this.#multiplexActiveStreams,
        streamCapacity,
        availableStreamSlots: Math.max(0, streamCapacity - this.#multiplexActiveStreams),
        queuedRequests: this.#multiplexQueuedRequests,
        desiredDataConnections: this.#multiplexDesiredConnections
      });
      if (this.#stopped) status.state = status.enabled ? 'stopped' : 'disabled';
      else if (status.connectedWorkers > 0) status.state = 'running';
      else if (status.lastError) status.state = 'reconnecting';
      else status.state = 'starting';
      for (const route of this.#splitRoutes.values()) {
        if (route.manager !== this) {
          route.manager.#multiplexPolicy = this.#multiplexPolicy;
          route.manager.#syncStatusFromSharedOwner();
        }
      }
      return;
    }
    Object.assign(status, {
      transportMode: 'legacy-v4',
      controlConnected: undefined,
      sharedSession: false,
      sessionId: undefined,
      routeId: undefined,
      streamLimit: undefined,
      dataConnections: undefined,
      maxDataConnections: undefined,
      streamsPerConnection: undefined,
      activeStreams: undefined,
      streamCapacity: undefined,
      availableStreamSlots: undefined,
      queuedRequests: undefined,
      desiredDataConnections: undefined
    });
    status.connectedWorkers = counts.idle + counts.busy;
    status.connectingWorkers = counts.connecting;
    status.idleWorkers = counts.idle;
    status.busyWorkers = counts.busy;
    status.recycledWorkers = this.#recycledWorkers;
    if (!this.#policy) status.workers = Math.max(1, counts.total);
    if (this.#stopped) status.state = status.enabled ? 'stopped' : 'disabled';
    else if (status.connectedWorkers > 0) status.state = 'running';
    else if (status.lastError) status.state = 'reconnecting';
    else status.state = 'starting';
  }

  async #rollbackSharedHubPromotion(previousOwner: BuiltinTunnelManager): Promise<void> {
    this.#abort.abort();
    this.#splitMode = false;
    this.#splitControlSocket?.terminate();
    if (this.#reconcileTimer) clearInterval(this.#reconcileTimer);
    this.#reconcileTimer = undefined;
    const tasks: Promise<void>[] = [];
    for (const worker of this.#workers.values()) {
      worker.retire = true;
      worker.socket?.terminate();
      if (worker.task) tasks.push(worker.task);
    }
    await Promise.allSettled(tasks);
    if (this.#splitControlTask) await Promise.allSettled([this.#splitControlTask]);
    this.#workers.clear();
    this.#splitRoutes.clear();
    this.#splitRoutesByManager.clear();
    this.#splitControlTask = undefined;
    this.#splitControlSocket = undefined;
    this.#abort = new AbortController();
    this.#sharedOwner = previousOwner;
    const previousRoute = previousOwner.#splitRoutesByManager.get(this);
    this.#splitMode = Boolean(previousRoute);
    this.#multiplexActive = Boolean(previousRoute);
    this.#multiplexPolicy = previousOwner.#multiplexPolicy;
    this.#splitSessionId = previousOwner.#splitSessionId;
    this.#splitRouteId = previousRoute?.routeId;
    this.#splitStreamLimit = previousRoute?.streamLimit;
    this.#syncStatusFromSharedOwner();
  }

  async #handoffSharedSplitHub(): Promise<boolean> {
    if (!this.#splitMode || this.#sharedOwner || !this.#sharedHubKey) return false;
    const candidates = [...this.#splitRoutesByManager.values()]
      .map(route => route.manager)
      .filter(manager => manager !== this && !manager.#stopped);
    const nextOwner = candidates[0];
    if (!nextOwner) return false;

    const previousOwner = nextOwner.#sharedOwner;
    nextOwner.#sharedOwner = undefined;
    const started = await nextOwner.#tryStartSplitControl();
    if (!started) {
      nextOwner.#sharedOwner = previousOwner;
      nextOwner.#preopenedWorker?.socket.terminate();
      nextOwner.#preopenedWorker = undefined;
      throw new Error('failed to hand off the shared split tunnel before stopping its owner');
    }
    const startConnections = Math.max(1, nextOwner.#multiplexPolicy?.start_connections ?? 1);
    for (let index = 0; index < startConnections; index += 1) nextOwner.#spawnWorker();
    if (!nextOwner.#reconcileTimer) {
      nextOwner.#reconcileTimer = setInterval(
        () => nextOwner.#reconcile(), Math.max(20, nextOwner.options.reconcileIntervalMs ?? 1_000)
      );
      nextOwner.#reconcileTimer.unref();
    }

    try {
      const migrated: Array<{ manager: BuiltinTunnelManager; route: SharedSplitRoute }> = [];
      for (const manager of candidates.slice(1)) {
        const route = await nextOwner.#requestSharedRoute({ kind: 'add', manager });
        migrated.push({ manager, route });
      }
      for (const { manager, route } of migrated) {
        manager.#sharedOwner = nextOwner;
        manager.#splitMode = true;
        manager.#multiplexActive = true;
        manager.#multiplexPolicy = nextOwner.#multiplexPolicy;
        manager.#splitSessionId = nextOwner.#splitSessionId;
        manager.#splitRouteId = route.routeId;
        manager.#splitStreamLimit = route.streamLimit;
        manager.#syncStatusFromSharedOwner();
      }
      nextOwner.options.sharedSplitHubs?.set(this.#sharedHubKey, nextOwner);
      nextOwner.#updateStatus();
      return true;
    } catch (error) {
      await nextOwner.#rollbackSharedHubPromotion(this);
      throw error;
    }
  }

  async #tryJoinSharedSplitHub(): Promise<boolean> {
    if (!this.#sharedHubKey || !this.#identity) return false;
    const owner = this.options.sharedSplitHubs?.get(this.#sharedHubKey);
    if (!owner || owner === this || owner.#stopped || !owner.#splitMode || !owner.#splitSessionId) return false;
    if (!owner.#identity || owner.#identity.deviceId !== this.#identity.deviceId
      || owner.#identity.serverId !== this.#identity.serverId
      || owner.#identity.publicKeyRaw !== this.#identity.publicKeyRaw) return false;
    const route = await owner.#requestSharedRoute({ kind: 'add', manager: this });
    this.#sharedOwner = owner;
    this.#splitMode = true;
    this.#multiplexActive = true;
    this.#multiplexPolicy = owner.#multiplexPolicy;
    this.#splitSessionId = owner.#splitSessionId;
    this.#splitRouteId = route.routeId;
    this.#splitStreamLimit = route.streamLimit;
    this.#syncStatusFromSharedOwner();
    return true;
  }

  #requestSharedRoute(input: { kind: 'add' | 'remove'; manager: BuiltinTunnelManager }): Promise<SharedSplitRoute> {
    return new Promise<SharedSplitRoute>((resolve, reject) => {
      if (input.kind === 'add') {
        let begun = false;
        let cancelled = false;
        const timer = setTimeout(() => {
          if (begun) return;
          cancelled = true;
          reject(new Error('shared split route command timed out'));
        }, 10_000);
        timer.unref();
        const begin = () => {
          if (cancelled) return false;
          if (!begun) { begun = true; clearTimeout(timer); }
          return true;
        };
        this.#splitControlCommands.push({
          kind: 'add', manager: input.manager, begin,
          resolve: route => { clearTimeout(timer); resolve(route); },
          reject: error => { clearTimeout(timer); reject(error); }
        });
        return;
      }
      const timer = setTimeout(() => reject(new Error('shared split route command timed out')), 10_000);
      timer.unref();
      this.#splitPendingRemovals.add(input.manager);
      this.#splitControlCommands.push({
        kind: 'remove', manager: input.manager,
        resolve: () => { clearTimeout(timer); resolve({ manager: input.manager, routeId: 0, streamLimit: 0 }); },
        reject: error => { clearTimeout(timer); reject(error); }
      });
    });
  }

  #syncStatusFromSharedOwner(): void {
    const owner = this.#sharedOwner;
    const status = this.context.tunnelStatus;
    const ownerStatus = owner?.context.tunnelStatus;
    if (!status || !ownerStatus) return;
    Object.assign(status, {
      state: ownerStatus.state, workers: ownerStatus.workers, connectedWorkers: ownerStatus.connectedWorkers,
      connectingWorkers: ownerStatus.connectingWorkers, idleWorkers: ownerStatus.idleWorkers,
      busyWorkers: ownerStatus.busyWorkers, recycledWorkers: ownerStatus.recycledWorkers,
      policyRevision: ownerStatus.policyRevision, lastError: ownerStatus.lastError,
      serverOrigin: ownerStatus.serverOrigin, websocketUrl: ownerStatus.websocketUrl,
      serverId: ownerStatus.serverId, deviceId: ownerStatus.deviceId,
      transportMode: ownerStatus.transportMode, controlConnected: ownerStatus.controlConnected,
      sharedSession: true, sessionId: ownerStatus.sessionId, routeId: this.#splitRouteId,
      streamLimit: this.#splitStreamLimit, dataConnections: ownerStatus.dataConnections,
      maxDataConnections: ownerStatus.maxDataConnections, streamsPerConnection: ownerStatus.streamsPerConnection,
      activeStreams: ownerStatus.activeStreams, streamCapacity: ownerStatus.streamCapacity,
      availableStreamSlots: ownerStatus.availableStreamSlots, queuedRequests: ownerStatus.queuedRequests,
      desiredDataConnections: ownerStatus.desiredDataConnections
    });
  }

  async #tryStartSplitControl(): Promise<boolean> {
    if (!this.#endpoint || !this.#identity) return false;
    const websocketUrl = this.options.websocketUrlOverride ?? this.#endpoint.websocketUrl;
    const connectionId = `${process.pid}-control-${randomUUID().slice(0, 8)}`;
    let lastActivity = this.#now();
    let opened: { socket: WebSocket; queue: AsyncQueue<TunnelMessage> } | undefined;
    try {
      opened = await openSplitWebSocket(
        websocketUrl, this.#endpoint, this.#identity, 'control', connectionId, undefined,
        () => { lastActivity = this.#now(); }
      );
      if (opened.socket.protocol !== MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL) {
        this.#preopenedWorker = {
          socket: opened.socket, queue: opened.queue, connectionId, workerId: connectionId, lastActivity
        };
        return false;
      }
      const ready = await authenticateSplitSession(opened.socket, opened.queue, this.#identity, connectionId, 'control');
      const writer = new MultiplexWriter(opened.socket);
      this.#multiplexPolicy = ready.policy;
      this.#multiplexActive = true;
      this.#policy = undefined;
      const route = await registerMultiplexRoute(writer, opened.queue, this.#identity.clientId, value => this.#applyMultiplexDemand(value));
      if (route.streamLimit > ready.policy.streams_per_connection) throw new Error('server route stream limit exceeds split connection policy');
      this.#splitMode = true;
      this.#splitSessionId = ready.sessionId;
      this.#splitRouteId = route.routeId;
      this.#splitStreamLimit = route.streamLimit;
      const ownRoute: SharedSplitRoute = { manager: this, routeId: route.routeId, streamLimit: route.streamLimit };
      this.#splitRoutes.set(route.routeId, ownRoute);
      this.#splitRoutesByManager.set(this, ownRoute);
      this.#splitControlSocket = opened.socket;
      this.#splitControlTask = this.#splitControlSupervisor(opened.socket, opened.queue, lastActivity);
      if (this.context.tunnelStatus) this.context.tunnelStatus.lastError = undefined;
      return true;
    } catch {
      if (opened?.socket !== this.#preopenedWorker?.socket) opened?.socket.terminate();
      this.#splitMode = false;
      this.#splitControlSocket = undefined;
      this.#splitSessionId = undefined;
      this.#splitRouteId = undefined;
      this.#splitStreamLimit = undefined;
      this.#multiplexActive = false;
      this.#multiplexPolicy = undefined;
      return false;
    }
  }

  async #nextSplitControlEvent(queue: AsyncQueue<TunnelMessage>): Promise<
    { type: 'control'; control: JsonObject } | { type: 'command'; command: SplitControlCommand }
  > {
    const controlAbort = new AbortController();
    const commandAbort = new AbortController();
    const stop = () => {
      const reason = this.#abort.signal.reason ?? new Error('split control stopped');
      controlAbort.abort(reason);
      commandAbort.abort(reason);
    };
    if (this.#abort.signal.aborted) stop();
    else this.#abort.signal.addEventListener('abort', stop, { once: true });
    try {
      return await Promise.race([
        nextControl(queue, 0, controlAbort.signal).then(control => ({ type: 'control' as const, control })),
        this.#splitControlCommands.shift(0, commandAbort.signal).then(command => ({ type: 'command' as const, command }))
      ]);
    } finally {
      this.#abort.signal.removeEventListener('abort', stop);
      controlAbort.abort(new Error('split control race completed'));
      commandAbort.abort(new Error('split control race completed'));
    }
  }

  async #handleSplitControlCommand(
    command: SplitControlCommand, socket: WebSocket, writer: MultiplexWriter, queue: AsyncQueue<TunnelMessage>
  ): Promise<void> {
    try {
      if (command.kind === 'add' && !command.begin()) return;
      if (command.kind === 'remove') {
        const existing = this.#splitRoutesByManager.get(command.manager);
        if (existing) {
          await writer.control({ kind: 'route_unregister', route_id: existing.routeId });
          this.#splitRoutes.delete(existing.routeId);
          this.#splitRoutesByManager.delete(command.manager);
        }
        this.#splitPendingRemovals.delete(command.manager);
        command.resolve();
        return;
      }
      const manager = command.manager;
      if (!this.#identity || !manager.#identity || !this.#endpoint || !manager.#endpoint
        || this.#identity.deviceId !== manager.#identity.deviceId
        || this.#identity.serverId !== manager.#identity.serverId
        || this.#identity.publicKeyRaw !== manager.#identity.publicKeyRaw
        || (this.options.websocketUrlOverride ?? this.#endpoint.websocketUrl)
          !== (manager.options.websocketUrlOverride ?? manager.#endpoint.websocketUrl)) {
        throw new Error('shared tunnel route does not belong to this device/server session');
      }
      const existing = this.#splitRoutesByManager.get(manager);
      if (existing) { command.resolve(existing); return; }
      const registered = await registerMultiplexRoute(
        writer, queue, manager.#identity.clientId, value => this.#applyMultiplexDemand(value)
      );
      if (this.#multiplexPolicy && registered.streamLimit > this.#multiplexPolicy.streams_per_connection) {
        throw new Error('shared tunnel route exceeds split connection policy');
      }
      const route: SharedSplitRoute = { manager, routeId: registered.routeId, streamLimit: registered.streamLimit };
      this.#splitRoutes.set(route.routeId, route);
      this.#splitRoutesByManager.set(manager, route);
      manager.#splitRouteId = route.routeId;
      manager.#splitStreamLimit = route.streamLimit;
      manager.#splitSessionId = this.#splitSessionId;
      manager.#multiplexPolicy = this.#multiplexPolicy;
      command.resolve(route);
      this.#updateStatus();
    } catch (error) {
      const commandError = error instanceof Error ? error : new Error(String(error));
      const routeWaitTimedOut = commandError.message === 'queue wait timed out';
      if ((socket.readyState !== WebSocket.OPEN || routeWaitTimedOut) && !this.#stopped && this.#splitMode) {
        if (routeWaitTimedOut && socket.readyState === WebSocket.OPEN) socket.terminate();
        this.#splitControlCommands.push(command);
        throw commandError;
      }
      if (command.kind === 'remove') this.#splitPendingRemovals.delete(command.manager);
      command.reject(commandError);
    }
  }

  async #replaceExpiredSplitSession(
    websocketUrl: string,
    touch: () => void
  ): Promise<{ socket: WebSocket; queue: AsyncQueue<TunnelMessage>; writer: MultiplexWriter }> {
    if (!this.#endpoint || !this.#identity) throw new Error('tunnel identity is unavailable');
    const connectionId = `${process.pid}-control-${randomUUID().slice(0, 8)}`;
    const opened = await openSplitWebSocket(
      websocketUrl, this.#endpoint, this.#identity, 'control', connectionId, undefined, touch
    );
    try {
      if (opened.socket.protocol !== MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL) {
        throw new Error('server stopped accepting split control sessions during reconnect');
      }
      const ready = await authenticateSplitSession(
        opened.socket, opened.queue, this.#identity, connectionId, 'control'
      );
      const writer = new MultiplexWriter(opened.socket);
      const members = [...this.#splitRoutesByManager.keys()]
        .filter(manager => !this.#splitPendingRemovals.has(manager));
      const nextRoutes = new Map<number, SharedSplitRoute>();
      const nextRoutesByManager = new Map<BuiltinTunnelManager, SharedSplitRoute>();
      for (const manager of members) {
        if (!manager.#identity) throw new Error('shared tunnel route identity disappeared during reconnect');
        const registered = await registerMultiplexRoute(
          writer, opened.queue, manager.#identity.clientId, value => this.#applyMultiplexDemand(value)
        );
        if (registered.streamLimit > ready.policy.streams_per_connection) {
          throw new Error('server route stream limit exceeds replacement split connection policy');
        }
        const route: SharedSplitRoute = {
          manager, routeId: registered.routeId, streamLimit: registered.streamLimit
        };
        nextRoutes.set(route.routeId, route);
        nextRoutesByManager.set(manager, route);
      }
      const ownRoute = nextRoutesByManager.get(this);
      if (!ownRoute) throw new Error('replacement split control session lost the owner route');

      this.#splitRoutes = nextRoutes;
      this.#splitRoutesByManager = nextRoutesByManager;
      this.#multiplexPolicy = ready.policy;
      this.#splitSessionId = ready.sessionId;
      this.#splitRouteId = ownRoute.routeId;
      this.#splitStreamLimit = ownRoute.streamLimit;
      this.#multiplexDemandSeenAt = 0;
      this.#multiplexQueuedRequests = 0;
      this.#multiplexDesiredConnections = Math.max(1, ready.policy.start_connections);
      for (const route of nextRoutesByManager.values()) {
        route.manager.#splitSessionId = ready.sessionId;
        route.manager.#splitRouteId = route.routeId;
        route.manager.#splitStreamLimit = route.streamLimit;
        route.manager.#multiplexPolicy = ready.policy;
      }
      for (const worker of this.#workers.values()) worker.socket?.terminate();
      this.#updateStatus();
      return { socket: opened.socket, queue: opened.queue, writer };
    } catch (error) {
      opened.socket.terminate();
      throw error;
    }
  }

  async #splitControlSupervisor(
    initialSocket: WebSocket, initialQueue: AsyncQueue<TunnelMessage>, initialLastActivity: number
  ): Promise<void> {
    let socket = initialSocket;
    let queue = initialQueue;
    let writer = new MultiplexWriter(socket);
    let lastActivity = initialLastActivity;
    let attempt = 0;
    while (!this.#stopped && this.#splitMode) {
      let heartbeat: NodeJS.Timeout | undefined;
      try {
        this.#splitControlSocket = socket;
        heartbeat = setInterval(() => {
          if (socket.readyState !== WebSocket.OPEN) return;
          if (this.#now() - lastActivity >= BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS) socket.terminate();
          else socket.ping();
        }, BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS);
        heartbeat.unref();
        while (!this.#stopped && socket.readyState === WebSocket.OPEN) {
          const event = await this.#nextSplitControlEvent(queue);
          lastActivity = this.#now();
          if (event.type === 'command') {
            await this.#handleSplitControlCommand(event.command, socket, writer, queue);
            continue;
          }
          const control = event.control;
          if (control.kind === 'capacity_update') { this.#applyMultiplexDemand(control.demand); continue; }
          if (control.kind === 'error') throw new Error(String(control.message ?? 'split control error'));
          throw new Error(`unexpected split control message: ${control.kind}`);
        }
      } catch (error) {
        if (!this.#stopped && this.context.tunnelStatus) this.context.tunnelStatus.lastError = error instanceof Error ? error.message : String(error);
      } finally {
        if (heartbeat) clearInterval(heartbeat);
        if (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING) socket.terminate();
      }
      if (this.#stopped || !this.#splitMode) break;
      const reconnectSessionId = this.#splitSessionId;
      attempt += 1;
      await new Promise<void>(resolve => {
        const timer = setTimeout(resolve, builtinTunnelReconnectDelayMs(attempt, false));
        timer.unref();
        this.#abort.signal.addEventListener('abort', () => { clearTimeout(timer); resolve(); }, { once: true });
      });
      if (this.#stopped || !this.#endpoint || !this.#identity) break;
      let opened: { socket: WebSocket; queue: AsyncQueue<TunnelMessage> } | undefined;
      try {
        const connectionId = `${process.pid}-control-${randomUUID().slice(0, 8)}`;
        const websocketUrl = this.options.websocketUrlOverride ?? this.#endpoint.websocketUrl;
        lastActivity = this.#now();
        opened = await openSplitWebSocket(
          websocketUrl, this.#endpoint, this.#identity, 'control', connectionId, reconnectSessionId,
          () => { lastActivity = this.#now(); }
        );
        const ready = await authenticateSplitSession(
          opened.socket, opened.queue, this.#identity, connectionId, 'control', reconnectSessionId
        );
        const nextWriter = new MultiplexWriter(opened.socket);
        this.#multiplexPolicy = ready.policy;
        this.#splitSessionId = ready.sessionId;
        for (const route of this.#splitRoutesByManager.values()) {
          route.manager.#splitSessionId = ready.sessionId;
          route.manager.#multiplexPolicy = ready.policy;
        }
        const ownRoute = this.#splitRoutesByManager.get(this);
        if (!ownRoute) throw new Error('split control reconnect lost the owner route');
        this.#splitRouteId = ownRoute.routeId;
        this.#splitStreamLimit = ownRoute.streamLimit;
        socket = opened.socket; queue = opened.queue; writer = nextWriter; attempt = 0;
        this.#reconcile();
      } catch (error) {
        opened?.socket.terminate();
        const message = error instanceof Error ? error.message : String(error);
        if (this.context.tunnelStatus) this.context.tunnelStatus.lastError = message;
        if (message === 'split control session is unavailable') {
          try {
            const websocketUrl = this.options.websocketUrlOverride ?? this.#endpoint.websocketUrl;
            const replacement = await this.#replaceExpiredSplitSession(
              websocketUrl, () => { lastActivity = this.#now(); }
            );
            socket = replacement.socket;
            queue = replacement.queue;
            writer = replacement.writer;
            attempt = 0;
            if (this.context.tunnelStatus) this.context.tunnelStatus.lastError = undefined;
            this.#reconcile();
          } catch (replacementError) {
            if (this.context.tunnelStatus) {
              this.context.tunnelStatus.lastError = replacementError instanceof Error
                ? replacementError.message : String(replacementError);
            }
          }
        }
        continue;
      }
    }
  }

  async #multiplexLoop(
    worker: ManagedWorker,
    socket: WebSocket,
    queue: AsyncQueue<TunnelMessage>,
    identity: DeviceIdentity,
    connectionId: string,
    split?: { sessionId: string; routeId: number; streamLimit: number }
  ): Promise<void> {
    const ready = split
      ? await authenticateSplitSession(socket, queue, identity, connectionId, 'data', split.sessionId)
      : { sessionId: '', policy: await authenticateMultiplex(socket, queue, identity, connectionId) };
    const policy = ready.policy;
    this.#multiplexActive = true;
    this.#multiplexPolicy = policy;
    this.#policy = undefined;
    worker.multiplex = true;
    worker.multiplexActiveStreams = 0;
    const writer = new MultiplexWriter(socket);
    const { routeId, streamLimit } = split ?? await registerMultiplexRoute(writer, queue, identity.clientId);
    if (streamLimit > policy.streams_per_connection) {
      throw new Error('server route stream limit exceeds authenticated connection policy');
    }
    const fixedRoute: SharedSplitRoute = { manager: this, routeId, streamLimit };
    const resolveRoute = (incomingRouteId: number): SharedSplitRoute | undefined =>
      split ? this.#splitRoutes.get(incomingRouteId) : (incomingRouteId === routeId ? fixedRoute : undefined);
    const streams = new Map<string, {
      head: MultiplexRequestHead;
      route: SharedSplitRoute;
      chunks: Buffer[];
      controller: AbortController;
      started: boolean;
    }>();
    const tasks = new Set<Promise<void>>();
    worker.connectedAt = this.#now();
    worker.completedRequests = 0;
    this.#setWorkerState(worker, 'idle');
    if (this.context.tunnelStatus) this.context.tunnelStatus.lastError = undefined;

    const startStream = (key: string, state: {
      head: MultiplexRequestHead; route: SharedSplitRoute; chunks: Buffer[]; controller: AbortController; started: boolean
    }) => {
      if (state.started) throw new Error('duplicate multiplex request end');
      state.started = true;
      this.#multiplexActiveStreams += 1;
      worker.multiplexActiveStreams += 1;
      this.#setWorkerState(worker, 'busy');
      this.#updateStatus();
      const streamId = BigInt(state.head.stream_id);
      const target = state.route.manager;
      const targetRouteId = state.route.routeId;
      const task = forwardMultiplexRequest(
        writer,
        target.config,
        targetRouteId,
        streamId,
        state.head,
        Buffer.concat(state.chunks),
        state.controller.signal,
        this.#abort.signal,
        target.options
      ).then(outcome => {
        if (outcome === 'complete') {
          worker.completedRequests += 1;
          if (target.context.tunnelStatus) target.context.tunnelStatus.completedRequests += 1;
        }
      }).catch(async error => {
        if (error instanceof LocalTunnelTimeoutError && target.context.tunnelStatus) {
          target.context.tunnelStatus.lastRequestTimeout = error.timeoutReason;
          target.context.tunnelStatus.lastRequestTimeoutAt = target.#now();
        }
        if (!state.controller.signal.aborted && !this.#abort.signal.aborted) {
          await writer.control({
            kind: 'error', route_id: targetRouteId, stream_id: Number(streamId),
            message: error instanceof Error ? error.message : String(error)
          }).catch(() => undefined);
        }
      }).finally(() => {
        streams.delete(key);
        this.#multiplexActiveStreams = Math.max(0, this.#multiplexActiveStreams - 1);
        worker.multiplexActiveStreams = Math.max(0, worker.multiplexActiveStreams - 1);
        if (worker.multiplexActiveStreams === 0 && !worker.retire) this.#setWorkerState(worker, 'idle');
        this.#updateStatus();
        this.#reconcile();
      });
      tasks.add(task);
      void task.finally(() => tasks.delete(task));
    };

    while (!this.#stopped && !worker.retire && socket.readyState === WebSocket.OPEN) {
      const message = await queue.shift(0, this.#abort.signal);
      if (message.binary) {
        const frame = decodeMultiplexBinaryFrame(message.data);
        if (frame.kind !== MultiplexBinaryFrameKind.RequestBody) throw new Error('server sent a non-request multiplex binary frame');
        const key = `${frame.routeId}:${frame.streamId}`;
        const state = streams.get(key);
        if (!state || state.started || state.route.routeId !== frame.routeId) {
          throw new Error('server sent request body for an unknown route or started stream');
        }
        state.chunks.push(frame.payload);
        continue;
      }
      const control = JSON.parse(message.data.toString('utf8')) as JsonObject;
      if (control.kind === 'error') throw new Error(String(control.message ?? 'tunnel server error'));
      if (control.kind === 'request_head') {
        const head = control as unknown as MultiplexRequestHead;
        const incomingRouteId = Number(head.route_id);
        const route = resolveRoute(incomingRouteId);
        if (!route) throw new Error('server sent request head for an unknown route');
        const streamId = BigInt(head.stream_id);
        if (streamId <= 0n) throw new Error('server sent invalid multiplex stream id');
        const key = `${incomingRouteId}:${streamId}`;
        if (streams.has(key)) throw new Error('server reused an active multiplex stream id');
        if (streams.size >= streamLimit) throw new Error('server exceeded multiplex stream limit');
        if (!split) this.#applyMultiplexDemand(head.demand);
        streams.set(key, { head, route, chunks: [], controller: new AbortController(), started: false });
        continue;
      }
      if (control.kind === 'request_end') {
        const incomingRouteId = Number(control.route_id);
        const streamId = BigInt(Number(control.stream_id));
        const key = `${incomingRouteId}:${streamId}`;
        const state = streams.get(key);
        if (!state || state.route.routeId !== incomingRouteId) throw new Error('server ended an unknown multiplex stream');
        startStream(key, state);
        continue;
      }
      if (control.kind === 'cancel') {
        const key = `${Number(control.route_id)}:${BigInt(Number(control.stream_id))}`;
        streams.get(key)?.controller.abort(new Error('request cancelled'));
        continue;
      }
      throw new Error(`unexpected multiplex control message: ${control.kind}`);
    }
    for (const state of streams.values()) state.controller.abort(new Error('multiplex connection closed'));
    await Promise.allSettled([...tasks]);
  }

  #applyMultiplexDemand(value: unknown): void {
    if (!value || typeof value !== 'object') return;
    const record = value as { queued_requests?: unknown; active_streams?: unknown; desired_connections?: unknown };
    const queued = Number(record.queued_requests);
    const active = Number(record.active_streams);
    const desired = Number(record.desired_connections);
    const queuedRequests = Number.isFinite(queued) && queued >= 0 ? Math.floor(queued) : 0;
    this.#multiplexReportedActiveStreams = Number.isFinite(active) && active >= 0 ? Math.floor(active) : 0;
    const requestedConnections = Number.isFinite(desired) && desired >= 1 ? Math.floor(desired) : 1;
    const projectedConnections = [...this.#workers.values()].filter(worker => !worker.retire).length;
    const burstTarget = queuedRequests > 0 && this.#multiplexPolicy
      ? projectedConnections + this.#multiplexPolicy.burst_scale_up_step
      : requestedConnections;
    this.#multiplexQueuedRequests = 0;
    this.#multiplexDesiredConnections = Math.max(requestedConnections, burstTarget);
    this.#multiplexDemandSeenAt = this.#demandNow();
    if (queuedRequests > 0 || this.#multiplexDesiredConnections > projectedConnections) this.#lastPressureAt = this.#now();
    this.#reconcile();
  }

  async #workerLoop(worker: ManagedWorker): Promise<void> {
    let attempt = 0;
    while (!this.#stopped && !worker.retire) {
      let connected = false;
      let heartbeat: NodeJS.Timeout | undefined;
      let lastActivity = this.#now();
      try {
        if (!this.#endpoint || !this.#identity) throw new Error('tunnel identity is unavailable');
        this.#setWorkerState(worker, 'connecting');
        let workerId = `${process.pid}-${worker.index}-${randomUUID().slice(0, 8)}`;
        let connectionId = `${process.pid}-mux-${worker.index}-${randomUUID().slice(0, 8)}`;
        const websocketUrl = this.options.websocketUrlOverride ?? this.#endpoint.websocketUrl;
        const touch = () => { lastActivity = this.#now(); };
        const preopened = this.#preopenedWorker;
        if (preopened) {
          this.#preopenedWorker = undefined;
          workerId = preopened.workerId;
          connectionId = preopened.connectionId;
          lastActivity = preopened.lastActivity;
          const { socket, queue } = preopened;
          worker.socket = socket;
          if (socket.protocol === MULTIPLEX_TUNNEL_SUBPROTOCOL) {
            connected = true; attempt = 0;
            heartbeat = setInterval(() => {
              if (socket.readyState !== WebSocket.OPEN) return;
              if (this.#now() - lastActivity >= BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS) socket.terminate();
              else socket.ping();
            }, BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS);
            heartbeat.unref();
            await this.#multiplexLoop(worker, socket, queue, this.#identity, connectionId);
            return;
          }
          const policy = await authenticate(socket, queue, this.#identity, workerId);
          this.#applyPolicy(policy);
          worker.connectedAt = this.#now(); worker.completedRequests = 0; connected = true; attempt = 0;
          if (this.context.tunnelStatus) this.context.tunnelStatus.lastError = undefined;
          this.#setWorkerState(worker, 'idle');
          await sendControl(socket, { kind: 'ready' });
          heartbeat = setInterval(() => {
            if (socket.readyState !== WebSocket.OPEN) return;
            if (this.#now() - lastActivity >= BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS) socket.terminate(); else socket.ping();
          }, BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS);
          heartbeat.unref();
          while (!this.#stopped && !worker.retire && socket.readyState === WebSocket.OPEN) {
            const message = await queue.shift();
            if (message.binary) throw new Error('unexpected binary frame while worker is idle');
            const control = JSON.parse(message.data.toString('utf8')) as JsonObject;
            if (control.kind === 'policy_update') { this.#applyPolicy(control.worker_policy); continue; }
            if (control.kind === 'error') throw new Error(String(control.message ?? 'tunnel server error'));
            if (control.kind !== 'request_head') throw new Error(`unexpected tunnel control message: ${control.kind}`);
            const head = control as unknown as RequestHead; this.#applyDemand(head.demand); this.#setWorkerState(worker, 'busy');
            try {
              const requestBody = await receiveRequest(queue, head, value => this.#applyPolicy(value));
              await forwardRequest(socket, this.config, head, requestBody, queue, this.#abort.signal, this.options);
              worker.completedRequests += 1;
              if (this.context.tunnelStatus) this.context.tunnelStatus.completedRequests += 1;
            } catch (error) {
              if (error instanceof LocalTunnelTimeoutError && this.context.tunnelStatus) {
                this.context.tunnelStatus.lastRequestTimeout = error.timeoutReason;
                this.context.tunnelStatus.lastRequestTimeoutAt = this.#now();
              }
              await sendControl(socket, { kind: 'error', request_id: head.request_id, message: error instanceof Error ? error.message : String(error) }).catch(() => undefined);
            }
            if (this.#stopped || worker.retire || socket.readyState !== WebSocket.OPEN) break;
            this.#setWorkerState(worker, 'idle'); await sendControl(socket, { kind: 'ready' });
          }
          return;
        }
        if (this.#splitMode) {
          const sessionId = this.#splitSessionId;
          const routeId = this.#splitRouteId;
          const streamLimit = this.#splitStreamLimit;
          if (!sessionId || !routeId || !streamLimit) throw new Error('split control session is reconnecting');
          const opened = await openSplitWebSocket(
            websocketUrl, this.#endpoint, this.#identity, 'data', connectionId, sessionId, touch
          );
          const { socket, queue } = opened;
          worker.socket = socket;
          connected = true;
          attempt = 0;
          heartbeat = setInterval(() => {
            if (socket.readyState !== WebSocket.OPEN) return;
            if (this.#now() - lastActivity >= BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS) socket.terminate();
            else socket.ping();
          }, BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS);
          heartbeat.unref();
          await this.#multiplexLoop(worker, socket, queue, this.#identity, connectionId, { sessionId, routeId, streamLimit });
          return;
        }
        const opened = await openWebSocket(websocketUrl, this.#endpoint, this.#identity, workerId, connectionId, touch);
        const { socket, queue } = opened;
        worker.socket = socket;
        if (socket.protocol === MULTIPLEX_TUNNEL_SUBPROTOCOL) {
          connected = true;
          attempt = 0;
          heartbeat = setInterval(() => {
            if (socket.readyState !== WebSocket.OPEN) return;
            if (this.#now() - lastActivity >= BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS) socket.terminate();
            else socket.ping();
          }, BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS);
          heartbeat.unref();
          await this.#multiplexLoop(worker, socket, queue, this.#identity, connectionId);
          return;
        }
        const policy = await authenticate(socket, queue, this.#identity, workerId);
        this.#applyPolicy(policy);
        worker.connectedAt = this.#now();
        worker.completedRequests = 0;
        connected = true;
        attempt = 0;
        if (this.context.tunnelStatus) this.context.tunnelStatus.lastError = undefined;
        this.#setWorkerState(worker, 'idle');
        if (this.options.clientStatus) await sendControl(socket, { kind: 'client_status', ...this.options.clientStatus });
        await sendControl(socket, { kind: 'ready' });
        heartbeat = setInterval(() => {
          if (socket.readyState !== WebSocket.OPEN) return;
          if (this.#now() - lastActivity >= BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS) socket.terminate();
          else socket.ping();
        }, BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS);
        heartbeat.unref();

        while (!this.#stopped && !worker.retire && socket.readyState === WebSocket.OPEN) {
          const activePolicy = this.#policy;
          if (activePolicy && worker.connectedAt !== undefined
            && workerShouldRecycle(activePolicy, worker.index, worker.completedRequests, this.#now() - worker.connectedAt)) {
            this.#recycledWorkers += 1;
            return;
          }
          const message = await queue.shift();
          if (message.binary) throw new Error('unexpected binary frame while worker is idle');
          const control = JSON.parse(message.data.toString('utf8')) as JsonObject;
          if (control.kind === 'policy_update') { this.#applyPolicy(control.worker_policy); continue; }
          if (control.kind === 'update_offer') {
            if (this.options.onUpdateOffer) {
              void this.options.onUpdateOffer(control, status => sendControl(socket, { kind: 'update_status', ...status }))
                .catch(error => sendControl(socket, {
                  kind: 'update_status',
                  update_id: String(control.update_id ?? ''),
                  version: String(control.version ?? ''),
                  state: 'failed',
                  message: error instanceof Error ? error.message : String(error)
                }).catch(() => undefined));
            }
            continue;
          }
          if (control.kind === 'error') throw new Error(String(control.message ?? 'tunnel server error'));
          if (control.kind !== 'request_head') throw new Error(`unexpected tunnel control message: ${control.kind}`);
          const head = control as unknown as RequestHead;
          this.#applyDemand(head.demand);
          this.#setWorkerState(worker, 'busy');
          try {
            const requestBody = await receiveRequest(queue, head, value => this.#applyPolicy(value));
            await forwardRequest(socket, this.config, head, requestBody, queue, this.#abort.signal, this.options);
            worker.completedRequests += 1;
            if (this.context.tunnelStatus) this.context.tunnelStatus.completedRequests += 1;
          } catch (error) {
            if (error instanceof LocalTunnelTimeoutError && this.context.tunnelStatus) {
              this.context.tunnelStatus.lastRequestTimeout = error.timeoutReason;
              this.context.tunnelStatus.lastRequestTimeoutAt = this.#now();
            }
            await sendControl(socket, { kind: 'error', request_id: head.request_id, message: error instanceof Error ? error.message : String(error) }).catch(() => undefined);
          }
          if (this.#stopped || worker.retire || socket.readyState !== WebSocket.OPEN) break;
          this.#setWorkerState(worker, 'idle');
          const currentPolicy = this.#policy;
          if (currentPolicy && worker.connectedAt !== undefined
            && workerShouldRecycle(currentPolicy, worker.index, worker.completedRequests, this.#now() - worker.connectedAt)) {
            this.#recycledWorkers += 1;
            return;
          }
          await sendControl(socket, { kind: 'ready' });
        }
      } catch (error) {
        if (!this.#stopped && !worker.retire && this.context.tunnelStatus) {
          this.context.tunnelStatus.lastError = error instanceof Error ? error.message : String(error);
        }
      } finally {
        if (heartbeat) clearInterval(heartbeat);
        if (worker.socket?.protocol === MULTIPLEX_TUNNEL_SUBPROTOCOL || worker.socket?.protocol === MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL) {
          worker.multiplex = false;
          worker.multiplexActiveStreams = 0;
        }
        const socket = worker.socket;
        worker.socket = undefined;
        if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) socket.terminate();
        this.#updateStatus();
      }
      if (this.#stopped || worker.retire) break;
      attempt = connected ? 0 : attempt + 1;
      const delay = builtinTunnelReconnectDelayMs(attempt, connected);
      await new Promise<void>(resolve => {
        const timer = setTimeout(resolve, delay);
        timer.unref();
        this.#abort.signal.addEventListener('abort', () => { clearTimeout(timer); resolve(); }, { once: true });
      });
    }
  }
}
