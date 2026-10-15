import test from 'node:test';
import assert from 'node:assert/strict';
import {
  createHash, createPublicKey, generateKeyPairSync, sign as signSignature,
  verify as verifySignature
} from 'node:crypto';
import { createServer } from 'node:http';
import { mkdtemp, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { WebSocketServer, WebSocket } from 'ws';
import { createToolContext } from '../dist/server.js';
import {
  authSigningPayload, builtinEndpointForClient, BuiltinTunnelManager, parseBuiltinPublicUrl,
  publicTunnelSecurityError, serverAckSigningPayload, serverChallengeSigningPayload,
  tunnelPathAllowed, BUILTIN_TUNNEL_DEMAND_TTL_MS, BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS,
  BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS, BUILTIN_TUNNEL_LOCAL_CONNECT_TIMEOUT_MS,
  BUILTIN_TUNNEL_LOCAL_REQUEST_TIMEOUT_MS, BUILTIN_TUNNEL_RECONNECT_INITIAL_MS,
  BUILTIN_TUNNEL_RECONNECT_MAX_MS, builtinTunnelReconnectDelayMs, TUNNEL_PROTOCOL_VERSION, TUNNEL_SUBPROTOCOL,
  MULTIPLEX_TUNNEL_PROTOCOL_VERSION, MULTIPLEX_TUNNEL_SUBPROTOCOL,
  MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL, MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL,
  deviceSessionAuthSigningPayload, multiplexServerChallengeSigningPayload, multiplexServerAckSigningPayload,
  splitSessionAuthSigningPayload, splitServerAckSigningPayload,
  encodeMultiplexBinaryFrame, decodeMultiplexBinaryFrame, MultiplexBinaryFrameKind
} from '../dist/tunnel.js';
import { loadOrEnroll } from '../dist/tunnel/identity.js';
import { multiplexPoolAdjustment } from '../dist/tunnel/multiplex.js';
import {
  configuredBurstWarmFloor, configuredMaxConnecting, jitteredLimit,
  normalizeWorkerPolicy, poolAdjustment, workerShouldRecycle
} from '../dist/tunnelPolicy.js';

const defaultPolicy = {
  start_workers: 1,
  min_idle_workers: 1,
  max_idle_workers: 2,
  max_workers: 8,
  max_requests_per_worker: 5000,
  max_lifetime_seconds: 86400,
  scale_down_delay_seconds: 60,
  recycle_jitter_percent: 20,
  max_pending_requests: 32,
  worker_acquire_timeout_ms: 10000,
  max_connecting_workers: 2,
  connecting_capacity_grace_ms: 1000,
  scale_down_step: 2,
  burst_warm_workers: 2,
  burst_warm_seconds: 60,
  revision: 1
};

const testServerKeyPair = generateKeyPairSync('ed25519');
const testServerPublicKeyRaw = Buffer.from(
  testServerKeyPair.publicKey.export({ format: 'der', type: 'spki' })
).subarray(-32).toString('base64url');
const testServerId = createHash('sha256')
  .update(Buffer.from(testServerPublicKeyRaw, 'base64url'))
  .digest('base64url');

function enrollmentResponse(deviceId, clientId) {
  return {
    device_id: deviceId,
    client_id: clientId,
    server_id: testServerId,
    server_public_key: testServerPublicKeyRaw
  };
}

function sendSignedServerChallenge(socket, request, nonce) {
  const expiresAtUnixMs = Date.now() + 30_000;
  const deviceId = String(request.headers['x-coding-tools-device-id'] ?? '');
  const clientId = String(request.headers['x-coding-tools-client-id'] ?? '');
  const workerId = String(request.headers['x-coding-tools-worker-id'] ?? '');
  assert.ok(deviceId);
  assert.ok(clientId);
  assert.ok(workerId);
  const serverSignature = signSignature(
    null,
    serverChallengeSigningPayload(nonce, expiresAtUnixMs, testServerId, deviceId, clientId, workerId),
    testServerKeyPair.privateKey
  ).toString('base64url');
  socket.send(JSON.stringify({
    kind: 'challenge',
    nonce,
    expires_at_unix_ms: expiresAtUnixMs,
    server_id: testServerId,
    server_signature: serverSignature
  }));
}

function sendSignedServerAck(socket, nonce, authenticate, workerPolicy) {
  const serverSignature = signSignature(
    null,
    serverAckSigningPayload(
      nonce,
      testServerId,
      authenticate.device_id,
      authenticate.hello.client_id,
      authenticate.hello.worker_id,
      workerPolicy
    ),
    testServerKeyPair.privateKey
  ).toString('base64url');
  socket.send(JSON.stringify({
    kind: 'hello_ack',
    protocol_version: TUNNEL_PROTOCOL_VERSION,
    worker_policy: workerPolicy,
    server_id: testServerId,
    server_signature: serverSignature
  }));
}

async function waitFor(predicate, message, timeoutMs = 5000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const value = predicate();
    if (value) return value;
    await new Promise(resolve => setTimeout(resolve, 20));
  }
  throw new Error(message);
}

async function withTimeout(promise, timeoutMs, message) {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(message())), timeoutMs);
        timer.unref();
      })
    ]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

async function closeServer(server) {
  await new Promise(resolve => server.close(() => resolve()));
}

test('built-in tunnel URL derives the v4 websocket endpoint', () => {
  const endpoint = parseBuiltinPublicUrl('https://tunnel.example/builtin/clients/device_1/mcp');
  assert.equal(endpoint.clientId, 'device_1');
  assert.equal(endpoint.baseUrl, 'https://tunnel.example/builtin/clients/device_1');
  assert.equal(endpoint.websocketUrl, 'wss://tunnel.example/_tunnel/v1');
  assert.equal(TUNNEL_PROTOCOL_VERSION, 4);
  assert.equal(TUNNEL_SUBPROTOCOL, 'coding-tools-tunnel-v4');
  assert.equal(BUILTIN_TUNNEL_DEMAND_TTL_MS, 3_000);
  assert.equal(BUILTIN_TUNNEL_LOCAL_CONNECT_TIMEOUT_MS, 10_000);
  assert.equal(BUILTIN_TUNNEL_LOCAL_REQUEST_TIMEOUT_MS, 5 * 60_000);
  assert.equal(BUILTIN_TUNNEL_HEARTBEAT_INTERVAL_MS, 30_000);
  assert.equal(BUILTIN_TUNNEL_HEARTBEAT_TIMEOUT_MS, 90_000);
  assert.equal(BUILTIN_TUNNEL_RECONNECT_INITIAL_MS, 1_000);
  assert.equal(BUILTIN_TUNNEL_RECONNECT_MAX_MS, 120_000);
  assert.equal(builtinTunnelReconnectDelayMs(1, false, () => 0), 1_000);
  assert.equal(builtinTunnelReconnectDelayMs(1, false, () => 1), 2_000);
  assert.equal(builtinTunnelReconnectDelayMs(16, false, () => 1), 120_000);
  assert.equal(builtinTunnelReconnectDelayMs(0, true, () => 0), 500);
});

test('multiplex v5 wire contract matches Rust framing and signing payloads', () => {
  assert.equal(MULTIPLEX_TUNNEL_PROTOCOL_VERSION, 5);
  assert.equal(MULTIPLEX_TUNNEL_SUBPROTOCOL, 'coding-tools-tunnel-v5');
  assert.equal(MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL, 'coding-tools-tunnel-v5-control');
  assert.equal(MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL, 'coding-tools-tunnel-v5-data');
  assert.equal(
    deviceSessionAuthSigningPayload('nonce', 'device', 'connection-1').toString(),
    '{"protocol_version":5,"nonce":"nonce","device_id":"device","connection_id":"connection-1"}'
  );
  assert.equal(
    multiplexServerChallengeSigningPayload('nonce', 1234, 'server', 'device', 'connection-1').toString(),
    '{"protocol_version":5,"nonce":"nonce","expires_at_unix_ms":1234,"server_id":"server","device_id":"device","connection_id":"connection-1"}'
  );
  assert.equal(
    splitSessionAuthSigningPayload('nonce', 'control', 'device', 'control-1').toString(),
    '{"protocol_version":5,"nonce":"nonce","connection_role":"control","device_id":"device","connection_id":"control-1"}'
  );
  assert.equal(
    splitSessionAuthSigningPayload('nonce', 'data', 'device', 'data-1', 'session-1').toString(),
    '{"protocol_version":5,"nonce":"nonce","connection_role":"data","device_id":"device","connection_id":"data-1","session_id":"session-1"}'
  );
  const policy = {
    start_connections: 1, min_connections: 1, max_idle_connections: 2, max_connections: 8,
    streams_per_connection: 16, scale_up_threshold_percent: 75, scale_up_step: 2,
    burst_scale_up_step: 4, scale_down_delay_seconds: 90, revision: 1
  };
  assert.equal(
    multiplexServerAckSigningPayload('nonce', 'server', 'device', 'connection-1', policy).toString(),
    '{"protocol_version":5,"nonce":"nonce","server_id":"server","device_id":"device","connection_id":"connection-1","connection_policy":{"start_connections":1,"min_connections":1,"max_idle_connections":2,"max_connections":8,"streams_per_connection":16,"scale_up_threshold_percent":75,"scale_up_step":2,"burst_scale_up_step":4,"scale_down_delay_seconds":90,"revision":1}}'
  );
  assert.equal(
    splitServerAckSigningPayload('nonce', 'server', 'data', 'device', 'data-1', 'session-1', policy).toString(),
    '{"protocol_version":5,"nonce":"nonce","server_id":"server","connection_role":"data","device_id":"device","connection_id":"data-1","session_id":"session-1","connection_policy":{"start_connections":1,"min_connections":1,"max_idle_connections":2,"max_connections":8,"streams_per_connection":16,"scale_up_threshold_percent":75,"scale_up_step":2,"burst_scale_up_step":4,"scale_down_delay_seconds":90,"revision":1}}'
  );

  const encoded = encodeMultiplexBinaryFrame(MultiplexBinaryFrameKind.RequestBody, 7, 99n, Buffer.from('body'));
  assert.equal(encoded.length, 18);
  assert.deepEqual([...encoded.subarray(0, 14)], [5, 1, 0, 0, 0, 7, 0, 0, 0, 0, 0, 0, 0, 99]);
  assert.deepEqual(decodeMultiplexBinaryFrame(encoded), {
    kind: MultiplexBinaryFrameKind.RequestBody, routeId: 7, streamId: 99n, payload: Buffer.from('body')
  });
  assert.throws(() => encodeMultiplexBinaryFrame(MultiplexBinaryFrameKind.RequestBody, 0, 1n, Buffer.alloc(0)), /route/i);
});

test('split v5 keeps one control socket, scales data sockets, and preserves route stream pairing', async t => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-v5-split-'));
  const stateFile = path.join(dataDir, 'identity.enc.json');
  let enrolledPublicKey = '';
  let controlSocket;
  let controlConnections = 0;
  let manager;
  let wss;
  const dataSockets = new Set();
  const errors = [];
  const response = { head: undefined, chunks: [], ended: false };
  const sessionId = 'session-node-split';
  const routeId = 23;
  const desiredConnections = 1;
  const streamId = 701;
  const policy = {
    start_connections: 1, min_connections: 1, max_idle_connections: 2, max_connections: 8,
    streams_per_connection: 16, scale_up_threshold_percent: 75, scale_up_step: 2,
    burst_scale_up_step: 4, scale_down_delay_seconds: 90, revision: 1
  };

  const server = createServer((request, result) => {
    if (!request.url?.startsWith('/builtin/clients/v5_split/mcp')) return result.writeHead(404).end('not found');
    const chunks = [];
    request.on('data', chunk => chunks.push(Buffer.from(chunk)));
    request.on('end', () => result.writeHead(202, { 'content-type': 'text/plain' }).end(`split:${Buffer.concat(chunks)}`));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => {
    try {
      await manager?.stop();
    } finally {
      for (const client of wss?.clients ?? []) client.terminate();
      if (wss) await new Promise(resolve => wss.close(resolve));
      await closeServer(server);
    }
  });
  const address = server.address();
  assert.ok(address && typeof address === 'object');

  wss = new WebSocketServer({
    server, path: '/_tunnel/v1',
    handleProtocols(protocols) {
      if (protocols.has(MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL)) return MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL;
      if (protocols.has(MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL)) return MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL;
      return false;
    }
  });

  wss.on('connection', (socket, request) => {
    const role = socket.protocol === MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL ? 'control' : 'data';
    if (role === 'control') { controlConnections += 1; controlSocket = socket; }
    else {
      dataSockets.add(socket);
      socket.on('close', () => dataSockets.delete(socket));
      assert.equal(String(request.headers['x-coding-tools-session-id'] ?? ''), sessionId);
    }
    const deviceId = String(request.headers['x-coding-tools-device-id'] ?? '');
    const connectionId = String(request.headers['x-coding-tools-connection-id'] ?? '');
    const nonce = `split-${role}-${connectionId}`;
    const expiresAtUnixMs = Date.now() + 30_000;
    socket.send(JSON.stringify({
      kind: 'challenge', nonce, expires_at_unix_ms: expiresAtUnixMs, server_id: testServerId,
      server_signature: signSignature(
        null,
        multiplexServerChallengeSigningPayload(nonce, expiresAtUnixMs, testServerId, deviceId, connectionId),
        testServerKeyPair.privateKey
      ).toString('base64url')
    }));
    socket.on('message', (data, isBinary) => {
      try {
        if (isBinary) {
          const frame = decodeMultiplexBinaryFrame(Buffer.from(data));
          assert.equal(role, 'data');
          assert.equal(frame.kind, MultiplexBinaryFrameKind.ResponseBody);
          assert.equal(frame.routeId, routeId);
          assert.equal(Number(frame.streamId), streamId);
          response.chunks.push(frame.payload);
          return;
        }
        const control = JSON.parse(data.toString());
        if (control.kind === 'session_authenticate') {
          const raw = Buffer.from(enrolledPublicKey, 'base64url');
          const publicKey = createPublicKey({
            key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), raw]), format: 'der', type: 'spki'
          });
          assert.equal(control.hello.connection_role, role);
          assert.equal(control.hello.session_id, role === 'data' ? sessionId : undefined);
          assert.equal(verifySignature(
            null,
            splitSessionAuthSigningPayload(nonce, role, control.device_id, connectionId, role === 'data' ? sessionId : undefined),
            publicKey, Buffer.from(control.signature, 'base64url')
          ), true);
          const ackSignature = signSignature(
            null,
            splitServerAckSigningPayload(nonce, testServerId, role, control.device_id, connectionId, sessionId, policy),
            testServerKeyPair.privateKey
          ).toString('base64url');
          socket.send(JSON.stringify({
            kind: 'session_ready', protocol_version: 5, connection_role: role, session_id: sessionId,
            connection_policy: policy, server_id: testServerId, server_signature: ackSignature
          }));
          if (role === 'data' && dataSockets.size === 1) {
            queueMicrotask(() => {
              socket.send(JSON.stringify({
                kind: 'request_head', route_id: routeId, stream_id: streamId, method: 'POST',
                path_and_query: '/builtin/clients/v5_split/mcp?split=1', headers: [], demand: null
              }));
              socket.send(encodeMultiplexBinaryFrame(MultiplexBinaryFrameKind.RequestBody, routeId, BigInt(streamId), Buffer.from('payload')));
              socket.send(JSON.stringify({ kind: 'request_end', route_id: routeId, stream_id: streamId }));
              controlSocket.send(JSON.stringify({
                kind: 'capacity_update', demand: {
                  queued_requests: 0, oldest_queue_wait_ms: 0, active_streams: 12,
                  available_stream_slots: 4, desired_connections: desiredConnections
                }
              }));
            });
          }
        } else if (control.kind === 'route_register') {
          assert.equal(role, 'control');
          assert.equal(control.client_id, 'v5_split');
          socket.send(JSON.stringify({
            kind: 'route_registered', registration_id: control.registration_id, route_id: routeId, stream_limit: 16
          }));
        } else if (control.kind === 'response_head') {
          assert.equal(role, 'data');
          assert.equal(control.route_id, routeId);
          assert.equal(control.stream_id, streamId);
          response.head = control;
        } else if (control.kind === 'response_end') {
          assert.equal(role, 'data');
          assert.equal(control.route_id, routeId);
          assert.equal(control.stream_id, streamId);
          response.ended = true;
        } else if (control.kind === 'error') {
          errors.push(new Error(control.message));
        } else if (control.kind === 'route_unregister') {
          assert.equal(role, 'control');
        } else {
          throw new Error(`unexpected ${role} client control: ${control.kind}`);
        }
      } catch (error) { errors.push(error); }
    });
  });

  const config = {
    host: '127.0.0.1', port: address.port,
    publicBaseUrl: 'https://tunnel.example/builtin/clients/v5_split', dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'password', tokenSecret: 'a sufficiently long split test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true, publicUrl: 'https://tunnel.example/builtin/clients/v5_split/mcp',
      enrollmentUrl: 'https://tunnel.example/_tunnel/enroll/V5SPLIT', workers: 1, stateFile
    }
  };
  const context = await createToolContext(config);
  manager = new BuiltinTunnelManager(config, context, {
    websocketUrlOverride: `ws://127.0.0.1:${address.port}/_tunnel/v1`, reconcileIntervalMs: 20,
    enrollmentFetch: async (_url, init) => {
      const body = JSON.parse(String(init?.body ?? '{}'));
      enrolledPublicKey = body.public_key;
      return new Response(JSON.stringify(enrollmentResponse(body.device_id, 'v5_split')), {
        status: 200, headers: { 'content-type': 'application/json' }
      });
    }
  });
  await manager.start();
  try {
    await waitFor(
      () => response.ended && dataSockets.size === 3 && context.tunnelStatus.dataConnections === 3,
      `split runtime did not respond and scale to three data sockets: data=${dataSockets.size}; status=${JSON.stringify(context.tunnelStatus)}`,
      6000
    );
  } catch (error) {
    throw new AggregateError([error, ...errors], `split runtime diagnostics: control=${controlConnections}; data=${dataSockets.size}`);
  }
  assert.equal(context.tunnelStatus.transportMode, 'split-v5');
  assert.equal(context.tunnelStatus.controlConnected, true);
  assert.equal(context.tunnelStatus.serverOrigin, 'https://tunnel.example');
  assert.equal(context.tunnelStatus.websocketUrl, 'wss://tunnel.example/_tunnel/v1');
  assert.equal(context.tunnelStatus.serverId, testServerId);
  assert.ok(context.tunnelStatus.deviceId);
  assert.equal(context.tunnelStatus.sessionId, 'session-node-split');
  assert.equal(context.tunnelStatus.routeId, routeId);
  assert.equal(context.tunnelStatus.streamLimit, 16);
  assert.equal(context.tunnelStatus.dataConnections, 3);
  assert.equal(context.tunnelStatus.maxDataConnections, 8);
  assert.equal(context.tunnelStatus.streamsPerConnection, 16);
  assert.equal(context.tunnelStatus.streamCapacity, 48);
  assert.ok(context.tunnelStatus.availableStreamSlots >= 0);
  assert.equal(context.tunnelStatus.desiredDataConnections, desiredConnections);
  const dataSocketCount = dataSockets.size;
  await manager.stop();
  assert.equal(controlConnections, 1);
  assert.equal(dataSocketCount, 3);
  assert.equal(response.head.status, 202);
  assert.equal(Buffer.concat(response.chunks).toString(), 'split:payload');
  assert.equal(context.tunnelStatus.completedRequests, 1);
  assert.equal(errors.length, 0, errors.map(String).join('; '));
});

test('split v5 rebuilds control routes and data sockets after the old session expires', async t => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-v5-expired-'));
  const stateFile = path.join(dataDir, 'identity.enc.json');
  let enrolledPublicKey = '';
  let controlConnections = 0;
  let dataConnections = 0;
  let initialControlSocket;
  let oldDataSeen = false;
  let replacementRequestSent = false;
  const controlSessionHeaders = [];
  const errors = [];
  const response = { head: undefined, chunks: [], ended: false };
  const oldSessionId = 'session-node-expired';
  const replacementSessionId = 'session-node-replacement';
  const policy = {
    start_connections: 1, min_connections: 1, max_idle_connections: 2, max_connections: 8,
    streams_per_connection: 16, scale_up_threshold_percent: 75, scale_up_step: 2,
    burst_scale_up_step: 4, scale_down_delay_seconds: 90, revision: 1
  };

  const server = createServer((request, result) => {
    if (!request.url?.startsWith('/builtin/clients/v5_expired/mcp')) return result.writeHead(404).end('not found');
    const chunks = [];
    request.on('data', chunk => chunks.push(Buffer.from(chunk)));
    request.on('end', () => result.writeHead(206, { 'content-type': 'text/plain' }).end(`recovered:${Buffer.concat(chunks)}`));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => closeServer(server));
  const address = server.address();
  assert.ok(address && typeof address === 'object');

  const wss = new WebSocketServer({
    server, path: '/_tunnel/v1',
    handleProtocols(protocols) {
      if (protocols.has(MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL)) return MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL;
      if (protocols.has(MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL)) return MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL;
      return false;
    }
  });
  t.after(() => {
    for (const client of wss.clients) client.terminate();
    wss.close();
  });

  wss.on('connection', (socket, request) => {
    const role = socket.protocol === MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL ? 'control' : 'data';
    const requestedSessionId = String(request.headers['x-coding-tools-session-id'] ?? '');
    if (role === 'control') {
      controlConnections += 1;
      controlSessionHeaders.push(requestedSessionId);
      if (controlConnections === 1) initialControlSocket = socket;
    } else {
      dataConnections += 1;
    }
    const deviceId = String(request.headers['x-coding-tools-device-id'] ?? '');
    const connectionId = String(request.headers['x-coding-tools-connection-id'] ?? '');
    const nonce = `expired-${role}-${connectionId}`;
    const expiresAtUnixMs = Date.now() + 30_000;
    socket.send(JSON.stringify({
      kind: 'challenge', nonce, expires_at_unix_ms: expiresAtUnixMs, server_id: testServerId,
      server_signature: signSignature(
        null,
        multiplexServerChallengeSigningPayload(nonce, expiresAtUnixMs, testServerId, deviceId, connectionId),
        testServerKeyPair.privateKey
      ).toString('base64url')
    }));
    socket.on('message', (data, isBinary) => {
      try {
        if (isBinary) {
          const frame = decodeMultiplexBinaryFrame(Buffer.from(data));
          assert.equal(frame.kind, MultiplexBinaryFrameKind.ResponseBody);
          assert.equal(frame.routeId, 42);
          assert.equal(Number(frame.streamId), 901);
          response.chunks.push(frame.payload);
          return;
        }
        const control = JSON.parse(data.toString());
        if (control.kind === 'session_authenticate') {
          if (role === 'control' && requestedSessionId === oldSessionId) {
            socket.send(JSON.stringify({ kind: 'error', message: 'split control session is unavailable' }));
            return;
          }
          const acceptedSessionId = role === 'control'
            ? (controlConnections >= 3 ? replacementSessionId : oldSessionId)
            : requestedSessionId;
          assert.ok(acceptedSessionId === oldSessionId || acceptedSessionId === replacementSessionId);
          const ackSignature = signSignature(
            null,
            splitServerAckSigningPayload(
              nonce, testServerId, role, control.device_id, connectionId, acceptedSessionId, policy
            ),
            testServerKeyPair.privateKey
          ).toString('base64url');
          socket.send(JSON.stringify({
            kind: 'session_ready', protocol_version: 5, connection_role: role,
            session_id: acceptedSessionId, connection_policy: policy,
            server_id: testServerId, server_signature: ackSignature
          }));
          if (role === 'data' && acceptedSessionId === oldSessionId && !oldDataSeen) {
            oldDataSeen = true;
            queueMicrotask(() => initialControlSocket?.terminate());
          } else if (role === 'data' && acceptedSessionId === replacementSessionId && !replacementRequestSent) {
            replacementRequestSent = true;
            queueMicrotask(() => {
              socket.send(JSON.stringify({
                kind: 'request_head', route_id: 42, stream_id: 901, method: 'POST',
                path_and_query: '/builtin/clients/v5_expired/mcp?recovered=1', headers: [], demand: null
              }));
              socket.send(encodeMultiplexBinaryFrame(
                MultiplexBinaryFrameKind.RequestBody, 42, 901n, Buffer.from('after-expiry')
              ));
              socket.send(JSON.stringify({ kind: 'request_end', route_id: 42, stream_id: 901 }));
            });
          }
        } else if (control.kind === 'route_register') {
          const routeId = controlConnections >= 3 ? 42 : 41;
          socket.send(JSON.stringify({
            kind: 'route_registered', registration_id: control.registration_id,
            route_id: routeId, stream_limit: 16
          }));
        } else if (control.kind === 'response_head') {
          response.head = control;
        } else if (control.kind === 'response_end') {
          response.ended = true;
        } else if (control.kind === 'route_unregister') {
          assert.equal(role, 'control');
        } else if (control.kind === 'error') {
          errors.push(new Error(control.message));
        } else {
          throw new Error(`unexpected ${role} client control after session expiry: ${control.kind}`);
        }
      } catch (error) { errors.push(error); }
    });
  });

  const config = {
    host: '127.0.0.1', port: address.port,
    publicBaseUrl: 'https://tunnel.example/builtin/clients/v5_expired', dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'password', tokenSecret: 'a sufficiently long expired session test secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true, publicUrl: 'https://tunnel.example/builtin/clients/v5_expired/mcp',
      enrollmentUrl: 'https://tunnel.example/_tunnel/enroll/V5EXPIRED', workers: 1, stateFile
    }
  };
  const context = await createToolContext(config);
  const manager = new BuiltinTunnelManager(config, context, {
    websocketUrlOverride: `ws://127.0.0.1:${address.port}/_tunnel/v1`, reconcileIntervalMs: 20,
    enrollmentFetch: async (_url, init) => {
      const body = JSON.parse(String(init?.body ?? '{}'));
      enrolledPublicKey = body.public_key;
      return new Response(JSON.stringify(enrollmentResponse(body.device_id, 'v5_expired')), {
        status: 200, headers: { 'content-type': 'application/json' }
      });
    }
  });
  t.after(() => manager.stop());
  await manager.start();

  await waitFor(
    () => response.ended && controlConnections >= 3 && dataConnections >= 2,
    `expired split session did not rebuild: control=${controlConnections}; data=${dataConnections}; status=${JSON.stringify(context.tunnelStatus)}`,
    6000
  );
  assert.deepEqual(controlSessionHeaders.slice(0, 3), ['', oldSessionId, '']);
  assert.equal(response.head.route_id, 42);
  assert.equal(response.head.status, 206);
  assert.equal(Buffer.concat(response.chunks).toString(), 'recovered:after-expiry');
  assert.equal(context.tunnelStatus.completedRequests, 1);
  assert.equal(errors.length, 0, errors.map(String).join('; '));
  await manager.stop();
});

test('split v5 shares one device control and data hub across workspace managers', async t => {
  const dataDirA = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-v5-shared-a-'));
  const dataDirB = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-v5-shared-b-'));
  const dataDirC = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-v5-shared-c-'));
  const stateFileA = path.join(dataDirA, 'identity.enc.json');
  const stateFileB = path.join(dataDirB, 'identity.enc.json');
  const stateFileC = path.join(dataDirC, 'identity.enc.json');
  const localA = createServer((request, result) => {
    const chunks = [];
    request.on('data', chunk => chunks.push(Buffer.from(chunk)));
    request.on('end', () => result.writeHead(200, { 'content-type': 'text/plain' }).end(`A:${Buffer.concat(chunks)}`));
  });
  const localB = createServer((request, result) => {
    const chunks = [];
    request.on('data', chunk => chunks.push(Buffer.from(chunk)));
    request.on('end', () => result.writeHead(200, { 'content-type': 'text/plain' }).end(`B:${Buffer.concat(chunks)}`));
  });
  const localC = createServer((request, result) => {
    const chunks = [];
    request.on('data', chunk => chunks.push(Buffer.from(chunk)));
    request.on('end', () => result.writeHead(200, { 'content-type': 'text/plain' }).end(`C:${Buffer.concat(chunks)}`));
  });
  await new Promise(resolve => localA.listen(0, '127.0.0.1', resolve));
  await new Promise(resolve => localB.listen(0, '127.0.0.1', resolve));
  await new Promise(resolve => localC.listen(0, '127.0.0.1', resolve));
  t.after(() => closeServer(localA));
  t.after(() => closeServer(localB));
  t.after(() => closeServer(localC));
  const localAddressA = localA.address();
  const localAddressB = localB.address();
  const localAddressC = localC.address();
  assert.ok(localAddressA && typeof localAddressA === 'object');
  assert.ok(localAddressB && typeof localAddressB === 'object');
  assert.ok(localAddressC && typeof localAddressC === 'object');

  const tunnelServer = createServer((_request, result) => result.writeHead(404).end('not found'));
  await new Promise(resolve => tunnelServer.listen(0, '127.0.0.1', resolve));
  t.after(() => closeServer(tunnelServer));
  const tunnelAddress = tunnelServer.address();
  assert.ok(tunnelAddress && typeof tunnelAddress === 'object');

  const sessionId = 'session-node-shared';
  const policy = {
    start_connections: 1, min_connections: 1, max_idle_connections: 2, max_connections: 8,
    streams_per_connection: 16, scale_up_threshold_percent: 75, scale_up_step: 2,
    burst_scale_up_step: 4, scale_down_delay_seconds: 90, revision: 1
  };
  let enrolledDeviceId = '';
  let enrolledPublicKey = '';
  let controlConnections = 0;
  let dataConnections = 0;
  let dataSocket;
  let requestsSent = false;
  let interruptedSharedRoute = false;
  let rejectNextControlRebind = false;
  let controlSocket;
  let trackReplacementRoutes = false;
  const replacementRoutes = [];
  const controlSessionHeaders = [];
  const routes = new Map();
  const responses = new Map([
    [31, { chunks: [], ended: false }],
    [32, { chunks: [], ended: false }],
    [33, { chunks: [], ended: false }]
  ]);
  const errors = [];
  const maybeSendRequests = () => {
    if (requestsSent || !dataSocket || routes.size !== 3) return;
    requestsSent = true;
    for (const [clientId, routeId, streamId, body] of [
      ['v5_a', 31, 801, 'alpha'],
      ['v5_b', 32, 802, 'beta'],
      ['v5_c', 33, 803, 'charlie']
    ]) {
      assert.equal(routes.get(clientId), routeId);
      dataSocket.send(JSON.stringify({
        kind: 'request_head', route_id: routeId, stream_id: streamId, method: 'POST',
        path_and_query: `/builtin/clients/${clientId}/mcp?shared=1`, headers: [], demand: null
      }));
      dataSocket.send(encodeMultiplexBinaryFrame(
        MultiplexBinaryFrameKind.RequestBody, routeId, BigInt(streamId), Buffer.from(body)
      ));
      dataSocket.send(JSON.stringify({ kind: 'request_end', route_id: routeId, stream_id: streamId }));
    }
  };

  const wss = new WebSocketServer({
    server: tunnelServer, path: '/_tunnel/v1',
    handleProtocols(protocols) {
      if (protocols.has(MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL)) return MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL;
      if (protocols.has(MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL)) return MULTIPLEX_DATA_TUNNEL_SUBPROTOCOL;
      return false;
    }
  });
  t.after(() => {
    for (const client of wss.clients) client.terminate();
    wss.close();
  });
  wss.on('connection', (socket, request) => {
    const role = socket.protocol === MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL ? 'control' : 'data';
    if (role === 'control') {
      controlConnections += 1;
      controlSocket = socket;
      controlSessionHeaders.push(String(request.headers['x-coding-tools-session-id'] ?? ''));
    } else {
      dataConnections += 1;
      dataSocket = socket;
      assert.equal(String(request.headers['x-coding-tools-session-id'] ?? ''), sessionId);
    }
    const deviceId = String(request.headers['x-coding-tools-device-id'] ?? '');
    const connectionId = String(request.headers['x-coding-tools-connection-id'] ?? '');
    const nonce = `shared-${role}-${connectionId}`;
    const expiresAtUnixMs = Date.now() + 30_000;
    socket.send(JSON.stringify({
      kind: 'challenge', nonce, expires_at_unix_ms: expiresAtUnixMs, server_id: testServerId,
      server_signature: signSignature(
        null,
        multiplexServerChallengeSigningPayload(nonce, expiresAtUnixMs, testServerId, deviceId, connectionId),
        testServerKeyPair.privateKey
      ).toString('base64url')
    }));
    socket.on('message', (data, isBinary) => {
      try {
        if (isBinary) {
          const frame = decodeMultiplexBinaryFrame(Buffer.from(data));
          const response = responses.get(frame.routeId);
          assert.ok(response);
          assert.equal(frame.kind, MultiplexBinaryFrameKind.ResponseBody);
          response.chunks.push(frame.payload);
          return;
        }
        const control = JSON.parse(data.toString());
        if (control.kind === 'session_authenticate') {
          assert.equal(control.device_id, enrolledDeviceId);
          const raw = Buffer.from(enrolledPublicKey, 'base64url');
          const publicKey = createPublicKey({
            key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), raw]), format: 'der', type: 'spki'
          });
          const requestedSessionId = String(request.headers['x-coding-tools-session-id'] ?? '') || undefined;
          assert.equal(verifySignature(
            null,
            splitSessionAuthSigningPayload(nonce, role, control.device_id, connectionId, requestedSessionId),
            publicKey, Buffer.from(control.signature, 'base64url')
          ), true);
          if (role === 'control' && requestedSessionId && rejectNextControlRebind) {
            rejectNextControlRebind = false;
            socket.send(JSON.stringify({ kind: 'error', message: 'split control session is unavailable' }));
            return;
          }
          const ackSignature = signSignature(
            null,
            splitServerAckSigningPayload(nonce, testServerId, role, control.device_id, connectionId, sessionId, policy),
            testServerKeyPair.privateKey
          ).toString('base64url');
          socket.send(JSON.stringify({
            kind: 'session_ready', protocol_version: 5, connection_role: role, session_id: sessionId,
            connection_policy: policy, server_id: testServerId, server_signature: ackSignature
          }));
          if (role === 'data') queueMicrotask(maybeSendRequests);
        } else if (control.kind === 'route_register') {
          assert.equal(role, 'control');
          if (control.client_id === 'v5_b' && !interruptedSharedRoute) {
            interruptedSharedRoute = true;
            socket.terminate();
            return;
          }
          const routeId = control.client_id === 'v5_a' ? 31 : control.client_id === 'v5_b' ? 32 : control.client_id === 'v5_c' ? 33 : 0;
          assert.ok(routeId);
          if (trackReplacementRoutes) replacementRoutes.push(control.client_id);
          routes.set(control.client_id, routeId);
          socket.send(JSON.stringify({
            kind: 'route_registered', registration_id: control.registration_id, route_id: routeId, stream_limit: 16
          }));
          queueMicrotask(maybeSendRequests);
        } else if (control.kind === 'response_head') {
          assert.equal(role, 'data');
          const response = responses.get(Number(control.route_id));
          assert.ok(response);
          assert.equal(control.status, 200);
        } else if (control.kind === 'response_end') {
          assert.equal(role, 'data');
          const response = responses.get(Number(control.route_id));
          assert.ok(response);
          response.ended = true;
        } else if (control.kind === 'route_unregister') {
          assert.equal(role, 'control');
        } else if (control.kind === 'error') {
          errors.push(new Error(control.message));
        } else {
          throw new Error(`unexpected shared ${role} control: ${control.kind}`);
        }
      } catch (error) { errors.push(error); }
    });
  });

  const sharedDeviceIdentities = new Map();
  const sharedSplitHubs = new Map();
  const enrollmentFetch = async (_url, init) => {
    const body = JSON.parse(String(init?.body ?? '{}'));
    if (!enrolledDeviceId) {
      enrolledDeviceId = body.device_id;
      enrolledPublicKey = body.public_key;
    } else {
      assert.equal(body.device_id, enrolledDeviceId);
      assert.equal(body.public_key, enrolledPublicKey);
    }
    return new Response(JSON.stringify(enrollmentResponse(body.device_id, body.client_id)), {
      status: 200, headers: { 'content-type': 'application/json' }
    });
  };
  const makeConfig = (clientId, port, dataDir, stateFile) => ({
    host: '127.0.0.1', port,
    publicBaseUrl: `https://tunnel.example/builtin/clients/${clientId}`, dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'password', tokenSecret: `a sufficiently long ${clientId} shared token secret` },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true, publicUrl: `https://tunnel.example/builtin/clients/${clientId}/mcp`,
      enrollmentUrl: `https://tunnel.example/_tunnel/enroll/${clientId.toUpperCase().replaceAll('_', '')}`, workers: 1, stateFile
    }
  });
  const configA = makeConfig('v5_a', localAddressA.port, dataDirA, stateFileA);
  const configB = makeConfig('v5_b', localAddressB.port, dataDirB, stateFileB);
  const configC = makeConfig('v5_c', localAddressC.port, dataDirC, stateFileC);
  const contextA = await createToolContext(configA);
  const contextB = await createToolContext(configB);
  const contextC = await createToolContext(configC);
  const sharedOptions = {
    websocketUrlOverride: `ws://127.0.0.1:${tunnelAddress.port}/_tunnel/v1`,
    reconcileIntervalMs: 20, enrollmentFetch, sharedDeviceIdentities, sharedSplitHubs
  };
  const managerA = new BuiltinTunnelManager(configA, contextA, sharedOptions);
  const managerB = new BuiltinTunnelManager(configB, contextB, sharedOptions);
  const managerC = new BuiltinTunnelManager(configC, contextC, sharedOptions);
  t.after(async () => { await managerC.stop(); await managerB.stop(); await managerA.stop(); });

  await managerA.start();
  await managerB.start();
  await managerC.start();
  await waitFor(
    () => responses.get(31).ended && responses.get(32).ended && responses.get(33).ended,
    `shared split routes did not all complete: ${JSON.stringify([...responses.entries()].map(([route, response]) => [route, response.ended]))}`,
    6000
  );
  assert.equal(interruptedSharedRoute, true);
  assert.equal(controlConnections, 2);
  assert.deepEqual(controlSessionHeaders.slice(0, 2), ['', sessionId]);
  assert.equal(dataConnections, 1);
  assert.equal(routes.size, 3);
  assert.equal(Buffer.concat(responses.get(31).chunks).toString(), 'A:alpha');
  assert.equal(Buffer.concat(responses.get(32).chunks).toString(), 'B:beta');
  assert.equal(Buffer.concat(responses.get(33).chunks).toString(), 'C:charlie');
  assert.equal(contextA.tunnelStatus.completedRequests, 1);
  assert.equal(contextB.tunnelStatus.completedRequests, 1);
  assert.equal(contextC.tunnelStatus.completedRequests, 1);
  assert.equal(errors.length, 0, errors.map(String).join('; '));

  await managerA.stop();
  await waitFor(
    () => controlConnections >= 3 && dataConnections >= 2 && contextB.tunnelStatus.state === 'running',
    `shared split hub was not handed off: control=${controlConnections} data=${dataConnections} state=${contextB.tunnelStatus.state}`,
    6000
  );
  responses.set(33, { chunks: [], ended: false });
  dataSocket.send(JSON.stringify({
    kind: 'request_head', route_id: 33, stream_id: 804, method: 'POST',
    path_and_query: '/builtin/clients/v5_c/mcp?handoff=1', headers: [], demand: null
  }));
  dataSocket.send(encodeMultiplexBinaryFrame(
    MultiplexBinaryFrameKind.RequestBody, 33, 804n, Buffer.from('delta')
  ));
  dataSocket.send(JSON.stringify({ kind: 'request_end', route_id: 33, stream_id: 804 }));
  await waitFor(
    () => responses.get(33).ended,
    'migrated secondary route did not complete a request after owner handoff',
    6000
  );
  assert.equal(Buffer.concat(responses.get(33).chunks).toString(), 'C:delta');
  assert.equal(contextA.tunnelStatus.state, 'stopped');
  assert.equal(contextB.tunnelStatus.state, 'running');
  assert.equal(contextC.tunnelStatus.completedRequests, 2);
  assert.equal(errors.length, 0, errors.map(String).join('; '));

  trackReplacementRoutes = true;
  rejectNextControlRebind = true;
  controlSocket.terminate();
  await managerC.stop();
  await waitFor(
    () => controlConnections >= 5 && dataConnections >= 3 && contextB.tunnelStatus.state === 'running',
    `pending remove did not survive fresh split rebuild: control=${controlConnections} data=${dataConnections}`,
    6000
  );
  assert.deepEqual(replacementRoutes, ['v5_b']);
  assert.equal(contextC.tunnelStatus.state, 'stopped');
  assert.equal(errors.length, 0, errors.map(String).join('; '));
  await managerB.stop();
});

test('built-in tunnel rebuilds the public endpoint from the authoritative client ID', () => {
  const endpoint = builtinEndpointForClient(
    'https://tunnel.example/builtin/clients/provisional/mcp',
    'server_assigned_1'
  );
  assert.equal(endpoint.publicUrl, 'https://tunnel.example/builtin/clients/server_assigned_1/mcp');
  assert.equal(endpoint.baseUrl, 'https://tunnel.example/builtin/clients/server_assigned_1');
  assert.equal(endpoint.clientId, 'server_assigned_1');
});

test('authentication signing payload matches the Rust field order and names', () => {
  const payload = authSigningPayload('nonce', 'device', 'client', 'worker');
  assert.equal(payload.toString(), '{"protocol_version":4,"nonce":"nonce","device_id":"device","client_id":"client","service":"mcp","worker_id":"worker"}');
});

test('built-in tunnel rejects non-HTTPS and non-client MCP paths', () => {
  assert.throws(() => parseBuiltinPublicUrl('http://tunnel.example/builtin/clients/device/mcp'), /HTTPS/);
  assert.throws(() => parseBuiltinPublicUrl('https://tunnel.example/other/path'), /builtin\/clients/);
});

test('public tunnel security is independent of permission mode and sandbox', () => {
  const config = {
    tunnel: { enabled: true, publicUrl: 'https://tunnel.example/builtin/clients/device/mcp' },
    sandbox: { enabled: false },
    permissionMode: 'guarded'
  };
  assert.equal(publicTunnelSecurityError(config), undefined);

  delete config.sandbox;
  config.permissionMode = 'trusted';
  assert.equal(publicTunnelSecurityError(config), undefined);

  config.sandbox = { enabled: true };
  config.permissionMode = 'dangerous';
  assert.equal(publicTunnelSecurityError(config), undefined);
});

test('invalid built-in tunnel configuration records a recoverable error status', async () => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-invalid-'));
  const config = {
    host: '127.0.0.1',
    port: 3789,
    dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'test-password', tokenSecret: 'a sufficiently long test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true,
      publicUrl: 'https://tunnel.example/mcp',
      stateFile: path.join(dataDir, 'identity.enc.json')
    }
  };
  const context = await createToolContext(config);
  const manager = new BuiltinTunnelManager(config, context);

  await assert.rejects(manager.start(), /builtin\/clients/);
  assert.equal(context.tunnelStatus.state, 'error');
  assert.equal(context.tunnelStatus.workers, 0);
  assert.equal(context.tunnelStatus.publicUrl, 'https://tunnel.example/mcp');
  assert.match(context.tunnelStatus.lastError, /builtin\/clients/);
});

test('built-in tunnel reconfigure can disable the runtime without restarting the agent', async () => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-reconfigure-'));
  const config = {
    host: '127.0.0.1',
    port: 3789,
    dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'test-password', tokenSecret: 'a sufficiently long test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: false,
      publicUrl: 'https://tunnel.example/builtin/clients/device_1/mcp'
    }
  };
  const context = await createToolContext(config);
  const manager = new BuiltinTunnelManager(config, context);

  await manager.reconfigure(undefined, undefined);
  assert.equal(config.tunnel, undefined);
  assert.equal(context.config.tunnel, undefined);
  assert.equal(config.publicBaseUrl, undefined);
  assert.equal(context.config.publicBaseUrl, undefined);
  assert.deepEqual(context.tunnelStatus, {
    enabled: false,
    state: 'disabled',
    workers: 0,
    connectedWorkers: 0,
    completedRequests: 0
  });
});

test('multiplex pool planner scales at threshold, bursts on queue, avoids overspawn, and returns to one idle connection', () => {
  const policy = {
    start_connections: 1,
    min_connections: 1,
    max_idle_connections: 2,
    max_connections: 8,
    streams_per_connection: 16,
    scale_up_threshold_percent: 75,
    scale_up_step: 2,
    burst_scale_up_step: 4,
    scale_down_delay_seconds: 90,
    revision: 1
  };

  assert.deepEqual(multiplexPoolAdjustment(policy, {
    connectedConnections: 1, connectingConnections: 0, activeStreams: 11,
    queuedRequests: 0, desiredConnections: 1, idleConnections: 0, idleExcessElapsed: false
  }), { spawn: 0, retire: 0 });
  assert.deepEqual(multiplexPoolAdjustment(policy, {
    connectedConnections: 1, connectingConnections: 0, activeStreams: 12,
    queuedRequests: 0, desiredConnections: 1, idleConnections: 0, idleExcessElapsed: false
  }), { spawn: 2, retire: 0 });
  assert.deepEqual(multiplexPoolAdjustment(policy, {
    connectedConnections: 1, connectingConnections: 0, activeStreams: 16,
    queuedRequests: 1, desiredConnections: 2, idleConnections: 0, idleExcessElapsed: false
  }), { spawn: 4, retire: 0 });
  assert.deepEqual(multiplexPoolAdjustment(policy, {
    connectedConnections: 1, connectingConnections: 2, activeStreams: 12,
    queuedRequests: 0, desiredConnections: 1, idleConnections: 0, idleExcessElapsed: false
  }), { spawn: 0, retire: 0 });
  assert.deepEqual(multiplexPoolAdjustment(policy, {
    connectedConnections: 5, connectingConnections: 0, activeStreams: 0,
    queuedRequests: 0, desiredConnections: 1, idleConnections: 5, idleExcessElapsed: false
  }), { spawn: 0, retire: 0 });
  assert.deepEqual(multiplexPoolAdjustment(policy, {
    connectedConnections: 5, connectingConnections: 0, activeStreams: 0,
    queuedRequests: 0, desiredConnections: 1, idleConnections: 5, idleExcessElapsed: true
  }), { spawn: 0, retire: 4 });
});

test('WorkerPolicy validation and pool planning match Rust behavior', () => {
  const policy = normalizeWorkerPolicy({
    ...defaultPolicy,
    start_workers: 4,
    min_idle_workers: 2,
    max_idle_workers: 4,
    max_workers: 16,
    max_connecting_workers: 4,
    scale_down_step: 4,
    burst_warm_workers: 8
  });
  assert.equal(configuredMaxConnecting(policy), 4);
  assert.equal(configuredBurstWarmFloor(policy), 8);
  assert.deepEqual(poolAdjustment(
    policy,
    { total: 1, connecting: 1, idle: 0, busy: 0 },
    1, 4, 0, false, 4
  ), { spawn: 3, retire: 0 });
  assert.deepEqual(poolAdjustment(
    policy,
    { total: 4, connecting: 0, idle: 1, busy: 3 },
    0, 4, 16, false, 8
  ), { spawn: 4, retire: 0 });
  assert.deepEqual(poolAdjustment(
    policy,
    { total: 16, connecting: 0, idle: 16, busy: 0 },
    0, 4, 0, true, 8
  ), { spawn: 0, retire: 4 });
  assert.throws(() => normalizeWorkerPolicy({ ...defaultPolicy, min_idle_workers: 5 }), /worker counts/);
});

test('worker recycle limits use bounded deterministic jitter', () => {
  const policy = normalizeWorkerPolicy(defaultPolicy);
  const first = jitteredLimit(policy.max_requests_per_worker, 7, policy.recycle_jitter_percent);
  const second = jitteredLimit(policy.max_requests_per_worker, 8, policy.recycle_jitter_percent);
  assert.ok(first >= 4_000 && first <= 6_000);
  assert.ok(second >= 4_000 && second <= 6_000);
  assert.notEqual(first, second);
  assert.equal(workerShouldRecycle(policy, 7, first - 1, 10_000), false);
  assert.equal(workerShouldRecycle(policy, 7, first, 10_000), true);
});

test('built-in tunnel exposes only scoped MCP and OAuth routes', () => {
  const config = {
    publicBaseUrl: 'https://tunnel.example/builtin/clients/device_1',
    tunnel: { publicUrl: 'https://tunnel.example/builtin/clients/device_1/mcp' }
  };
  assert.equal(tunnelPathAllowed(config, '/builtin/clients/device_1/mcp'), true);
  assert.equal(tunnelPathAllowed(config, '/builtin/clients/device_1/oauth/authorize'), true);
  assert.equal(tunnelPathAllowed(config, '/.well-known/oauth-protected-resource/builtin/clients/device_1/mcp'), true);
  assert.equal(tunnelPathAllowed(config, '/ui'), false);
  assert.equal(tunnelPathAllowed(config, '/admin/api/config'), false);
  assert.equal(tunnelPathAllowed(config, '/builtin/clients/device_1/../ui'), false);
});

test('built-in tunnel reuses an enrolled identity when a stale enrollment URL remains after restart', async () => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-restart-'));
  const stateFile = path.join(dataDir, 'identity.enc.json');
  const tokenSecret = 'a sufficiently long restart test token secret';
  const initialEndpoint = parseBuiltinPublicUrl('https://tunnel.example/builtin/clients/provisional_1/mcp');
  const enrollmentUrl = 'https://tunnel.example/_tunnel/enroll/TESTCODE';
  const config = {
    oauth: { tokenSecret },
    tunnel: {
      enabled: true,
      publicUrl: initialEndpoint.publicUrl,
      enrollmentUrl,
      stateFile
    }
  };
  let enrollmentCalls = 0;
  const enrolled = await loadOrEnroll(config, initialEndpoint, async (_url, init) => {
    enrollmentCalls += 1;
    const body = JSON.parse(String(init?.body ?? '{}'));
    return new Response(JSON.stringify(enrollmentResponse(body.device_id, 'server_assigned_1')), {
      status: 200,
      headers: { 'content-type': 'application/json' }
    });
  });

  assert.equal(enrolled.enrollmentCompleted, true);
  assert.equal(enrolled.identity.enrolled, true);
  assert.equal(enrolled.identity.clientId, 'server_assigned_1');
  assert.equal(enrollmentCalls, 1);

  const restartedEndpoint = parseBuiltinPublicUrl('https://tunnel.example/builtin/clients/server_assigned_1/mcp');
  const restarted = await loadOrEnroll({
    ...config,
    tunnel: {
      ...config.tunnel,
      publicUrl: restartedEndpoint.publicUrl,
      enrollmentUrl
    }
  }, restartedEndpoint, async () => {
    enrollmentCalls += 1;
    throw new Error('an enrolled identity must not re-enroll during restart');
  });

  assert.equal(restarted.enrollmentCompleted, false);
  assert.equal(enrollmentCalls, 1);
  assert.equal(restarted.identity.enrolled, true);
  assert.equal(restarted.identity.clientId, enrolled.identity.clientId);
  assert.equal(restarted.identity.deviceId, enrolled.identity.deviceId);
  assert.equal(restarted.identity.privateKeyDer, enrolled.identity.privateKeyDer);
  assert.equal(restarted.identity.publicKeyRaw, enrolled.identity.publicKeyRaw);
});

test('multiplex v5 manager uses one physical WSS for concurrent logical streams', async t => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-v5-runtime-'));
  const stateFile = path.join(dataDir, 'identity.enc.json');
  let enrolledPublicKey = '';
  let connectionCount = 0;
  let routeId;
  const responses = new Map();
  const errors = [];
  const events = [];

  const server = createServer(async (request, result) => {
    if (!request.url?.startsWith('/builtin/clients/v5_client/mcp')) {
      result.writeHead(404).end('not found');
      return;
    }
    const chunks = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const body = Buffer.concat(chunks).toString('utf8');
    if (body === 'one') await new Promise(resolve => setTimeout(resolve, 30));
    result.writeHead(body === 'one' ? 201 : 202, { 'x-stream': body });
    result.write(`head-${body}:`);
    await new Promise(resolve => setTimeout(resolve, body === 'one' ? 10 : 1));
    result.end(`tail-${body}`);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => { if (server.listening) return closeServer(server); });
  const address = server.address();
  assert.ok(address && typeof address === 'object');

  const connectionPolicy = {
    start_connections: 1,
    min_connections: 1,
    max_idle_connections: 2,
    max_connections: 8,
    streams_per_connection: 16,
    scale_up_threshold_percent: 75,
    scale_up_step: 2,
    burst_scale_up_step: 4,
    scale_down_delay_seconds: 90,
    revision: 1
  };
  const wss = new WebSocketServer({
    server,
    path: '/_tunnel/v1',
    handleProtocols(protocols) {
      return protocols.has(MULTIPLEX_TUNNEL_SUBPROTOCOL) ? MULTIPLEX_TUNNEL_SUBPROTOCOL : false;
    }
  });
  t.after(() => {
    for (const client of wss.clients) client.terminate();
    wss.close();
  });

  const responseFor = streamId => {
    const key = String(streamId);
    let state = responses.get(key);
    if (!state) {
      state = { heads: [], chunks: [], ended: false };
      responses.set(key, state);
    }
    return state;
  };

  wss.on('connection', (socket, request) => {
    connectionCount += 1;
    events.push(`connection:${connectionCount}`);
    const deviceId = String(request.headers['x-coding-tools-device-id'] ?? '');
    const connectionId = String(request.headers['x-coding-tools-connection-id'] ?? '');
    assert.ok(deviceId);
    assert.ok(connectionId);
    assert.equal(request.headers['x-coding-tools-client-id'], 'v5_client', 'v4 compatibility header remains available');
    const nonce = `v5-runtime-${connectionCount}`;
    const expiresAtUnixMs = Date.now() + 30_000;
    const serverSignature = signSignature(
      null,
      multiplexServerChallengeSigningPayload(nonce, expiresAtUnixMs, testServerId, deviceId, connectionId),
      testServerKeyPair.privateKey
    ).toString('base64url');
    socket.send(JSON.stringify({
      kind: 'challenge', nonce, expires_at_unix_ms: expiresAtUnixMs,
      server_id: testServerId, server_signature: serverSignature
    }));

    socket.on('message', (data, isBinary) => {
      try {
        if (isBinary) {
          const frame = decodeMultiplexBinaryFrame(Buffer.from(data));
          assert.equal(frame.kind, MultiplexBinaryFrameKind.ResponseBody);
          responseFor(frame.streamId).chunks.push(frame.payload);
          return;
        }
        const control = JSON.parse(data.toString());
        events.push(`control:${control.kind}`);
        if (control.kind === 'authenticate') {
          const raw = Buffer.from(enrolledPublicKey, 'base64url');
          const spki = Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), raw]);
          const publicKey = createPublicKey({ key: spki, format: 'der', type: 'spki' });
          assert.equal(control.hello.protocol_version, 5);
          assert.equal(control.hello.connection_id, connectionId);
          const valid = verifySignature(
            null,
            deviceSessionAuthSigningPayload(nonce, control.device_id, control.hello.connection_id),
            publicKey,
            Buffer.from(control.signature, 'base64url')
          );
          assert.equal(valid, true);
          const ackSignature = signSignature(
            null,
            multiplexServerAckSigningPayload(nonce, testServerId, control.device_id, connectionId, connectionPolicy),
            testServerKeyPair.privateKey
          ).toString('base64url');
          socket.send(JSON.stringify({
            kind: 'hello_ack', protocol_version: 5, connection_policy: connectionPolicy,
            server_id: testServerId, server_signature: ackSignature
          }));
        } else if (control.kind === 'route_register') {
          assert.equal(control.client_id, 'v5_client');
          assert.equal(control.service, 'mcp');
          routeId = 7;
          events.push('route-registered');
          socket.send(JSON.stringify({
            kind: 'route_registered', registration_id: control.registration_id,
            route_id: routeId, stream_limit: 16
          }));
          queueMicrotask(() => {
            for (const [streamId, payload] of [[11n, 'one'], [12n, 'two']]) {
              socket.send(JSON.stringify({
                kind: 'request_head', route_id: routeId, stream_id: Number(streamId), method: 'POST',
                path_and_query: `/builtin/clients/v5_client/mcp?stream=${streamId}`,
                headers: [{ name: 'content-type', value: 'text/plain' }]
              }));
              socket.send(encodeMultiplexBinaryFrame(MultiplexBinaryFrameKind.RequestBody, routeId, streamId, Buffer.from(payload)));
              socket.send(JSON.stringify({ kind: 'request_end', route_id: routeId, stream_id: Number(streamId) }));
            }
          });
        } else if (control.kind === 'response_head') {
          responseFor(BigInt(control.stream_id)).heads.push(control);
        } else if (control.kind === 'response_end') {
          responseFor(BigInt(control.stream_id)).ended = true;
        } else if (control.kind === 'error') {
          errors.push(new Error(control.message));
        }
      } catch (error) {
        errors.push(error);
      }
    });
  });

  const config = {
    host: '127.0.0.1',
    port: address.port,
    publicBaseUrl: 'https://tunnel.example/builtin/clients/v5_client',
    dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'password', tokenSecret: 'a sufficiently long test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true,
      publicUrl: 'https://tunnel.example/builtin/clients/v5_client/mcp',
      enrollmentUrl: 'https://tunnel.example/_tunnel/enroll/V5RUNTIME',
      workers: 1,
      stateFile
    }
  };
  const context = await createToolContext(config);
  const manager = new BuiltinTunnelManager(config, context, {
    websocketUrlOverride: `ws://127.0.0.1:${address.port}/_tunnel/v1`,
    reconcileIntervalMs: 20,
    enrollmentFetch: async (_url, init) => {
      const body = JSON.parse(String(init?.body ?? '{}'));
      enrolledPublicKey = body.public_key;
      return new Response(JSON.stringify(enrollmentResponse(body.device_id, 'v5_client')), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    }
  });
  t.after(() => manager.stop());
  await manager.start();

  try {
    await waitFor(
      () => responses.get('11')?.ended && responses.get('12')?.ended,
      `multiplex responses did not finish; events=${events.join(',')}; status=${JSON.stringify(context.tunnelStatus)}`
    );
  } catch (error) {
    throw new AggregateError([error, ...errors], `v5 runtime diagnostics: events=${events.join(',')}; status=${JSON.stringify(context.tunnelStatus)}`);
  }
  await manager.stop();
  assert.equal(connectionCount, 1);
  assert.equal(routeId, 7);
  assert.equal(responses.get('11').heads[0].status, 201);
  assert.equal(responses.get('12').heads[0].status, 202);
  assert.equal(Buffer.concat(responses.get('11').chunks).toString(), 'head-one:tail-one');
  assert.equal(Buffer.concat(responses.get('12').chunks).toString(), 'head-two:tail-two');
  assert.equal(context.tunnelStatus.completedRequests, 2);
  assert.equal(errors.length, 0, errors.map(String).join('; '));
});

test('multiplex v5 manager expands physical connections under stream pressure and scales back to one', async t => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-v5-scaling-'));
  const stateFile = path.join(dataDir, 'identity.enc.json');
  let enrolledPublicKey = '';
  let fakeNow = 1;
  let nextStreamId = 100;
  let firstSocket;
  const activeSockets = new Set();
  const heldResponses = new Set();
  const errors = [];
  t.after(() => {
    for (const response of [...heldResponses]) response.end('cleanup');
  });

  const server = createServer((request, result) => {
    if (!request.url?.startsWith('/builtin/clients/v5_scale/mcp')) {
      result.writeHead(404).end('not found');
      return;
    }
    result.writeHead(200, { 'content-type': 'text/plain' });
    result.flushHeaders();
    heldResponses.add(result);
    result.on('close', () => heldResponses.delete(result));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => closeServer(server));
  const address = server.address();
  assert.ok(address && typeof address === 'object');

  const connectionPolicy = {
    start_connections: 1,
    min_connections: 1,
    max_idle_connections: 2,
    max_connections: 8,
    streams_per_connection: 16,
    scale_up_threshold_percent: 75,
    scale_up_step: 2,
    burst_scale_up_step: 4,
    scale_down_delay_seconds: 1,
    revision: 1
  };
  const wss = new WebSocketServer({
    server,
    path: '/_tunnel/v1',
    handleProtocols(protocols) {
      return protocols.has(MULTIPLEX_TUNNEL_SUBPROTOCOL) ? MULTIPLEX_TUNNEL_SUBPROTOCOL : false;
    }
  });
  t.after(() => {
    for (const client of wss.clients) client.terminate();
    wss.close();
  });

  const sendRequest = (socket, routeId, demand) => {
    const streamId = nextStreamId++;
    socket.send(JSON.stringify({
      kind: 'request_head', route_id: routeId, stream_id: streamId, method: 'GET',
      path_and_query: `/builtin/clients/v5_scale/mcp?stream=${streamId}`,
      headers: [], demand
    }));
    socket.send(JSON.stringify({ kind: 'request_end', route_id: routeId, stream_id: streamId }));
  };

  wss.on('connection', (socket, request) => {
    activeSockets.add(socket);
    socket.on('close', () => activeSockets.delete(socket));
    const deviceId = String(request.headers['x-coding-tools-device-id'] ?? '');
    const connectionId = String(request.headers['x-coding-tools-connection-id'] ?? '');
    const nonce = `v5-scale-${connectionId}`;
    const expiresAtUnixMs = Date.now() + 30_000;
    socket.send(JSON.stringify({
      kind: 'challenge', nonce, expires_at_unix_ms: expiresAtUnixMs,
      server_id: testServerId,
      server_signature: signSignature(
        null,
        multiplexServerChallengeSigningPayload(nonce, expiresAtUnixMs, testServerId, deviceId, connectionId),
        testServerKeyPair.privateKey
      ).toString('base64url')
    }));
    socket.on('message', (data, isBinary) => {
      if (isBinary) return;
      try {
        const control = JSON.parse(data.toString());
        if (control.kind === 'authenticate') {
          const raw = Buffer.from(enrolledPublicKey, 'base64url');
          const publicKey = createPublicKey({
            key: Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), raw]),
            format: 'der', type: 'spki'
          });
          assert.equal(verifySignature(
            null,
            deviceSessionAuthSigningPayload(nonce, control.device_id, connectionId),
            publicKey,
            Buffer.from(control.signature, 'base64url')
          ), true);
          socket.send(JSON.stringify({
            kind: 'hello_ack', protocol_version: 5, connection_policy: connectionPolicy,
            server_id: testServerId,
            server_signature: signSignature(
              null,
              multiplexServerAckSigningPayload(nonce, testServerId, control.device_id, connectionId, connectionPolicy),
              testServerKeyPair.privateKey
            ).toString('base64url')
          }));
        } else if (control.kind === 'route_register') {
          const routeId = 9;
          socket.send(JSON.stringify({
            kind: 'route_registered', registration_id: control.registration_id,
            route_id: routeId, stream_limit: 16
          }));
          if (!firstSocket) {
            firstSocket = socket;
            queueMicrotask(() => {
              for (let index = 0; index < 12; index += 1) {
                sendRequest(socket, routeId, {
                  queued_requests: 0, oldest_queue_wait_ms: 0,
                  active_streams: index, available_stream_slots: 16 - index,
                  desired_connections: 1
                });
              }
            });
          }
        } else if (control.kind === 'error') {
          errors.push(new Error(control.message));
        }
      } catch (error) {
        errors.push(error);
      }
    });
  });

  const config = {
    host: '127.0.0.1', port: address.port,
    publicBaseUrl: 'https://tunnel.example/builtin/clients/v5_scale', dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'password', tokenSecret: 'a sufficiently long scaling test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true,
      publicUrl: 'https://tunnel.example/builtin/clients/v5_scale/mcp',
      enrollmentUrl: 'https://tunnel.example/_tunnel/enroll/V5SCALE', workers: 1, stateFile
    }
  };
  const context = await createToolContext(config);
  const manager = new BuiltinTunnelManager(config, context, {
    websocketUrlOverride: `ws://127.0.0.1:${address.port}/_tunnel/v1`,
    reconcileIntervalMs: 20,
    now: () => fakeNow,
    demandNow: () => fakeNow,
    enrollmentFetch: async (_url, init) => {
      const body = JSON.parse(String(init?.body ?? '{}'));
      enrolledPublicKey = body.public_key;
      return new Response(JSON.stringify(enrollmentResponse(body.device_id, 'v5_scale')), {
        status: 200, headers: { 'content-type': 'application/json' }
      });
    }
  });
  await manager.start();

  try {
    await withTimeout(
      waitFor(
        () => activeSockets.size === 3,
      `75% stream utilization did not expand to three connections: sockets=${activeSockets.size}; status=${JSON.stringify(context.tunnelStatus)}`
    ),
    6_000,
    () => `stage 1->3 timed out: sockets=${activeSockets.size}; held=${heldResponses.size}; status=${JSON.stringify(context.tunnelStatus)}; errors=${errors.map(String).join('; ')}`
    );
    sendRequest(firstSocket, 9, {
      queued_requests: 1, oldest_queue_wait_ms: 25,
    active_streams: 12, available_stream_slots: 4, desired_connections: 2
  });
  await withTimeout(
    waitFor(
      () => activeSockets.size === 7,
      `queued pressure did not burst by four connections: sockets=${activeSockets.size}; status=${JSON.stringify(context.tunnelStatus)}`
    ),
    6_000,
    () => `stage 3->7 timed out: sockets=${activeSockets.size}; held=${heldResponses.size}; status=${JSON.stringify(context.tunnelStatus)}; errors=${errors.map(String).join('; ')}`
    );

    for (const response of [...heldResponses]) response.end('done');
    await waitFor(() => context.tunnelStatus.completedRequests === 13, 'multiplex scaling requests did not finish');
  fakeNow += BUILTIN_TUNNEL_DEMAND_TTL_MS + 1;
  await new Promise(resolve => setTimeout(resolve, 50));
  fakeNow += 1_001;
  await withTimeout(
    waitFor(
      () => activeSockets.size === 1,
      `idle multiplex pool did not scale back to one connection: sockets=${activeSockets.size}; status=${JSON.stringify(context.tunnelStatus)}`
    ),
    6_000,
    () => `stage 7->1 timed out: sockets=${activeSockets.size}; held=${heldResponses.size}; status=${JSON.stringify(context.tunnelStatus)}; errors=${errors.map(String).join('; ')}`
  );
    assert.equal(errors.length, 0, errors.map(String).join('; '));
  } finally {
    for (const response of [...heldResponses]) response.end('cleanup');
    await withTimeout(manager.stop(), 5_000, () => `manager stop timed out: sockets=${activeSockets.size}; status=${JSON.stringify(context.tunnelStatus)}`);
    for (const client of wss.clients) client.terminate();
    await new Promise(resolve => setTimeout(resolve, 20));
  }
});

test('new Node client falls back to a v4-only server for enrollment, auth and forwarding', async t => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-'));
  const stateFile = path.join(dataDir, 'identity.enc.json');
  let fakeNow = 1;
  let enrolledPublicKey = '';
  const offeredProtocolSets = [];
  const acceptedProtocols = new Set();
  const errors = [];
  const readySockets = new Set();
  const allSockets = new Set();
  const response = { status: 0, headers: [], chunks: [] };
  let resolveResponse;
  let rejectResponse;
  const responseDone = new Promise((resolve, reject) => { resolveResponse = resolve; rejectResponse = reject; });

  const server = createServer(async (request, result) => {
    if (!request.url?.startsWith('/builtin/clients/server_assigned_1/mcp')) {
      result.writeHead(404).end('not found');
      return;
    }
    const chunks = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    result.writeHead(201, { 'content-type': 'application/json', 'x-node-agent-test': 'ok', 'x-coding-tools-streaming': '1' });
    result.write('\n');
    await new Promise(resolve => setTimeout(resolve, 20));
    result.end(JSON.stringify({ echo: Buffer.concat(chunks).toString('utf8') }));
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => closeServer(server));
  const address = server.address();
  assert.ok(address && typeof address === 'object');

  const wss = new WebSocketServer({
    server,
    path: '/_tunnel/v1',
    handleProtocols(protocols) {
      offeredProtocolSets.push(new Set(protocols));
      return protocols.has(TUNNEL_SUBPROTOCOL) ? TUNNEL_SUBPROTOCOL : false;
    }
  });
  t.after(() => wss.close());

  const growPolicy = {
    ...defaultPolicy,
    start_workers: 2,
    min_idle_workers: 1,
    max_idle_workers: 2,
    max_workers: 4,
    max_requests_per_worker: 0,
    max_lifetime_seconds: 0,
    scale_down_delay_seconds: 1,
    burst_warm_seconds: 0,
    revision: 1
  };
  const shrinkPolicy = {
    ...growPolicy,
    start_workers: 1,
    min_idle_workers: 1,
    max_idle_workers: 1,
    max_workers: 1,
    max_connecting_workers: 1,
    scale_down_step: 1,
    burst_warm_workers: 1,
    revision: 2
  };

  wss.on('connection', (socket, request) => {
    allSockets.add(socket);
    acceptedProtocols.add(socket.protocol);
    const nonce = `nonce-${allSockets.size}`;
    assert.equal(request.headers['x-coding-tools-client-id'], 'server_assigned_1');
    assert.equal(request.headers['x-coding-tools-service'], 'mcp');
    sendSignedServerChallenge(socket, request, nonce);
    socket.on('close', () => { readySockets.delete(socket); allSockets.delete(socket); });
    socket.on('message', (data, isBinary) => {
      try {
        if (isBinary) {
          response.chunks.push(Buffer.from(data));
          return;
        }
        const control = JSON.parse(data.toString());
        if (control.kind === 'authenticate') {
          const raw = Buffer.from(enrolledPublicKey, 'base64url');
          const spki = Buffer.concat([Buffer.from('302a300506032b6570032100', 'hex'), raw]);
          const publicKey = createPublicKey({ key: spki, format: 'der', type: 'spki' });
          const valid = verifySignature(
            null,
            authSigningPayload(nonce, control.device_id, control.hello.client_id, control.hello.worker_id),
            publicKey,
            Buffer.from(control.signature, 'base64url')
          );
          assert.equal(valid, true);
          sendSignedServerAck(socket, nonce, control, growPolicy);
        } else if (control.kind === 'ready') {
          readySockets.add(socket);
        } else if (control.kind === 'response_head') {
          response.status = control.status;
          response.headers = control.headers;
        } else if (control.kind === 'response_end') {
          resolveResponse(response);
        } else if (control.kind === 'error') {
          rejectResponse(new Error(control.message));
        }
      } catch (error) {
        errors.push(error);
        rejectResponse(error);
      }
    });
  });

  const config = {
    host: '127.0.0.1',
    port: address.port,
    publicBaseUrl: 'https://tunnel.example/builtin/clients/provisional_1',
    dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'password', tokenSecret: 'a sufficiently long test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true,
      publicUrl: 'https://tunnel.example/builtin/clients/provisional_1/mcp',
      enrollmentUrl: 'https://tunnel.example/_tunnel/enroll/TESTCODE',
      workers: 2,
      stateFile
    }
  };
  const context = await createToolContext(config);
  let resolvedEndpoint;
  const manager = new BuiltinTunnelManager(config, context, {
    websocketUrlOverride: `ws://127.0.0.1:${address.port}/_tunnel/v1`,
    reconcileIntervalMs: 20,
    demandNow: () => fakeNow,
    enrollmentFetch: async (_url, init) => {
      const body = JSON.parse(String(init?.body ?? '{}'));
      enrolledPublicKey = body.public_key;
      assert.equal(body.client_id, 'provisional_1');
      return new Response(JSON.stringify(enrollmentResponse(body.device_id, 'server_assigned_1')), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    },
    onEndpointResolved: endpoint => { resolvedEndpoint = endpoint; }
  });
  t.after(() => manager.stop());
  await manager.start();

  assert.deepEqual(resolvedEndpoint, {
    publicUrl: 'https://tunnel.example/builtin/clients/server_assigned_1/mcp',
    publicBaseUrl: 'https://tunnel.example/builtin/clients/server_assigned_1',
    enrollmentCompleted: true
  });
  assert.equal(config.tunnel.publicUrl, resolvedEndpoint.publicUrl);
  assert.equal(config.publicBaseUrl, resolvedEndpoint.publicBaseUrl);
  assert.equal(config.tunnel.enrollmentUrl, undefined);
  assert.equal(context.tunnelStatus.publicUrl, resolvedEndpoint.publicUrl);

  await waitFor(
    () => readySockets.size === 2,
    `expected two ready workers; ready=${readySockets.size}; sockets=${allSockets.size}; status=${JSON.stringify(context.tunnelStatus)}; errors=${errors.map(String).join('; ')}`
  );
  assert.equal(context.tunnelStatus.policyRevision, 1);
  assert.equal(context.tunnelStatus.connectedWorkers, 2);
  assert.equal(context.tunnelStatus.idleWorkers, 2);
  assert.equal(context.tunnelStatus.workers, 4);
  assert.ok(
    offeredProtocolSets.some(protocols =>
      protocols.has(MULTIPLEX_CONTROL_TUNNEL_SUBPROTOCOL)
      && protocols.has(MULTIPLEX_TUNNEL_SUBPROTOCOL)
      && protocols.has(TUNNEL_SUBPROTOCOL)
    ),
    'new client never offered v5 control, mixed v5, and v4 together to the v4-only server'
  );
  assert.deepEqual([...acceptedProtocols], [TUNNEL_SUBPROTOCOL]);

  const worker = [...readySockets][0];
  readySockets.delete(worker);
  worker.send(JSON.stringify({
    kind: 'request_head',
    request_id: 'request-1',
    method: 'POST',
    path_and_query: '/builtin/clients/server_assigned_1/mcp?source=tunnel',
    headers: [{ name: 'content-type', value: 'text/plain' }],
    demand: { queued_requests: 1, oldest_queue_wait_ms: 25, desired_workers: 4 }
  }));
  worker.send(Buffer.from('payload'));
  worker.send(JSON.stringify({ kind: 'request_end', request_id: 'request-1' }));

  const forwarded = await withTimeout(
    responseDone,
    5000,
    () => `tunnel response timeout; errors=${errors.map(String).join('; ')}`
  );
  assert.equal(forwarded.status, 201);
  assert.ok(forwarded.chunks.length >= 2, `expected heartbeat and final response chunks, received ${forwarded.chunks.length}`);
  assert.equal(Buffer.concat(forwarded.chunks).toString('utf8'), '\n{"echo":"payload"}');
  assert.ok(forwarded.headers.some(header => header.name === 'x-node-agent-test' && header.value === 'ok'));
  assert.ok(forwarded.headers.some(header => header.name === 'x-coding-tools-streaming' && header.value === '1'));
  await waitFor(() => context.tunnelStatus.completedRequests === 1, 'completed request metric was not updated');
  await waitFor(
    () => context.tunnelStatus.connectedWorkers === 4 && readySockets.size === 4,
    `demand hint did not scale the pool to four workers: ${JSON.stringify(context.tunnelStatus)}; errors=${errors.map(String).join('; ')}`
  );

  fakeNow = 1 + BUILTIN_TUNNEL_DEMAND_TTL_MS;
  await new Promise(resolve => setTimeout(resolve, 80));
  assert.equal(context.tunnelStatus.connectedWorkers, 4, 'demand hint expired at the inclusive Rust boundary');
  fakeNow += 1;
  await waitFor(
    () => context.tunnelStatus.connectedWorkers === 2,
    `pool did not return to its configured idle floor after demand TTL: ${JSON.stringify(context.tunnelStatus)}`
  );

  for (const socket of allSockets) {
    if (socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ kind: 'policy_update', worker_policy: shrinkPolicy }));
  }
  await waitFor(
    () => context.tunnelStatus.policyRevision === 2 && context.tunnelStatus.connectedWorkers === 1,
    `pool did not shrink: ${JSON.stringify(context.tunnelStatus)}`
  );
  assert.equal(context.tunnelStatus.workers, 1);
  assert.equal(context.tunnelStatus.idleWorkers, 1);
  assert.equal(errors.length, 0);

  const encrypted = await readFile(stateFile, 'utf8');
  assert.doesNotMatch(encrypted, /privateKeyDer|publicKeyRaw|deviceId/);
  await manager.stop();
});

test('built-in tunnel bounds local connect and overall request phases without leaking request data', async t => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-timeout-'));
  const stateFile = path.join(dataDir, 'identity.enc.json');
  const pendingResponses = new Set();
  let delayedHeadersStarted = false;
  let delayedHeadersClosed = false;

  const localServer = createServer((request, result) => {
    const requestUrl = new URL(request.url ?? '/', 'http://127.0.0.1');
    const phase = requestUrl.searchParams.get('phase') ?? 'normal';
    pendingResponses.add(result);
    result.on('close', () => {
      pendingResponses.delete(result);
      if (phase === 'headers') delayedHeadersClosed = true;
    });
    if (phase === 'headers') {
      delayedHeadersStarted = true;
      return;
    }
    result.writeHead(200, { 'content-type': 'text/plain' });
    result.end(`ok:${phase}`);
  });
  await new Promise(resolve => localServer.listen(0, '127.0.0.1', resolve));
  const localAddress = localServer.address();
  assert.ok(localAddress && typeof localAddress === 'object');
  const localPort = localAddress.port;
  await closeServer(localServer);

  const tunnelServer = createServer((_request, result) => result.writeHead(404).end('not found'));
  await new Promise(resolve => tunnelServer.listen(0, '127.0.0.1', resolve));
  const tunnelAddress = tunnelServer.address();
  assert.ok(tunnelAddress && typeof tunnelAddress === 'object');
  const wss = new WebSocketServer({
    server: tunnelServer,
    path: '/_tunnel/v1',
    handleProtocols(protocols) { return protocols.has(TUNNEL_SUBPROTOCOL) ? TUNNEL_SUBPROTOCOL : false; }
  });

  const policy = {
    ...defaultPolicy,
    start_workers: 1,
    min_idle_workers: 1,
    max_idle_workers: 1,
    max_workers: 1,
    max_requests_per_worker: 0,
    max_lifetime_seconds: 0,
    scale_down_delay_seconds: 0,
    max_connecting_workers: 1,
    scale_down_step: 1,
    burst_warm_workers: 1,
    burst_warm_seconds: 0,
    revision: 1
  };
  const responses = new Map();
  const pongPayloads = [];
  let worker;
  let readyCount = 0;
  let activeRequestId;
  let lookupMode = 'normal';
  let hangingLookupCalls = 0;

  const responseFor = requestId => {
    let response = responses.get(requestId);
    if (!response) {
      response = { heads: [], chunks: [], ended: false, errors: [] };
      responses.set(requestId, response);
    }
    return response;
  };
  const sendRequest = (requestId, phase) => {
    assert.ok(worker && worker.readyState === WebSocket.OPEN);
    activeRequestId = requestId;
    responseFor(requestId);
    worker.send(JSON.stringify({
      kind: 'request_head',
      request_id: requestId,
      method: 'GET',
      path_and_query: `/builtin/clients/device_1/mcp?phase=${phase}`,
      headers: []
    }));
    worker.send(JSON.stringify({ kind: 'request_end', request_id: requestId }));
  };

  wss.on('connection', (socket, request) => {
    worker = socket;
    sendSignedServerChallenge(socket, request, 'timeout-test');
    socket.on('pong', data => pongPayloads.push(Buffer.from(data).toString('utf8')));
    socket.on('message', (data, isBinary) => {
      if (isBinary) {
        if (activeRequestId) responseFor(activeRequestId).chunks.push(Buffer.from(data));
        return;
      }
      const control = JSON.parse(data.toString());
      if (control.kind === 'authenticate') {
        sendSignedServerAck(socket, 'timeout-test', control, policy);
      } else if (control.kind === 'ready') {
        readyCount += 1;
        activeRequestId = undefined;
      } else if (control.kind === 'response_head') {
        activeRequestId = control.request_id;
        responseFor(control.request_id).heads.push(control);
      } else if (control.kind === 'response_end') {
        responseFor(control.request_id).ended = true;
      } else if (control.kind === 'error') {
        responseFor(control.request_id ?? activeRequestId ?? 'unknown').errors.push(control.message);
      }
    });
  });

  const config = {
    host: '127.0.0.1',
    port: localPort,
    publicBaseUrl: 'https://tunnel.example/builtin/clients/device_1',
    dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'test-password', tokenSecret: 'a sufficiently long test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true,
      publicUrl: 'https://tunnel.example/builtin/clients/device_1/mcp',
      enrollmentUrl: 'https://tunnel.example/_tunnel/enroll/TIMEOUTTEST',
      workers: 1,
      stateFile
    }
  };
  const context = await createToolContext(config);
  const manager = new BuiltinTunnelManager(config, context, {
    websocketUrlOverride: `ws://127.0.0.1:${tunnelAddress.port}/_tunnel/v1`,
    localOriginOverride: `http://loopback.test:${localPort}`,
    localConnectTimeoutMs: 100,
    localRequestTimeoutMs: 250,
    reconcileIntervalMs: 20,
    localLookup: (_hostname, options, callback) => {
      if (lookupMode === 'hang') {
        hangingLookupCalls += 1;
        return;
      }
      if (options?.all) callback(null, [{ address: '127.0.0.1', family: 4 }]);
      else callback(null, '127.0.0.1', 4);
    },
    enrollmentFetch: async (_url, init) => {
      const body = JSON.parse(String(init?.body ?? '{}'));
      return new Response(JSON.stringify(enrollmentResponse(body.device_id, 'device_1')), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    }
  });
  t.after(async () => {
    await manager.stop();
    for (const result of pendingResponses) result.destroy();
    await new Promise(resolve => wss.close(() => resolve()));
    tunnelServer.closeAllConnections?.();
    await closeServer(tunnelServer);
    if (localServer.listening) {
      localServer.closeAllConnections?.();
      await closeServer(localServer);
    }
  });
  await manager.start();
  await waitFor(() => readyCount === 1, 'timeout test worker did not become ready');

  sendRequest('refused-request', 'refused');
  await waitFor(() => responseFor('refused-request').errors.length === 1, 'refused local connection did not fail');
  await waitFor(() => readyCount === 2, 'worker was not reusable after refused connection');
  assert.match(responseFor('refused-request').errors[0], /ECONNREFUSED|connect/i);
  assert.equal(context.tunnelStatus.lastRequestTimeout, undefined);

  await new Promise(resolve => localServer.listen(localPort, '127.0.0.1', resolve));

  lookupMode = 'hang';
  sendRequest('cancel-during-connect', 'cancel-connect');
  await waitFor(() => hangingLookupCalls === 1, 'connect-cancel fixture did not enter DNS/connect phase');
  worker.send(JSON.stringify({ kind: 'cancel', request_id: 'cancel-during-connect' }));
  await waitFor(() => readyCount === 3, 'worker was not reusable after connect cancellation');
  assert.deepEqual(responseFor('cancel-during-connect').errors, []);
  assert.equal(context.tunnelStatus.lastRequestTimeout, undefined);

  sendRequest('connect-timeout-request', 'connect-timeout');
  await waitFor(
    () => responseFor('connect-timeout-request').errors.includes('local tunnel connection timed out'),
    'hanging local connection did not hit the connect timeout'
  );
  await waitFor(() => readyCount === 4, 'worker was not reusable after connect timeout');
  assert.equal(context.tunnelStatus.lastRequestTimeout, 'connect');
  assert.equal(typeof context.tunnelStatus.lastRequestTimeoutAt, 'number');

  lookupMode = 'normal';
  sendRequest('overall-timeout-request', 'headers');
  await waitFor(() => delayedHeadersStarted, 'overall-timeout fixture did not reach the local server');
  worker.ping(Buffer.from('timeout-heartbeat'));
  await waitFor(() => pongPayloads.includes('timeout-heartbeat'), 'heartbeat stalled during local response wait');
  await waitFor(
    () => responseFor('overall-timeout-request').errors.includes('local tunnel request timed out'),
    'delayed local response did not hit the overall timeout'
  );
  await waitFor(() => delayedHeadersClosed, 'overall timeout did not abort the delayed local response');
  await waitFor(() => readyCount === 5, 'worker was not reusable after overall timeout');
  assert.equal(context.tunnelStatus.lastRequestTimeout, 'overall');
  assert.equal(typeof context.tunnelStatus.lastRequestTimeoutAt, 'number');
  const boundedStatus = JSON.stringify(context.tunnelStatus);
  assert.doesNotMatch(boundedStatus, /connect-timeout-request|overall-timeout-request|phase=headers/);

  sendRequest('reuse-after-timeouts', 'normal');
  await waitFor(() => responseFor('reuse-after-timeouts').ended, 'worker could not complete a request after timeouts');
  await waitFor(() => readyCount === 6, 'worker did not return to ready after timeout reuse');
  assert.equal(Buffer.concat(responseFor('reuse-after-timeouts').chunks).toString('utf8'), 'ok:normal');
  assert.equal(context.tunnelStatus.completedRequests, 2);
  assert.equal(context.tunnelStatus.connectedWorkers, 1);
  assert.equal(context.tunnelStatus.idleWorkers, 1);
});

test('built-in tunnel cancels delayed local responses and keeps the worker reusable', async t => {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-tunnel-cancel-'));
  const stateFile = path.join(dataDir, 'identity.enc.json');
  const pendingResponses = new Set();
  const localState = {
    headersStarted: false,
    headersClosed: false,
    streamStarted: false,
    streamClosed: false
  };

  const server = createServer((request, result) => {
    const requestUrl = new URL(request.url ?? '/', 'http://127.0.0.1');
    const phase = requestUrl.searchParams.get('phase') ?? 'normal';
    pendingResponses.add(result);
    result.on('close', () => {
      pendingResponses.delete(result);
      if (phase === 'headers') localState.headersClosed = true;
      if (phase === 'stream') localState.streamClosed = true;
    });
    if (phase === 'headers') {
      localState.headersStarted = true;
      return;
    }
    if (phase === 'stream') {
      result.writeHead(200, { 'content-type': 'text/plain', 'x-cancel-phase': 'stream' });
      result.write('first-chunk');
      localState.streamStarted = true;
      return;
    }
    result.writeHead(200, { 'content-type': 'text/plain' });
    result.end(`ok:${phase}`);
  });
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  assert.ok(address && typeof address === 'object');

  const wss = new WebSocketServer({
    server,
    path: '/_tunnel/v1',
    handleProtocols(protocols) { return protocols.has(TUNNEL_SUBPROTOCOL) ? TUNNEL_SUBPROTOCOL : false; }
  });
  const policy = {
    ...defaultPolicy,
    start_workers: 1,
    min_idle_workers: 1,
    max_idle_workers: 1,
    max_workers: 1,
    max_requests_per_worker: 0,
    max_lifetime_seconds: 0,
    scale_down_delay_seconds: 0,
    max_connecting_workers: 1,
    scale_down_step: 1,
    burst_warm_workers: 1,
    burst_warm_seconds: 0,
    revision: 1
  };
  const responses = new Map();
  const pongPayloads = [];
  let worker;
  let readyCount = 0;
  let activeRequestId;

  const responseFor = requestId => {
    let response = responses.get(requestId);
    if (!response) {
      response = { heads: [], chunks: [], ended: false, errors: [] };
      responses.set(requestId, response);
    }
    return response;
  };
  const sendRequest = (requestId, phase) => {
    assert.ok(worker && worker.readyState === WebSocket.OPEN);
    activeRequestId = requestId;
    responseFor(requestId);
    worker.send(JSON.stringify({
      kind: 'request_head',
      request_id: requestId,
      method: 'GET',
      path_and_query: `/builtin/clients/device_1/mcp?phase=${phase}`,
      headers: []
    }));
    worker.send(JSON.stringify({ kind: 'request_end', request_id: requestId }));
  };

  wss.on('connection', (socket, request) => {
    worker = socket;
    sendSignedServerChallenge(socket, request, 'cancel-test');
    socket.on('pong', data => pongPayloads.push(Buffer.from(data).toString('utf8')));
    socket.on('message', (data, isBinary) => {
      if (isBinary) {
        if (activeRequestId) responseFor(activeRequestId).chunks.push(Buffer.from(data));
        return;
      }
      const control = JSON.parse(data.toString());
      if (control.kind === 'authenticate') {
        sendSignedServerAck(socket, 'cancel-test', control, policy);
      } else if (control.kind === 'ready') {
        readyCount += 1;
        activeRequestId = undefined;
      } else if (control.kind === 'response_head') {
        activeRequestId = control.request_id;
        responseFor(control.request_id).heads.push(control);
      } else if (control.kind === 'response_end') {
        responseFor(control.request_id).ended = true;
      } else if (control.kind === 'error') {
        responseFor(control.request_id ?? activeRequestId ?? 'unknown').errors.push(control.message);
      }
    });
  });

  const config = {
    host: '127.0.0.1',
    port: address.port,
    publicBaseUrl: 'https://tunnel.example/builtin/clients/device_1',
    dataDir,
    permissionMode: 'guarded',
    sandbox: { enabled: true, backend: 'appcontainer', externalPaths: [], options: {} },
    oauth: { clientId: 'chatgpt', password: 'test password', tokenSecret: 'a sufficiently long test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: dataDir }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 },
    tunnel: {
      enabled: true,
      publicUrl: 'https://tunnel.example/builtin/clients/device_1/mcp',
      enrollmentUrl: 'https://tunnel.example/_tunnel/enroll/CANCELTEST',
      workers: 1,
      stateFile
    }
  };
  const context = await createToolContext(config);
  const manager = new BuiltinTunnelManager(config, context, {
    websocketUrlOverride: `ws://127.0.0.1:${address.port}/_tunnel/v1`,
    reconcileIntervalMs: 20,
    enrollmentFetch: async (_url, init) => {
      const body = JSON.parse(String(init?.body ?? '{}'));
      return new Response(JSON.stringify(enrollmentResponse(body.device_id, 'device_1')), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    }
  });
  t.after(async () => {
    await manager.stop();
    for (const result of pendingResponses) result.destroy();
    await new Promise(resolve => wss.close(() => resolve()));
    server.closeAllConnections?.();
    await closeServer(server);
  });
  await manager.start();
  await waitFor(() => readyCount === 1, 'cancellation test worker did not become ready');

  sendRequest('cancel-before-headers', 'headers');
  await waitFor(() => localState.headersStarted, 'local delayed-header request did not start');
  worker.ping(Buffer.from('headers'));
  await waitFor(() => pongPayloads.includes('headers'), 'worker did not answer heartbeat ping while awaiting headers');
  worker.send(JSON.stringify({ kind: 'cancel', request_id: 'cancel-before-headers' }));
  await waitFor(() => localState.headersClosed, 'cancel did not abort the local fetch before headers');
  await waitFor(() => readyCount === 2, 'worker did not return to ready after header cancellation');
  const headerCancel = responseFor('cancel-before-headers');
  assert.equal(headerCancel.heads.length, 0);
  assert.equal(headerCancel.ended, false);
  assert.deepEqual(headerCancel.errors, []);

  sendRequest('reuse-after-headers', 'reuse-headers');
  await waitFor(() => responseFor('reuse-after-headers').ended, 'worker could not complete a request after header cancellation');
  await waitFor(() => readyCount === 3, 'worker did not return to ready after header-cancel reuse');
  assert.equal(Buffer.concat(responseFor('reuse-after-headers').chunks).toString('utf8'), 'ok:reuse-headers');

  sendRequest('cancel-during-stream', 'stream');
  await waitFor(() => localState.streamStarted, 'local streaming response did not start');
  await waitFor(() => responseFor('cancel-during-stream').heads.length === 1, 'streaming response head was not forwarded');
  await waitFor(
    () => Buffer.concat(responseFor('cancel-during-stream').chunks).toString('utf8') === 'first-chunk',
    'first streaming response chunk was not forwarded'
  );
  worker.ping(Buffer.from('stream'));
  await waitFor(() => pongPayloads.includes('stream'), 'worker did not answer heartbeat ping during response streaming');
  worker.send(JSON.stringify({ kind: 'cancel', request_id: 'cancel-during-stream' }));
  await waitFor(() => localState.streamClosed, 'cancel did not close the local streaming response');
  await waitFor(() => readyCount === 4, 'worker did not return to ready after stream cancellation');
  const streamCancel = responseFor('cancel-during-stream');
  assert.equal(streamCancel.ended, false);
  assert.deepEqual(streamCancel.errors, []);

  sendRequest('reuse-after-stream', 'reuse-stream');
  await waitFor(() => responseFor('reuse-after-stream').ended, 'worker could not complete a request after stream cancellation');
  await waitFor(() => readyCount === 5, 'worker did not return to ready after stream-cancel reuse');
  assert.equal(Buffer.concat(responseFor('reuse-after-stream').chunks).toString('utf8'), 'ok:reuse-stream');
  assert.equal(context.tunnelStatus.completedRequests, 4);
  assert.equal(context.tunnelStatus.connectedWorkers, 1);
  assert.equal(context.tunnelStatus.idleWorkers, 1);
  assert.equal(context.tunnelStatus.busyWorkers, 0);
});
