import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { PNG } from 'pngjs';
import { createAgentServer } from '../dist/server.js';
import { CLIENT_COMPAT_VERSION } from '../dist/version.js';

async function rpc(endpoint, token, request) {
  const response = await fetch(endpoint, {
    method: 'POST',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify(request)
  });
  assert.equal(response.status, 200);
  return response.json();
}

async function rpcResponse(endpoint, token, request, extraHeaders = {}) {
  const response = await fetch(endpoint, {
    method: 'POST',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json', ...extraHeaders },
    body: JSON.stringify(request)
  });
  assert.equal(response.status, 200);
  return { response, body: await response.json() };
}

test('scoped OAuth PKCE and MCP workspace flow', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-http-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-http-state-'));
  await writeFile(path.join(root, 'hello.txt'), 'hello node agent');
  const largePayload = `HTTP_LARGE_MARKER:${'x'.repeat(32 * 1024)}`;
  const httpSecret = 'sk-http-contract-secret-abcdefghijklmnopqrstuvwxyz';
  await writeFile(path.join(root, 'large.txt'), largePayload);
  await writeFile(path.join(root, '.env'), httpSecret);
  const image = new PNG({ width: 2, height: 1 });
  image.data = Buffer.from([255, 0, 0, 255, 0, 0, 255, 255]);
  await writeFile(path.join(root, 'pixel.bin'), PNG.sync.write(image));
  const prefix = '/builtin/clients/node-test';
  const publicBaseUrl = `https://public.example${prefix}`;
  const config = {
    host: '127.0.0.1', port: 0, publicBaseUrl, dataDir, permissionMode: 'trusted',
    oauth: { clientId: 'chatgpt', password: 'test-password', tokenSecret: 'a sufficiently long test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 }
  };
  const server = await createAgentServer(config);
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => server.close(resolve)));
  const address = server.address();
  assert.ok(address && typeof address === 'object');
  const localBase = `http://127.0.0.1:${address.port}`;

  const health = await fetch(`${localBase}${prefix}/health`).then(response => response.json());
  assert.equal(health.clientCompatVersion, CLIENT_COMPAT_VERSION);
  const info = await fetch(`${localBase}${prefix}/mcp/info`).then(response => response.json());
  assert.equal(info.clientCompatVersion, CLIENT_COMPAT_VERSION);

  const metadata = await fetch(`${localBase}/.well-known/oauth-authorization-server${prefix}`).then(response => response.json());
  assert.equal(metadata.issuer, publicBaseUrl);
  assert.equal(metadata.authorization_endpoint, `${publicBaseUrl}/oauth/authorize`);
  const resource = await fetch(`${localBase}/.well-known/oauth-protected-resource${prefix}/mcp`).then(response => response.json());
  assert.equal(resource.resource, `${publicBaseUrl}/mcp`);

  const verifier = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~';
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  const redirectUri = 'https://chatgpt.com/connector_platform_oauth_redirect';
  const form = new URLSearchParams({ client_id: 'chatgpt', redirect_uri: redirectUri, code_challenge: challenge, code_challenge_method: 'S256', state: 'state-1', password: 'test-password' });
  const authorized = await fetch(`${localBase}${prefix}/oauth/authorize`, { method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' }, body: form, redirect: 'manual' });
  assert.equal(authorized.status, 303);
  const callback = new URL(authorized.headers.get('location'));
  assert.equal(callback.searchParams.get('state'), 'state-1');
  const code = callback.searchParams.get('code');
  assert.ok(code);

  const tokenResponse = await fetch(`${localBase}${prefix}/oauth/token`, {
    method: 'POST', headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: new URLSearchParams({ grant_type: 'authorization_code', code, redirect_uri: redirectUri, code_verifier: verifier, client_id: 'chatgpt' })
  });
  assert.equal(tokenResponse.status, 200);
  const { access_token: token } = await tokenResponse.json();
  assert.ok(token);

  const endpoint = `${localBase}${prefix}/mcp`;
  const initialized = await rpc(endpoint, token, { jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-11-25' } });
  assert.equal(initialized.result.protocolVersion, '2025-11-25');
  const listedResponse = await rpcResponse(endpoint, token, { jsonrpc: '2.0', id: 2, method: 'tools/list', params: {} });
  const listedTools = listedResponse.body;
  assert.equal(listedTools.result.tools.length, 50);
  assert.equal(listedResponse.response.headers.get('x-coding-tools-toolset-revision'), listedTools.result.toolsetRevision);
  assert.equal(listedResponse.response.headers.get('x-coding-tools-agent-version'), initialized.result.serverInfo.version);
  assert.ok(Number(listedResponse.response.headers.get('x-coding-tools-runtime-started-at')) > 0);

  const staleCatalog = await rpc(endpoint, token, {
    jsonrpc: '2.0', id: 22, method: 'tools/call',
    params: {
      name: 'list_workspace_folders',
      arguments: {},
      _meta: { 'coding-tools/toolset-revision': 'stale-revision' }
    }
  });
  assert.equal(staleCatalog.result, undefined);
  assert.equal(staleCatalog.error.code, -32602);
  assert.equal(staleCatalog.error.data.error_code, 'TOOLSET_REVISION_MISMATCH');
  assert.equal(staleCatalog.error.data.reason, 'stale_tool_catalog');
  assert.equal(staleCatalog.error.data.toolset_revision, listedTools.result.toolsetRevision);

  const missingMetaList = await rpc(endpoint, token, {
    jsonrpc: '2.0', id: 20, method: 'tools/call',
    params: { name: 'list_workspace_folders', arguments: {} }
  });
  assert.equal(missingMetaList.result.structuredContent.selected_folder_id, null);
  assert.equal(missingMetaList.result.structuredContent.selection_scope, 'unselected');
  assert.equal(missingMetaList.result.structuredContent.conversation_source, 'missing_mcp_conversation');
  const missingMetaSwitch = await rpc(endpoint, token, {
    jsonrpc: '2.0', id: 21, method: 'tools/call',
    params: { name: 'switch_workspace_folder', arguments: { folder_id: 'repo' } }
  });
  assert.equal(missingMetaSwitch.result.isError, true);
  assert.equal(missingMetaSwitch.result.structuredContent.error.code, 'WORKSPACE_FOLDER_NOT_SELECTED');

  const meta = { 'openai/session': 'integration' };
  const list = await rpc(endpoint, token, { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'list_workspace_folders', arguments: {}, _meta: meta } });
  assert.equal(list.result.structuredContent.folders.length, 1);
  const selected = await rpc(endpoint, token, { jsonrpc: '2.0', id: 4, method: 'tools/call', params: { name: 'switch_workspace_folder', arguments: { folder_id: 'repo' }, _meta: meta } });
  assert.equal(selected.result.structuredContent.ok, true);

  const read = await rpc(endpoint, token, { jsonrpc: '2.0', id: 5, method: 'tools/call', params: { name: 'read_file', arguments: { path: 'hello.txt' }, _meta: meta } });
  assert.equal(read.result.content.length, 1);
  assert.equal(read.result.content[0].type, 'text');
  assert.ok(Buffer.byteLength(read.result.content[0].text) < 128);
  assert.doesNotMatch(read.result.content[0].text, /hello node agent/);
  assert.equal(read.result.structuredContent.content, 'hello node agent');

  const viewed = await rpc(endpoint, token, { jsonrpc: '2.0', id: 6, method: 'tools/call', params: { name: 'view_image', arguments: { path: 'pixel.bin' }, _meta: meta } });
  assert.equal(viewed.result.content.length, 1);
  assert.equal(viewed.result.content[0].type, 'image');
  assert.equal(viewed.result.content[0].mimeType, 'image/png');
  assert.ok(viewed.result.content[0].data.length > 0);
  assert.equal(viewed.result.structuredContent.mime_type, 'image/png');
  assert.deepEqual([viewed.result.structuredContent.width, viewed.result.structuredContent.height], [2, 1]);
  assert.equal(viewed.result.structuredContent.content, undefined);
  assert.equal(viewed.result.structuredContent.base64, undefined);
  assert.equal(viewed.result.structuredContent.data_url, undefined);

  const largeRead = await rpc(endpoint, token, { jsonrpc: '2.0', id: 7, method: 'tools/call', params: { name: 'read_file', arguments: { path: 'large.txt' }, _meta: meta } });
  assert.equal(largeRead.result.structuredContent.content, largePayload);
  assert.ok(Buffer.byteLength(largeRead.result.content[0].text) < 128);
  assert.doesNotMatch(largeRead.result.content[0].text, /HTTP_LARGE_MARKER/);
  const largeSerialized = JSON.stringify(largeRead.result);
  assert.equal(largeSerialized.split('HTTP_LARGE_MARKER:').length - 1, 1);
  assert.ok(
    Buffer.byteLength(largeSerialized) <= Buffer.byteLength(JSON.stringify(largeRead.result.structuredContent)) + 1024
  );

  const missing = await rpc(endpoint, token, { jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'read_file', arguments: { path: 'missing.txt' }, _meta: meta } });
  const missingStructured = missing.result.structuredContent;
  assert.equal(missing.result.isError, true);
  assert.equal(missingStructured.ok, false);
  assert.equal(missingStructured.status, 'error');
  assert.equal(missingStructured.summary, missingStructured.error.message);
  assert.equal(missingStructured.error.code, 'NOT_FOUND');
  assert.equal(missingStructured.error.category, 'not_found');
  assert.equal(missingStructured.error.retryable, false);
  assert.deepEqual(missingStructured.error.details, {});
  assert.equal(missing.result.content[0].text, missingStructured.error.message);

  const secretRead = await rpc(endpoint, token, { jsonrpc: '2.0', id: 9, method: 'tools/call', params: { name: 'read_file', arguments: { path: '.env' }, _meta: meta } });
  assert.equal(secretRead.result.structuredContent.content, '[REDACTED]');
  assert.equal(secretRead.result.structuredContent.sensitive_data_redacted, true);
  assert.doesNotMatch(JSON.stringify(secretRead), new RegExp(httpSecret));

  const unknown = await rpc(endpoint, token, { jsonrpc: '2.0', id: 10, method: 'tools/call', params: { name: 'missing_tool', arguments: {}, _meta: meta } });
  assert.equal(unknown.result, undefined);
  assert.equal(unknown.error.code, -32602);
  assert.equal(unknown.error.data.error_code, 'UNKNOWN_TOOL');
  assert.equal(unknown.error.data.reason, 'unknown_tool');

  const assignedPrefix = '/builtin/clients/server-assigned';
  const assignedPublicBaseUrl = `https://public.example${assignedPrefix}`;
  config.publicBaseUrl = assignedPublicBaseUrl;
  const reassignedHealth = await fetch(`${localBase}${assignedPrefix}/health`).then(response => response.json());
  assert.equal(reassignedHealth.clientCompatVersion, CLIENT_COMPAT_VERSION);
  const reassignedMetadata = await fetch(`${localBase}/.well-known/oauth-authorization-server${assignedPrefix}`).then(response => response.json());
  assert.equal(reassignedMetadata.issuer, assignedPublicBaseUrl);
  assert.equal(reassignedMetadata.authorization_endpoint, `${assignedPublicBaseUrl}/oauth/authorize`);
});
