import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { identitySnapshot, IDENTITY_DIAGNOSTICS_ENV } from '../dist/server/mcp/identityDiagnostics.js';
import { createMcpFixture, mcpRequest } from './mcpTestHelpers.mjs';

test('identity snapshot records only key names and non-credential identity hashes', () => {
  const snapshot = identitySnapshot({
    authorization: 'Bearer credential-value', cookie: 'private-cookie-value',
    'mcp-session-id': 'transport-identity', 'x-session-token': 'excluded-token-value'
  }, { method: 'tools/call', params: {
    name: 'read_file', arguments: { path: 'private-path' },
    _meta: { 'openai/session': 'conversation-identity', 'gemini/session_token': 'excluded-meta-token' }
  } });
  assert.equal(snapshot.identity_candidates.length, 2);
  assert.equal(snapshot.identity_candidates[1].sha256, createHash('sha256').update('conversation-identity').digest('hex'));
  for (const raw of ['credential-value', 'private-cookie-value', 'transport-identity', 'excluded-token-value', 'conversation-identity', 'excluded-meta-token', 'private-path']) {
    assert.equal(JSON.stringify(snapshot).includes(raw), false, `raw value leaked: ${raw}`);
  }
  assert.deepEqual(identitySnapshot({}, {}).identity_candidates, []);
});

test('identity snapshot supports modern client info and bounds names and keys', () => {
  const snapshot = identitySnapshot({}, { method: 'server/discover', params: { _meta: {
    ...Object.fromEntries(Array.from({ length: 80 }, (_, index) => [`field-${index}`, 'ignored'])),
    'io.modelcontextprotocol/clientInfo': { name: 'gemini-cli\n', version: '1.2.3' }
  } } });
  assert.equal(snapshot.client_name, 'gemini-cli');
  assert.equal(snapshot.client_version, '1.2.3');
  assert.equal(snapshot.meta_keys.length, 64);
  assert.equal(snapshot.keys_truncated, true);
  assert.equal(identitySnapshot({}, { params: { clientInfo: { name: 'x'.repeat(1000) } } }).client_name.length, 128);
});

test('HTTP diagnostics are opt-in and capture a routing failure without leaking values', async t => {
  const original = process.env[IDENTITY_DIAGNOSTICS_ENV];
  t.after(() => { if (original === undefined) delete process.env[IDENTITY_DIAGNOSTICS_ENV]; else process.env[IDENTITY_DIAGNOSTICS_ENV] = original; });
  delete process.env[IDENTITY_DIAGNOSTICS_ENV];
  const state = await createMcpFixture(t);
  const logPath = path.join(state.runtime.context.config.dataDir, 'logs', 'diagnostics.jsonl');
  const ping = { jsonrpc: '2.0', id: 1, method: 'ping' };
  assert.equal((await mcpRequest(state, ping)).status, 200);
  await state.runtime.context.usageStore.flush();
  assert.equal((await readFile(logPath, 'utf8')).includes('mcp_identity_observed'), false);
  process.env[IDENTITY_DIAGNOSTICS_ENV] = '1';
  assert.equal((await mcpRequest(state, ping, { auth: false })).status, 401);
  await mcpRequest(state, { jsonrpc: '2.0', id: 2, method: 'initialize', params: {
    protocolVersion: '2025-11-25', clientInfo: { name: 'gemini-cli', version: 'test' }
  } });
  const response = await mcpRequest(state, { jsonrpc: '2.0', id: 3, method: 'tools/call', params: {
    name: 'read_file', arguments: { path: 'marker.txt' }, _meta: { 'gemini/conversation': 'private-conversation' }
  } }, { headers: { 'mcp-session-id': 'private-transport', 'x-session-token': 'private-session-token' } });
  const wire = await response.json();
  assert.equal(wire.result.structuredContent.error.code, 'WORKSPACE_FOLDER_NOT_SELECTED', JSON.stringify(wire));
  await state.runtime.context.usageStore.flush();
  const records = (await readFile(logPath, 'utf8')).trim().split('\n').map(line => JSON.parse(line))
    .filter(record => record.event === 'mcp_identity_observed');
  assert.equal(records.length, 2);
  assert.equal(records[0].client_name, 'gemini-cli');
  assert.equal(records[1].identity_candidates.length, 2);
  assert.ok(records[1].meta_keys.includes('gemini/conversation'));
  const encoded = JSON.stringify(records);
  for (const secret of [state.authorization, 'private-conversation', 'private-transport', 'private-session-token']) {
    assert.equal(encoded.includes(secret), false);
  }
});
