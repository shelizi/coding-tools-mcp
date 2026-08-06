import test from 'node:test';
import assert from 'node:assert/strict';
import {
  LATEST_MCP_PROTOCOL_VERSION,
  SUPPORTED_MCP_PROTOCOL_VERSIONS,
  mcpOriginAllowed
} from '../dist/mcpTransport.js';
import { forwardedTunnelRequestHeaders } from '../dist/tunnel.js';
import { createMcpFixture, mcpRequest, responseJson } from './mcpTestHelpers.mjs';

const ping = { jsonrpc: '2.0', id: 1, method: 'ping', params: {} };

async function assertTransportError(response, status, code, message) {
  assert.equal(response.status, status);
  assert.equal(response.headers.get('cache-control'), 'no-store');
  const body = await responseJson(response);
  assert.equal(body.jsonrpc, '2.0');
  assert.equal(body.id, null);
  assert.equal(body.error.code, code);
  assert.equal(body.error.message, message);
}

test('MCP Origin validation matches listener, public and ChatGPT allowlists', async t => {
  assert.equal(mcpOriginAllowed('http://192.0.2.10:4321', { host: '0.0.0.0', publicBaseUrl: '' }, 4321), true);
  assert.equal(mcpOriginAllowed('http://localhost:4321', { host: '0.0.0.0', publicBaseUrl: '' }, 4321), true);
  assert.equal(mcpOriginAllowed('http://192.0.2.10:4321', { host: '192.0.2.10', publicBaseUrl: '' }, 4321), true);
  assert.equal(mcpOriginAllowed('http://192.0.2.11:4321', { host: '192.0.2.10', publicBaseUrl: '' }, 4321), false);
  assert.equal(mcpOriginAllowed('http://localhost:4321', { host: '192.0.2.10', publicBaseUrl: '' }, 4321), false);
  assert.equal(mcpOriginAllowed('http://127.0.0.2:4321', { host: '127.0.0.1', publicBaseUrl: '' }, 4321), true);
  assert.equal(mcpOriginAllowed('http://[2001:db8::1]:4321', { host: '::', publicBaseUrl: '' }, 4321), true);
  assert.equal(mcpOriginAllowed('http://localhost:4321', { host: '::1', publicBaseUrl: '' }, 4321), true);
  const state = await createMcpFixture(t);
  const acceptedOrigins = [
    undefined,
    `${state.localBase}/ignored/path`,
    `http://localhost:${state.port}`,
    `${new URL(state.config.publicBaseUrl).origin}/another/path`,
    'https://chatgpt.com',
    'https://chat.openai.com'
  ];
  for (const origin of acceptedOrigins) {
    const response = await mcpRequest(state, ping, {
      headers: origin === undefined ? {} : { origin }
    });
    assert.equal(response.status, 200, origin ?? 'missing Origin');
    assert.deepEqual((await responseJson(response)).result, {});
  }

  const rejectedOrigins = [
    'https://attacker.example',
    `http://127.0.0.1:${state.port + 1}`,
    `https://127.0.0.1:${state.port}`,
    `http://example.test:${state.port}`,
    'null'
  ];
  for (const origin of rejectedOrigins) {
    await assertTransportError(
      await mcpRequest(state, ping, { headers: { origin } }),
      403,
      -32000,
      'Invalid Origin header'
    );
  }

  assert.equal(mcpOriginAllowed(undefined, state.config, state.port), true);
  assert.equal(mcpOriginAllowed('https://attacker.example', state.config, state.port), false);
  assert.equal(mcpOriginAllowed(' https://chatgpt.com ', state.config, state.port), true);
});

test('MCP protocol header accepts the Rust set and rejects invalid values before dispatch', async t => {
  const state = await createMcpFixture(t);
  assert.equal(LATEST_MCP_PROTOCOL_VERSION, '2025-11-25');
  assert.deepEqual([...SUPPORTED_MCP_PROTOCOL_VERSIONS], ['2025-11-25', '2025-06-18', '2025-03-26']);
  for (const version of SUPPORTED_MCP_PROTOCOL_VERSIONS) {
    const response = await mcpRequest(state, ping, {
      headers: { 'mcp-protocol-version': version }
    });
    assert.equal(response.status, 200, version);
  }

  await assertTransportError(
    await mcpRequest(state, ping, { headers: { 'mcp-protocol-version': '2024-01-01' } }),
    400,
    -32600,
    'Unsupported MCP protocol version: 2024-01-01'
  );
  await assertTransportError(
    await mcpRequest(state, ping, { headers: { 'mcp-protocol-version': '' } }),
    400,
    -32600,
    'Unsupported MCP protocol version: '
  );
  await assertTransportError(
    await mcpRequest(state, ping, { headers: { 'mcp-protocol-version': '2025-11-25, 2025-06-18' } }),
    400,
    -32600,
    'Unsupported MCP protocol version: 2025-11-25, 2025-06-18'
  );
});

test('MCP accepts exactly one valid JSON-RPC request, notification or client response', async t => {
  const state = await createMcpFixture(t);
  const invalid = [
    { value: [], message: 'The request body must be one JSON-RPC message' },
    { value: null, message: 'The request body must be one JSON-RPC message' },
    { value: 1, message: 'The request body must be one JSON-RPC message' },
    { value: { id: 1, method: 'ping' }, message: "jsonrpc must be '2.0'" },
    { value: { jsonrpc: '1.0', id: 1, method: 'ping' }, message: "jsonrpc must be '2.0'" },
    { value: { jsonrpc: '2.0', id: 1 }, message: 'Invalid JSON-RPC request, notification, or response' }
  ];
  for (const fixture of invalid) {
    await assertTransportError(
      await mcpRequest(state, fixture.value),
      400,
      -32600,
      fixture.message
    );
  }

  await assertTransportError(
    await mcpRequest(state, undefined, { rawBody: '{' }),
    400,
    -32700,
    'Parse error'
  );

  const notification = await mcpRequest(state, {
    jsonrpc: '2.0', method: 'notifications/initialized', params: {}
  });
  assert.equal(notification.status, 202);
  assert.equal(await notification.text(), '');

  const clientResponse = await mcpRequest(state, {
    jsonrpc: '2.0', id: 9, result: { accepted: true }
  });
  assert.equal(clientResponse.status, 202);
  assert.equal(await clientResponse.text(), '');
});

test('built-in tunnel forwards Origin and MCP protocol headers into local validation', () => {
  const headers = forwardedTunnelRequestHeaders([
    { name: 'Origin', value: 'https://attacker.example' },
    { name: 'MCP-Protocol-Version', value: '2024-01-01' },
    { name: 'Connection', value: 'keep-alive' },
    { name: 'X-Test', value: 'ok' }
  ]);
  assert.equal(headers.origin, 'https://attacker.example');
  assert.equal(headers['mcp-protocol-version'], '2024-01-01');
  assert.equal(headers.connection, undefined);
  assert.equal(headers['x-test'], 'ok');
});
