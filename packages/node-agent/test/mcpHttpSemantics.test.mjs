import test from 'node:test';
import assert from 'node:assert/strict';
import { createMcpFixture, mcpRequest, responseJson } from './mcpTestHelpers.mjs';

const ping = { jsonrpc: '2.0', id: 1, method: 'ping', params: {} };

test('MCP HTTP methods validate connection and auth before returning Allow: POST', async t => {
  const state = await createMcpFixture(t);
  const unauthorized = await mcpRequest(state, undefined, {
    auth: false,
    method: 'GET',
    body: false
  });
  assert.equal(unauthorized.status, 401);
  assert.match(unauthorized.headers.get('www-authenticate') ?? '', /^Bearer /);
  assert.equal(unauthorized.headers.get('cache-control'), 'no-store');

  for (const method of ['GET', 'DELETE', 'PATCH']) {
    const response = await mcpRequest(state, undefined, { method, body: false });
    assert.equal(response.status, 405, method);
    assert.equal(response.headers.get('allow'), 'POST');
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.equal(await response.text(), '');
  }

  const badOrigin = await mcpRequest(state, undefined, {
    method: 'GET',
    body: false,
    headers: { origin: 'https://attacker.example' }
  });
  assert.equal(badOrigin.status, 403);
  assert.equal((await responseJson(badOrigin)).error.code, -32000);
});

test('MCP notifications and client responses return 202 without an RPC body', async t => {
  const state = await createMcpFixture(t);
  const fixtures = [
    { jsonrpc: '2.0', method: 'notifications/initialized', params: {} },
    { jsonrpc: '2.0', method: 'unknown/notification', params: {} },
    { jsonrpc: '2.0', id: 11, result: { ok: true } },
    { jsonrpc: '2.0', id: 12, error: { code: -1, message: 'client-side' } }
  ];
  for (const payload of fixtures) {
    const response = await mcpRequest(state, payload);
    assert.equal(response.status, 202, JSON.stringify(payload));
    assert.equal(response.headers.get('cache-control'), 'no-store');
    assert.equal(await response.text(), '');
  }
});

test('transport errors use HTTP 400 or 403 while JSON-RPC method errors remain HTTP 200', async t => {
  const state = await createMcpFixture(t);
  const invalid = await mcpRequest(state, { jsonrpc: '2.0', id: 1 });
  assert.equal(invalid.status, 400);
  assert.equal((await responseJson(invalid)).error.code, -32600);

  const forbidden = await mcpRequest(state, ping, {
    headers: { origin: 'https://attacker.example' }
  });
  assert.equal(forbidden.status, 403);
  assert.equal((await responseJson(forbidden)).error.code, -32000);

  const unknown = await mcpRequest(state, {
    jsonrpc: '2.0', id: 13, method: 'unknown/method', params: {}
  });
  assert.equal(unknown.status, 200);
  assert.equal(unknown.headers.get('x-coding-tools-streaming'), '1');
  assert.equal(unknown.headers.get('x-accel-buffering'), 'no');
  const body = await responseJson(unknown);
  assert.equal(body.id, 13);
  assert.equal(body.error.code, -32601);
  assert.equal(body.error.message, 'Method not found: unknown/method');
});

test('MCP discovery reports streamable HTTP and the complete supported protocol set', async t => {
  const state = await createMcpFixture(t);
  const response = await fetch(state.infoEndpoint);
  assert.equal(response.status, 200);
  assert.equal(response.headers.get('cache-control'), 'no-store');
  const info = await responseJson(response);
  assert.equal(info.transport, 'streamable-http');
  assert.equal(info.protocolVersion, '2025-11-25');
  assert.deepEqual(info.supportedProtocolVersions, ['2025-11-25', '2025-06-18', '2025-03-26']);
  assert.equal(info.name, 'coding-tools-mcp-node');
});
