import {
  createCipheriv, createDecipheriv, createHash, createPrivateKey, generateKeyPairSync,
  randomBytes, randomUUID, sign, type KeyObject
} from 'node:crypto';
import { mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import {
  request as httpRequest, type ClientRequest, type IncomingMessage,
  type OutgoingHttpHeaders, type RequestOptions
} from 'node:http';
import { hostname } from 'node:os';
import path from 'node:path';
import WebSocket from 'ws';
import type { AgentConfig, JsonObject, ToolContext } from './types.js';
import { AsyncQueue } from './runtime.js';
import {
  configuredBurstWarmFloor, configuredMaxConnecting, normalizeWorkerPolicy,
  poolAdjustment, workerShouldRecycle, type PoolCounts, type WorkerPolicy
} from './tunnelPolicy.js';

export const TUNNEL_PROTOCOL_VERSION = 3;
export const TUNNEL_WS_PATH = '/_tunnel/v1';
export const TUNNEL_ENROLL_PATH = '/_tunnel/enroll';
export const TUNNEL_SUBPROTOCOL = 'coding-tools-tunnel-v3';
export const MAX_TUNNEL_REQUEST_BYTES = 8 * 1024 * 1024;
export const BUILTIN_TUNNEL_DEMAND_TTL_MS = 3_000;
export const BUILTIN_TUNNEL_LOCAL_CONNECT_TIMEOUT_MS = 10_000;
export const BUILTIN_TUNNEL_LOCAL_REQUEST_TIMEOUT_MS = 5 * 60_000;

const HEARTBEAT_INTERVAL_MS = 15_000;
const HEARTBEAT_TIMEOUT_MS = 45_000;

const HOP_BY_HOP = new Set([
  'connection', 'keep-alive', 'proxy-authenticate', 'proxy-authorization', 'te',
  'trailer', 'transfer-encoding', 'upgrade', 'host', 'content-length'
]);

export interface TunnelEndpoint {
  publicUrl: string;
  baseUrl: string;
  clientId: string;
  websocketUrl: string;
}

interface DeviceIdentity {
  deviceId: string;
  clientId: string;
  privateKeyDer: string;
  publicKeyRaw: string;
  enrolled: boolean;
}

interface EncryptedIdentity {
  version: 1;
  iv: string;
  tag: string;
  ciphertext: string;
}

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
  socket?: WebSocket;
  task?: Promise<void>;
}

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
  now?: () => number;
  onEndpointResolved?: (endpoint: ResolvedBuiltinTunnelEndpoint) => Promise<void> | void;
}

export function parseBuiltinPublicUrl(value: string): TunnelEndpoint {
  const url = new URL(value.trim());
  if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash) {
    throw new Error('built-in tunnel public URL must be a clean HTTPS URL');
  }
  const match = /^\/builtin\/clients\/([A-Za-z0-9_-]{1,64})\/mcp\/?$/.exec(url.pathname);
  if (!match) throw new Error('built-in tunnel public URL must use /builtin/clients/<client-id>/mcp');
  url.pathname = url.pathname.replace(/\/$/, '');
  const publicUrl = url.toString().replace(/\/$/, '');
  const base = new URL(publicUrl);
  base.pathname = base.pathname.replace(/\/mcp$/, '');
  const websocket = new URL(publicUrl);
  websocket.protocol = 'wss:';
  websocket.pathname = TUNNEL_WS_PATH;
  websocket.search = '';
  websocket.hash = '';
  return { publicUrl, baseUrl: base.toString().replace(/\/$/, ''), clientId: match[1], websocketUrl: websocket.toString() };
}

export function builtinEndpointForClient(value: string, clientId: string): TunnelEndpoint {
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(clientId)) {
    throw new Error('built-in tunnel client ID must contain only letters, numbers, underscores, or hyphens');
  }
  const configured = parseBuiltinPublicUrl(value);
  const url = new URL(configured.publicUrl);
  url.pathname = `/builtin/clients/${clientId}/mcp`;
  return parseBuiltinPublicUrl(url.toString());
}

export function authSigningPayload(nonce: string, deviceId: string, clientId: string, workerId: string): Buffer {
  return Buffer.from(JSON.stringify({
    protocol_version: TUNNEL_PROTOCOL_VERSION,
    nonce,
    device_id: deviceId,
    client_id: clientId,
    service: 'mcp',
    worker_id: workerId
  }));
}

function encryptionKey(secret: string): Buffer {
  return createHash('sha256').update(secret).digest();
}

function encryptIdentity(identity: DeviceIdentity, secret: string): EncryptedIdentity {
  const iv = randomBytes(12);
  const cipher = createCipheriv('aes-256-gcm', encryptionKey(secret), iv);
  const ciphertext = Buffer.concat([cipher.update(JSON.stringify(identity), 'utf8'), cipher.final()]);
  return { version: 1, iv: iv.toString('base64url'), tag: cipher.getAuthTag().toString('base64url'), ciphertext: ciphertext.toString('base64url') };
}

function decryptIdentity(envelope: EncryptedIdentity, secret: string): DeviceIdentity {
  if (envelope.version !== 1) throw new Error('unsupported tunnel identity version');
  const decipher = createDecipheriv('aes-256-gcm', encryptionKey(secret), Buffer.from(envelope.iv, 'base64url'));
  decipher.setAuthTag(Buffer.from(envelope.tag, 'base64url'));
  const plaintext = Buffer.concat([decipher.update(Buffer.from(envelope.ciphertext, 'base64url')), decipher.final()]);
  return JSON.parse(plaintext.toString('utf8')) as DeviceIdentity;
}

async function saveIdentity(file: string, identity: DeviceIdentity, secret: string): Promise<void> {
  await mkdir(path.dirname(file), { recursive: true });
  const temporary = `${file}.${process.pid}.tmp`;
  await writeFile(temporary, `${JSON.stringify(encryptIdentity(identity, secret), null, 2)}\n`, { mode: 0o600 });
  await rename(temporary, file);
}

async function readIdentity(file: string, secret: string): Promise<DeviceIdentity | undefined> {
  try {
    return decryptIdentity(JSON.parse(await readFile(file, 'utf8')) as EncryptedIdentity, secret);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
}

function newIdentity(clientId: string): DeviceIdentity {
  const pair = generateKeyPairSync('ed25519');
  const privateKeyDer = pair.privateKey.export({ format: 'der', type: 'pkcs8' }).toString('base64');
  const publicDer = pair.publicKey.export({ format: 'der', type: 'spki' });
  const publicKeyRaw = publicDer.subarray(publicDer.length - 32).toString('base64url');
  return { deviceId: randomUUID().replaceAll('-', ''), clientId, privateKeyDer, publicKeyRaw, enrolled: false };
}

function privateKey(identity: DeviceIdentity): KeyObject {
  return createPrivateKey({ key: Buffer.from(identity.privateKeyDer, 'base64'), format: 'der', type: 'pkcs8' });
}

function validateEnrollmentUrl(publicUrl: string, value: string): URL {
  const publicEndpoint = new URL(publicUrl);
  const enrollment = new URL(value.trim());
  const sameOrigin = enrollment.protocol === 'https:'
    && enrollment.hostname === publicEndpoint.hostname
    && enrollment.port === publicEndpoint.port;
  if (!sameOrigin || enrollment.username || enrollment.password || enrollment.search || enrollment.hash) {
    throw new Error('enrollment URL must use the same HTTPS origin as the built-in tunnel');
  }
  if (!/^\/_tunnel\/enroll\/[A-Za-z0-9]{1,128}$/.test(enrollment.pathname)) {
    throw new Error('enrollment URL must use /_tunnel/enroll/<code>');
  }
  return enrollment;
}

async function loadOrEnroll(
  config: AgentConfig,
  endpoint: TunnelEndpoint,
  enrollmentFetch: typeof fetch
): Promise<{ identity: DeviceIdentity; enrollmentCompleted: boolean }> {
  if (!config.tunnel) throw new Error('built-in tunnel is not configured');
  const enrollmentUrl = config.tunnel.enrollmentUrl?.trim();
  const stored = await readIdentity(config.tunnel.stateFile, config.oauth.tokenSecret);
  if (stored?.enrolled && !enrollmentUrl) {
    return { identity: stored, enrollmentCompleted: false };
  }

  let identity = stored && !stored.enrolled ? stored : newIdentity(endpoint.clientId);
  if (!enrollmentUrl) {
    await saveIdentity(config.tunnel.stateFile, identity, config.oauth.tokenSecret);
    throw new Error('built-in tunnel enrollment URL is required for the first connection');
  }
  const enrollment = validateEnrollmentUrl(endpoint.publicUrl, enrollmentUrl);
  const response = await enrollmentFetch(enrollment, {
    method: 'POST', redirect: 'manual', headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ device_id: identity.deviceId, client_id: identity.clientId, device_name: hostname() || 'Coding Tools MCP Node', public_key: identity.publicKeyRaw })
  });
  if (!response.ok) throw new Error(`built-in tunnel enrollment failed (${response.status}): ${(await response.text()).trim()}`);
  const enrolled = await response.json() as { device_id?: string; client_id?: string };
  if (enrolled.device_id !== identity.deviceId) throw new Error('built-in tunnel enrollment returned a different device ID');
  const clientId = String(enrolled.client_id || identity.clientId).trim();
  if (!/^[A-Za-z0-9_-]{1,64}$/.test(clientId)) throw new Error('built-in tunnel enrollment returned an invalid client ID');
  identity = { ...identity, clientId, enrolled: true };
  await saveIdentity(config.tunnel.stateFile, identity, config.oauth.tokenSecret);
  return { identity, enrollmentCompleted: true };
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

async function nextControl(queue: AsyncQueue<TunnelMessage>, timeoutMs = 30_000): Promise<JsonObject> {
  const message = await queue.shift(timeoutMs);
  if (message.binary) throw new Error('unexpected binary frame');
  return JSON.parse(message.data.toString('utf8')) as JsonObject;
}

async function openWebSocket(
  websocketUrl: string,
  endpoint: TunnelEndpoint,
  touch: () => void
): Promise<{ socket: WebSocket; queue: AsyncQueue<TunnelMessage> }> {
  const socket = new WebSocket(websocketUrl, TUNNEL_SUBPROTOCOL, {
    handshakeTimeout: 15_000,
    headers: { 'x-coding-tools-client-id': endpoint.clientId, 'x-coding-tools-service': 'mcp' }
  });
  const queue = messageQueue(socket, touch);
  await new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => { socket.terminate(); reject(new Error('websocket connect timeout')); }, 20_000);
    timer.unref();
    socket.once('open', () => { clearTimeout(timer); resolve(); });
    socket.once('error', error => { clearTimeout(timer); reject(error); });
  });
  if (socket.protocol !== TUNNEL_SUBPROTOCOL) {
    socket.close();
    throw new Error(`server did not accept ${TUNNEL_SUBPROTOCOL}`);
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
  const hello = { protocol_version: TUNNEL_PROTOCOL_VERSION, client_id: identity.clientId, service: 'mcp', worker_id: workerId };
  const signature = sign(null, authSigningPayload(String(challenge.nonce), identity.deviceId, identity.clientId, workerId), privateKey(identity)).toString('base64url');
  await sendControl(ws, { kind: 'authenticate', hello, device_id: identity.deviceId, signature });
  const acknowledgment = await nextControl(queue);
  if (acknowledgment.kind === 'error') throw new Error(String(acknowledgment.message ?? 'tunnel authentication failed'));
  if (acknowledgment.kind !== 'hello_ack' || Number(acknowledgment.protocol_version) !== TUNNEL_PROTOCOL_VERSION) {
    throw new Error('server did not acknowledge tunnel authentication');
  }
  return normalizeWorkerPolicy(acknowledgment.worker_policy);
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

  constructor(config: AgentConfig, context: ToolContext, options: BuiltinTunnelManagerOptions = {}) {
    this.config = config;
    this.context = context;
    this.options = options;
  }

  async start(): Promise<void> {
    if (!this.config.tunnel?.enabled || !this.config.tunnel.publicUrl || !this.#stopped) return;
    this.#stopped = false;
    this.#abort = new AbortController();
    this.context.tunnelStatus = {
      enabled: true, state: 'starting', publicUrl: this.config.tunnel.publicUrl,
      workers: 1, connectedWorkers: 0, connectingWorkers: 0, idleWorkers: 0,
      busyWorkers: 0, recycledWorkers: 0, completedRequests: 0, startedAt: this.#now()
    };
    try {
      const configuredEndpoint = parseBuiltinPublicUrl(this.config.tunnel.publicUrl);
      const loaded = await loadOrEnroll(this.config, configuredEndpoint, this.options.enrollmentFetch ?? fetch);
      this.#identity = loaded.identity;
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
    this.#spawnWorker();
    this.#reconcileTimer = setInterval(() => this.#reconcile(), Math.max(20, this.options.reconcileIntervalMs ?? 1_000));
    this.#reconcileTimer.unref();
  }

  async stop(): Promise<void> {
    if (this.#stopped) return;
    this.#stopped = true;
    this.#abort.abort();
    if (this.#reconcileTimer) clearInterval(this.#reconcileTimer);
    this.#reconcileTimer = undefined;
    const tasks: Promise<void>[] = [];
    for (const worker of this.#workers.values()) {
      worker.retire = true;
      worker.state = 'retiring';
      worker.socket?.close(1000, 'shutdown');
      if (worker.task) tasks.push(worker.task);
    }
    await Promise.allSettled(tasks);
    this.#workers.clear();
    if (this.context.tunnelStatus) {
      Object.assign(this.context.tunnelStatus, {
        state: 'stopped', workers: 0, connectedWorkers: 0,
        connectingWorkers: 0, idleWorkers: 0, busyWorkers: 0
      });
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
      retire: false, completedRequests: 0
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

  async #workerLoop(worker: ManagedWorker): Promise<void> {
    let attempt = 0;
    while (!this.#stopped && !worker.retire) {
      let connected = false;
      let heartbeat: NodeJS.Timeout | undefined;
      let lastActivity = this.#now();
      try {
        if (!this.#endpoint || !this.#identity) throw new Error('tunnel identity is unavailable');
        this.#setWorkerState(worker, 'connecting');
        const workerId = `${process.pid}-${worker.index}-${randomUUID().slice(0, 8)}`;
        const websocketUrl = this.options.websocketUrlOverride ?? this.#endpoint.websocketUrl;
        const touch = () => { lastActivity = this.#now(); };
        const opened = await openWebSocket(websocketUrl, this.#endpoint, touch);
        const { socket, queue } = opened;
        worker.socket = socket;
        const policy = await authenticate(socket, queue, this.#identity, workerId);
        this.#applyPolicy(policy);
        worker.connectedAt = this.#now();
        worker.completedRequests = 0;
        connected = true;
        attempt = 0;
        if (this.context.tunnelStatus) this.context.tunnelStatus.lastError = undefined;
        this.#setWorkerState(worker, 'idle');
        await sendControl(socket, { kind: 'ready' });
        heartbeat = setInterval(() => {
          if (socket.readyState !== WebSocket.OPEN) return;
          if (this.#now() - lastActivity >= HEARTBEAT_TIMEOUT_MS) socket.terminate();
          else socket.ping();
        }, HEARTBEAT_INTERVAL_MS);
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
        const socket = worker.socket;
        worker.socket = undefined;
        if (socket && (socket.readyState === WebSocket.OPEN || socket.readyState === WebSocket.CONNECTING)) socket.terminate();
        this.#updateStatus();
      }
      if (this.#stopped || worker.retire) break;
      attempt = connected ? 0 : attempt + 1;
      const delay = Math.min(30_000, 500 * 2 ** Math.min(attempt, 6)) + Math.floor(Math.random() * 250);
      await new Promise<void>(resolve => {
        const timer = setTimeout(resolve, delay);
        timer.unref();
        this.#abort.signal.addEventListener('abort', () => { clearTimeout(timer); resolve(); }, { once: true });
      });
    }
  }
}
