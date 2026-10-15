export const MULTIPLEX_TUNNEL_PROTOCOL_VERSION = 5;
export const MULTIPLEX_TUNNEL_SUBPROTOCOL = 'coding-tools-tunnel-v5';
export const MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL = 'coding-tools-tunnel-v5-control';
export const MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL = 'coding-tools-tunnel-v5-data';
export const MULTIPLEX_CONNECTION_ID_HEADER = 'x-coding-tools-connection-id';
export const MULTIPLEX_SESSION_ID_HEADER = 'x-coding-tools-session-id';
export const MULTIPLEX_BINARY_HEADER_BYTES = 14;

export const MultiplexBinaryFrameKind = {
  RequestBody: 1,
  ResponseBody: 2
} as const;
export type MultiplexBinaryFrameKindValue = typeof MultiplexBinaryFrameKind[keyof typeof MultiplexBinaryFrameKind];


export type MultiplexConnectionRole = 'control' | 'data';

export function splitSessionAuthSigningPayload(
  nonce: string, role: MultiplexConnectionRole, deviceId: string, connectionId: string, sessionId?: string
): Buffer {
  const payload: Record<string, unknown> = {
    protocol_version: MULTIPLEX_TUNNEL_PROTOCOL_VERSION, nonce, connection_role: role,
    device_id: deviceId, connection_id: connectionId
  };
  if (sessionId) payload.session_id = sessionId;
  return Buffer.from(JSON.stringify(payload));
}

export function splitServerAckSigningPayload(
  nonce: string, serverId: string, role: MultiplexConnectionRole, deviceId: string,
  connectionId: string, sessionId: string, connectionPolicy: unknown
): Buffer {
  return Buffer.from(JSON.stringify({
    protocol_version: MULTIPLEX_TUNNEL_PROTOCOL_VERSION, nonce, server_id: serverId,
    connection_role: role, device_id: deviceId, connection_id: connectionId,
    session_id: sessionId, connection_policy: connectionPolicy
  }));
}

export interface MultiplexConnectionPolicy {
  start_connections: number;
  min_connections: number;
  max_idle_connections: number;
  max_connections: number;
  streams_per_connection: number;
  scale_up_threshold_percent: number;
  scale_up_step: number;
  burst_scale_up_step: number;
  scale_down_delay_seconds: number;
  revision: number;
}

export interface MultiplexBinaryFrame {
  kind: MultiplexBinaryFrameKindValue;
  routeId: number;
  streamId: bigint;
  payload: Buffer;
}

export interface MultiplexPoolSnapshot {
  connectedConnections: number;
  connectingConnections: number;
  activeStreams: number;
  queuedRequests: number;
  desiredConnections: number;
  idleConnections: number;
  idleExcessElapsed: boolean;
}

export interface MultiplexPoolAdjustment {
  spawn: number;
  retire: number;
}

export function multiplexPoolAdjustment(
  policy: MultiplexConnectionPolicy,
  snapshot: MultiplexPoolSnapshot
): MultiplexPoolAdjustment {
  const connected = Math.max(0, Math.floor(snapshot.connectedConnections));
  const connecting = Math.max(0, Math.floor(snapshot.connectingConnections));
  const projected = connected + connecting;
  const availableToSpawn = Math.max(0, policy.max_connections - projected);

  if (projected < policy.min_connections) {
    return { spawn: Math.min(availableToSpawn, policy.min_connections - projected), retire: 0 };
  }
  if (projected < policy.start_connections && snapshot.activeStreams === 0 && snapshot.queuedRequests === 0) {
    return { spawn: Math.min(availableToSpawn, policy.start_connections - projected), retire: 0 };
  }

  const projectedCapacity = Math.max(1, projected) * policy.streams_per_connection;
  const utilizationPercent = (Math.max(0, snapshot.activeStreams) * 100) / projectedCapacity;
  let spawn = 0;
  if (snapshot.queuedRequests > 0 || snapshot.desiredConnections > projected) {
    const demandGap = Math.max(0, Math.ceil(snapshot.desiredConnections) - projected);
    spawn = Math.max(policy.burst_scale_up_step, demandGap);
  } else if (utilizationPercent >= policy.scale_up_threshold_percent) {
    spawn = policy.scale_up_step;
  }
  if (spawn > 0) {
    return { spawn: Math.min(availableToSpawn, spawn), retire: 0 };
  }

  if (!snapshot.idleExcessElapsed || connected <= policy.min_connections) {
    return { spawn: 0, retire: 0 };
  }
  const target = snapshot.activeStreams === 0
    ? policy.min_connections
    : Math.max(policy.min_connections, policy.max_idle_connections);
  const retire = Math.min(
    Math.max(0, snapshot.idleConnections),
    Math.max(0, connected - target)
  );
  return { spawn: 0, retire };
}

function validateRouteAndStream(routeId: number, streamId: bigint): void {
  if (!Number.isInteger(routeId) || routeId <= 0 || routeId > 0xffff_ffff) {
    throw new Error('multiplex route id must be between 1 and 4294967295');
  }
  if (streamId <= 0n || streamId > 0xffff_ffff_ffff_ffffn) {
    throw new Error('multiplex stream id must be between 1 and 18446744073709551615');
  }
}

export function deviceSessionAuthSigningPayload(nonce: string, deviceId: string, connectionId: string): Buffer {
  return Buffer.from(JSON.stringify({
    protocol_version: MULTIPLEX_TUNNEL_PROTOCOL_VERSION,
    nonce,
    device_id: deviceId,
    connection_id: connectionId
  }));
}

export function multiplexServerChallengeSigningPayload(
  nonce: string, expiresAtUnixMs: number, serverId: string, deviceId: string, connectionId: string
): Buffer {
  return Buffer.from(JSON.stringify({
    protocol_version: MULTIPLEX_TUNNEL_PROTOCOL_VERSION,
    nonce,
    expires_at_unix_ms: expiresAtUnixMs,
    server_id: serverId,
    device_id: deviceId,
    connection_id: connectionId
  }));
}

export function multiplexServerAckSigningPayload(
  nonce: string, serverId: string, deviceId: string, connectionId: string, connectionPolicy: unknown
): Buffer {
  return Buffer.from(JSON.stringify({
    protocol_version: MULTIPLEX_TUNNEL_PROTOCOL_VERSION,
    nonce,
    server_id: serverId,
    device_id: deviceId,
    connection_id: connectionId,
    connection_policy: connectionPolicy
  }));
}

export function encodeMultiplexBinaryFrame(
  kind: MultiplexBinaryFrameKindValue, routeId: number, streamId: bigint, payload: Buffer
): Buffer {
  validateRouteAndStream(routeId, streamId);
  if (kind !== MultiplexBinaryFrameKind.RequestBody && kind !== MultiplexBinaryFrameKind.ResponseBody) {
    throw new Error(`unknown multiplex binary frame kind ${kind}`);
  }
  const frame = Buffer.allocUnsafe(MULTIPLEX_BINARY_HEADER_BYTES + payload.length);
  frame[0] = MULTIPLEX_TUNNEL_PROTOCOL_VERSION;
  frame[1] = kind;
  frame.writeUInt32BE(routeId, 2);
  frame.writeBigUInt64BE(streamId, 6);
  payload.copy(frame, MULTIPLEX_BINARY_HEADER_BYTES);
  return frame;
}

export function decodeMultiplexBinaryFrame(frame: Buffer): MultiplexBinaryFrame {
  if (frame.length < MULTIPLEX_BINARY_HEADER_BYTES) throw new Error('multiplex binary frame is too short');
  if (frame[0] !== MULTIPLEX_TUNNEL_PROTOCOL_VERSION) {
    throw new Error(`unsupported multiplex binary protocol version ${frame[0]}`);
  }
  const kind = frame[1] as MultiplexBinaryFrameKindValue;
  if (kind !== MultiplexBinaryFrameKind.RequestBody && kind !== MultiplexBinaryFrameKind.ResponseBody) {
    throw new Error(`unknown multiplex binary frame kind ${kind}`);
  }
  const routeId = frame.readUInt32BE(2);
  const streamId = frame.readBigUInt64BE(6);
  validateRouteAndStream(routeId, streamId);
  return { kind, routeId, streamId, payload: frame.subarray(MULTIPLEX_BINARY_HEADER_BYTES) };
}

function integerField(record: Record<string, unknown>, name: string, minimum: number, maximum: number): number {
  const value = Number(record[name]);
  if (!Number.isInteger(value) || value < minimum || value > maximum) {
    throw new Error(`invalid multiplex connection policy ${name}`);
  }
  return value;
}

export function normalizeMultiplexConnectionPolicy(value: unknown): MultiplexConnectionPolicy {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new Error('invalid multiplex connection policy');
  const record = value as Record<string, unknown>;
  const policy: MultiplexConnectionPolicy = {
    start_connections: integerField(record, 'start_connections', 1, 64),
    min_connections: integerField(record, 'min_connections', 1, 64),
    max_idle_connections: integerField(record, 'max_idle_connections', 1, 64),
    max_connections: integerField(record, 'max_connections', 1, 64),
    streams_per_connection: integerField(record, 'streams_per_connection', 1, 64),
    scale_up_threshold_percent: integerField(record, 'scale_up_threshold_percent', 50, 95),
    scale_up_step: integerField(record, 'scale_up_step', 1, 64),
    burst_scale_up_step: integerField(record, 'burst_scale_up_step', 1, 64),
    scale_down_delay_seconds: integerField(record, 'scale_down_delay_seconds', 0, 3600),
    revision: integerField(record, 'revision', 1, Number.MAX_SAFE_INTEGER)
  };
  if (!(policy.min_connections <= policy.start_connections
    && policy.start_connections <= policy.max_idle_connections
    && policy.max_idle_connections <= policy.max_connections)) {
    throw new Error('invalid multiplex connection policy connection counts');
  }
  if (policy.scale_up_step > policy.max_connections
    || policy.burst_scale_up_step < policy.scale_up_step
    || policy.burst_scale_up_step > policy.max_connections) {
    throw new Error('invalid multiplex connection policy scale-up steps');
  }
  return policy;
}
