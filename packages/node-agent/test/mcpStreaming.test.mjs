import test from 'node:test';
import assert from 'node:assert/strict';
import { request as httpRequest } from 'node:http';
import path from 'node:path';
import { runtimeForFolderId } from '../dist/folderRuntime.js';
import {
  BoundedMcpStreamQueue,
  MCP_STREAM_CHANNEL_CAPACITY,
  MCP_STREAM_HEARTBEAT_INTERVAL_MS
} from '../dist/mcpTransport.js';
import { createMcpFixture, mcpRequest, responseJson } from './mcpTestHelpers.mjs';

const nodeProgram = path.basename(process.execPath);

async function selectWorkspace(state, session) {
  const response = await mcpRequest(state, {
    jsonrpc: '2.0',
    id: 1,
    method: 'tools/call',
    params: {
      name: 'switch_workspace_folder',
      arguments: { folder_id: 'repo' },
      _meta: { 'openai/session': session }
    }
  });
  assert.equal(response.status, 200);
  const body = await responseJson(response);
  assert.equal(body.result.structuredContent.ok, true, JSON.stringify(body));
}

function openStreamingRequest(state, payload, extraHeaders = {}) {
  const url = new URL(state.endpoint);
  const serialized = JSON.stringify(payload);
  let request;
  const response = new Promise((resolve, reject) => {
    request = httpRequest({
      protocol: url.protocol,
      hostname: url.hostname,
      port: url.port,
      path: `${url.pathname}${url.search}`,
      method: 'POST',
      headers: {
        authorization: state.authorization,
        'content-type': 'application/json',
        'content-length': Buffer.byteLength(serialized),
        ...extraHeaders
      }
    }, resolve);
    request.once('error', reject);
    request.end(serialized);
  });
  return { request, response };
}

function responseBody(response) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    response.on('data', chunk => chunks.push(Buffer.from(chunk)));
    response.once('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    response.once('error', reject);
  });
}

async function waitFor(read, message, timeoutMs = 3000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const value = read();
    if (value) return value;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  throw new Error(message);
}

test('bounded MCP stream queue drops excess heartbeats and preserves order while the final payload waits', () => {
  assert.equal(MCP_STREAM_HEARTBEAT_INTERVAL_MS, 10_000);
  assert.equal(MCP_STREAM_CHANNEL_CAPACITY, 2);
  const queue = new BoundedMcpStreamQueue();
  assert.equal(queue.enqueueHeartbeat(), true);
  assert.equal(queue.enqueueHeartbeat(), true);
  assert.equal(queue.enqueueHeartbeat(), false);
  assert.equal(queue.length, 2);
  assert.equal(queue.enqueueFinal('{"done":true}'), false);
  assert.equal(queue.length, 2);
  assert.deepEqual(queue.shift(), { payload: '\n', final: false });
  assert.equal(queue.enqueueFinal('{"done":true}'), true);
  assert.equal(queue.length, 2);
  assert.deepEqual(queue.shift(), { payload: '\n', final: false });
  assert.deepEqual(queue.shift(), { payload: '{"done":true}', final: true });
  assert.equal(queue.length, 0);
});

test('slow MCP tool calls flush headers, stream JSON whitespace heartbeats and finish once', async t => {
  const state = await createMcpFixture(t, { mcpHeartbeatIntervalMs: 40 });
  const session = `stream-${Date.now()}`;
  await selectWorkspace(state, session);
  const payload = {
    jsonrpc: '2.0',
    id: 2,
    method: 'tools/call',
    params: {
      name: 'exec_command',
      arguments: {
        program: nodeProgram,
        args: ['-e', 'setTimeout(() => process.stdout.write("done"), 500)'],
        yield_time_ms: 5000,
        timeout_ms: 5000
      },
      _meta: { 'openai/session': session }
    }
  };

  const started = Date.now();
  const opened = openStreamingRequest(state, payload);
  const response = await opened.response;
  const headersAt = Date.now();
  assert.equal(response.statusCode, 200);
  assert.equal(response.headers['content-type'], 'application/json');
  assert.equal(response.headers['cache-control'], 'no-store');
  assert.equal(response.headers['x-coding-tools-streaming'], '1');
  assert.equal(response.headers['x-accel-buffering'], 'no');

  let firstChunkAt;
  let firstChunk;
  response.once('data', chunk => {
    firstChunkAt = Date.now();
    firstChunk = Buffer.from(chunk).toString('utf8');
  });
  const body = await responseBody(response);
  const finishedAt = Date.now();
  assert.ok(headersAt - started < finishedAt - started - 200, `headers=${headersAt - started} total=${finishedAt - started}`);
  assert.ok(firstChunkAt < finishedAt - 150, `first=${firstChunkAt - started} total=${finishedAt - started}`);
  assert.match(firstChunk, /^\s+$/);
  const parsed = JSON.parse(body);
  assert.equal(parsed.id, 2);
  assert.ok(parsed.result);
  assert.equal((body.match(/\{/g) ?? []).length > 0, true);
});

test('modern subscriptions/listen opens SSE, acknowledges the honored filter, and stays alive with comments', async t => {
  const state = await createMcpFixture(t, { mcpHeartbeatIntervalMs: 40 });
  const subscriptionId = 41;
  const opened = openStreamingRequest(state, {
    jsonrpc: '2.0',
    id: subscriptionId,
    method: 'subscriptions/listen',
    params: {
      notifications: {
        toolsListChanged: true,
        promptsListChanged: true,
        resourcesListChanged: true,
        resourceSubscriptions: ['file:///unsupported.txt']
      },
      _meta: {
        'io.modelcontextprotocol/protocolVersion': '2026-07-28',
        'io.modelcontextprotocol/clientCapabilities': {}
      }
    }
  }, {
    'mcp-protocol-version': '2026-07-28',
    'mcp-method': 'subscriptions/listen'
  });
  const response = await opened.response;
  assert.equal(response.statusCode, 200);
  assert.equal(response.headers['content-type'], 'text/event-stream');
  assert.equal(response.headers['cache-control'], 'no-store');
  assert.equal(response.headers['x-accel-buffering'], 'no');

  let received = '';
  response.on('data', chunk => { received += Buffer.from(chunk).toString('utf8'); });
  await waitFor(
    () => received.includes('\n\n') && received.includes('notifications/subscriptions/acknowledged'),
    'subscription acknowledgement was not streamed'
  );
  const dataLine = received.split('\n').find(line => line.startsWith('data: '));
  assert.ok(dataLine);
  const acknowledged = JSON.parse(dataLine.slice('data: '.length));
  assert.equal(acknowledged.jsonrpc, '2.0');
  assert.equal(acknowledged.method, 'notifications/subscriptions/acknowledged');
  assert.deepEqual(acknowledged.params.notifications, {});
  assert.equal(
    acknowledged.params._meta['io.modelcontextprotocol/subscriptionId'],
    subscriptionId
  );

  await waitFor(() => received.includes(':\n\n'), 'subscription keepalive comment was not streamed');
  response.destroy();
  opened.request.destroy();
});

test('disconnecting a streaming MCP response aborts and detaches the retained process request', async t => {
  const state = await createMcpFixture(t, { mcpHeartbeatIntervalMs: 30 });
  const session = `disconnect-${Date.now()}`;
  await selectWorkspace(state, session);
  const opened = openStreamingRequest(state, {
    jsonrpc: '2.0',
    id: 3,
    method: 'tools/call',
    params: {
      name: 'exec_command',
      arguments: {
        program: nodeProgram,
        args: ['-e', 'setTimeout(() => process.stdout.write("late"), 5000)'],
        yield_time_ms: 10000,
        timeout_ms: 10000
      },
      _meta: { 'openai/session': session }
    }
  });
  const response = await opened.response;
  await waitFor(
    () => [...runtimeForFolderId(state.runtime.context, 'repo').sessions.values()][0],
    'streaming request did not retain a process session'
  );
  await new Promise(resolve => response.once('data', resolve));
  response.destroy();
  opened.request.destroy();

  const detached = await waitFor(
    () => [...runtimeForFolderId(state.runtime.context, 'repo').sessions.values()]
      .find(value => value.detachedGeneration !== 0),
    'streaming disconnect did not detach the retained process session'
  );
  assert.ok(detached.detachedGeneration > 0);
  assert.equal(detached.finalizedAt, undefined);
});
